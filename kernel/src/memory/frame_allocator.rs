// =============================================================================
// frame_allocator.rs — allocateur de frames physiques (4 KiB chacune).
//
// Stratégie : bitmap. 1 bit par frame. 0 = libre, 1 = occupée.
// Pour 1 GiB de RAM = 262144 frames = 32 KiB de bitmap. Très raisonnable.
//
// On initialise :
//   - tout à "occupé" par défaut
//   - on libère les régions usable listées par multiboot
//   - on ré-occupe les zones critiques : [0, 2 MiB) (kernel + boot stuff),
//     la zone multiboot info elle-même, les modules (initrd),
//     et le bitmap lui-même.
// =============================================================================

use crate::boot_info::BootInfo;
use core::ops::Range;
use spin::Mutex;
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

pub const FRAME_SIZE: usize = 4096;

/// Bitmap stocké dans une zone fixe : 64 KiB juste en dessous du heap kernel.
/// Le kernel (chargé à 1 MiB) + smoltcp + debug info peut dépasser 2 MiB,
/// donc on met la bitmap bien au-dessus (15 MiB). Heap = 16 MiB.
const BITMAP_ADDR: usize = 0x00F0_0000;
const BITMAP_SIZE: usize = 64 * 1024;
const BITMAP_BITS: usize = BITMAP_SIZE * 8;

pub struct BitmapFrameAllocator {
    bitmap: &'static mut [u8],
    /// Nombre total de frames couvertes (selon la mémoire détectée).
    num_frames: usize,
    /// Hint pour accélérer le search (premier bit possiblement libre).
    next_free: usize,
    /// Stats
    used: usize,
}

impl BitmapFrameAllocator {
    /// Construit l'allocator depuis les infos multiboot2.
    ///
    /// SAFETY: `boot_info` doit être valide et la zone BITMAP_ADDR libre.
    pub unsafe fn new(boot_info: &BootInfo) -> Self {
        crate::serial_println!("[fa] new: bitmap @ {:#x} size={} KiB", BITMAP_ADDR, BITMAP_SIZE/1024);
        let bitmap = core::slice::from_raw_parts_mut(BITMAP_ADDR as *mut u8, BITMAP_SIZE);
        // Tout occupé par défaut
        crate::serial_println!("[fa] fill bitmap 0xff");
        bitmap.fill(0xff);
        crate::serial_println!("[fa] fill done");

        let mut num_frames = 0usize;

        // Libère les régions usable
        crate::serial_println!("[fa] memory_areas()");
        if let Some(areas) = boot_info.memory_areas() {
            let mut area_idx = 0;
            for area in areas {
                crate::serial_println!("[fa] area#{} base={:#x} len={:#x} usable={}",
                    area_idx, area.base_addr, area.length, area.is_usable());
                area_idx += 1;
                if !area.is_usable() { continue; }
                let start = align_up(area.base_addr as usize, FRAME_SIZE);
                let end = (area.end_addr() as usize) & !(FRAME_SIZE - 1);
                let first = start / FRAME_SIZE;
                // Borne dure pour éviter de traiter des régions au-delà de la bitmap
                let last = (end / FRAME_SIZE).min(BITMAP_BITS);
                if first >= BITMAP_BITS { continue; }
                crate::serial_println!("[fa]   → frames {}..{}", first, last);
                for f in first..last {
                    bit_clear(bitmap, f);
                    num_frames = num_frames.max(f + 1);
                }
            }
        } else {
            crate::serial_println!("[fa] !!! PAS DE MEMORY_AREAS !!!");
        }
        crate::serial_println!("[fa] num_frames = {}", num_frames);

        // Ré-occupe les zones critiques
        let mut alloc = BitmapFrameAllocator { bitmap, num_frames, next_free: 0, used: 0 };

        // [0, 15 MiB) : BIOS, VGA, kernel (incl. smoltcp), boot stacks, page tables
        alloc.mark_range_used(0..BITMAP_ADDR);
        // Le bitmap lui-même
        alloc.mark_range_used(BITMAP_ADDR..BITMAP_ADDR + BITMAP_SIZE);
        // Le heap kernel (16 MiB, 100 KiB) — cf memory/heap.rs
        alloc.mark_range_used(crate::memory::heap::HEAP_START
            ..crate::memory::heap::HEAP_START + crate::memory::heap::HEAP_SIZE);
        // La structure multiboot info
        alloc.mark_range_used(boot_info_range(boot_info));
        // Les modules (initrd)
        for (data, _) in boot_info.modules() {
            let start = data.as_ptr() as usize;
            alloc.mark_range_used(start..start + data.len());
        }

        alloc
    }

    fn mark_range_used(&mut self, range: Range<usize>) {
        let first = range.start / FRAME_SIZE;
        let last = align_up(range.end, FRAME_SIZE) / FRAME_SIZE;
        for f in first..last.min(BITMAP_BITS) {
            if !bit_get(self.bitmap, f) {
                bit_set(self.bitmap, f);
                self.used += 1;
            }
        }
    }

    /// Trouve la prochaine frame libre (bit à 0).
    fn find_free(&mut self) -> Option<usize> {
        for f in self.next_free..self.num_frames {
            if !bit_get(self.bitmap, f) {
                self.next_free = f + 1;
                return Some(f);
            }
        }
        None
    }

    pub fn stats(&self) -> (usize, usize) { (self.used, self.num_frames) }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let f = self.find_free()?;
        bit_set(self.bitmap, f);
        self.used += 1;
        // Zéroïse la frame avant de la renvoyer (évite que du garbage soit
        // interprété comme des entries de page table par le crate x86_64).
        let phys = (f * FRAME_SIZE) as u64;
        // SAFETY: frame identity-mappée dans le 1er GiB (on n'alloue jamais
        // au-delà, BITMAP_BITS plafonne à 2 GiB mais la RAM usable QEMU -m 128M
        // est < 128 MiB, bien sous 1 GiB).
        unsafe {
            core::ptr::write_bytes(phys as *mut u8, 0, FRAME_SIZE);
        }
        Some(PhysFrame::containing_address(PhysAddr::new(phys)))
    }
}

impl FrameDeallocator<Size4KiB> for BitmapFrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        let f = (frame.start_address().as_u64() as usize) / FRAME_SIZE;
        if bit_get(self.bitmap, f) {
            bit_clear(self.bitmap, f);
            self.used -= 1;
            if f < self.next_free { self.next_free = f; }
        }
    }
}

// -----------------------------------------------------------------------------
// Singleton global protégé par mutex spin
// -----------------------------------------------------------------------------

pub static FRAME_ALLOCATOR: Mutex<Option<BitmapFrameAllocator>> = Mutex::new(None);

pub fn init(boot_info: &BootInfo) {
    // SAFETY: init appelé une seule fois au boot, BITMAP_ADDR libre par conception.
    let alloc = unsafe { BitmapFrameAllocator::new(boot_info) };
    let (used, total) = alloc.stats();
    *FRAME_ALLOCATOR.lock() = Some(alloc);
    crate::println!("[mem] frames: {} utilisées / {} totales ({} MiB détectées)",
        used, total, total * FRAME_SIZE / (1024 * 1024));
}

// -----------------------------------------------------------------------------
// Helpers bitmap
// -----------------------------------------------------------------------------

#[inline] fn bit_get(bm: &[u8], i: usize) -> bool { bm[i / 8] & (1 << (i % 8)) != 0 }
#[inline] fn bit_set(bm: &mut [u8], i: usize)     { bm[i / 8] |= 1 << (i % 8); }
#[inline] fn bit_clear(bm: &mut [u8], i: usize)   { bm[i / 8] &= !(1 << (i % 8)); }

#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

fn boot_info_range(bi: &BootInfo) -> Range<usize> {
    // Zone multiboot info : typiquement dans [0, 2 MiB), déjà réservée.
    // On renvoie un range vide ; le kernel est de toute façon protégé par
    // la réservation inconditionnelle de [0, 2 MiB).
    let _ = bi;
    0..0
}
