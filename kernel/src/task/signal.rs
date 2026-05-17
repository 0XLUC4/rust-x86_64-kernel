// =============================================================================
// task/signal.rs — signaux POSIX minimaux.
//
// On supporte :
//   SIGINT  (2)  : terminaison (Ctrl-C depuis shell)
//   SIGKILL (9)  : terminaison immédiate, pas catchable
//   SIGTERM (15) : terminaison polie
//   SIGSEGV (11) : fault mémoire
//
// Pas de handler userspace pour l'instant — tous les signaux provoquent la
// terminaison du process. Extension future : `sigaction(sig, handler)` avec
// setup d'une frame user pour invoquer le handler, puis `sigreturn` via
// syscall pour restaurer l'état.
// =============================================================================

use alloc::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Int,
    Kill,
    Term,
    Segv,
    Ill,
    Bus,
}

impl Signal {
    pub fn from_num(n: u32) -> Option<Self> {
        match n {
            2  => Some(Signal::Int),
            7  => Some(Signal::Bus),
            9  => Some(Signal::Kill),
            11 => Some(Signal::Segv),
            4  => Some(Signal::Ill),
            15 => Some(Signal::Term),
            _ => None,
        }
    }

    pub fn num(self) -> u32 {
        match self {
            Signal::Int => 2,
            Signal::Ill => 4,
            Signal::Bus => 7,
            Signal::Kill => 9,
            Signal::Segv => 11,
            Signal::Term => 15,
        }
    }

    /// Exit code produit quand le signal termine le process.
    pub fn exit_code(self) -> i32 { 128 + self.num() as i32 }
}

pub struct SignalQueue {
    pending: VecDeque<Signal>,
}

impl SignalQueue {
    pub const fn new() -> Self { SignalQueue { pending: VecDeque::new() } }
    pub fn push(&mut self, sig: Signal) { self.pending.push_back(sig); }
    pub fn pop(&mut self) -> Option<Signal> { self.pending.pop_front() }
    pub fn is_empty(&self) -> bool { self.pending.is_empty() }
}

/// Appelé depuis le timer IRQ : livre les signaux en attente au process courant.
/// Pour l'instant : toute réception équivaut à un kill (terminaison).
pub fn deliver_pending(_frame: &mut crate::task::preempt::TrapFrame) {
    let sig = {
        let mut table = crate::task::process::PROCS.lock();
        let pid = table.current_pid();
        if pid == 0 { return; }
        let cur = match table.current() {
            Some(p) => p,
            None => return,
        };
        cur.signals.pop()
    };

    if let Some(s) = sig {
        crate::serial_println!("[sig] process courant reçoit {:?}", s);
        crate::task::process::exit_current(s.exit_code());
    }
}
