// =============================================================================
// paging.rs — gestion mémoire virtuelle (Phase II : user + per-process).
//
// Au boot : identity map des 1ers 1 GiB via huge pages 2 MiB.
// On maintient le "kernel mapper" pour accéder aux structures du kernel,
// et on expose de quoi construire / manipuler des page tables **arbitraires**
// (pas uniquement la courante) pour chaque process.
//
// Ajouts Phase II :
//   - flags standards user : USER_RW = PRESENT | WRITABLE | USER_ACCESSIBLE
//   - flags CoW : USER_RO_COW = PRESENT | USER_ACCESSIBLE (pas WRITABLE) + bit OS_9
//   - `OffsetPageTable::new` réutilisable via `new_offset_mapper(frame)` pour
//     taper dans la page table d'un autre process (identity-mapée côté phys)
//   - `current_cr3()` / `switch_cr3(frame)`
// =============================================================================

use spin::Mutex;
use x86_64::{
    registers::control::{Cr3, Cr3Flags},
    structures::paging::{
        FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
        PhysFrame, Size4KiB, Translate,
    },
    PhysAddr, VirtAddr,
};

/// Offset du mapping physique -> virtuel. Identity map -> 0.
pub const PHYS_OFFSET: u64 = 0;

/// Flags couramment utilisés.
pub const KERNEL_RW: PageTableFlags = PageTableFlags::from_bits_truncate(
    PageTableFlags::PRESENT.bits() | PageTableFlags::WRITABLE.bits()
);
pub const USER_RW: PageTableFlags = PageTableFlags::from_bits_truncate(
    PageTableFlags::PRESENT.bits()
        | PageTableFlags::WRITABLE.bits()
        | PageTableFlags::USER_ACCESSIBLE.bits()
);
pub const USER_RX: PageTableFlags = PageTableFlags::from_bits_truncate(
    PageTableFlags::PRESENT.bits()
        | PageTableFlags::USER_ACCESSIBLE.bits()
);

/// Bit marqueur "CoW" logé dans les bits "disponibles" de l'entrée de page.
/// PageTableFlags::BIT_9..BIT_11 sont disponibles pour l'OS. On prend BIT_9.
pub const COW_MARKER: PageTableFlags = PageTableFlags::BIT_9;

static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);

/// Init le mapper à partir de CR3. À appeler une seule fois au boot.
///
/// SAFETY: doit être appelé avec paging actif et CR3 pointant sur une P4 valide.
pub unsafe fn init() {
    let (level_4_frame, _) = Cr3::read();
    let phys = level_4_frame.start_address();
    let virt = VirtAddr::new(phys.as_u64() + PHYS_OFFSET);
    let page_table: &'static mut PageTable = &mut *virt.as_mut_ptr();
    *MAPPER.lock() = Some(OffsetPageTable::new(page_table, VirtAddr::new(PHYS_OFFSET)));
    crate::println!("[mem] page table kernel active, CR3 = {:#x}", phys.as_u64());
}

/// Map une page virtuelle sur une frame physique donnée, dans la page table KERNEL.
pub fn map_to(
    page: Page<Size4KiB>,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let mut mapper_guard = MAPPER.lock();
    let mapper = mapper_guard.as_mut().ok_or("paging non initialisé")?;
    let mut fa_guard = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
    let fa = fa_guard.as_mut().ok_or("frame alloc non initialisé")?;

    // SAFETY: frame fournie par l'appelant, table kernel protégée par mutex.
    unsafe {
        mapper.map_to(page, frame, flags, fa)
            .map_err(|_| "map_to failed")?
            .flush();
    }
    Ok(())
}

/// Alloue une nouvelle frame et la map à l'adresse virtuelle donnée (table kernel).
pub fn alloc_and_map(page: Page<Size4KiB>, flags: PageTableFlags) -> Result<(), &'static str> {
    let frame = alloc_frame()?;
    map_to(page, frame, flags)
}

/// Traduit une adresse virtuelle (table kernel) en adresse physique.
pub fn translate(addr: VirtAddr) -> Option<PhysAddr> {
    let mapper_guard = MAPPER.lock();
    let mapper = mapper_guard.as_ref()?;
    mapper.translate_addr(addr)
}

/// Identity-map une région MMIO (kernel table).
pub fn map_mmio(phys_addr: u64, size: usize) -> Result<u64, &'static str> {
    use x86_64::structures::paging::mapper::MapToError;
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH;

    let start = phys_addr & !0xfff;
    let end = (phys_addr + size as u64 + 0xfff) & !0xfff;

    let mut mapper_guard = MAPPER.lock();
    let mapper = mapper_guard.as_mut().ok_or("paging non initialisé")?;
    let mut fa_guard = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
    let fa = fa_guard.as_mut().ok_or("frame alloc non init")?;

    let mut cur = start;
    while cur < end {
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(cur));
        let page  = Page::<Size4KiB>::containing_address(VirtAddr::new(cur));
        // SAFETY: identity-map idempotent d'une région MMIO.
        match unsafe { mapper.map_to(page, frame, flags, fa) } {
            Ok(f) => f.flush(),
            Err(MapToError::PageAlreadyMapped(_)) => {}
            Err(_) => return Err("map_mmio: map_to a échoué"),
        }
        cur += 4096;
    }
    Ok(phys_addr)
}

// -----------------------------------------------------------------------------
// Manipulation de page tables ARBITRAIRES (pour les process userspace)
// -----------------------------------------------------------------------------

/// Construit un `OffsetPageTable` vers la page table dont la P4 est `p4_frame`.
/// Utile pour mapper des pages dans l'espace d'adressage d'un autre process.
///
/// SAFETY: l'appelant garantit que `p4_frame` pointe sur une P4 valide et
/// que personne d'autre ne modifie cette table pendant l'usage.
pub unsafe fn new_offset_mapper(p4_frame: PhysFrame<Size4KiB>) -> OffsetPageTable<'static> {
    let virt = VirtAddr::new(p4_frame.start_address().as_u64() + PHYS_OFFSET);
    let page_table: &'static mut PageTable = &mut *virt.as_mut_ptr();
    OffsetPageTable::new(page_table, VirtAddr::new(PHYS_OFFSET))
}

/// Alloue une frame libre.
pub fn alloc_frame() -> Result<PhysFrame<Size4KiB>, &'static str> {
    let mut fa = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
    fa.as_mut().ok_or("frame alloc non init")?
        .allocate_frame().ok_or("plus de frames libres")
}

/// Libère une frame.
pub fn free_frame(frame: PhysFrame<Size4KiB>) {
    let mut fa = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
    if let Some(a) = fa.as_mut() {
        // SAFETY: l'appelant garantit que la frame n'est plus référencée.
        unsafe { a.deallocate_frame(frame); }
    }
}

/// Alloue + zéroïse une frame, utile pour une nouvelle P4 / page user.
pub fn alloc_zeroed_frame() -> Result<PhysFrame<Size4KiB>, &'static str> {
    let frame = alloc_frame()?;
    let virt = VirtAddr::new(frame.start_address().as_u64() + PHYS_OFFSET);
    // SAFETY: la frame est identity-mapée (< 1 GiB dans QEMU -m 128M).
    unsafe {
        core::ptr::write_bytes(virt.as_mut_ptr::<u8>(), 0, 4096);
    }
    Ok(frame)
}

/// Lit CR3 courant (frame + flags).
pub fn current_cr3() -> (PhysFrame<Size4KiB>, Cr3Flags) { Cr3::read() }

/// Bascule CR3 sur la page table donnée. Invalide le TLB implicitement.
///
/// SAFETY: `frame` doit pointer sur une P4 valide contenant au minimum le
/// kernel mapping (moitiés hautes).
pub unsafe fn switch_cr3(frame: PhysFrame<Size4KiB>) {
    Cr3::write(frame, Cr3Flags::empty());
}

/// Invalide une seule entrée TLB (après modification d'un mapping existant).
pub fn invalidate_tlb(addr: VirtAddr) {
    x86_64::instructions::tlb::flush(addr);
}
