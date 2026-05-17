// =============================================================================
// acpi — parseur minimal des tables ACPI.
//
// Pipeline :
//   1) RSDP (Root System Description Pointer) — cherché :
//        a) à l'adresse pointée par *(EBDA_SEG_PTR << 4), sur 1 KiB
//        b) dans la ROM BIOS [0xE0000 .. 0xFFFFF]
//      Signature = b"RSD PTR " (8 octets, avec l'espace final).
//   2) RSDP rev 0  -> RSDT (pointeurs 32 bits)
//      RSDP rev ≥1 -> XSDT (pointeurs 64 bits)
//   3) Parcourt les SDT enfants par signature :
//        "APIC" = MADT  (CPU cores, LAPIC addr, I/O APICs, IRQ overrides)
//        "FACP" = FADT  (SCI int, PM1 regs, boot flags, reset register...)
//        "HPET" = HPET   (haute résolution — pas utilisé ici)
//        "MCFG" = PCIe ECAM base (Phase II)
//
// On reste no_std, zero-copy, validation de checksum systématique.
// =============================================================================

use alloc::vec::Vec;
use core::{mem, ptr, slice};
use spin::Once;

// -----------------------------------------------------------------------------
// Constantes & structures brutes
// -----------------------------------------------------------------------------

const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
const EBDA_SEG_PTR: usize = 0x40E;
const BIOS_ROM_START: usize = 0xE_0000;
const BIOS_ROM_END:   usize = 0xF_FFFF;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpV1 {
    signature: [u8; 8],
    checksum:  u8,
    oem_id:    [u8; 6],
    revision:  u8,
    rsdt_addr: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpV2 {
    v1:                RsdpV1,
    length:            u32,
    xsdt_addr:         u64,
    extended_checksum: u8,
    _reserved:         [u8; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SdtHeader {
    pub signature:       [u8; 4],
    pub length:          u32,
    pub revision:        u8,
    pub checksum:        u8,
    pub oem_id:          [u8; 6],
    pub oem_table_id:    [u8; 8],
    pub oem_revision:    u32,
    pub creator_id:      u32,
    pub creator_rev:     u32,
}

// -----------------------------------------------------------------------------
// MADT (Multiple APIC Description Table)
// -----------------------------------------------------------------------------

#[repr(C, packed)]
struct MadtHeader {
    header:               SdtHeader,
    local_apic_address:   u32,
    flags:                u32,
    // entries[] : (type:u8, length:u8, ...)
}

#[derive(Debug, Clone, Copy)]
pub struct CpuCore {
    pub acpi_processor_id: u8,
    pub lapic_id:          u8,
    pub enabled:           bool,
}

#[derive(Debug, Clone, Copy)]
pub struct IoApic {
    pub id:            u8,
    pub address:       u32,
    pub gsi_base:      u32,
}

#[derive(Debug, Clone, Copy)]
pub struct IrqOverride {
    pub bus_source:    u8,
    pub irq_source:    u8,
    pub gsi:           u32,
    pub flags:         u16,
}

#[derive(Default)]
pub struct AcpiInfo {
    pub revision:          u8,
    pub lapic_phys_addr:   u64,    // Copie de la valeur MADT (peut être patchée par LAPIC Address Override entry 5)
    pub cores:             Vec<CpuCore>,
    pub io_apics:          Vec<IoApic>,
    pub overrides:         Vec<IrqOverride>,
    /// SCI (System Control Interrupt) du FADT.
    pub sci_interrupt:     u16,
    /// Adresse physique du port RESET_REG (FADT v2+).
    pub reset_reg_addr:    u64,
    pub reset_value:       u8,
}

static ACPI: Once<AcpiInfo> = Once::new();

// -----------------------------------------------------------------------------
// API publique
// -----------------------------------------------------------------------------

/// Parse l'ensemble des tables ACPI et retourne un snapshot exploitable.
/// Idempotent : stocké dans un `Once`.
pub fn init() -> &'static AcpiInfo {
    ACPI.call_once(|| unsafe { parse_all() })
}

/// Accès au snapshot déjà parsé. Panique si `init()` n'a pas été appelé.
pub fn info() -> &'static AcpiInfo {
    ACPI.get().expect("acpi::init() non appelé")
}

// -----------------------------------------------------------------------------
// Implémentation
// -----------------------------------------------------------------------------

unsafe fn parse_all() -> AcpiInfo {
    let mut info = AcpiInfo::default();

    let rsdp_ptr = match find_rsdp() {
        Some(p) => p,
        None => {
            crate::println!("[acpi] RSDP introuvable — mode dégradé (pas d'APIC)");
            return info;
        }
    };

    let rsdp_v1 = &*(rsdp_ptr as *const RsdpV1);
    if !checksum_ok(rsdp_ptr, mem::size_of::<RsdpV1>()) {
        crate::println!("[acpi] RSDP v1 checksum KO");
        return info;
    }
    info.revision = rsdp_v1.revision;

    let (table_ptrs, is_xsdt) = if rsdp_v1.revision >= 2 {
        if !checksum_ok(rsdp_ptr, mem::size_of::<RsdpV2>()) {
            crate::println!("[acpi] RSDP v2 ext. checksum KO");
            return info;
        }
        let rsdp_v2 = &*(rsdp_ptr as *const RsdpV2);
        let xsdt_addr = rsdp_v2.xsdt_addr;
        (collect_sdt_ptrs(xsdt_addr, true), true)
    } else {
        let rsdt_addr = rsdp_v1.rsdt_addr as u64;
        (collect_sdt_ptrs(rsdt_addr, false), false)
    };

    crate::println!("[acpi] RSDP rev={} {}SDT, {} tables",
        info.revision, if is_xsdt { "X" } else { "R" }, table_ptrs.len());

    for ptr in table_ptrs {
        let hdr = &*(ptr as *const SdtHeader);
        let sig = hdr.signature;
        if !checksum_ok(ptr, hdr.length as usize) {
            continue;
        }
        match &sig {
            b"APIC" => parse_madt(ptr, &mut info),
            b"FACP" => parse_fadt(ptr, &mut info),
            _ => {}
        }
    }

    crate::println!("[acpi] CPU cores: {}  IOAPICs: {}  IRQ overrides: {}",
        info.cores.len(), info.io_apics.len(), info.overrides.len());
    if info.lapic_phys_addr != 0 {
        crate::println!("[acpi] LAPIC @ {:#x}", info.lapic_phys_addr);
    }

    info
}

unsafe fn find_rsdp() -> Option<usize> {
    // 1) EBDA
    let ebda_seg = ptr::read(EBDA_SEG_PTR as *const u16) as usize;
    if ebda_seg != 0 {
        let ebda = ebda_seg << 4;
        if let Some(p) = scan_range(ebda, ebda + 1024) {
            return Some(p);
        }
    }
    // 2) BIOS ROM
    scan_range(BIOS_ROM_START, BIOS_ROM_END + 1)
}

unsafe fn scan_range(start: usize, end: usize) -> Option<usize> {
    let mut p = start & !0xf;
    while p + 16 <= end {
        let sig = slice::from_raw_parts(p as *const u8, 8);
        if sig == RSDP_SIG {
            return Some(p);
        }
        p += 16;
    }
    None
}

unsafe fn checksum_ok(addr: usize, len: usize) -> bool {
    let bytes = slice::from_raw_parts(addr as *const u8, len);
    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

unsafe fn collect_sdt_ptrs(sdt_phys: u64, is_xsdt: bool) -> Vec<usize> {
    let mut out = Vec::new();
    if sdt_phys == 0 { return out; }
    let sdt_ptr = sdt_phys as usize;
    let hdr = &*(sdt_ptr as *const SdtHeader);
    let total = hdr.length as usize;
    let entries_off = mem::size_of::<SdtHeader>();
    if total <= entries_off { return out; }
    let entries_bytes = total - entries_off;
    let ptr = (sdt_ptr + entries_off) as *const u8;

    if is_xsdt {
        let n = entries_bytes / 8;
        for i in 0..n {
            let raw = ptr::read_unaligned(ptr.add(i * 8) as *const u64);
            if raw != 0 { out.push(raw as usize); }
        }
    } else {
        let n = entries_bytes / 4;
        for i in 0..n {
            let raw = ptr::read_unaligned(ptr.add(i * 4) as *const u32);
            if raw != 0 { out.push(raw as usize); }
        }
    }
    out
}

unsafe fn parse_madt(ptr: usize, info: &mut AcpiInfo) {
    let madt = &*(ptr as *const MadtHeader);
    info.lapic_phys_addr = madt.local_apic_address as u64;

    let total = madt.header.length as usize;
    let hdr_size = mem::size_of::<MadtHeader>();
    let mut off = hdr_size;
    while off + 2 <= total {
        let entry = (ptr + off) as *const u8;
        let etype = *entry;
        let elen  = *entry.add(1) as usize;
        if elen < 2 || off + elen > total { break; }

        match etype {
            // 0 : Processor Local APIC
            0 if elen >= 8 => {
                let acpi_id = *entry.add(2);
                let apic_id = *entry.add(3);
                let flags   = ptr::read_unaligned(entry.add(4) as *const u32);
                info.cores.push(CpuCore {
                    acpi_processor_id: acpi_id,
                    lapic_id:          apic_id,
                    enabled:           (flags & 1) != 0,
                });
            }
            // 1 : I/O APIC
            1 if elen >= 12 => {
                let id   = *entry.add(2);
                let addr = ptr::read_unaligned(entry.add(4)  as *const u32);
                let gsi  = ptr::read_unaligned(entry.add(8)  as *const u32);
                info.io_apics.push(IoApic { id, address: addr, gsi_base: gsi });
            }
            // 2 : Interrupt Source Override
            2 if elen >= 10 => {
                let bus = *entry.add(2);
                let irq = *entry.add(3);
                let gsi = ptr::read_unaligned(entry.add(4) as *const u32);
                let fl  = ptr::read_unaligned(entry.add(8) as *const u16);
                info.overrides.push(IrqOverride {
                    bus_source: bus, irq_source: irq, gsi, flags: fl,
                });
            }
            // 5 : Local APIC Address Override (64 bits)
            5 if elen >= 12 => {
                let addr = ptr::read_unaligned(entry.add(4) as *const u64);
                info.lapic_phys_addr = addr;
            }
            _ => {}
        }
        off += elen;
    }
}

unsafe fn parse_fadt(ptr: usize, info: &mut AcpiInfo) {
    // Layout FADT (ACPI 6.x). On lit seulement les champs qui nous intéressent,
    // aux offsets documentés par la spec — on évite d'allouer une struct
    // complète, qui varie selon la révision.
    let hdr = &*(ptr as *const SdtHeader);
    let len = hdr.length as usize;
    let base = ptr as *const u8;

    // SCI_INT : offset 46, u16.
    if len >= 48 {
        info.sci_interrupt = ptr::read_unaligned(base.add(46) as *const u16);
    }

    // RESET_REG est un Generic Address Structure (12 octets) à offset 116.
    // RESET_VALUE à offset 128 (u8). Seulement valide si revision ≥ 2 ET flags bit 10.
    if hdr.revision >= 2 && len >= 129 {
        // GAS layout: address_space:u8, bit_width:u8, bit_offset:u8, access_size:u8, address:u64
        let addr = ptr::read_unaligned(base.add(116 + 4) as *const u64);
        info.reset_reg_addr = addr;
        info.reset_value = *base.add(128);
    }
}

