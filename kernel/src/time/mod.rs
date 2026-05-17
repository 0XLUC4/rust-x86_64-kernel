// =============================================================================
// time — PIT (Programmable Interval Timer) et horloge monotonique.
//
// Le PIT 8254 est cadencé à 1.193182 MHz. On programme le canal 0 pour
// générer une IRQ0 à intervalles réguliers.
//
// Expose :
//   - uptime_ms() : ms depuis le boot
//   - Sleep future : async sleep(ms)
// =============================================================================

pub mod sleep;

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

/// Fréquence de base du PIT (Hz).
const PIT_FREQ: u32 = 1_193_182;

/// Fréquence désirée : 100 Hz = 10 ms de résolution. Bon compromis
/// charge CPU / précision.
pub const TICKS_PER_SEC: u64 = 100;

/// Compteur de ticks depuis le boot. Incrémenté dans le handler timer.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Initialise le PIT à TICKS_PER_SEC Hz.
pub fn init() {
    let divisor = (PIT_FREQ / TICKS_PER_SEC as u32) as u16;

    // SAFETY: écriture sur les ports PIT standards, aucune interférence attendue
    // vu qu'on est en phase d'init.
    unsafe {
        let mut cmd: Port<u8> = Port::new(0x43);
        let mut data: Port<u8> = Port::new(0x40);

        // Canal 0, lobyte+hibyte, rate generator (mode 2), binary
        cmd.write(0b00_11_010_0);
        data.write((divisor & 0xff) as u8);
        data.write((divisor >> 8) as u8);
    }

    crate::println!("[time] PIT programmé à {} Hz ({} ms/tick)",
        TICKS_PER_SEC, 1000 / TICKS_PER_SEC);
}

/// Appelé depuis l'ISR timer. DOIT être rapide.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    sleep::advance();
}

/// Ticks depuis le boot.
pub fn ticks() -> u64 { TICKS.load(Ordering::Relaxed) }

/// Millisecondes depuis le boot.
pub fn uptime_ms() -> u64 {
    ticks() * 1000 / TICKS_PER_SEC
}

/// Uptime formaté (ex: "1h 23m 45s").
pub fn format_uptime() -> alloc::string::String {
    use alloc::format;
    let total = uptime_ms() / 1000;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 { format!("{}h {}m {}s", h, m, s) }
    else if m > 0 { format!("{}m {}s", m, s) }
    else { format!("{}s", s) }
}
