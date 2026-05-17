// =============================================================================
// gfx — interface kernel ↔ display-server.
//
// Trois syscalls exposés :
//   FB_ACQUIRE — réserve l'unique handle scanout au caller (display-server)
//   FB_PRESENT — flush dirty rect du backbuffer caller-mappé vers la scanout
//   INPUT_POLL — drain la queue events kernel vers un buffer user
//
// Garantie d'exclusivité : un seul process à la fois peut détenir le FB.
// Toute autre tentative renvoie -EBUSY.
//
// Frontière nette : ce module ne sait rien de "fenêtre", "focus", "z-order".
// Il fournit juste un linear framebuffer + une queue d'events bruts, et
// laisse au display-server (user space) toute la sémantique au-dessus.
// =============================================================================

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};

use crate::memory::paging::{self, PHYS_OFFSET};
use crate::task::process::{self, Pid};

mod fb_info;
mod input_queue;

pub use fb_info::FbInfoAbi;
pub use input_queue::{push_event, InputEventAbi};

/// PID du process qui détient actuellement le FB (0 = libre).
static FB_OWNER: AtomicU64 = AtomicU64::new(0);

/// Lock d'arbitrage pour acquire/release atomiques + bookkeeping.
static FB_LOCK: Mutex<()> = Mutex::new(());

/// Frames physiques persistantes du backbuffer partagé (allouées au 1er ACQUIRE,
/// jamais libérées — réutilisées entre owners successifs).
static FB_BACKBUF_FRAMES: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// VA du backbuffer dans l'espace du owner courant (0 si pas mappé).
static FB_BACKBUF_VA: AtomicU64 = AtomicU64::new(0);
/// Taille du backbuffer en bytes (set au 1er ACQUIRE).
static FB_BACKBUF_LEN: AtomicU64 = AtomicU64::new(0);
/// VA fixe où le backbuffer est exposé chez le display-server.
const FB_USER_VA: u64 = 0x0000_5000_0000_0000;

// -----------------------------------------------------------------------------
// FB_ACQUIRE
// -----------------------------------------------------------------------------
//
// Sortie : remplit le FbInfoAbi pointé par `out_ptr`. Sur succès retourne 0.
// Sémantique : le caller obtient l'exclusivité du scanout. Le kernel n'expose
// PAS le pointeur MMIO direct ; il expose le backbuffer RAM (le `present_buf`
// du driver fb) mappé dans l'espace virtuel du caller via une fenêtre user.
//
// Stub : pour Phase V step 1, on remplit FbInfoAbi avec width/height/pitch
// du framebuffer kernel mais buffer_ptr=0 (pas encore de mmap user). Le mmap
// arrivera quand on aura sys_shm_map fonctionnel + une frame physique stable
// pour le backbuffer.
pub fn sys_fb_acquire(out_ptr: u64) -> u64 {
    let _g = FB_LOCK.lock();

    let caller = process::PROCS.lock().current_pid() as u64;
    let prev = FB_OWNER.load(Ordering::Acquire);
    if prev != 0 && prev != caller {
        return u64::MAX; // EBUSY — un autre process détient déjà le FB
    }
    FB_OWNER.store(caller, Ordering::Release);

    // Dimensions & pitch du framebuffer kernel.
    let (width, height, pitch) = match crate::drivers::fb::fb() {
        Some(fb_mx) => {
            let fb = fb_mx.lock();
            (fb.width(), fb.height(), fb.pitch_bytes())
        }
        None => return u64::MAX,
    };
    let buffer_len = pitch as u64 * height as u64;
    let pages = (buffer_len + 4095) / 4096;

    // Alloue les frames une seule fois (lazy), réutilisées pour tous les
    // owners successifs.
    {
        let mut frames = FB_BACKBUF_FRAMES.lock();
        if frames.is_empty() {
            for _ in 0..pages {
                match paging::alloc_zeroed_frame() {
                    Ok(pf) => frames.push(pf.start_address().as_u64()),
                    Err(_) => {
                        // Rollback partiel.
                        for &pa in frames.iter() {
                            if let Ok(pf) = PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(pa)) {
                                paging::free_frame(pf);
                            }
                        }
                        frames.clear();
                        FB_OWNER.store(0, Ordering::Release);
                        return u64::MAX;
                    }
                }
            }
            FB_BACKBUF_LEN.store(buffer_len, Ordering::Release);
        }
    }

    // Map le backbuffer dans l'AS du caller à FB_USER_VA.
    let frames_copy: Vec<u64> = FB_BACKBUF_FRAMES.lock().clone();
    {
        let mut table = process::PROCS.lock();
        let proc = match table.current() {
            Some(p) => p, None => {
                FB_OWNER.store(0, Ordering::Release);
                return u64::MAX;
            }
        };
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;
        for (i, &pa) in frames_copy.iter().enumerate() {
            let va = FB_USER_VA + i as u64 * 4096;
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
            let frame = match PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(pa)) {
                Ok(f) => f, Err(_) => {
                    FB_OWNER.store(0, Ordering::Release);
                    return u64::MAX;
                }
            };
            if proc.address_space.map_to(page, frame, flags).is_err() {
                // Si déjà mappé (re-ACQUIRE par le même owner), on tolère.
            }
        }
    }
    FB_BACKBUF_VA.store(FB_USER_VA, Ordering::Release);

    let info = FbInfoAbi {
        buffer_ptr: FB_USER_VA,
        buffer_len,
        width, height, pitch,
        format: fb_info::PIXEL_FORMAT_BGRA8888,
        caps:   fb_info::CAP_DOUBLE_BUF,
        _reserved: 0,
    };

    // Validation user pointer.
    let dst = match crate::syscall::syscall_user_slice_mut(out_ptr, core::mem::size_of::<FbInfoAbi>() as u64) {
        Some(s) => s,
        None => return u64::MAX,
    };
    // SAFETY: dst.len() == size_of::<FbInfoAbi>(), repr(C), aligné u32 — OK
    // pour un write par memcpy.
    let bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(
            &info as *const _ as *const u8,
            core::mem::size_of::<FbInfoAbi>(),
        )
    };
    dst.copy_from_slice(bytes);
    0
}

// -----------------------------------------------------------------------------
// FB_PRESENT
// -----------------------------------------------------------------------------
//
// Le caller a écrit dans le backbuffer mappé ; il demande la copie vers la
// scanout. Pour Phase V step 1 (pas encore de mmap user), on flushe la
// console kernel : le display-server prendra le relais une fois le mmap fait.
pub fn sys_fb_present(rect_ptr: u64) -> u64 {
    let caller = process::PROCS.lock().current_pid() as u64;
    if FB_OWNER.load(Ordering::Acquire) != caller {
        return u64::MAX; // EPERM
    }

    // Lire la Rect (16 bytes : 4×u32). 0 = full present (ignoré : on copie tout).
    let _rect: (u32, u32, u32, u32) = if rect_ptr == 0 {
        (0, 0, 0, 0)
    } else {
        let bytes = match crate::syscall::syscall_user_slice(rect_ptr, 16) {
            Some(b) => b, None => return u64::MAX,
        };
        let r = |o: usize| u32::from_le_bytes([bytes[o], bytes[o+1], bytes[o+2], bytes[o+3]]);
        (r(0), r(4), r(8), r(12))
    };

    // Blit le backbuffer partagé (frames identity-mappées via PHYS_OFFSET)
    // vers le present_buf du driver fb, puis pipeline standard.
    let frames = FB_BACKBUF_FRAMES.lock().clone();
    let buffer_len = FB_BACKBUF_LEN.load(Ordering::Acquire);
    if let Some(fb_mx) = crate::drivers::fb::fb() {
        let mut fb = fb_mx.lock();
        let pitch_u32 = fb.pitch_bytes() / 4;
        let width = fb.width();
        let height = fb.height();
        // SAFETY: frames physiques identity-mappées, présentes pour la durée
        // de vie du backbuffer (alloué au 1er ACQUIRE).
        let mut copied = 0u64;
        for (i, &pa) in frames.iter().enumerate() {
            let src_base = (pa + PHYS_OFFSET) as *const u32;
            let bytes_in_page = (4096u64).min(buffer_len - copied);
            let words = (bytes_in_page / 4) as usize;
            unsafe {
                for j in 0..words {
                    let offset_bytes = i as u64 * 4096 + j as u64 * 4;
                    let pixel_index = offset_bytes / 4;
                    let y = (pixel_index / pitch_u32 as u64) as u32;
                    let x = (pixel_index % pitch_u32 as u64) as u32;
                    if y < height && x < width {
                        let px = core::ptr::read_volatile(src_base.add(j));
                        fb.put_pixel(x, y, px);
                    }
                }
            }
            copied += bytes_in_page;
        }
        fb.commit();
        fb.present();
    }
    0
}

// -----------------------------------------------------------------------------
// INPUT_POLL
// -----------------------------------------------------------------------------
//
// Drain jusqu'à `max` événements vers `buf`. Retourne le nombre effectif lu.
// Non-bloquant : si la queue est vide, retourne 0 immédiatement.
pub fn sys_input_poll(buf_ptr: u64, max: u64) -> u64 {
    let caller = process::PROCS.lock().current_pid() as u64;
    if FB_OWNER.load(Ordering::Acquire) != caller {
        return u64::MAX; // EPERM — réservé au display-server
    }
    let evt_size = core::mem::size_of::<InputEventAbi>() as u64;
    let total = match max.checked_mul(evt_size) {
        Some(t) => t, None => return u64::MAX,
    };
    let dst = match crate::syscall::syscall_user_slice_mut(buf_ptr, total) {
        Some(s) => s, None => return u64::MAX,
    };

    let mut written = 0u64;
    while written < max {
        let Some(evt) = input_queue::pop_event() else { break };
        let off = (written * evt_size) as usize;
        let bytes: &[u8] = unsafe {
            core::slice::from_raw_parts(
                &evt as *const _ as *const u8,
                evt_size as usize,
            )
        };
        dst[off..off + evt_size as usize].copy_from_slice(bytes);
        written += 1;
    }
    written
}

/// Hook process::exit : si le process qui meurt détenait le FB, on libère.
/// Le mapping du backbuffer dans son AS est détruit avec la P4 du process.
/// Les frames du backbuffer restent allouées (pool persistant cross-owners).
pub fn release_if_owner(pid: Pid) {
    let _g = FB_LOCK.lock();
    if FB_OWNER.load(Ordering::Acquire) == pid as u64 {
        FB_OWNER.store(0, Ordering::Release);
        FB_BACKBUF_VA.store(0, Ordering::Release);
    }
}
