// =============================================================================
// cow.rs — Copy-on-Write page fault resolution.
//
// Scénario :
//   fork()  -> parent et child partagent les mêmes frames userspace, toutes
//             marquées RO + COW_MARKER (bit OS 9)
//   write dans parent OU child → page fault #PF (PROTECTION_VIOLATION | WRITE)
//   `try_resolve_cow(addr)` :
//     - regarde l'entrée de page table de l'AS courante
//     - si COW_MARKER présent :
//         - si refcount == 1 : juste re-marquer WRITABLE (on est seul) + clear CoW
//         - sinon : alloue une nouvelle frame, copie, re-map WRITABLE sur la copie
//     - sinon : pas une page CoW → retourne Err
//
// Appelé depuis le handler de page fault (idt.rs).
// =============================================================================

use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        Mapper, Page, PageTableFlags, PhysFrame, Size4KiB, Translate,
        mapper::TranslateResult,
    },
};

use crate::memory::paging::{self, COW_MARKER, PHYS_OFFSET};
use crate::memory::frame_refcount::FRAME_REFCOUNT;

pub fn try_resolve_cow(fault_addr: VirtAddr) -> Result<(), &'static str> {
    // Lit la page table du process courant.
    let (p4_frame, _) = paging::current_cr3();
    // SAFETY: p4_frame est CR3 courant, valide.
    let mut mapper = unsafe { paging::new_offset_mapper(p4_frame) };

    let page = Page::<Size4KiB>::containing_address(fault_addr);

    // Récupère le mapping (frame + flags) actuel
    let (old_frame, flags) = match mapper.translate(fault_addr) {
        TranslateResult::Mapped { frame, flags, .. } => {
            let phys = frame.start_address();
            (PhysFrame::<Size4KiB>::containing_address(phys), flags)
        }
        _ => return Err("not mapped"),
    };

    if !flags.contains(COW_MARKER) {
        return Err("not CoW");
    }
    if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
        return Err("not user");
    }

    // Cas 1 : on est seul à détenir la frame → juste remove CoW + rétablit WRITABLE
    let refcount = FRAME_REFCOUNT.lock().get(old_frame);
    if refcount <= 1 {
        let new_flags = (flags | PageTableFlags::WRITABLE) - COW_MARKER;
        // SAFETY: page mappée, on n'ajoute que WRITABLE + clear marker.
        unsafe {
            mapper.update_flags(page, new_flags)
                .map_err(|_| "update_flags failed")?
                .flush();
        }
        return Ok(());
    }

    // Cas 2 : plusieurs refs → alloue une nouvelle frame, copie, re-map.
    let new_frame = paging::alloc_zeroed_frame()?;
    // SAFETY: both frames are identity-mapped (PHYS_OFFSET = 0).
    unsafe {
        let src = (old_frame.start_address().as_u64() + PHYS_OFFSET) as *const u8;
        let dst = (new_frame.start_address().as_u64() + PHYS_OFFSET) as *mut u8;
        core::ptr::copy_nonoverlapping(src, dst, 4096);
    }

    // Décrément refcount de l'ancienne frame
    FRAME_REFCOUNT.lock().dec(old_frame);
    // Nouvelle frame a refcount 1
    FRAME_REFCOUNT.lock().set(new_frame, 1);

    // Re-map sur la nouvelle frame avec WRITABLE, sans CoW
    let new_flags = (flags | PageTableFlags::WRITABLE) - COW_MARKER;

    // Unmap + map_to : on ne peut pas simplement update_flags+changer la frame
    let _ = mapper.unmap(page)
        .map_err(|_| "unmap failed")?;

    let mut fa = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
    let fa_ref = fa.as_mut().ok_or("frame alloc non init")?;
    // SAFETY: page était mappée précédemment sur old_frame, unmap OK ci-dessus.
    unsafe {
        mapper.map_to(page, new_frame, new_flags, fa_ref)
            .map_err(|_| "map_to failed")?
            .flush();
    }

    // Enregistre la nouvelle frame dans l'AddressSpace courant (via process)
    crate::task::process::replace_mapping(page.start_address(), new_frame);

    Ok(())
}

/// Marque une plage d'adresses comme RO + CoW dans l'AS donné. Helper pour fork.
#[allow(dead_code)]
pub fn mark_range_cow(p4_frame: PhysFrame<Size4KiB>, start: VirtAddr, end: VirtAddr)
    -> Result<(), &'static str>
{
    // SAFETY: p4_frame supposée être une P4 valide identity-mappée.
    let mut mapper = unsafe { paging::new_offset_mapper(p4_frame) };
    let mut cur = start.align_down(4096u64);
    while cur < end {
        let page = Page::<Size4KiB>::containing_address(cur);
        if let TranslateResult::Mapped { flags, .. } = mapper.translate(cur) {
            if flags.contains(PageTableFlags::USER_ACCESSIBLE)
                && flags.contains(PageTableFlags::WRITABLE)
            {
                let new_flags = (flags - PageTableFlags::WRITABLE) | COW_MARKER;
                unsafe {
                    let _ = mapper.update_flags(page, new_flags).map(|f| f.flush());
                }
            }
        }
        cur = VirtAddr::new(cur.as_u64() + 4096);
    }
    Ok(())
}
