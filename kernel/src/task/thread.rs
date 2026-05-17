// =============================================================================
// thread.rs — threads kernel avec context switch coopératif.
//
// Ce scheduler est SÉPARÉ de l'executor async. Les deux cohabitent :
//   - executor async (task/executor.rs) : futures poll-driven, tout dans la
//     même "thread" kernel (le thread "main")
//   - threads kernel (ici)              : vraies piles, context switch, peuvent
//                                         tourner en parallèle conceptuellement
//
// Dans un vrai OS, on aurait un seul scheduler qui gère les deux. Pour
// l'exercice, on garde les choses séparées — les threads kernel servent
// typiquement à des travaux longs qui ne fit pas bien dans async
// (ex: worker qui scanne un disque, daemon).
//
// Modèle : round-robin coopératif. Chaque thread appelle `yield_now()`
// volontairement. Passer en préemptif = yield_now() depuis le handler PIT.
// =============================================================================

use alloc::{boxed::Box, collections::VecDeque, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

extern "C" {
    fn context_switch(old_rsp: *mut usize, new_rsp: usize);
}

const STACK_SIZE: usize = 16 * 1024;  // 16 KiB par thread

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState { Runnable, Running, Finished }

pub struct Thread {
    pub id: u64,
    pub name: String,
    pub state: ThreadState,
    rsp: usize,
    // On garde la stack vivante tant que le thread existe
    _stack: Box<[u8]>,
}

impl Thread {
    /// Crée un nouveau thread avec la fonction d'entrée donnée.
    pub fn new(name: &str, entry: fn()) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        // Alloue la stack (alignée 16 octets comme exige la SysV ABI au moment du call)
        let mut stack = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
        let stack_top = stack.as_mut_ptr() as usize + STACK_SIZE;

        // On prépare la stack pour que le premier `context_switch` vers ce
        // thread fasse un `ret` directement vers `thread_bootstrap`.
        //
        // État attendu par la seconde moitié de context_switch :
        //   popfq, pop r15, pop r14, pop r13, pop r12, pop rbx, pop rbp, ret
        //
        // Layout (bottom -> top) juste avant le ret :
        //   [r15=0][r14=0][r13=0][r12=0][rbx=0][rbp=0][rflags=0x202][return_addr]
        //                                                     ^          ^
        //                                                     |          |
        //   ret va pop return_addr et jumper dessus -----------+          |
        //   popfq va charger RFLAGS avant -------------------------------+
        //
        // NB: 0x202 = IF|bit1(réservé toujours 1). Active les interrupts
        //     au démarrage du thread.
        //
        // On push aussi `entry as usize` en tant que "donnée" que le
        // bootstrap va chercher via un mécanisme à part. Ici on utilise
        // une astuce simple : le thread démarre directement sur `entry`
        // via le return address.

        let stack_top = (stack_top & !0xf) - 8;  // align 16, ajusté pour ret

        // Pour permettre au bootstrap d'appeler `entry` puis de marquer
        // le thread comme fini, on construit une petite trampoline asm
        // en Rust via une fn #[naked] — ou plus simplement, on rend le
        // thread responsable d'appeler `thread_exit()` quand entry return.
        // On empile donc :
        //   [thread_exit_wrapper (comme return addr après entry)]
        //   [entry               (comme return addr du context_switch)]
        // Non : le premier pop après le switch est RFLAGS, puis callee-saved,
        // puis le ret final va sur la première return-addr. On veut qu'il
        // atterrisse sur `thread_bootstrap`, qui va ensuite call `entry`
        // puis `thread_exit`. Donc on pousse juste thread_bootstrap et on
        // utilise un slot de callee-saved (r15) pour passer `entry`.
        //
        // Astuce plus simple adoptée ici : le kernel garde `entry` dans
        // une map id->fn, et thread_bootstrap la lit. Voir plus bas.

        // SAFETY: on manipule une stack fraîchement allouée, personne n'y touche.
        let rsp = unsafe {
            let mut sp = stack_top as *mut usize;
            // Push la "return address" = thread_bootstrap
            sp = sp.offset(-1); *sp = thread_bootstrap as usize;
            // Push 6 callee-saved + rflags (= 7 valeurs, tous à 0 sauf rflags)
            sp = sp.offset(-1); *sp = 0;          // rbp
            sp = sp.offset(-1); *sp = 0;          // rbx
            sp = sp.offset(-1); *sp = 0;          // r12
            sp = sp.offset(-1); *sp = 0;          // r13
            sp = sp.offset(-1); *sp = 0;          // r14
            sp = sp.offset(-1); *sp = 0;          // r15
            sp = sp.offset(-1); *sp = 0x202;      // rflags (IF set)
            sp as usize
        };

        // Enregistre l'entry point pour le bootstrap
        ENTRY_POINTS.lock().insert(id, entry);

        Thread {
            id,
            name: String::from(name),
            state: ThreadState::Runnable,
            rsp,
            _stack: stack,
        }
    }
}

/// Entrée des threads : lit entry depuis la map, l'exécute, termine.
/// Doit être extern "C" pour être appelable comme return address.
extern "C" fn thread_bootstrap() -> ! {
    let id = SCHEDULER.lock().current_id();
    let entry = ENTRY_POINTS.lock().get(&id).copied().expect("entry manquante");
    entry();
    exit();
}

/// Termine le thread courant.
pub fn exit() -> ! {
    {
        let mut s = SCHEDULER.lock();
        s.mark_current_finished();
    }
    // Cède la main : on ne reviendra jamais ici
    yield_now();
    unreachable!("yield après exit d'un thread")
}

/// Cède volontairement le CPU au prochain thread runnable.
pub fn yield_now() {
    let (old_rsp_ptr, new_rsp) = {
        let mut s = SCHEDULER.lock();
        match s.pick_next() {
            Some((old, new)) => (old, new),
            None => return,  // pas d'autre thread, on continue
        }
    };
    // SAFETY: old_rsp_ptr pointe sur le field `rsp` d'un Thread vivant,
    // new_rsp est le RSP sauvegardé d'un autre Thread vivant.
    unsafe { context_switch(old_rsp_ptr, new_rsp); }
}

// -----------------------------------------------------------------------------
// Scheduler global
// -----------------------------------------------------------------------------

use alloc::collections::BTreeMap;

pub struct Scheduler {
    threads: Vec<Thread>,
    current: usize,  // index dans threads
    ready_queue: VecDeque<usize>,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler { threads: Vec::new(), current: 0, ready_queue: VecDeque::new() }
    }

    fn current_id(&self) -> u64 {
        self.threads.get(self.current).map(|t| t.id).unwrap_or(0)
    }

    fn mark_current_finished(&mut self) {
        if let Some(t) = self.threads.get_mut(self.current) {
            t.state = ThreadState::Finished;
        }
    }

    /// Retourne (old_rsp_ptr, new_rsp) si un switch est nécessaire.
    fn pick_next(&mut self) -> Option<(*mut usize, usize)> {
        // Cherche le prochain runnable différent du courant
        let n = self.threads.len();
        if n < 2 { return None; }
        for offset in 1..=n {
            let idx = (self.current + offset) % n;
            if self.threads[idx].state == ThreadState::Runnable {
                // Courant redevient runnable (s'il n'est pas fini)
                if self.threads[self.current].state == ThreadState::Running {
                    self.threads[self.current].state = ThreadState::Runnable;
                }
                let old_rsp_ptr = &mut self.threads[self.current].rsp as *mut usize;
                let new_rsp = self.threads[idx].rsp;
                self.threads[idx].state = ThreadState::Running;
                self.current = idx;
                return Some((old_rsp_ptr, new_rsp));
            }
        }
        None
    }

    pub fn spawn(&mut self, thread: Thread) -> u64 {
        let id = thread.id;
        self.threads.push(thread);
        // Le premier thread ajouté devient le "main" (Running)
        if self.threads.len() == 1 {
            self.threads[0].state = ThreadState::Running;
            self.current = 0;
        }
        id
    }

    pub fn list(&self) -> Vec<(u64, String, ThreadState)> {
        self.threads.iter().map(|t| (t.id, t.name.clone(), t.state)).collect()
    }

    /// Nettoie les threads finis (sauf le courant).
    pub fn reap(&mut self) {
        let cur = self.current;
        let mut removed_before_cur = 0;
        self.threads.retain_with_index(|i, t| {
            if t.state == ThreadState::Finished && i != cur {
                if i < cur { removed_before_cur += 1; }
                ENTRY_POINTS.lock().remove(&t.id);
                false
            } else { true }
        });
        self.current -= removed_before_cur;
    }
}

// Petit helper qui manque à Vec (stable)
trait RetainWithIndex<T> {
    fn retain_with_index<F: FnMut(usize, &T) -> bool>(&mut self, f: F);
}
impl<T> RetainWithIndex<T> for Vec<T> {
    fn retain_with_index<F: FnMut(usize, &T) -> bool>(&mut self, mut f: F) {
        let mut i = 0;
        self.retain(|x| { let k = f(i, x); i += 1; k });
    }
}

pub static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());
static ENTRY_POINTS: Mutex<BTreeMap<u64, fn()>> = Mutex::new(BTreeMap::new());

/// Spawn un nouveau thread kernel.
pub fn spawn(name: &str, entry: fn()) -> u64 {
    let thread = Thread::new(name, entry);
    SCHEDULER.lock().spawn(thread)
}

/// Initialise le scheduler en enregistrant le thread courant comme "main".
/// À appeler une fois depuis `_start` avant tout spawn.
pub fn init_as_main() {
    let mut s = SCHEDULER.lock();
    if s.threads.is_empty() {
        // On crée un Thread "placeholder" pour le contexte actuel : son rsp
        // sera rempli au premier yield_now() via context_switch.
        let stack = alloc::vec![0u8; 0].into_boxed_slice();
        s.threads.push(Thread {
            id: 0,
            name: String::from("main"),
            state: ThreadState::Running,
            rsp: 0,
            _stack: stack,
        });
        s.current = 0;
    }
}
