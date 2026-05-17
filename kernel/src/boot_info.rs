// =============================================================================
// boot_info.rs — parser de la structure d'info multiboot2.
//
// GRUB passe un pointeur vers une structure :
//   +0  u32 total_size
//   +4  u32 reserved (0)
//   +8  début des tags, chaque tag :
//        u32 type
//        u32 size
//        ... (payload, padded à 8 octets)
//
// Liste des types utiles :
//   1  = boot command line (string)
//   2  = bootloader name    (string)
//   3  = module             (start/end + string)
//   6  = memory map         (ce qu'on veut le plus)
//   8  = framebuffer info
//
// Spec: https://www.gnu.org/software/grub/manual/multiboot2/multiboot.html
// =============================================================================

use core::slice;
use core::str;

#[repr(C)]
struct TagHeader {
    typ: u32,
    size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryArea {
    pub base_addr: u64,
    pub length: u64,
    pub typ: u32,      // 1 = available RAM, 3 = ACPI reclaimable, etc.
    _reserved: u32,
}

impl MemoryArea {
    pub fn is_usable(&self) -> bool { self.typ == 1 }
    pub fn end_addr(&self) -> u64 { self.base_addr + self.length }
}

#[repr(C)]
struct MemoryMapTag {
    typ: u32,
    size: u32,
    entry_size: u32,
    entry_version: u32,
    // entries[] suivent
}

#[repr(C)]
pub struct Module {
    pub start: u32,
    pub end: u32,
    // string NUL-terminée suit
}

pub struct BootInfo {
    addr: usize,
    total_size: usize,
}

impl BootInfo {
    /// Crée une vue sur la structure d'info multiboot2.
    ///
    /// SAFETY: `addr` doit pointer sur une structure multiboot2 valide,
    /// typiquement le pointeur passé par GRUB dans RDI.
    pub unsafe fn from_addr(addr: usize) -> Self {
        let total_size = *(addr as *const u32) as usize;
        BootInfo { addr, total_size }
    }

    pub fn total_size(&self) -> usize { self.total_size }

    /// Itère sur tous les tags.
    fn tags(&self) -> TagIter {
        TagIter { ptr: (self.addr + 8) as *const u8, end: (self.addr + self.total_size) as *const u8 }
    }

    /// Retourne la ligne de commande passée au kernel, si présente.
    pub fn cmdline(&self) -> Option<&str> {
        self.tags().find_map(|t| if t.typ == 1 { tag_string(t) } else { None })
    }

    /// Nom du bootloader (ex: "GRUB 2.06").
    pub fn bootloader_name(&self) -> Option<&str> {
        self.tags().find_map(|t| if t.typ == 2 { tag_string(t) } else { None })
    }

    /// Itérateur sur les régions mémoire. Utilisé par le frame allocator.
    pub fn memory_areas(&self) -> Option<MemoryAreaIter> {
        for tag in self.tags() {
            if tag.typ == 6 {
                // SAFETY: tag de type 6 = MemoryMapTag bien formé
                let mm = unsafe { &*(tag.ptr as *const MemoryMapTag) };
                let entries_start = unsafe { tag.ptr.add(16) };
                let entries_end = unsafe { tag.ptr.add(tag.size as usize) };
                return Some(MemoryAreaIter {
                    current: entries_start,
                    end: entries_end,
                    entry_size: mm.entry_size as usize,
                });
            }
        }
        None
    }

    /// Itérateur sur les modules (initrd, etc.).
    pub fn modules(&self) -> ModuleIter {
        ModuleIter { inner: self.tags() }
    }

    /// Infos framebuffer (tag type 8). None si GRUB n'a pas fourni de mode graphique.
    pub fn framebuffer(&self) -> Option<FramebufferInfo> {
        for tag in self.tags() {
            if tag.typ == 8 {
                // SAFETY: tag.size >= 32, layout standard multiboot2 section 3.1.11
                let fb = unsafe { &*(tag.ptr as *const FramebufferTag) };
                return Some(FramebufferInfo {
                    addr: fb.addr,
                    pitch: fb.pitch,
                    width: fb.width,
                    height: fb.height,
                    bpp: fb.bpp,
                    fb_type: fb.fb_type,
                });
            }
        }
        None
    }
}

#[repr(C)]
struct FramebufferTag {
    typ: u32,
    size: u32,
    addr: u64,
    pitch: u32,
    width: u32,
    height: u32,
    bpp: u8,
    fb_type: u8,
    _reserved: u16,
    // champs spécifiques au type suivent (palette / champs RGB)
}

/// Vue haut-niveau d'un framebuffer fourni par GRUB.
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    pub addr: u64,     // adresse physique du début du framebuffer
    pub pitch: u32,    // octets par ligne (≥ width * bpp/8)
    pub width: u32,
    pub height: u32,
    pub bpp: u8,       // 32, 24, 16, 8
    pub fb_type: u8,   // 0=indexed, 1=RGB, 2=EGA text
}

// -----------------------------------------------------------------------------
// Itérateurs bas-niveau
// -----------------------------------------------------------------------------

struct TagRef { typ: u32, size: u32, ptr: *const u8 }

struct TagIter { ptr: *const u8, end: *const u8 }

impl Iterator for TagIter {
    type Item = TagRef;
    fn next(&mut self) -> Option<TagRef> {
        if self.ptr >= self.end { return None; }
        // SAFETY: on a vérifié qu'on est avant end
        let h = unsafe { &*(self.ptr as *const TagHeader) };
        // type 0 = tag de fin
        if h.typ == 0 && h.size == 8 { return None; }
        let tag = TagRef { typ: h.typ, size: h.size, ptr: self.ptr };
        // Avance au prochain tag, aligné sur 8
        let next = (self.ptr as usize + h.size as usize + 7) & !7;
        self.ptr = next as *const u8;
        Some(tag)
    }
}

fn tag_string(tag: TagRef) -> Option<&'static str> {
    // Payload = tag.size - 8 octets d'en-tête, NUL-terminé
    let len = (tag.size as usize).saturating_sub(8);
    if len == 0 { return Some(""); }
    let bytes = unsafe { slice::from_raw_parts(tag.ptr.add(8), len) };
    // Trim du NUL terminal
    let trimmed = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
    str::from_utf8(trimmed).ok()
}

pub struct MemoryAreaIter {
    current: *const u8,
    end: *const u8,
    entry_size: usize,
}

impl Iterator for MemoryAreaIter {
    type Item = MemoryArea;
    fn next(&mut self) -> Option<MemoryArea> {
        if self.current >= self.end { return None; }
        // SAFETY: le parser garantit que current pointe sur une MemoryArea bien formée
        let area = unsafe { *(self.current as *const MemoryArea) };
        self.current = unsafe { self.current.add(self.entry_size) };
        Some(area)
    }
}

pub struct ModuleIter { inner: TagIter }

impl Iterator for ModuleIter {
    type Item = (&'static [u8], &'static str);  // (data, name)
    fn next(&mut self) -> Option<Self::Item> {
        for tag in &mut self.inner {
            if tag.typ == 3 {
                let m = unsafe { &*(tag.ptr.add(8) as *const Module) };
                let len = (m.end - m.start) as usize;
                // SAFETY: GRUB garantit que [start,end) est mappé
                let data = unsafe { slice::from_raw_parts(m.start as *const u8, len) };
                // Nom après les 8 octets de (start,end)
                let name_bytes_len = (tag.size as usize).saturating_sub(16);
                let name_bytes = unsafe { slice::from_raw_parts(tag.ptr.add(16), name_bytes_len) };
                let name_trim = name_bytes.split(|&b| b == 0).next().unwrap_or(b"");
                let name = str::from_utf8(name_trim).unwrap_or("");
                return Some((data, name));
            }
        }
        None
    }
}
