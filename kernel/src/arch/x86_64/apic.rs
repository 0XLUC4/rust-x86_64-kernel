// =============================================================================
// apic.rs — Local APIC (xAPIC mode) + I/O APIC.
//
// Cette brique remplace conceptuellement le PIC 8259 :
//   - Local APIC (LAPIC)  : un par CPU ; gère timer, IPI, EOI, LINT0/1.
//   - I/O APIC            : un (ou plusieurs) ; route les IRQ hardware vers
//                           les LAPIC via le système GSI.
//
// Phase I : on expose les primitives (read/write, EOI, mask PIC, enable
// IOAPIC routes). On ne bascule PAS la source d'IRQ timer/keyboard — le PIC
// reste maître de IRQ0/IRQ1 tant que Phase II n'active pas la préemption.
//
// Référence : Intel SDM Vol 3, chap 10 (LAPIC) & 11 (IOAPIC spec Intel 82093AA).
// =============================================================================

use core::ptr::{read_volatile, write_volatile};
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

use crate::acpi::{AcpiInfo, IoApic as IoApicInfo};
use crate::memory::paging;

// -----------------------------------------------------------------------------
// Registres Local APIC (offsets en bytes depuis la MMIO base)
// -----------------------------------------------------------------------------

const LAPIC_ID:           usize = 0x020;
const LAPIC_VERSION:      usize = 0x030;
const LAPIC_TPR:          usize = 0x080;   // Task Priority
const LAPIC_EOI:          usize = 0x0B0;
const LAPIC_LDR:          usize = 0x0D0;
const LAPIC_DFR:          usize = 0x0E0;
const LAPIC_SPURIOUS:     usize = 0x0F0;
const LAPIC_ESR:          usize = 0x280;
const LAPIC_ICR_LOW:      usize = 0x300;
const LAPIC_ICR_HIGH:     usize = 0x310;
const LAPIC_LVT_TIMER:    usize = 0x320;
const LAPIC_LVT_LINT0:    usize = 0x350;
const LAPIC_LVT_LINT1:    usize = 0x360;
const LAPIC_LVT_ERROR:    usize = 0x370;
const LAPIC_TIMER_INIT:   usize = 0x380;
const LAPIC_TIMER_CURR:   usize = 0x390;
const LAPIC_TIMER_DIV:    usize = 0x3E0;

/// Bit « Mask » dans les LVT.
const LVT_MASK: u32 = 1 << 16;
/// Bit « APIC Enable » dans SVR.
const SVR_ENABLE: u32 = 1 << 8;
/// Vecteur dédié aux interrupts spurious (doit avoir bit 0..3 = 0xF).
pub const SPURIOUS_VECTOR: u8 = 0xFF;

// -----------------------------------------------------------------------------
// Singleton LAPIC
// -----------------------------------------------------------------------------

pub struct LocalApic {
    base: *mut u32,      // VirtAddr de la MMIO LAPIC (identity-mapped)
}

// SAFETY: LAPIC est mappé identity avec NO_CACHE, accès strict via volatile.
unsafe impl Send for LocalApic {}

impl LocalApic {
    #[inline]
    unsafe fn read(&self, off: usize) -> u32 {
        read_volatile(self.base.add(off / 4))
    }
    #[inline]
    unsafe fn write(&self, off: usize, val: u32) {
        write_volatile(self.base.add(off / 4), val);
    }

    pub fn id(&self) -> u32 { unsafe { self.read(LAPIC_ID) >> 24 } }
    pub fn version(&self) -> u32 { unsafe { self.read(LAPIC_VERSION) & 0xFF } }

    /// Signale la fin de l'interrupt courante.
    #[inline]
    pub fn eoi(&self) {
        unsafe { self.write(LAPIC_EOI, 0) }
    }

    /// Active le LAPIC (Software Enable + vecteur spurious).
    pub fn enable(&self) {
        unsafe {
            // Clear TPR pour accepter toutes les priorités
            self.write(LAPIC_TPR, 0);
            // Mask tous les LVT pour un démarrage propre
            self.write(LAPIC_LVT_TIMER,  LVT_MASK);
            self.write(LAPIC_LVT_LINT0,  LVT_MASK);
            self.write(LAPIC_LVT_LINT1,  LVT_MASK);
            self.write(LAPIC_LVT_ERROR,  LVT_MASK);
            // Spurious Vector Register : enable bit + vecteur 0xFF
            self.write(LAPIC_SPURIOUS,   SVR_ENABLE | SPURIOUS_VECTOR as u32);
            // Error Status : clear
            self.write(LAPIC_ESR, 0);
            self.write(LAPIC_ESR, 0);
        }
    }
}

static LAPIC: Once<Mutex<LocalApic>> = Once::new();

/// Accès global sécurisé au LAPIC (panique si non initialisé).
pub fn lapic() -> &'static Mutex<LocalApic> {
    LAPIC.get().expect("apic::init() non appelé")
}

/// Envoie un EOI si le LAPIC est initialisé (no-op sinon).
pub fn eoi_if_ready() {
    if let Some(m) = LAPIC.get() {
        m.lock().eoi();
    }
}

// -----------------------------------------------------------------------------
// I/O APIC
// -----------------------------------------------------------------------------

const IOAPIC_REG_ID:   u32 = 0x00;
const IOAPIC_REG_VER:  u32 = 0x01;
const IOAPIC_REG_ARB:  u32 = 0x02;
const IOAPIC_REDTBL_BASE: u32 = 0x10;  // 24 entrées, chacune sur 2 u32

pub struct IoApicDev {
    info: IoApicInfo,
    base: *mut u32,   // MMIO identity-mapped
}
unsafe impl Send for IoApicDev {}

impl IoApicDev {
    #[inline]
    unsafe fn read(&self, reg: u32) -> u32 {
        // Select puis data
        write_volatile(self.base.add(0), reg);
        read_volatile(self.base.add(4))   // offset 0x10 / 4 = 4
    }
    #[inline]
    unsafe fn write(&self, reg: u32, val: u32) {
        write_volatile(self.base.add(0), reg);
        write_volatile(self.base.add(4), val);
    }

    pub fn id(&self) -> u32 { unsafe { (self.read(IOAPIC_REG_ID) >> 24) & 0xF } }

    /// Nombre d'entrées dans la table de redirection.
    pub fn max_redirection(&self) -> u32 {
        unsafe { ((self.read(IOAPIC_REG_VER) >> 16) & 0xFF) + 1 }
    }

    /// Programme une entrée de redirection : GSI → (vecteur, LAPIC cible).
    pub fn set_redirection(
        &self,
        gsi: u32,
        vector: u8,
        lapic_id: u8,
        masked: bool,
        flags: u16,
    ) {
        if gsi < self.info.gsi_base { return; }
        let idx = gsi - self.info.gsi_base;
        if idx >= self.max_redirection() { return; }

        // Flags MPS (du MADT IRQ override) :
        //   bits 0-1 polarity  : 00=bus default, 01=active high, 11=active low
        //   bits 2-3 trigger   : 00=bus default, 01=edge, 11=level
        let polarity = (flags >> 0) & 0b11;
        let trigger  = (flags >> 2) & 0b11;
        let active_low  = polarity == 0b11;
        let level_trig  = trigger  == 0b11;

        let mut low: u32 = vector as u32;
        // Delivery mode : Fixed (000)
        // Destination mode : Physical (0)
        if active_low { low |= 1 << 13; }
        if level_trig { low |= 1 << 15; }
        if masked     { low |= 1 << 16; }
        let high: u32 = (lapic_id as u32) << 24;

        unsafe {
            self.write(IOAPIC_REDTBL_BASE + idx * 2 + 1, high);
            self.write(IOAPIC_REDTBL_BASE + idx * 2,     low);
        }
    }

    /// Masque toutes les entrées (état post-reset sûr).
    pub fn mask_all(&self) {
        let n = self.max_redirection();
        for i in 0..n {
            unsafe {
                self.write(IOAPIC_REDTBL_BASE + i * 2,     LVT_MASK);
                self.write(IOAPIC_REDTBL_BASE + i * 2 + 1, 0);
            }
        }
    }

    pub fn info(&self) -> &IoApicInfo { &self.info }
}

static IOAPICS: Once<Mutex<alloc::vec::Vec<IoApicDev>>> = Once::new();

pub fn io_apics() -> Option<&'static Mutex<alloc::vec::Vec<IoApicDev>>> {
    IOAPICS.get()
}

// -----------------------------------------------------------------------------
// Init global : masque PIC, init LAPIC, map IOAPICs
// -----------------------------------------------------------------------------

/// Masque complètement le PIC 8259 (ICW + OCW1 0xFF sur les deux).
/// Appelé AVANT de basculer sur l'APIC pour les nouvelles sources d'IRQ.
/// Ici, Phase I, on ne l'appelle PAS (le PIC reste maître du timer/keyboard).
#[allow(dead_code)]
pub fn disable_pic() {
    unsafe {
        let mut p21: Port<u8> = Port::new(0x21);
        let mut pa1: Port<u8> = Port::new(0xA1);
        pa1.write(0xFF);
        p21.write(0xFF);
    }
}

/// Init complet : doit être appelé APRÈS `paging::init()` et `acpi::init()`.
/// Ne panique pas si l'ACPI n'a rien trouvé (fallback gracieux).
pub fn init(info: &AcpiInfo) {
    if info.lapic_phys_addr == 0 {
        crate::println!("[apic] aucune info ACPI exploitable — skip");
        return;
    }

    // 1) Map la MMIO LAPIC (1 page)
    let lapic_virt = match paging::map_mmio(info.lapic_phys_addr, 0x400) {
        Ok(v) => v,
        Err(e) => { crate::println!("[apic] map LAPIC: {}", e); return; }
    };
    let lapic = LocalApic { base: lapic_virt as *mut u32 };
    lapic.enable();
    let lid = lapic.id();
    let lver = lapic.version();
    crate::println!("[apic] LAPIC v{:#x} enabled (id={}) @ {:#x}", lver, lid, lapic_virt);
    LAPIC.call_once(|| Mutex::new(lapic));

    // 2) Map chaque IOAPIC (1 page) + mask all
    let mut devs = alloc::vec::Vec::new();
    for ioapic in &info.io_apics {
        let virt = match paging::map_mmio(ioapic.address as u64, 0x20) {
            Ok(v) => v,
            Err(e) => { crate::println!("[apic] map IOAPIC: {}", e); continue; }
        };
        let dev = IoApicDev { info: *ioapic, base: virt as *mut u32 };
        let n = dev.max_redirection();
        dev.mask_all();
        crate::println!("[apic] IOAPIC id={} @ {:#x} GSI base={} entries={}",
            ioapic.id, virt, ioapic.gsi_base, n);
        devs.push(dev);
    }
    IOAPICS.call_once(|| Mutex::new(devs));
}
