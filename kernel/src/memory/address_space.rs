// =============================================================================
// address_space.rs — espace d'adressage d'un process userspace.
//
// Chaque process possède sa propre P4 (top-level page table). La moitié basse
// (0x0000_0000..0x0000_8000_0000_0000) est privée par process (code, données,
// stack user, heap user). La moitié haute (0xFFFF_8000...) est partagée : on
// clone les entrées de la P4 kernel au moment de la création.
//
// API :
//   - AddressSpace::new_user()          : P4 fraîche + moitié haute partagée
//   - as.map_alloc(page, flags)         : alloue une frame + map
//   - as.map_to(page, frame, flags)     : map sur frame existante
//   - as.write_data(addr, data, flags)  : écrit dans l'AS (alloue les pages)
//   - as.clone_cow()                    : clone "fork-style" avec CoW
//   - as.activate()                     : CR3 := this
//   - as.translate(addr)                : tradition user → phys
// =============================================================================

use alloc::{collections::BTreeMap, vec::Vec};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
        PhysFrame, Size4KiB, Translate,
        mapper::{MapToError, TranslateResult},
    },
};

use crate::memory::paging::{self, COW_MARKER, PHYS_OFFSET};
use crate::memory::frame_refcount::FRAME_REFCOUNT;

pub struct AddressSpace {
    p4: PhysFrame<Size4KiB>,
    mapped_pages: BTreeMap<u64, PhysFrame<Size4KiB>>,
}

impl AddressSpace {
    pub fn new_user() -> Result<Self, &'static str> {
        let p4_frame = paging::alloc_zeroed_frame()?;
        let new_p3_for_low = paging::alloc_zeroed_frame()?;

        // Stratégie : on partage les entries P4 hautes (pas utilisées par le
        // kernel actuellement, réservées pour higher-half futur), mais on
        // **deep-clone P4[0]** — le kernel y a son identity mapping sur
        // [0, 1 GiB), et on veut que chaque process puisse mapper SES pages
        // user sans toucher à la P3 du kernel.
        //
        //   new_p4[0]        := new_p3_for_low (copie des entries kernel P3)
        //   new_p4[1..512]   := kernel_p4[1..512] (shallow, 0 pour l'instant)
        //
        // Le user process doit donc mapper ses pages à des adresses qui
        // tombent dans cette P3. Comme une P3 couvre 512 GiB, tout userland
        // jusqu'à 0x0000_0080_0000_0000 utilise cette P3 — largement assez.
        unsafe {
            let dst_p4 = &mut *((p4_frame.start_address().as_u64() + PHYS_OFFSET)
                as *mut PageTable);

            let (cur_p4_frame, _) = paging::current_cr3();
            let src_p4 = &*((cur_p4_frame.start_address().as_u64() + PHYS_OFFSET)
                as *const PageTable);

            // Deep clone de P4[0] → nouvelle P3 dédiée
            let new_p3_virt = new_p3_for_low.start_address().as_u64() + PHYS_OFFSET;
            let dst_p3 = &mut *(new_p3_virt as *mut PageTable);
            if src_p4[0].flags().contains(PageTableFlags::PRESENT) {
                let src_p3_phys = src_p4[0].addr().as_u64();
                let src_p3 = &*((src_p3_phys + PHYS_OFFSET) as *const PageTable);
                // Copie shallow des entries P3 du kernel (huge pages 2 MiB
                // identity-mappées — pas besoin de deep copy, les P2 sont
                // stables et read-only du point de vue allocateur).
                for i in 0..512 {
                    dst_p3[i] = src_p3[i].clone();
                }
            }
            // Installe la nouvelle P3 dans new_p4[0]
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE;
            dst_p4[0].set_addr(new_p3_for_low.start_address(), flags);

            // Shallow copy le reste (higher half)
            for i in 1..512 {
                dst_p4[i] = src_p4[i].clone();
            }
        }

        Ok(AddressSpace {
            p4: p4_frame,
            mapped_pages: BTreeMap::new(),
        })
    }

    /// Active cet AddressSpace (CR3 := this.p4).
    ///
    /// SAFETY: appelant garantit que le kernel mapping est intact dans this.
    pub unsafe fn activate(&self) {
        paging::switch_cr3(self.p4);
    }

    pub fn p4_frame(&self) -> PhysFrame<Size4KiB> { self.p4 }

    pub fn mapped_pages(&self) -> &BTreeMap<u64, PhysFrame<Size4KiB>> {
        &self.mapped_pages
    }

    /// Remplace le mapping enregistré pour `vaddr` par une nouvelle frame.
    /// Appelé après un CoW réussi.
    pub fn replace_mapping(&mut self, vaddr: VirtAddr, new_frame: PhysFrame<Size4KiB>) {
        let key = vaddr.align_down(4096u64).as_u64();
        self.mapped_pages.insert(key, new_frame);
    }

    /// Alloue une frame et map la page userspace avec les flags donnés.
    pub fn map_alloc(&mut self, page: Page<Size4KiB>, flags: PageTableFlags)
        -> Result<PhysFrame<Size4KiB>, &'static str>
    {
        let frame = paging::alloc_zeroed_frame()?;
        FRAME_REFCOUNT.lock().set(frame, 1);
        self.map_to(page, frame, flags)?;
        Ok(frame)
    }

    pub fn map_to(
        &mut self,
        page: Page<Size4KiB>,
        frame: PhysFrame<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<(), &'static str> {
        let mut mapper_guard = MAPPER_SLOTS.lock();
        let mapper = mapper_guard.get_or_init(self.p4);

        let mut fa_guard = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
        let fa = fa_guard.as_mut().ok_or("frame alloc non init")?;

        // SAFETY: p4 privée au process, mutex MAPPER_SLOTS sérialise.
        unsafe {
            match mapper.map_to(page, frame, flags, fa) {
                Ok(f) => f.flush(),
                Err(MapToError::PageAlreadyMapped(_)) => {
                    return Err("page déjà mappée");
                }
                Err(_) => return Err("map_to failed"),
            }
        }

        self.mapped_pages.insert(page.start_address().as_u64(), frame);
        Ok(())
    }

    /// Écrit des données dans l'address space user sans l'activer.
    /// Utilisé par le ELF loader. Alloue les pages manquantes.
    pub fn write_data(&mut self, vaddr: VirtAddr, data: &[u8], flags: PageTableFlags)
        -> Result<(), &'static str>
    {
        let mut remaining_len = data.len();
        let mut cur_addr = vaddr;
        let mut src_offset = 0usize;

        while remaining_len > 0 {
            let page_base = cur_addr.align_down(4096u64);
            let page = Page::<Size4KiB>::containing_address(page_base);
            let off_in_page = (cur_addr.as_u64() - page_base.as_u64()) as usize;
            let avail = 4096 - off_in_page;
            let to_write = avail.min(remaining_len);

            let frame = match self.mapped_pages.get(&page.start_address().as_u64()) {
                Some(&f) => f,
                None => self.map_alloc(page, flags)?,
            };

            let phys = frame.start_address().as_u64() + PHYS_OFFSET;
            // SAFETY: frame identity-mappée.
            unsafe {
                let dst = (phys + off_in_page as u64) as *mut u8;
                core::ptr::copy_nonoverlapping(
                    data.as_ptr().add(src_offset),
                    dst,
                    to_write,
                );
            }

            remaining_len -= to_write;
            src_offset += to_write;
            cur_addr = VirtAddr::new(cur_addr.as_u64() + to_write as u64);
        }
        Ok(())
    }

    /// Garantit que les pages de [vaddr, vaddr+len) sont mappées avec `flags`.
    /// Utilisé pour p_memsz > p_filesz dans ELF (BSS) et pour allouer la stack.
    pub fn ensure_mapped(&mut self, vaddr: VirtAddr, len: u64, flags: PageTableFlags)
        -> Result<(), &'static str>
    {
        let start = vaddr.align_down(4096u64).as_u64();
        let end = (vaddr.as_u64() + len + 4095) & !4095;
        let mut cur = start;
        while cur < end {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(cur));
            if !self.mapped_pages.contains_key(&cur) {
                self.map_alloc(page, flags)?;
            }
            cur += 4096;
        }
        Ok(())
    }

    pub fn translate(&self, addr: VirtAddr) -> Option<PhysAddr> {
        let mut mapper_guard = MAPPER_SLOTS.lock();
        let mapper = mapper_guard.get_or_init(self.p4);
        mapper.translate_addr(addr)
    }

    /// Clone pour fork : copie la P4, marque toutes les pages basses RO + CoW.
    pub fn clone_cow(&mut self) -> Result<Self, &'static str> {
        let new_p4 = paging::alloc_zeroed_frame()?;

        // Nouvelle P3 pour P4[0] (kernel identity partagée, user privé).
        let new_p3_for_low = paging::alloc_zeroed_frame()?;

        // SAFETY: P4 fraîches identity-mappées.
        unsafe {
            let dst_p4 = &mut *((new_p4.start_address().as_u64() + PHYS_OFFSET)
                as *mut PageTable);
            let src_p4 = &*((self.p4.start_address().as_u64() + PHYS_OFFSET)
                as *const PageTable);

            // Copie la P3 du parent dans la nouvelle P3 fille (shallow).
            let dst_p3 = &mut *((new_p3_for_low.start_address().as_u64() + PHYS_OFFSET)
                as *mut PageTable);
            if src_p4[0].flags().contains(PageTableFlags::PRESENT) {
                let src_p3_phys = src_p4[0].addr().as_u64();
                let src_p3 = &*((src_p3_phys + PHYS_OFFSET) as *const PageTable);
                for i in 0..512 {
                    dst_p3[i] = src_p3[i].clone();
                }
            }
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE;
            dst_p4[0].set_addr(new_p3_for_low.start_address(), flags);

            for i in 1..512 {
                dst_p4[i] = src_p4[i].clone();
            }
        }

        let mapped_copy: Vec<(u64, PhysFrame<Size4KiB>)> = self.mapped_pages
            .iter().map(|(&k, &v)| (k, v)).collect();

        // 1) downgrade parent pages RW→RO+CoW
        {
            let mut src_guard = MAPPER_SLOTS.lock();
            let src_mapper = src_guard.get_or_init(self.p4);
            // SAFETY: src_mapper sérialisé par MAPPER_SLOTS.
            unsafe {
                for (vaddr, _frame) in &mapped_copy {
                    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(*vaddr));
                    let current_flags = match src_mapper.translate(page.start_address()) {
                        TranslateResult::Mapped { flags, .. } => flags,
                        _ => continue,
                    };
                    if current_flags.contains(PageTableFlags::USER_ACCESSIBLE)
                        && current_flags.contains(PageTableFlags::WRITABLE)
                    {
                        let new_flags = (current_flags - PageTableFlags::WRITABLE) | COW_MARKER;
                        let _ = src_mapper.update_flags(page, new_flags)
                            .map(|f| f.flush());
                    }
                }
            }
        }

        // 2) map les mêmes frames dans la child P4 avec les flags (RO+CoW)
        let mut new_mapped: BTreeMap<u64, PhysFrame<Size4KiB>> = BTreeMap::new();

        // Collect flags en verrouillant brièvement
        let flags_map: Vec<(u64, PageTableFlags)> = {
            let mut src_guard = MAPPER_SLOTS.lock();
            let m = src_guard.get_or_init(self.p4);
            mapped_copy.iter().map(|(v, _)| {
                let flags = match m.translate(VirtAddr::new(*v)) {
                    TranslateResult::Mapped { flags, .. } => flags,
                    _ => paging::USER_RX,
                };
                (*v, flags)
            }).collect()
        };

        {
            let mut fa_guard = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
            let fa = fa_guard.as_mut().ok_or("frame alloc non init")?;
            // SAFETY: new_p4 privée (pas encore activée).
            unsafe {
                let mut dst_mapper = paging::new_offset_mapper(new_p4);
                for ((vaddr, frame), (_, flags)) in mapped_copy.iter().zip(flags_map.iter()) {
                    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(*vaddr));
                    let _ = dst_mapper.map_to(page, *frame, *flags, fa)
                        .map(|f| f.flush());
                    new_mapped.insert(*vaddr, *frame);
                    FRAME_REFCOUNT.lock().inc(*frame);
                }
            }
        }

        Ok(AddressSpace { p4: new_p4, mapped_pages: new_mapped })
    }

    /// Libère toutes les frames détenues uniquement par cet address space.
    pub fn destroy(&mut self) {
        let frames: Vec<PhysFrame<Size4KiB>> = self.mapped_pages.values().copied().collect();
        {
            let mut rc = FRAME_REFCOUNT.lock();
            for frame in &frames {
                let remaining = rc.dec(*frame);
                if remaining == 0 {
                    paging::free_frame(*frame);
                }
            }
        }
        paging::free_frame(self.p4);
        self.mapped_pages.clear();
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        if !self.mapped_pages.is_empty() {
            self.destroy();
        }
    }
}

// -----------------------------------------------------------------------------
// Cache OffsetPageTable par P4 — évite de reconstruire à chaque map.
// -----------------------------------------------------------------------------

struct MapperSlots {
    current: Option<(PhysFrame<Size4KiB>, OffsetPageTable<'static>)>,
}

impl MapperSlots {
    const fn new() -> Self { MapperSlots { current: None } }

    fn get_or_init(&mut self, p4: PhysFrame<Size4KiB>) -> &mut OffsetPageTable<'static> {
        let need_new = match &self.current {
            Some((f, _)) => *f != p4,
            None => true,
        };
        if need_new {
            // SAFETY: p4 provient d'un AddressSpace vivant, identity-mappée.
            let m = unsafe { paging::new_offset_mapper(p4) };
            self.current = Some((p4, m));
        }
        &mut self.current.as_mut().unwrap().1
    }
}

static MAPPER_SLOTS: Mutex<MapperSlots> = Mutex::new(MapperSlots::new());
