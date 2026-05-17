// =============================================================================
// e1000.rs — Intel 82540EM (e1000) NIC driver pour QEMU.
//
// QEMU fournit un e1000 comme NIC par défaut (-netdev user,id=n0 -device
// e1000,netdev=n0). PCI vendor=8086 device=100E.
//
// Architecture :
//   - MMIO via BAR0 (registres mappés en mémoire)
//   - Ring buffer TX : descripteurs 16 octets, statique (64 entrées)
//   - Ring buffer RX : descripteurs 16 octets, pre-alloués (64 buffers de 2 KiB)
//   - Pas d'interruption pour l'instant : polling depuis le network stack
//
// Registres principaux :
//   CTRL    (0x0000) : Device Control
//   STATUS  (0x0008) : Device Status
//   EERD    (0x0014) : EEPROM Read
//   ICR     (0x00C0) : Interrupt Cause Read
//   IMS     (0x00D0) : Interrupt Mask Set
//   IMC     (0x00D8) : Interrupt Mask Clear
//   RCTL    (0x0100) : Receive Control
//   RDBAL   (0x2800) : RX Descriptor Base Low
//   RDBAH   (0x2804) : RX Descriptor Base High
//   RDLEN   (0x2808) : RX Descriptor Length
//   RDH     (0x2810) : RX Descriptor Head
//   RDT     (0x2818) : RX Descriptor Tail
//   TCTL    (0x0400) : Transmit Control
//   TDBAL   (0x3800) : TX Descriptor Base Low
//   TDBAH   (0x3804) : TX Descriptor Base High
//   TDLEN   (0x3808) : TX Descriptor Length
//   TDH     (0x3810) : TX Descriptor Head
//   TDT     (0x3818) : TX Descriptor Tail
//   RAL     (0x5400) : Receive Address Low
//   RAH     (0x5404) : Receive Address High
//   MTA     (0x5200) : Multicast Table Array (128 × u32)
// =============================================================================

use alloc::vec;
use core::ptr;
use spin::{Mutex, Once};

use crate::pci;
use crate::memory::paging;

// e1000 PCI identifiers
const E1000_VENDOR: u16 = 0x8086;
const E1000_DEVICE_82540EM: u16 = 0x100E;
const E1000_DEVICE_82545EM: u16 = 0x100F;
const E1000_DEVICE_82574L: u16 = 0x10D3;

// Registres
const REG_CTRL:   u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EERD:   u32 = 0x0014;
const REG_ICR:    u32 = 0x00C0;
const REG_IMS:    u32 = 0x00D0;
const REG_IMC:    u32 = 0x00D8;
const REG_RCTL:   u32 = 0x0100;
const REG_RDBAL:  u32 = 0x2800;
const REG_RDBAH:  u32 = 0x2804;
const REG_RDLEN:  u32 = 0x2808;
const REG_RDH:    u32 = 0x2810;
const REG_RDT:    u32 = 0x2818;
const REG_TCTL:   u32 = 0x0400;
const REG_TDBAL:  u32 = 0x3800;
const REG_TDBAH:  u32 = 0x3804;
const REG_TDLEN:  u32 = 0x3808;
const REG_TDH:    u32 = 0x3810;
const REG_TDT:    u32 = 0x3818;
const REG_RAL:    u32 = 0x5400;
const REG_RAH:    u32 = 0x5404;
const REG_MTA:    u32 = 0x5200;

// CTRL bits
const CTRL_FD:     u32 = 1 << 0;   // Full Duplex
const CTRL_ASDE:   u32 = 1 << 5;   // Auto-Speed Detection Enable
const CTRL_SLU:    u32 = 1 << 6;   // Set Link Up
const CTRL_RST:    u32 = 1 << 26;  // Device Reset

// RCTL bits
const RCTL_EN:     u32 = 1 << 1;   // Receiver Enable
const RCTL_SBP:    u32 = 1 << 2;   // Store Bad Packets
const RCTL_UPE:    u32 = 1 << 3;   // Unicast Promiscuous Enable
const RCTL_MPE:    u32 = 1 << 4;   // Multicast Promiscuous Enable
const RCTL_LBM:    u32 = 0 << 6;   // Loopback mode off
const RCTL_BAM:    u32 = 1 << 15;  // Broadcast Accept Mode
const RCTL_BSIZE:  u32 = 0 << 16;  // Buffer size = 2048 (00)
const RCTL_SECRC:  u32 = 1 << 26;  // Strip Ethernet CRC

// TCTL bits
const TCTL_EN:     u32 = 1 << 1;   // Transmit Enable
const TCTL_PSP:    u32 = 1 << 3;   // Pad Short Packets
const TCTL_CT_SHIFT: u32 = 4;       // Collision Threshold
const TCTL_COLD_SHIFT: u32 = 12;    // Collision Distance

// Descriptor bits
const RDESC_STATUS_DD:   u8 = 1 << 0;
const RDESC_STATUS_EOP:  u8 = 1 << 1;
const TDESC_CMD_EOP:     u8 = 1 << 0;
const TDESC_CMD_IFCS:    u8 = 1 << 1;
const TDESC_CMD_RS:      u8 = 1 << 3;
const TDESC_STATUS_DD:   u8 = 1 << 0;

const NUM_RX_DESC: usize = 64;
const NUM_TX_DESC: usize = 64;
const RX_BUF_SIZE: usize = 2048;

/// RX descriptor (legacy format, 16 bytes)
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct RxDesc {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

/// TX descriptor (legacy format, 16 bytes)
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxDesc {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

/// Alignement garanti pour les descriptor rings (128 bytes).
#[repr(C, align(128))]
struct RxRing([RxDesc; NUM_RX_DESC]);
#[repr(C, align(128))]
struct TxRing([TxDesc; NUM_TX_DESC]);

pub struct E1000 {
    mmio_base: u64,
    mac: [u8; 6],
    rx_ring: *mut RxRing,
    tx_ring: *mut TxRing,
    rx_bufs: *mut [[u8; RX_BUF_SIZE]; NUM_RX_DESC],
    rx_cur: usize,
    tx_cur: usize,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

// SAFETY: E1000 n'est accédé que via NIC_LOCK (Mutex).
unsafe impl Send for E1000 {}
unsafe impl Sync for E1000 {}

impl E1000 {
    #[inline]
    fn read_reg(&self, reg: u32) -> u32 {
        unsafe { ptr::read_volatile((self.mmio_base + reg as u64) as *const u32) }
    }

    #[inline]
    fn write_reg(&self, reg: u32, val: u32) {
        unsafe { ptr::write_volatile((self.mmio_base + reg as u64) as *mut u32, val); }
    }

    /// Lit l'adresse MAC depuis les registres RAL/RAH.
    fn read_mac(&self) -> [u8; 6] {
        let lo = self.read_reg(REG_RAL);
        let hi = self.read_reg(REG_RAH);
        [
            (lo & 0xFF) as u8,
            ((lo >> 8) & 0xFF) as u8,
            ((lo >> 16) & 0xFF) as u8,
            ((lo >> 24) & 0xFF) as u8,
            (hi & 0xFF) as u8,
            ((hi >> 8) & 0xFF) as u8,
        ]
    }

    /// Init MMIO, reset, link up, configure RX/TX rings.
    fn init_device(mmio_base: u64) -> Result<Self, &'static str> {
        crate::serial_println!("[e1000] init_device: mmio_base={:#x}", mmio_base);
        let mut dev = E1000 {
            mmio_base,
            mac: [0; 6],
            rx_ring: ptr::null_mut(),
            tx_ring: ptr::null_mut(),
            rx_bufs: ptr::null_mut(),
            rx_cur: 0,
            tx_cur: 0,
            rx_packets: 0,
            tx_packets: 0,
        };

        crate::serial_println!("[e1000] read REG_CTRL");
        let ctrl = dev.read_reg(REG_CTRL);
        crate::serial_println!("[e1000] CTRL = {:#x}", ctrl);
        dev.write_reg(REG_CTRL, ctrl | CTRL_SLU | CTRL_ASDE);
        crate::serial_println!("[e1000] write REG_CTRL ok");

        crate::serial_println!("[e1000] disable IRQs");
        dev.write_reg(REG_IMC, 0xFFFFFFFF);
        let _ = dev.read_reg(REG_ICR);

        crate::serial_println!("[e1000] clear MTA");
        for i in 0..128 {
            dev.write_reg(REG_MTA + i * 4, 0);
        }

        crate::serial_println!("[e1000] read MAC");
        dev.mac = dev.read_mac();
        crate::serial_println!("[e1000] MAC read: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            dev.mac[0], dev.mac[1], dev.mac[2], dev.mac[3], dev.mac[4], dev.mac[5]);

        // Force RAL/RAH avec bit Address Valid (AV = bit 31 de RAH).
        // Indispensable : sans AV, la NIC drop tous les paquets unicast vers nous.
        let ral = (dev.mac[0] as u32)
            | ((dev.mac[1] as u32) << 8)
            | ((dev.mac[2] as u32) << 16)
            | ((dev.mac[3] as u32) << 24);
        let rah = (dev.mac[4] as u32)
            | ((dev.mac[5] as u32) << 8)
            | (1u32 << 31); // AV
        dev.write_reg(REG_RAL, ral);
        dev.write_reg(REG_RAH, rah);
        crate::serial_println!("[e1000] RAL/RAH programmés avec AV=1");

        crate::serial_println!("[e1000] init_rx — allouer {} KiB", 64*2048/1024);
        dev.init_rx()?;
        crate::serial_println!("[e1000] init_rx OK");
        crate::serial_println!("[e1000] init_tx");
        dev.init_tx()?;
        crate::serial_println!("[e1000] init_tx OK");

        Ok(dev)
    }

    fn init_rx(&mut self) -> Result<(), &'static str> {
        use alloc::{boxed::Box, vec};

        // Alloue le ring (1 KiB, OK sur stack) puis box-ifie
        let ring_initial = RxRing([RxDesc {
            addr: 0, length: 0, checksum: 0, status: 0, errors: 0, special: 0,
        }; NUM_RX_DESC]);
        let ring = Box::into_raw(Box::new(ring_initial));

        // Alloue les buffers (128 KiB) via Vec pour éviter un array stack-allocated
        let mut bufs_vec: alloc::vec::Vec<[u8; RX_BUF_SIZE]> = vec![[0u8; RX_BUF_SIZE]; NUM_RX_DESC];
        let bufs_slice = bufs_vec.as_mut_slice();
        let bufs_ptr = bufs_slice.as_mut_ptr() as *mut [[u8; RX_BUF_SIZE]; NUM_RX_DESC];
        core::mem::forget(bufs_vec);
        let bufs = bufs_ptr;

        // Configure chaque descripteur avec l'adresse physique de son buffer
        // (identity-mapping : virt == phys pour les adresses < 1 GiB)
        unsafe {
            for i in 0..NUM_RX_DESC {
                let buf_phys = &(*bufs)[i] as *const [u8; RX_BUF_SIZE] as u64;
                (*ring).0[i].addr = buf_phys;
                (*ring).0[i].status = 0;
            }
        }

        let ring_phys = ring as u64;
        let ring_len = (NUM_RX_DESC * core::mem::size_of::<RxDesc>()) as u32;

        self.write_reg(REG_RDBAL, ring_phys as u32);
        self.write_reg(REG_RDBAH, (ring_phys >> 32) as u32);
        self.write_reg(REG_RDLEN, ring_len);
        self.write_reg(REG_RDH, 0);
        self.write_reg(REG_RDT, (NUM_RX_DESC - 1) as u32);

        // Enable receiver. UPE/MPE = promiscuous (dev/debug) — garantit qu'on
        // voit tous les paquets même si le filtre unicast/multicast cloche.
        self.write_reg(REG_RCTL,
            RCTL_EN | RCTL_BAM | RCTL_BSIZE | RCTL_SECRC | RCTL_LBM
            | RCTL_UPE | RCTL_MPE);

        self.rx_ring = ring;
        self.rx_bufs = bufs;
        self.rx_cur = 0;

        Ok(())
    }

    fn init_tx(&mut self) -> Result<(), &'static str> {
        use alloc::boxed::Box;

        let ring = Box::into_raw(Box::new(TxRing([TxDesc {
            addr: 0, length: 0, cso: 0, cmd: 0, status: 0, css: 0, special: 0,
        }; NUM_TX_DESC])));

        let ring_phys = ring as u64;
        let ring_len = (NUM_TX_DESC * core::mem::size_of::<TxDesc>()) as u32;

        self.write_reg(REG_TDBAL, ring_phys as u32);
        self.write_reg(REG_TDBAH, (ring_phys >> 32) as u32);
        self.write_reg(REG_TDLEN, ring_len);
        self.write_reg(REG_TDH, 0);
        self.write_reg(REG_TDT, 0);

        // Enable transmitter
        self.write_reg(REG_TCTL,
            TCTL_EN | TCTL_PSP
            | (15 << TCTL_CT_SHIFT)
            | (64 << TCTL_COLD_SHIFT));

        self.tx_ring = ring;
        self.tx_cur = 0;

        Ok(())
    }

    /// Envoie un paquet. Copie les données dans un buffer TX interne.
    pub fn send(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() > 1514 { return Err("paquet trop grand"); }

        let idx = self.tx_cur;

        // Attend que le descripteur soit libre (DD = done)
        // Pour le premier envoi, status = 0 (non-DD) → on le considère libre.
        unsafe {
            let desc = &mut (*self.tx_ring).0[idx];
            // Alloue un buffer temporaire pour les données (on pourrait optimiser)
            let buf = alloc::vec![0u8; data.len()];
            let buf_ptr = alloc::boxed::Box::into_raw(buf.into_boxed_slice());
            let slice = &mut *buf_ptr;
            slice.copy_from_slice(data);

            desc.addr = slice.as_ptr() as u64;
            desc.length = data.len() as u16;
            desc.cmd = TDESC_CMD_EOP | TDESC_CMD_IFCS | TDESC_CMD_RS;
            desc.status = 0;
        }

        self.tx_cur = (self.tx_cur + 1) % NUM_TX_DESC;
        self.write_reg(REG_TDT, self.tx_cur as u32);

        self.tx_packets += 1;
        Ok(())
    }

    /// Tente de recevoir un paquet. Retourne None si rien de disponible.
    pub fn recv(&mut self) -> Option<alloc::vec::Vec<u8>> {
        let idx = self.rx_cur;

        unsafe {
            let desc = &mut (*self.rx_ring).0[idx];
            if desc.status & RDESC_STATUS_DD == 0 {
                return None; // Pas de paquet prêt
            }

            let len = desc.length as usize;
            let buf = &(*self.rx_bufs)[idx];
            let data = buf[..len].to_vec();

            // Réinitialise le descripteur
            desc.status = 0;
            desc.length = 0;

            // Avance le tail
            let old_cur = self.rx_cur;
            self.rx_cur = (self.rx_cur + 1) % NUM_RX_DESC;
            self.write_reg(REG_RDT, old_cur as u32);

            self.rx_packets += 1;
            Some(data)
        }
    }

    pub fn mac(&self) -> [u8; 6] { self.mac }

    pub fn link_up(&self) -> bool {
        self.read_reg(REG_STATUS) & 2 != 0
    }

    /// Diagnostic : dump état RX (registres + 1er descripteur).
    pub fn rx_debug(&self) -> (u32, u32, u32, u32, u8, u16) {
        let rctl = self.read_reg(REG_RCTL);
        let rdh = self.read_reg(REG_RDH);
        let rdt = self.read_reg(REG_RDT);
        let status = self.read_reg(REG_STATUS);
        let (d0_status, d0_len) = unsafe {
            let d = &(*self.rx_ring).0[0];
            (d.status, d.length)
        };
        (rctl, rdh, rdt, status, d0_status, d0_len)
    }
}

static NIC: Once<Mutex<E1000>> = Once::new();

/// Initialise le premier e1000 trouvé sur le bus PCI.
pub fn init() {
    crate::serial_println!("[e1000] step 1: scan PCI");
    let (addr, bar) = {
        let devs_lock = match pci::devices() {
            Some(l) => l.lock(),
            None => { crate::println!("[e1000] PCI non initialisé"); return; }
        };

        let found = devs_lock.iter().find(|d| {
            d.vendor_id == E1000_VENDOR
                && (d.device_id == E1000_DEVICE_82540EM
                    || d.device_id == E1000_DEVICE_82545EM
                    || d.device_id == E1000_DEVICE_82574L)
        });

        match found {
            Some(dev) => {
                crate::serial_println!("[e1000] found at {:02x}:{:02x}.{} dev={:#06x} bar0={:#x}",
                    dev.addr.bus, dev.addr.dev, dev.addr.func, dev.device_id, dev.bars[0]);
                (dev.addr, dev.bars[0] & !0xF)
            }
            None => {
                crate::println!("[e1000] aucun NIC Intel e1000 détecté");
                return;
            }
        }
    };

    if bar == 0 {
        crate::println!("[e1000] BAR0 invalide");
        return;
    }

    let (cmd_before, cmd_after) = pci::enable_device_io_mmio_bus_master(addr);
    if cmd_after != cmd_before {
        crate::serial_println!(
            "[e1000] PCI COMMAND {:#06x} -> {:#06x} (io|mem|busmaster)",
            cmd_before,
            cmd_after
        );
    } else {
        crate::serial_println!(
            "[e1000] PCI COMMAND {:#06x} (io|mem|busmaster déjà actifs)",
            cmd_before
        );
    }

    let mmio_base = bar as u64;
    crate::serial_println!("[e1000] step 2: map_mmio @ {:#x}", mmio_base);
    if let Err(e) = paging::map_mmio(mmio_base, 128 * 1024) {
        crate::serial_println!("[e1000] map_mmio FAILED: {}", e);
        return;
    }
    crate::serial_println!("[e1000] step 3: map_mmio OK, init_device");

    match E1000::init_device(mmio_base) {
        Ok(dev) => {
            crate::serial_println!("[e1000] step 4: init_device OK");
            let mac = dev.mac();
            let link = dev.link_up();
            crate::println!("[e1000] MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  link={}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                if link { "up" } else { "down" });
            NIC.call_once(|| Mutex::new(dev));
        }
        Err(e) => crate::serial_println!("[e1000] init_device FAILED: {}", e),
    }
}

pub fn nic() -> Option<&'static Mutex<E1000>> {
    NIC.get()
}

pub fn mac_address() -> Option<[u8; 6]> {
    NIC.get().map(|n| n.lock().mac())
}
