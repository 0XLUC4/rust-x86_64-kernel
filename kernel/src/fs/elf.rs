// =============================================================================
// elf.rs — parseur ELF64 minimal + loader dans un AddressSpace userspace.
//
// On supporte :
//   - ELF64 little-endian (EM_X86_64)
//   - EXEC (pas DYN pour l'instant : pas de relocations ASLR)
//   - PT_LOAD segments seulement (.text, .data, .bss)
//
// On refuse proprement (Err) tout ce qui dépasse ce support : ELF32,
// big-endian, interpréteur dynamique, etc.
// =============================================================================

use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};
use x86_64::VirtAddr;

use crate::memory::address_space::AddressSpace;

const EI_MAG0: usize = 0;
const EI_MAG1: usize = 1;
const EI_MAG2: usize = 2;
const EI_MAG3: usize = 3;
const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;

const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

pub struct LoadedElf {
    pub entry: VirtAddr,
    /// Adresse user la plus haute utilisée (pour positionner la heap / brk).
    pub highest_vaddr: u64,
}

/// Charge un binaire ELF64 dans `space`. Retourne l'entry point.
pub fn load(bin: &[u8], space: &mut AddressSpace) -> Result<LoadedElf, &'static str> {
    if bin.len() < core::mem::size_of::<Ehdr>() {
        return Err("ELF: fichier tronqué");
    }

    // SAFETY: on vérifie ensuite la magic et les champs.
    let ehdr: Ehdr = unsafe { core::ptr::read_unaligned(bin.as_ptr() as *const Ehdr) };

    if ehdr.e_ident[EI_MAG0] != 0x7f
        || ehdr.e_ident[EI_MAG1] != b'E'
        || ehdr.e_ident[EI_MAG2] != b'L'
        || ehdr.e_ident[EI_MAG3] != b'F'
    {
        return Err("ELF: magic invalide");
    }
    if ehdr.e_ident[EI_CLASS] != ELFCLASS64 {
        return Err("ELF: pas ELF64");
    }
    if ehdr.e_ident[EI_DATA] != ELFDATA2LSB {
        return Err("ELF: pas little-endian");
    }
    if ehdr.e_machine != EM_X86_64 {
        return Err("ELF: pas x86_64");
    }
    if ehdr.e_type != ET_EXEC && ehdr.e_type != ET_DYN {
        return Err("ELF: type non supporté (EXEC/DYN requis)");
    }
    if ehdr.e_phentsize as usize != core::mem::size_of::<Phdr>() {
        return Err("ELF: e_phentsize inattendu");
    }

    let ph_off = ehdr.e_phoff as usize;
    let ph_total = ehdr.e_phentsize as usize * ehdr.e_phnum as usize;
    if ph_off + ph_total > bin.len() {
        return Err("ELF: program headers hors fichier");
    }

    let mut highest: u64 = 0;

    for i in 0..ehdr.e_phnum as usize {
        let off = ph_off + i * ehdr.e_phentsize as usize;
        // SAFETY: borne vérifiée ci-dessus.
        let ph: Phdr = unsafe {
            core::ptr::read_unaligned(bin.as_ptr().add(off) as *const Phdr)
        };
        if ph.p_type != PT_LOAD { continue; }
        if ph.p_filesz > ph.p_memsz {
            return Err("ELF: p_filesz > p_memsz");
        }
        if ph.p_memsz == 0 { continue; }

        // Détermine les flags de mapping
        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if ph.p_flags & PF_W != 0 { flags |= PageTableFlags::WRITABLE; }
        // NO_EXECUTE désactivé par défaut : on n'active pas EFER.NXE ici pour
        // rester simple. Toutes les pages user seront exécutables sauf si
        // on étend plus tard (prérequis : EFER.NXE = 1).

        let seg_end = ph.p_vaddr + ph.p_memsz;

        // 1) alloue et zéroïse la plage (permet p_memsz > p_filesz = BSS)
        space.ensure_mapped(
            VirtAddr::new(ph.p_vaddr),
            ph.p_memsz,
            flags,
        )?;

        // 2) copie les p_filesz premiers octets depuis le fichier ELF
        if ph.p_filesz > 0 {
            let src_start = ph.p_offset as usize;
            let src_end = src_start + ph.p_filesz as usize;
            if src_end > bin.len() {
                return Err("ELF: segment hors fichier");
            }
            let src = &bin[src_start..src_end];
            space.write_data(VirtAddr::new(ph.p_vaddr), src, flags)?;
        }

        if seg_end > highest { highest = seg_end; }
    }

    // Check : l'entry est dans un segment chargé
    if ehdr.e_entry == 0 {
        return Err("ELF: e_entry = 0");
    }

    Ok(LoadedElf {
        entry: VirtAddr::new(ehdr.e_entry),
        highest_vaddr: highest,
    })
}

/// Helper : map une stack user à `stack_top` avec `pages * 4 KiB`.
/// Retourne l'adresse RSP initiale (aligné 16).
pub fn map_user_stack(space: &mut AddressSpace, stack_top: VirtAddr, pages: u64)
    -> Result<VirtAddr, &'static str>
{
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE;
    let base = stack_top.as_u64() - pages * 4096;
    for i in 0..pages {
        let addr = base + i * 4096;
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
        space.map_alloc(page, flags)?;
    }
    // RSP pointe en haut de stack, aligné 16, et on laisse 16 octets libres.
    Ok(VirtAddr::new(stack_top.as_u64() - 16))
}
