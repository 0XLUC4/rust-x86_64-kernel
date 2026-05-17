pub mod gdt;
pub mod idt;
pub mod pic;
pub mod apic;
pub mod backtrace;
pub mod percpu;

/// Boucle HLT — halt le CPU jusqu'à la prochaine interrupt.
/// Utilisé comme fallback (panic, idle, fin de _start).
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
