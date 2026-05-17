// =============================================================================
// virtio — transport PCI commun à tous les devices virtio (1.0+).
//
// Un device virtio 1.0 expose via PCI plusieurs "capabilities" custom (type 0x09)
// qui pointent vers différents registres MMIO :
//
//   - VIRTIO_PCI_CAP_COMMON_CFG (1)    : reset, status, features, queue select
//   - VIRTIO_PCI_CAP_NOTIFY_CFG (2)    : écriture pour notifier le device
//   - VIRTIO_PCI_CAP_ISR_CFG    (3)    : ISR status
//   - VIRTIO_PCI_CAP_DEVICE_CFG (4)    : config spécifique au device
//   - VIRTIO_PCI_CAP_PCI_CFG    (5)    : alternative, on l'ignore
//
// On traite ici le transport générique ; les drivers spécifiques (virtio-gpu,
// virtio-net) utilisent ce transport pour envoyer des commandes via des
// virtqueues.
// =============================================================================

pub mod queue;
pub mod gpu;

use alloc::vec::Vec;
use core::ptr;

use crate::memory::paging;
use crate::pci::{self, PciDevice};

// PCI vendor ID attribué à Red Hat pour tous les devices virtio.
pub const VIRTIO_VENDOR_ID: u16 = 0x1af4;

// Device IDs virtio 1.0+ (transitional IDs 0x1000+N, non-transitional 0x1040+N).
pub const VIRTIO_DEVICE_ID_GPU:         u16 = 0x1050; // non-transitional
pub const VIRTIO_DEVICE_ID_GPU_LEGACY:  u16 = 0x1040 + 16;

// Types de capabilities virtio.
pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG:    u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;
pub const VIRTIO_PCI_CAP_PCI_CFG:    u8 = 5;

// Status bits (common_cfg.device_status).
pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1 << 0;
pub const VIRTIO_STATUS_DRIVER:      u8 = 1 << 1;
pub const VIRTIO_STATUS_DRIVER_OK:   u8 = 1 << 2;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 1 << 3;
pub const VIRTIO_STATUS_FAILED:      u8 = 1 << 7;

/// Structure VIRTIO_PCI_CAP (commune à tous les types de caps).
#[derive(Debug, Clone, Copy)]
pub struct VirtioPciCap {
    pub cfg_type: u8,    // VIRTIO_PCI_CAP_*
    pub bar:      u8,    // BAR index (0..5)
    pub offset:   u32,   // offset dans le BAR
    pub length:   u32,   // taille de la structure
    pub notify_off_multiplier: u32, // uniquement pour NOTIFY_CFG
}

/// Layout d'un common_cfg virtio (spec v1.0 §4.1.4.3).
/// On l'accède pixel-par-pixel via `read_*`/`write_*` pour contourner l'absence
/// d'alignement garanti côté Rust.
#[repr(C)]
pub struct VirtioPciCommonCfg {
    // Device-level
    pub device_feature_select: u32,   // 0x00 (RW)
    pub device_feature:        u32,   // 0x04 (RO)
    pub driver_feature_select: u32,   // 0x08 (RW)
    pub driver_feature:        u32,   // 0x0C (RW)
    pub config_msix_vector:    u16,   // 0x10 (RW)
    pub num_queues:            u16,   // 0x12 (RO)
    pub device_status:         u8,    // 0x14 (RW)
    pub config_generation:     u8,    // 0x15 (RO)
    // Queue-level (sélection par queue_select)
    pub queue_select:          u16,   // 0x16 (RW)
    pub queue_size:            u16,   // 0x18 (RW)
    pub queue_msix_vector:     u16,   // 0x1A (RW)
    pub queue_enable:          u16,   // 0x1C (RW)
    pub queue_notify_off:      u16,   // 0x1E (RO)
    pub queue_desc:            u64,   // 0x20 (RW)
    pub queue_driver:          u64,   // 0x28 (RW) = avail ring
    pub queue_device:          u64,   // 0x30 (RW) = used ring
}

/// Transport PCI virtio concret, rattaché à un device.
pub struct VirtioTransport {
    pub pci_dev:       PciDevice,
    pub common_cfg:    *mut u8,     // MMIO ptr vers common_cfg
    pub notify_base:   *mut u8,     // MMIO ptr vers notify_cfg
    pub notify_off_multiplier: u32,
    pub isr_cfg:       *mut u8,
    pub device_cfg:    *mut u8,
}

// SAFETY: tous les accès MMIO sont volatiles et sérialisés par les locks
// des drivers au-dessus.
unsafe impl Send for VirtioTransport {}
unsafe impl Sync for VirtioTransport {}

impl VirtioTransport {
    /// Tente d'initialiser un transport à partir d'un PciDevice virtio.
    /// Walk les capabilities, map chaque région MMIO nécessaire.
    pub fn probe(dev: PciDevice) -> Result<Self, &'static str> {
        if dev.vendor_id != VIRTIO_VENDOR_ID {
            return Err("virtio: vendor ID inattendu");
        }

        // Active I/O+MMIO+bus master.
        pci::enable_device_io_mmio_bus_master(dev.addr);

        // Cherche le pointeur de la première capability : offset 0x34 dans la
        // config PCI (device with Capabilities List bit in Status set).
        let status = pci::read_u16(dev.addr, 0x06);
        if status & (1 << 4) == 0 {
            return Err("virtio: pas de capabilities list");
        }
        let mut cap_off = pci::read_u8(dev.addr, 0x34) & 0xfc;

        let mut common_cfg: Option<(u8, u32, u32)> = None;
        let mut notify_cfg: Option<(u8, u32, u32, u32)> = None;
        let mut isr_cfg:    Option<(u8, u32, u32)> = None;
        let mut device_cfg: Option<(u8, u32, u32)> = None;

        // Walk jusqu'à une profondeur raisonnable pour éviter boucles infinies.
        for _ in 0..64 {
            if cap_off == 0 { break; }
            let vendor_id = pci::read_u8(dev.addr, cap_off);       // 0x09 = Vendor-specific
            let next      = pci::read_u8(dev.addr, cap_off + 1) & 0xfc;
            if vendor_id == 0x09 {
                let _len     = pci::read_u8(dev.addr, cap_off + 2);
                let cfg_type = pci::read_u8(dev.addr, cap_off + 3);
                let bar      = pci::read_u8(dev.addr, cap_off + 4);
                let offset   = pci::read_u32(dev.addr, cap_off + 8);
                let length   = pci::read_u32(dev.addr, cap_off + 12);

                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => common_cfg = Some((bar, offset, length)),
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        let mult = pci::read_u32(dev.addr, cap_off + 16);
                        notify_cfg = Some((bar, offset, length, mult));
                    }
                    VIRTIO_PCI_CAP_ISR_CFG    => isr_cfg    = Some((bar, offset, length)),
                    VIRTIO_PCI_CAP_DEVICE_CFG => device_cfg = Some((bar, offset, length)),
                    _ => {}
                }
            }
            cap_off = next;
        }

        let common = common_cfg.ok_or("virtio: pas de COMMON_CFG")?;
        let notify = notify_cfg.ok_or("virtio: pas de NOTIFY_CFG")?;
        let isr    = isr_cfg.ok_or("virtio: pas d'ISR_CFG")?;
        let devcfg = device_cfg.ok_or("virtio: pas de DEVICE_CFG")?;

        let common_ptr = map_bar_region(&dev, common.0, common.1, common.2)?;
        let notify_ptr = map_bar_region(&dev, notify.0, notify.1, notify.2)?;
        let isr_ptr    = map_bar_region(&dev, isr.0, isr.1, isr.2)?;
        let dev_ptr    = map_bar_region(&dev, devcfg.0, devcfg.1, devcfg.2)?;

        Ok(VirtioTransport {
            pci_dev: dev,
            common_cfg: common_ptr,
            notify_base: notify_ptr,
            notify_off_multiplier: notify.3,
            isr_cfg: isr_ptr,
            device_cfg: dev_ptr,
        })
    }

    /// Offsets dans common_cfg (cf §4.1.4.3).
    const OFF_DEV_FEAT_SEL:    usize = 0x00;
    const OFF_DEV_FEAT:        usize = 0x04;
    const OFF_DRV_FEAT_SEL:    usize = 0x08;
    const OFF_DRV_FEAT:        usize = 0x0C;
    const OFF_NUM_QUEUES:      usize = 0x12;
    const OFF_DEV_STATUS:      usize = 0x14;
    const OFF_QUEUE_SELECT:    usize = 0x16;
    const OFF_QUEUE_SIZE:      usize = 0x18;
    const OFF_QUEUE_ENABLE:    usize = 0x1C;
    const OFF_QUEUE_NOTIFY_OFF:usize = 0x1E;
    const OFF_QUEUE_DESC:      usize = 0x20;
    const OFF_QUEUE_DRIVER:    usize = 0x28;
    const OFF_QUEUE_DEVICE:    usize = 0x30;

    // --- common_cfg getters/setters (volatiles) ---
    pub fn read_u8 (&self, off: usize) -> u8  { unsafe { ptr::read_volatile(self.common_cfg.add(off)) } }
    pub fn read_u16(&self, off: usize) -> u16 { unsafe { ptr::read_volatile(self.common_cfg.add(off) as *const u16) } }
    pub fn read_u32(&self, off: usize) -> u32 { unsafe { ptr::read_volatile(self.common_cfg.add(off) as *const u32) } }
    pub fn write_u8 (&self, off: usize, v: u8)  { unsafe { ptr::write_volatile(self.common_cfg.add(off), v) } }
    pub fn write_u16(&self, off: usize, v: u16) { unsafe { ptr::write_volatile(self.common_cfg.add(off) as *mut u16, v) } }
    pub fn write_u32(&self, off: usize, v: u32) { unsafe { ptr::write_volatile(self.common_cfg.add(off) as *mut u32, v) } }
    pub fn write_u64(&self, off: usize, v: u64) { unsafe { ptr::write_volatile(self.common_cfg.add(off) as *mut u64, v) } }

    pub fn status(&self) -> u8 { self.read_u8(Self::OFF_DEV_STATUS) }
    pub fn set_status(&self, s: u8) { self.write_u8(Self::OFF_DEV_STATUS, s); }

    pub fn num_queues(&self) -> u16 { self.read_u16(Self::OFF_NUM_QUEUES) }

    /// Séquence de reset → features OK → driver OK (§3.1).
    pub fn init_base(&self) -> Result<(), &'static str> {
        // Reset : écrire 0 et attendre que le device le confirme.
        self.set_status(0);
        while self.status() != 0 { core::hint::spin_loop(); }

        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE);
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

        // On ne négocie aucune feature étendue : on accepte juste VERSION_1.
        // Feature bit 32 = VIRTIO_F_VERSION_1.
        self.write_u32(Self::OFF_DEV_FEAT_SEL, 1);
        let dev_feat_hi = self.read_u32(Self::OFF_DEV_FEAT);
        if dev_feat_hi & (1 << (32 - 32)) == 0 {
            // Vraiment rare — device pré-1.0. On abandonne.
            self.set_status(VIRTIO_STATUS_FAILED);
            return Err("virtio: device non-1.0, pas supporté");
        }
        // On set VERSION_1 uniquement.
        self.write_u32(Self::OFF_DRV_FEAT_SEL, 1);
        self.write_u32(Self::OFF_DRV_FEAT, 1 << (32 - 32));
        self.write_u32(Self::OFF_DRV_FEAT_SEL, 0);
        self.write_u32(Self::OFF_DRV_FEAT, 0);

        // FEATURES_OK → relire status pour vérifier que le device accepte.
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);
        if self.status() & VIRTIO_STATUS_FEATURES_OK == 0 {
            self.set_status(VIRTIO_STATUS_FAILED);
            return Err("virtio: features pas acceptées");
        }
        Ok(())
    }

    /// Marque le driver complètement prêt (après setup des queues).
    pub fn driver_ok(&self) {
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK);
    }

    /// Configure une virtqueue (select=index, size, descriptors addresses).
    pub fn setup_queue(&self, index: u16, size: u16,
                       desc_phys: u64, avail_phys: u64, used_phys: u64) -> u16
    {
        self.write_u16(Self::OFF_QUEUE_SELECT, index);
        let max = self.read_u16(Self::OFF_QUEUE_SIZE);
        let actual = if size > max { max } else { size };
        self.write_u16(Self::OFF_QUEUE_SIZE, actual);
        self.write_u64(Self::OFF_QUEUE_DESC,   desc_phys);
        self.write_u64(Self::OFF_QUEUE_DRIVER, avail_phys);
        self.write_u64(Self::OFF_QUEUE_DEVICE, used_phys);
        self.write_u16(Self::OFF_QUEUE_ENABLE, 1);
        self.read_u16(Self::OFF_QUEUE_NOTIFY_OFF)
    }

    /// Notifie le device qu'une queue a des descripteurs à traiter.
    pub fn notify(&self, queue_notify_off: u16) {
        let off = queue_notify_off as usize * self.notify_off_multiplier as usize;
        unsafe {
            ptr::write_volatile(self.notify_base.add(off) as *mut u16, 0);
        }
    }
}

/// Mappe une région du BAR `bar_index` avec offset+length donnés.
/// Retourne un pointeur virtuel utilisable pour accès MMIO.
fn map_bar_region(dev: &PciDevice, bar_index: u8, offset: u32, length: u32)
    -> Result<*mut u8, &'static str>
{
    let bar_raw = dev.bars[bar_index as usize] as u64;
    let is_mmio = bar_raw & 1 == 0;
    if !is_mmio { return Err("virtio: BAR en port I/O, non supporté"); }
    let is_64bit = (bar_raw >> 1) & 0b11 == 0b10;

    let mut base = bar_raw & !0xF;
    if is_64bit {
        let hi = dev.bars[bar_index as usize + 1] as u64;
        base |= hi << 32;
    }
    let phys = base + offset as u64;

    // On map une page de plus que nécessaire pour absorber les alignements.
    let len = (length as usize + 0xFFF) & !0xFFF;
    paging::map_mmio(phys, len)?;
    Ok(phys as *mut u8)
}

/// Détecte toutes les devices virtio présentes sur le bus PCI.
pub fn detect_virtio_devices() -> Vec<PciDevice> {
    let devs = match pci::devices() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let guard = devs.lock();
    guard.iter()
        .filter(|d| d.vendor_id == VIRTIO_VENDOR_ID)
        .cloned()
        .collect()
}
