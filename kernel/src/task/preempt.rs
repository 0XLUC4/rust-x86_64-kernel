// =============================================================================
// task/preempt.rs — structure TrapFrame + dispatcher d'interruption préemptif.
//
// Le TrapFrame est remplie par `boot/preempt_entry.asm` avant d'appeler Rust.
// Ordre des champs : rigoureusement le même que l'asm (ne pas modifier !).
//
// Layout (adresses montantes) :
//   gs_base       : u64      (offset  0)
//   fs_base       : u64      (offset  8)
//   r15           : u64      (offset 16)
//   r14           : u64      (offset 24)
//   r13           : u64
//   r12
//   r11
//   r10
//   r9
//   r8
//   rbp
//   rdi
//   rsi
//   rdx
//   rcx
//   rbx
//   rax
//   _pad          : u64      (slot "alignement" empilé en 1er par l'asm)
//   rip           : u64      (iret frame: pushé par CPU)
//   cs            : u64
//   rflags        : u64
//   rsp           : u64
//   ss            : u64
// =============================================================================

#[repr(C)]
#[derive(Default, Clone)]
pub struct TrapFrame {
    pub gs_base: u64,
    pub fs_base: u64,
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub _pad: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Appelé par `timer_preempt_entry` via `timer_tick_rust` (défini dans idt.rs).
/// Le timer doit :
///   1. déclencher un reschedule si un autre process est runnable
///   2. délivrer éventuellement un signal en attente au process courant
pub fn on_timer(frame: *mut TrapFrame) {
    // SAFETY: frame pointe sur une TrapFrame valide sur la kernel stack, écriture
    // sérialisée par "un seul thread à la fois dans le handler IRQ" (IF=0).
    let frame = unsafe { &mut *frame };

    // Livre les signaux pending au process courant
    crate::task::signal::deliver_pending(frame);

    // Si on vient de ring 3, tenter un reschedule
    if (frame.cs & 3) == 3 {
        crate::task::process::reschedule(frame);
    }
}
