// =============================================================================
// pci — énumération PCI (config space I/O ports 0xCF8 / 0xCFC).
//
// Méthode "mechanism #1" universellement supportée. Pas besoin de MCFG ici
// (ECAM sera ajouté en Phase II pour PCIe étendu).
//
// Layout de l'adresse 0xCF8 (u32) :
//   bit 31     : enable (1)
//   bits 30-24 : reserved
//   bits 23-16 : bus (8 bits)
//   bits 15-11 : device (5 bits)
//   bits 10-8  : function (3 bits)
//   bits 7-2   : register offset (aligné 4)
//   bits 1-0   : 00
//
// Puis lecture/écriture du u32 via 0xCFC.
// =============================================================================

use alloc::vec::Vec;
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA:    u16 = 0xCFC;

#[derive(Debug, Clone, Copy)]
pub struct PciAddr {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl PciAddr {
    #[inline]
    fn encode(self, offset: u8) -> u32 {
        (1 << 31)
            | ((self.bus  as u32) << 16)
            | ((self.dev  as u32) << 11)
            | ((self.func as u32) << 8)
            | ((offset as u32) & 0xFC)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub addr:        PciAddr,
    pub vendor_id:   u16,
    pub device_id:   u16,
    pub class_code:  u8,
    pub subclass:    u8,
    pub prog_if:     u8,
    pub revision:    u8,
    pub header_type: u8,
    pub bars:        [u32; 6],
    pub irq_line:    u8,
    pub irq_pin:     u8,
}

impl PciDevice {
    pub fn class_name(&self) -> &'static str {
        // Sous-ensemble des classes PCI les plus courantes.
        match (self.class_code, self.subclass) {
            (0x00, _)     => "Unclassified",
            (0x01, 0x00)  => "SCSI Controller",
            (0x01, 0x01)  => "IDE Controller",
            (0x01, 0x05)  => "ATA Controller",
            (0x01, 0x06)  => "SATA Controller (AHCI)",
            (0x01, 0x08)  => "NVMe Controller",
            (0x01, _)     => "Mass Storage",
            (0x02, 0x00)  => "Ethernet NIC",
            (0x02, _)     => "Network Controller",
            (0x03, 0x00)  => "VGA Display",
            (0x03, _)     => "Display Controller",
            (0x04, _)     => "Multimedia",
            (0x06, 0x00)  => "Host Bridge",
            (0x06, 0x01)  => "ISA Bridge",
            (0x06, 0x04)  => "PCI-to-PCI Bridge",
            (0x06, _)     => "Bridge Device",
            (0x0C, 0x03)  => "USB Controller",
            (0x0C, _)     => "Serial Bus",
            _ => "Other",
        }
    }
}

// -----------------------------------------------------------------------------
// Accès bas-niveau au config space
// -----------------------------------------------------------------------------

/// Verrou pour sérialiser l'accès 0xCF8/0xCFC entre cores/IRQ futurs.
static PORTS: Mutex<()> = Mutex::new(());

pub fn read_u32(addr: PciAddr, offset: u8) -> u32 {
    let _g = PORTS.lock();
    let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
    let mut data_port: Port<u32> = Port::new(CONFIG_DATA);
    unsafe {
        addr_port.write(addr.encode(offset));
        data_port.read()
    }
}

pub fn write_u32(addr: PciAddr, offset: u8, val: u32) {
    let _g = PORTS.lock();
    let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
    let mut data_port: Port<u32> = Port::new(CONFIG_DATA);
    unsafe {
        addr_port.write(addr.encode(offset));
        data_port.write(val);
    }
}

pub fn read_u16(addr: PciAddr, offset: u8) -> u16 {
    let aligned = read_u32(addr, offset & 0xFC);
    let shift = (offset & 0x3) * 8;
    ((aligned >> shift) & 0xFFFF) as u16
}

pub fn write_u16(addr: PciAddr, offset: u8, val: u16) {
    let aligned_off = offset & 0xFC;
    let mut aligned = read_u32(addr, aligned_off);
    let shift = ((offset & 0x2) * 8) as u32;
    let mask = !(0xFFFFu32 << shift);
    aligned = (aligned & mask) | ((val as u32) << shift);
    write_u32(addr, aligned_off, aligned);
}

pub fn read_u8(addr: PciAddr, offset: u8) -> u8 {
    let aligned = read_u32(addr, offset & 0xFC);
    let shift = (offset & 0x3) * 8;
    ((aligned >> shift) & 0xFF) as u8
}

/// Active I/O + MMIO + bus mastering pour un device PCI.
/// Retourne (avant, après) du registre COMMAND.
pub fn enable_device_io_mmio_bus_master(addr: PciAddr) -> (u16, u16) {
    const PCI_COMMAND_OFFSET: u8 = 0x04;
    const CMD_IO_SPACE: u16 = 1 << 0;
    const CMD_MEM_SPACE: u16 = 1 << 1;
    const CMD_BUS_MASTER: u16 = 1 << 2;

    let before = read_u16(addr, PCI_COMMAND_OFFSET);
    let after = before | CMD_IO_SPACE | CMD_MEM_SPACE | CMD_BUS_MASTER;
    if after != before {
        write_u16(addr, PCI_COMMAND_OFFSET, after);
    }
    (before, after)
}

// -----------------------------------------------------------------------------
// Enumération
// -----------------------------------------------------------------------------

static DEVICES: Once<Mutex<Vec<PciDevice>>> = Once::new();

fn probe_function(bus: u8, dev: u8, func: u8) -> Option<PciDevice> {
    let addr = PciAddr { bus, dev, func };
    let vendor_device = read_u32(addr, 0x00);
    let vendor_id = (vendor_device & 0xFFFF) as u16;
    if vendor_id == 0xFFFF { return None; }
    let device_id = (vendor_device >> 16) as u16;

    let class_reg = read_u32(addr, 0x08);
    let revision    = (class_reg & 0xFF) as u8;
    let prog_if     = ((class_reg >> 8)  & 0xFF) as u8;
    let subclass    = ((class_reg >> 16) & 0xFF) as u8;
    let class_code  = ((class_reg >> 24) & 0xFF) as u8;

    let header_type = read_u8(addr, 0x0E);

    // BARs (seulement pour type 0 = standard device)
    let mut bars = [0u32; 6];
    if header_type & 0x7F == 0x00 {
        for i in 0..6 {
            bars[i] = read_u32(addr, 0x10 + (i as u8) * 4);
        }
    }

    let irq_line = read_u8(addr, 0x3C);
    let irq_pin  = read_u8(addr, 0x3D);

    Some(PciDevice {
        addr, vendor_id, device_id,
        class_code, subclass, prog_if, revision,
        header_type, bars, irq_line, irq_pin,
    })
}

fn probe_slot(bus: u8, dev: u8, out: &mut Vec<PciDevice>) {
    // Function 0 en premier : nécessaire pour savoir si multi-function (bit 7).
    let f0 = match probe_function(bus, dev, 0) {
        Some(d) => d,
        None => return,
    };
    let multi = (f0.header_type & 0x80) != 0;
    out.push(f0);
    if !multi { return; }
    for func in 1..8 {
        if let Some(d) = probe_function(bus, dev, func) {
            out.push(d);
        }
    }
}

/// Scan brute-force bus 0..=255, device 0..32. Suffisant pour un émulateur QEMU.
pub fn init() {
    let mut v = Vec::new();
    for bus in 0u8..=255 {
        for dev in 0u8..32 {
            probe_slot(bus, dev, &mut v);
        }
    }
    crate::println!("[pci] {} device(s) détecté(s)", v.len());
    DEVICES.call_once(|| Mutex::new(v));
}

pub fn devices() -> Option<&'static Mutex<Vec<PciDevice>>> {
    DEVICES.get()
}

/// Trouve le premier device matchant (vendor, device).
pub fn find(vendor: u16, device: u16) -> Option<PciDevice> {
    let lock = DEVICES.get()?.lock();
    lock.iter().find(|d| d.vendor_id == vendor && d.device_id == device).copied()
}

/// Trouve le premier device d'une (class, subclass) donnée.
pub fn find_class(class: u8, subclass: u8) -> Option<PciDevice> {
    let lock = DEVICES.get()?.lock();
    lock.iter().find(|d| d.class_code == class && d.subclass == subclass).copied()
}

// -----------------------------------------------------------------------------
// Petits utilitaires pour le shell
// -----------------------------------------------------------------------------

pub fn vendor_name(id: u16) -> &'static str {
    match id {
        0x1022 => "AMD",
        0x106B => "Apple",
        0x10DE => "NVIDIA",
        0x10EC => "Realtek",
        0x1234 => "QEMU (BOCHS)",
        0x1AF4 => "Red Hat (virtio)",
        0x1B36 => "Red Hat (QEMU)",
        0x14E4 => "Broadcom",
        0x15AD => "VMware",
        0x8086 => "Intel",
        _      => "??",
    }
}
