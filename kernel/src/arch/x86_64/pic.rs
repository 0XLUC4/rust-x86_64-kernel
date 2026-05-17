// =============================================================================
// pic.rs — gestion du PIC 8259 (Programmable Interrupt Controller).
//
// Par défaut, les IRQ hardware atterrissent sur les vecteurs 0..15 — qui
// entrent en conflit avec les exceptions CPU (ex: IRQ8 = vecteur 8 =
// double fault). Il faut remapper sur 32..47.
//
// On passe à l'APIC sur les systèmes modernes, mais le PIC 8259 reste
// universellement supporté et parfait pour apprendre.
// =============================================================================

use pic8259::ChainedPics;
use spin::Mutex;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// Paire de PICs en cascade (master + slave).
/// `initialize()` doit être appelé avant toute interrupt.
pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

/// Index des IRQ remappées qu'on gère.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,         // IRQ0  -> 32
    Keyboard,                     // IRQ1  -> 33
    // IRQ2 = cascade (réservé au PIC slave)
    Mouse = PIC_2_OFFSET + 4,     // IRQ12 -> 44 (PS/2 mouse)
}

impl InterruptIndex {
    pub fn as_u8(self) -> u8 { self as u8 }
    pub fn as_usize(self) -> usize { usize::from(self.as_u8()) }
}
