// =============================================================================
// drivers/nvme.rs — NVMe controller skeleton.
//
// État : *skeleton*. Détecte un device PCI NVMe (class 0x01, subclass 0x08),
// mappe BAR0 (registres MMIO), construit l'Admin Submission/Completion Queue
// (queue 0), envoie un IDENTIFY CONTROLLER, parse vendor + model + serial.
//
// Ne fait PAS encore d'I/O :
//   - Pas de Submission Queue I/O (queue 1+)
//   - Pas de namespace enumeration via IDENTIFY NAMESPACE
//   - Pas de READ / WRITE
//   - Pas d'IRQ — polling MMIO uniquement
//
// Spec : NVMe Base Specification 2.0 (https://nvmexpress.org/specifications/).
// =============================================================================

use alloc::string::String;
use alloc::vec::Vec;
use spin::{Mutex, Once};

use crate::pci::{self, PciAddr};

// -----------------------------------------------------------------------------
// Registres MMIO du contrôleur (offsets dans BAR0)
// -----------------------------------------------------------------------------

const REG_CAP:    u64 = 0x00; // Controller Capabilities (8 bytes)
const REG_VS:     u64 = 0x08; // Version
const REG_CC:     u64 = 0x14; // Controller Configuration
const REG_CSTS:   u64 = 0x1C; // Controller Status
const REG_AQA:    u64 = 0x24; // Admin Queue Attributes
const REG_ASQ:    u64 = 0x28; // Admin Submission Queue base (8 bytes)
const REG_ACQ:    u64 = 0x30; // Admin Completion Queue base (8 bytes)
const REG_SQ0TDBL:u64 = 0x1000; // Admin SQ Tail Doorbell (assume stride=0)

const CC_ENABLE: u32 = 1 << 0;
const CSTS_RDY:  u32 = 1 << 0;

const ADMIN_QUEUE_DEPTH: u16 = 32;

// -----------------------------------------------------------------------------
// Commandes admin
// -----------------------------------------------------------------------------

const ADMIN_OP_IDENTIFY: u8 = 0x06;
const IDENT_CNS_CONTROLLER: u32 = 0x01;

// -----------------------------------------------------------------------------
// Représentation in-memory
// -----------------------------------------------------------------------------

pub struct Nvme {
    pub addr: PciAddr,
    mmio_base: u64,
    /// Admin Submission Queue (64-byte entries × QD).
    asq: alloc::boxed::Box<[u8; 64 * ADMIN_QUEUE_DEPTH as usize]>,
    /// Admin Completion Queue (16-byte entries × QD).
    acq: alloc::boxed::Box<[u8; 16 * ADMIN_QUEUE_DEPTH as usize]>,
    sq_tail: u16,
    cq_head: u16,
    cq_phase: u8,
    pub vendor_id: u16,
    pub model: String,
    pub serial: String,
}

static NVME: Once<Mutex<Nvme>> = Once::new();

impl Nvme {
    /// Lit un u32 dans la zone MMIO du controller.
    #[inline]
    fn mmio_read32(&self, off: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.mmio_base + off) as *const u32) }
    }
    #[inline]
    fn mmio_read64(&self, off: u64) -> u64 {
        unsafe { core::ptr::read_volatile((self.mmio_base + off) as *const u64) }
    }
    #[inline]
    fn mmio_write32(&self, off: u64, val: u32) {
        unsafe { core::ptr::write_volatile((self.mmio_base + off) as *mut u32, val) }
    }
    #[inline]
    fn mmio_write64(&self, off: u64, val: u64) {
        unsafe { core::ptr::write_volatile((self.mmio_base + off) as *mut u64, val) }
    }

    /// Initialise le controller : disable, configure queues, enable, wait READY.
    fn init(&mut self) -> Result<(), &'static str> {
        // Disable
        let cc = self.mmio_read32(REG_CC);
        self.mmio_write32(REG_CC, cc & !CC_ENABLE);

        // Wait until !RDY
        let mut spins = 0;
        while self.mmio_read32(REG_CSTS) & CSTS_RDY != 0 {
            spins += 1;
            if spins > 1_000_000 { return Err("nvme: timeout disable"); }
        }

        // Configure AQA (admin queue sizes, log2 entries).
        let aqa = ((ADMIN_QUEUE_DEPTH as u32 - 1) << 16)
                | (ADMIN_QUEUE_DEPTH as u32 - 1);
        self.mmio_write32(REG_AQA, aqa);

        // Charge ASQ et ACQ bases (PA = VA, identity-mapped).
        let asq_pa = self.asq.as_ptr() as u64;
        let acq_pa = self.acq.as_ptr() as u64;
        self.mmio_write64(REG_ASQ, asq_pa);
        self.mmio_write64(REG_ACQ, acq_pa);

        // Configure CC : IOSQES=6 (64), IOCQES=4 (16), MPS=0 (4K), CSS=0 (NVM).
        let cc_new: u32 = (6 << 20)   // IOCQES
                        | (4 << 16)   // IOSQES — note layout NVMe : IOSQES=20:23, IOCQES=16:19
                        | (0 << 11)   // AMS
                        | (0 <<  7)   // MPS
                        | (0 <<  4)   // CSS
                        | CC_ENABLE;
        self.mmio_write32(REG_CC, cc_new);

        // Wait RDY=1
        spins = 0;
        while self.mmio_read32(REG_CSTS) & CSTS_RDY == 0 {
            spins += 1;
            if spins > 5_000_000 { return Err("nvme: timeout enable"); }
        }
        Ok(())
    }

    /// Soumet une commande IDENTIFY CONTROLLER, attend la complétion,
    /// remplit `self.model` / `self.serial` / `self.vendor_id`.
    fn identify_controller(&mut self) -> Result<(), &'static str> {
        // Buffer de 4 KiB pour la struct Identify Controller.
        let mut buf: alloc::boxed::Box<[u8; 4096]> = alloc::boxed::Box::new([0u8; 4096]);
        let buf_pa = buf.as_ptr() as u64;

        // Construit l'entrée SQ 64 bytes.
        // Layout NVMe Submission Queue Entry (SQE) :
        //   bytes 0..4  : CDW0 (opcode + flags + cid)
        //   4..8        : NSID
        //   16..32      : MPTR / DPTR (PRP1, PRP2)
        //   40..44      : CDW10 (CNS for Identify)
        let mut sqe = [0u8; 64];
        let cid: u16 = 1;
        sqe[0] = ADMIN_OP_IDENTIFY;            // CDW0 opcode
        sqe[2] = (cid & 0xff) as u8;           // CDW0 cid lo
        sqe[3] = (cid >> 8)   as u8;           // CDW0 cid hi
        // NSID = 0 (controller-level Identify)
        // PRP1 @ bytes 24..32 (after MPTR 16..24)
        sqe[24..32].copy_from_slice(&buf_pa.to_le_bytes());
        // CDW10 : CNS = 0x01 (Identify Controller)
        sqe[40..44].copy_from_slice(&IDENT_CNS_CONTROLLER.to_le_bytes());

        let slot = self.sq_tail as usize * 64;
        self.asq[slot..slot + 64].copy_from_slice(&sqe);
        self.sq_tail = (self.sq_tail + 1) % ADMIN_QUEUE_DEPTH;

        // Sonne la doorbell SQ.
        self.mmio_write32(REG_SQ0TDBL, self.sq_tail as u32);

        // Poll la CQE (16 bytes). Le bit phase de l'entrée doit matcher self.cq_phase.
        let mut spins = 0;
        loop {
            let cq_off = self.cq_head as usize * 16;
            let status = u16::from_le_bytes([
                self.acq[cq_off + 14], self.acq[cq_off + 15],
            ]);
            let phase = (status & 1) as u8;
            if phase == self.cq_phase {
                // Complétion reçue. Status code = bits 1..16 de status.
                let sc = status >> 1;
                self.cq_head = (self.cq_head + 1) % ADMIN_QUEUE_DEPTH;
                if self.cq_head == 0 { self.cq_phase ^= 1; }
                if sc != 0 { return Err("nvme: identify failed (non-zero SC)"); }
                break;
            }
            spins += 1;
            if spins > 50_000_000 { return Err("nvme: timeout identify"); }
        }

        // Parse Identify Controller :
        //   VID at 0..2, SSVID at 2..4, SN at 4..24, MN at 24..64, FR at 64..72.
        self.vendor_id = u16::from_le_bytes([buf[0], buf[1]]);
        self.serial = trim_ascii(&buf[4..24]).into();
        self.model  = trim_ascii(&buf[24..64]).into();
        Ok(())
    }
}

fn trim_ascii(b: &[u8]) -> &str {
    let s = core::str::from_utf8(b).unwrap_or("");
    s.trim_end_matches(' ').trim_end_matches('\0')
}

// -----------------------------------------------------------------------------
// API globale
// -----------------------------------------------------------------------------

/// Scanne le bus PCI pour un controller NVMe, monte le premier trouvé,
/// envoie IDENTIFY CONTROLLER.
pub fn probe_and_init() -> Result<(), &'static str> {
    let devs = pci::devices().ok_or("nvme: pci non init")?;
    let lock = devs.lock();
    let dev = lock.iter()
        .find(|d| d.class_code == 0x01 && d.subclass == 0x08)
        .ok_or("nvme: pas de controller trouvé")?;
    let bar0 = dev.bars[0] as u64;
    if bar0 == 0 { return Err("nvme: BAR0 vide"); }
    // Mask les bits flags (4 LSB pour mémoire, 1 LSB pour I/O).
    let mmio_base = bar0 & !0xF;
    let pci_addr = dev.addr;
    drop(lock);

    // Map MMIO du controller (taille minimum 0x2000 : registres + doorbells).
    crate::memory::paging::map_mmio(mmio_base, 0x2000)?;
    // Active bus master + MMIO sur le device.
    pci::enable_device_io_mmio_bus_master(pci_addr);

    let mut nvme = Nvme {
        addr: pci_addr,
        mmio_base,
        asq: alloc::boxed::Box::new([0u8; 64 * ADMIN_QUEUE_DEPTH as usize]),
        acq: alloc::boxed::Box::new([0u8; 16 * ADMIN_QUEUE_DEPTH as usize]),
        sq_tail: 0,
        cq_head: 0,
        cq_phase: 1, // initial phase 1 (clean queue read as 0)
        vendor_id: 0,
        model:  String::new(),
        serial: String::new(),
    };

    crate::serial_println!("[nvme] controller @ MMIO {:#x}, CAP={:#x} VS={:#x}",
        nvme.mmio_base, nvme.mmio_read64(REG_CAP), nvme.mmio_read32(REG_VS));

    nvme.init()?;
    nvme.identify_controller()?;
    crate::println!("[nvme] {} '{}' sn='{}' VID={:#06x}",
        nvme.addr.bus, nvme.model, nvme.serial, nvme.vendor_id);

    NVME.call_once(|| Mutex::new(nvme));
    Ok(())
}

pub fn controller() -> Option<&'static Mutex<Nvme>> { NVME.get() }
