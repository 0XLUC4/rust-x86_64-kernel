// =============================================================================
// task/process.rs — process userspace (Phase II).
//
// Un `Process` possède :
//   - un PID unique
//   - un AddressSpace privé (P4 + mappings user)
//   - un TrapFrame sauvegardé (registres au moment du dernier context switch
//     ring 0 -> ring 3, via iretq, ou ring 3 -> ring 0 via le timer)
//   - une kernel stack (pour les entrées syscall/IRQ qui doivent sauver l'état)
//   - une queue de signaux pending
//   - un état (Runnable / Blocked / Zombie)
//
// Ce module est le **front-end** qui orchestre :
//   - execve(path)  → charge un ELF depuis le ramfs, prépare le process
//   - fork()        → clone l'AS (CoW), duplique TrapFrame
//   - exit(code)    → marque Zombie, wake parent en wait()
//   - kill(pid, sig)→ pousse signal dans la queue
//   - schedule()    → élit le prochain process runnable et l'active
//
// Convention : PID 1 = init (premier exec). PID 0 = kernel "idle".
// =============================================================================

use alloc::{boxed::Box, collections::VecDeque, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::PhysFrame;
use x86_64::structures::paging::Size4KiB;

use crate::memory::address_space::AddressSpace;
use crate::task::preempt::TrapFrame;
use crate::task::signal::{Signal, SignalQueue};

pub type Pid = u32;
pub type Uid = u32;
pub type Gid = u32;

/// UID/GID du super-utilisateur.
pub const ROOT_UID: Uid = 0;
pub const ROOT_GID: Gid = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Runnable,
    Sleeping,
    Zombie(i32),   // exit code
}

/// Adresses standards d'un process user (layout "classique") :
///   code   : chargé par ELF à son p_vaddr (typiquement 0x40_0000)
///   stack  : top à USER_STACK_TOP, 64 KiB (16 pages)
///   heap   : après le code, géré par le user (brk non implémenté ici)
pub const USER_STACK_TOP: u64 = 0x0000_7fff_ffff_f000;
pub const USER_STACK_PAGES: u64 = 16;

pub struct Process {
    pub pid: Pid,
    pub parent: Pid,
    pub name: String,
    pub state: ProcessState,
    pub address_space: Box<AddressSpace>,
    /// Stack kernel (dédiée par process pour supporter les syscalls blockants).
    pub kernel_stack: Box<[u8; KERNEL_STACK_SIZE]>,
    /// TrapFrame : état user sauvegardé avant context switch vers kernel.
    pub trap_frame: TrapFrame,
    pub signals: SignalQueue,
    /// Processus fils en attente de wait().
    pub children_exited: VecDeque<(Pid, i32)>,
    /// Identité réelle (celle qui a lancé le process, pour l'audit).
    pub uid: Uid,
    pub gid: Gid,
    /// Identité effective (celle utilisée pour les checks de permission).
    /// Peut différer de `uid` via setuid.
    pub euid: Uid,
    pub egid: Gid,
}

pub const KERNEL_STACK_SIZE: usize = 16 * 1024;

impl Process {
    /// Top (inclusif - 8) de la kernel stack — utilisé par TSS.rsp0
    /// au moment du switch vers ce process.
    pub fn kernel_stack_top(&self) -> VirtAddr {
        let addr = self.kernel_stack.as_ptr() as u64 + KERNEL_STACK_SIZE as u64;
        VirtAddr::new(addr & !0xf)
    }
}

// -----------------------------------------------------------------------------
// PID allocator
// -----------------------------------------------------------------------------

static NEXT_PID: AtomicU64 = AtomicU64::new(1);
const DEFAULT_EXEC_PATHS: [&str; 4] = ["/", "/bin", "/fat", "/fat/bin"];
static EXEC_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn alloc_pid() -> Pid {
    NEXT_PID.fetch_add(1, Ordering::Relaxed) as Pid
}

// -----------------------------------------------------------------------------
// Table globale des process
// -----------------------------------------------------------------------------

pub struct ProcessTable {
    /// Process actifs indexés par PID.
    procs: alloc::collections::BTreeMap<Pid, Box<Process>>,
    /// Queue runnable (FIFO simple).
    runnable: VecDeque<Pid>,
    /// PID du process courant (0 = kernel idle).
    current: Pid,
}

impl ProcessTable {
    const fn new() -> Self {
        ProcessTable {
            procs: alloc::collections::BTreeMap::new(),
            runnable: VecDeque::new(),
            current: 0,
        }
    }

    pub fn current_pid(&self) -> Pid { self.current }

    pub fn current(&mut self) -> Option<&mut Process> {
        let pid = self.current;
        self.procs.get_mut(&pid).map(|b| b.as_mut())
    }

    /// Snapshot (pid, uid, euid) du process courant, pour affichage.
    /// Retourne None si on est en contexte kernel (pas de process user actif).
    pub fn iter_current(&self) -> Option<(Pid, Uid, Uid)> {
        let pid = self.current;
        self.procs.get(&pid).map(|p| (p.pid, p.uid, p.euid))
    }

    pub fn get_mut(&mut self, pid: Pid) -> Option<&mut Process> {
        self.procs.get_mut(&pid).map(|b| b.as_mut())
    }

    pub fn list(&self) -> Vec<(Pid, Pid, String, ProcessState)> {
        self.procs.values()
            .map(|p| (p.pid, p.parent, p.name.clone(), p.state))
            .collect()
    }

    fn insert(&mut self, proc: Box<Process>) {
        let pid = proc.pid;
        if matches!(proc.state, ProcessState::Runnable) {
            self.runnable.push_back(pid);
        }
        self.procs.insert(pid, proc);
    }

    fn make_runnable(&mut self, pid: Pid) {
        if let Some(p) = self.procs.get_mut(&pid) {
            if !matches!(p.state, ProcessState::Zombie(_)) {
                p.state = ProcessState::Runnable;
                self.runnable.push_back(pid);
            }
        }
    }

    fn pick_next(&mut self) -> Option<Pid> {
        while let Some(pid) = self.runnable.pop_front() {
            if let Some(p) = self.procs.get(&pid) {
                if matches!(p.state, ProcessState::Runnable) {
                    return Some(pid);
                }
            }
        }
        None
    }
}

pub static PROCS: Mutex<ProcessTable> = Mutex::new(ProcessTable::new());

// -----------------------------------------------------------------------------
// execve : charge un ELF et prépare un nouveau process
// -----------------------------------------------------------------------------

/// Charge un ELF (bytes bruts) dans un nouvel AddressSpace et crée le Process.
///
/// L'identité (uid/gid) est héritée du parent si celui-ci existe ; sinon le
/// process naît en tant que root (PID 1 / init).
pub fn spawn_from_elf(name: &str, elf_bytes: &[u8], parent: Pid)
    -> Result<Pid, &'static str>
{
    let mut space = AddressSpace::new_user()?;
    let loaded = crate::fs::elf::load(elf_bytes, &mut space)?;

    // User stack : 64 KiB à USER_STACK_TOP
    let rsp_user = crate::fs::elf::map_user_stack(
        &mut space,
        VirtAddr::new(USER_STACK_TOP),
        USER_STACK_PAGES,
    )?;

    // TrapFrame initial : registres à 0, RIP=entry, RSP=stack_top, CS/SS=user
    let sel = crate::arch::x86_64::gdt::selectors();
    let mut tf = TrapFrame::default();
    tf.rip = loaded.entry.as_u64();
    tf.rsp = rsp_user.as_u64();
    tf.cs = sel.user_code.0 as u64;
    tf.ss = sel.user_data.0 as u64;
    tf.rflags = 0x202;  // IF=1, bit 1 réservé

    let kstack = alloc::boxed::Box::new([0u8; KERNEL_STACK_SIZE]);

    // Hérite de l'identité du parent si connu, sinon root.
    let (uid, gid, euid, egid) = {
        let table = PROCS.lock();
        match table.procs.get(&parent) {
            Some(p) => (p.uid, p.gid, p.euid, p.egid),
            None => (ROOT_UID, ROOT_GID, ROOT_UID, ROOT_GID),
        }
    };

    let pid = alloc_pid();
    let proc = Box::new(Process {
        pid,
        parent,
        name: String::from(name),
        state: ProcessState::Runnable,
        address_space: Box::new(space),
        kernel_stack: kstack,
        trap_frame: tf,
        signals: SignalQueue::new(),
        children_exited: VecDeque::new(),
        uid, gid, euid, egid,
    });

    crate::serial_println!("[proc] spawn pid={} '{}' entry={:#x} rsp={:#x}",
        pid, name, loaded.entry.as_u64(), rsp_user.as_u64());

    PROCS.lock().insert(proc);
    Ok(pid)
}

/// Charge un ELF depuis le ramfs et l'exécute.
pub fn exec_from_fs(path: &str, parent: Pid) -> Result<Pid, &'static str> {
    let data = {
        let fs = crate::fs::FS.lock();
        match fs.read(path) {
            Ok(d) => d,
            Err(_) => return Err("exec: fichier introuvable"),
        }
    };
    spawn_from_elf(path, &data, parent)
}

/// Charge un ELF depuis FAT32 et l'exécute.
/// Accepte un chemin FAT direct (`/bin/app`) ou préfixé (`/fat/bin/app`).
pub fn exec_from_fat(path: &str, parent: Pid) -> Result<Pid, &'static str> {
    let fat_path = path.strip_prefix("/fat").unwrap_or(path);
    let data = {
        let fat = crate::drivers::fat32::mounted().ok_or("exec: FAT32 non monté")?;
        let lock = fat.lock();
        lock.read_file(fat_path).map_err(|_| "exec: fichier introuvable")?
    };
    spawn_from_elf(path, &data, parent)
}

/// Charge un ELF depuis RAMFS, sinon fallback FAT32.
/// Si le chemin commence par `/fat`, on force la résolution FAT32.
pub fn exec_from_any(path: &str, parent: Pid) -> Result<Pid, &'static str> {
    if path == "/fat" || path.starts_with("/fat/") {
        return exec_from_fat(path, parent);
    }

    if let Ok(pid) = exec_from_fs(path, parent) {
        return Ok(pid);
    }

    exec_from_fat(path, parent)
}

fn ensure_exec_paths(paths: &mut Vec<String>) {
    if paths.is_empty() {
        for p in DEFAULT_EXEC_PATHS {
            paths.push(String::from(p));
        }
    }
}

fn push_exec_dir_unique(paths: &mut Vec<String>, dir: String) {
    if !paths.iter().any(|p| p == &dir) {
        paths.push(dir);
    }
}

fn normalize_exec_dir(dir: &str) -> Result<String, &'static str> {
    let trimmed = dir.trim();
    if trimmed.is_empty() {
        return Err("path: répertoire vide");
    }

    let mut out = if trimmed.starts_with('/') {
        String::from(trimmed)
    } else {
        alloc::format!("/{}", trimmed)
    };

    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }

    Ok(out)
}

fn join_exec_dir_and_name(dir: &str, name: &str) -> String {
    if dir == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", dir, name)
    }
}

fn exec_candidate_exists(path: &str) -> bool {
    if path == "/fat" || path.starts_with("/fat/") {
        let fat_path = path.strip_prefix("/fat").unwrap_or(path);
        if fat_path.is_empty() || fat_path == "/" {
            return false;
        }

        if let Some(fat) = crate::drivers::fat32::mounted() {
            return fat.lock().read_file(fat_path).is_ok();
        }
        return false;
    }

    {
        let fs = crate::fs::FS.lock();
        if fs.read(path).is_ok() {
            return true;
        }
    }

    if let Some(fat) = crate::drivers::fat32::mounted() {
        return fat.lock().read_file(path).is_ok();
    }

    false
}

/// Retourne la liste PATH actuelle utilisée par exec_from_search().
pub fn exec_paths() -> Vec<String> {
    let mut paths = EXEC_PATHS.lock();
    ensure_exec_paths(&mut paths);
    paths.clone()
}

/// Réinitialise PATH à la valeur par défaut du kernel.
pub fn reset_exec_paths() {
    let mut paths = EXEC_PATHS.lock();
    paths.clear();
    ensure_exec_paths(&mut paths);
}

/// Ajoute un répertoire à PATH (si non déjà présent).
pub fn add_exec_path(dir: &str) -> Result<(), &'static str> {
    let normalized = normalize_exec_dir(dir)?;
    let mut paths = EXEC_PATHS.lock();
    ensure_exec_paths(&mut paths);
    push_exec_dir_unique(&mut paths, normalized);
    Ok(())
}

/// Remplace PATH à partir d'une spécification `dir1:dir2:...`.
pub fn set_exec_paths_from_spec(spec: &str) -> Result<(), &'static str> {
    let mut next = Vec::new();
    for raw in spec.split(':') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = normalize_exec_dir(trimmed)?;
        push_exec_dir_unique(&mut next, normalized);
    }

    if next.is_empty() {
        return Err("path: liste vide");
    }

    let mut paths = EXEC_PATHS.lock();
    *paths = next;
    Ok(())
}

/// Résout un nom de commande vers un chemin exécutable, selon PATH.
pub fn resolve_exec_path(path: &str) -> Option<String> {
    if path.contains('/') {
        return if exec_candidate_exists(path) { Some(String::from(path)) } else { None };
    }

    for dir in exec_paths() {
        let candidate = join_exec_dir_and_name(&dir, path);
        if exec_candidate_exists(&candidate) {
            return Some(candidate);
        }
    }

    None
}

/// Résout un nom de binaire de façon "PATH-like".
/// Si `path` contient déjà '/', on l'exécute tel quel.
/// Sinon on parcourt la liste PATH courante.
pub fn exec_from_search(path: &str, parent: Pid) -> Result<Pid, &'static str> {
    if path.contains('/') {
        return exec_from_any(path, parent);
    }

    let resolved = resolve_exec_path(path).ok_or("exec: binaire introuvable")?;
    exec_from_any(&resolved, parent)
}

// -----------------------------------------------------------------------------
// fork (avec CoW)
// -----------------------------------------------------------------------------

/// Fork : duplique le process courant. Child reçoit un AddressSpace CoW-cloné.
/// Retourne (pid_parent_value, pid_child_value).
/// Parent voit le PID du child ; child voit 0 — convention POSIX.
pub fn fork() -> Result<Pid, &'static str> {
    let mut table = PROCS.lock();
    let cur_pid = table.current;

    // Extrait les données parent dont on a besoin, en borrow court
    let (child_space, child_tf, parent_name, uid, gid, euid, egid) = {
        let parent_proc = table.procs.get_mut(&cur_pid).ok_or("pas de process courant")?;
        let child_space = parent_proc.address_space.clone_cow()?;
        let mut child_tf = parent_proc.trap_frame.clone();
        child_tf.rax = 0;  // child voit fork() = 0
        (child_space, child_tf, parent_proc.name.clone(),
         parent_proc.uid, parent_proc.gid, parent_proc.euid, parent_proc.egid)
    };

    let child_pid = alloc_pid();
    let kstack = alloc::boxed::Box::new([0u8; KERNEL_STACK_SIZE]);

    let child_proc = Box::new(Process {
        pid: child_pid,
        parent: cur_pid,
        name: parent_name,
        state: ProcessState::Runnable,
        address_space: Box::new(child_space),
        kernel_stack: kstack,
        trap_frame: child_tf,
        signals: SignalQueue::new(),
        children_exited: VecDeque::new(),
        uid, gid, euid, egid,
    });

    crate::serial_println!("[proc] fork pid={} -> {} (CoW)", cur_pid, child_pid);

    table.insert(child_proc);

    // Parent voit le PID du child comme return value
    if let Some(parent) = table.procs.get_mut(&cur_pid) {
        parent.trap_frame.rax = child_pid as u64;
    }

    Ok(child_pid)
}

// -----------------------------------------------------------------------------
// exit / wait / kill
// -----------------------------------------------------------------------------

pub fn exit_current(code: i32) -> ! {
    // Hooks Phase V : on doit libérer les ressources kernel possédées par
    // ce process (FB ownership, mailbox IPC, refs SHM) AVANT de prendre le
    // lock PROCS pour éviter tout deadlock (ces hooks prennent leurs propres
    // locks indépendants).
    let dying_pid = PROCS.lock().current;
    crate::gfx::release_if_owner(dying_pid);
    crate::ipc::cleanup_pid(dying_pid);

    {
        let mut table = PROCS.lock();
        let pid = table.current;
        let parent_pid;
        {
            let p = table.procs.get_mut(&pid).expect("process courant manquant");
            p.state = ProcessState::Zombie(code);
            parent_pid = p.parent;
        }
        crate::serial_println!("[proc] pid={} exit({})", pid, code);

        let mut wake_parent = false;
        if let Some(parent) = table.procs.get_mut(&parent_pid) {
            parent.children_exited.push_back((pid, code));
            if matches!(parent.state, ProcessState::Sleeping) {
                parent.state = ProcessState::Runnable;
                wake_parent = true;
            }
        }
        if wake_parent {
            table.runnable.push_back(parent_pid);
        }
    }

    // Schedule un autre process, on ne revient jamais
    schedule_next();
}

/// Marque le process courant comme tué suite à une faute matérielle.
pub fn current_fault_kill(reason: &str) -> ! {
    crate::serial_println!("[proc] fault kill: {}", reason);
    exit_current(-1);
}

pub fn wait_any(caller: Pid) -> Option<(Pid, i32)> {
    let mut table = PROCS.lock();
    let child_info = {
        let p = table.procs.get_mut(&caller)?;
        p.children_exited.pop_front()
    };
    if let Some((child_pid, code)) = child_info {
        // Reap le zombie
        let _ = table.procs.remove(&child_pid);
        return Some((child_pid, code));
    }
    // Pas de child exit → on marque le parent sleeping (wake par exit_current)
    if let Some(p) = table.procs.get_mut(&caller) {
        p.state = ProcessState::Sleeping;
    }
    None
}

pub fn kill(pid: Pid, sig: Signal) -> Result<(), &'static str> {
    let mut table = PROCS.lock();
    let wake = {
        let p = table.procs.get_mut(&pid).ok_or("pid inconnu")?;
        p.signals.push(sig);
        if matches!(p.state, ProcessState::Sleeping) {
            p.state = ProcessState::Runnable;
            true
        } else { false }
    };
    if wake {
        table.runnable.push_back(pid);
    }
    Ok(())
}

/// Helper appelé depuis CoW quand on remplace une frame pour le process courant.
pub fn replace_mapping(vaddr: VirtAddr, new_frame: PhysFrame<Size4KiB>) {
    let mut table = PROCS.lock();
    if let Some(cur) = table.current() {
        cur.address_space.replace_mapping(vaddr, new_frame);
    }
}

// -----------------------------------------------------------------------------
// Contexte kernel sauvegardé — permet de reprendre le shell après qu'un
// process user ait fini (exit_current → `kernel_return` si on a un contexte).
// -----------------------------------------------------------------------------

#[repr(C)]
#[derive(Default)]
pub struct KernelCtx {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rsp: u64,
    pub rip: u64,
}

extern "C" {
    fn kernel_save_and_run(
        ctx: *mut KernelCtx,
        f: extern "C" fn(u64) -> !,
        arg: u64,
    ) -> u64;
    fn kernel_return(ctx: *const KernelCtx, retval: u64) -> !;
}

/// Contexte kernel global pour retour après exit du dernier process user.
/// Mono-thread : OK pour l'instant (shell serialisé).
static mut KERNEL_RETURN_CTX: Option<KernelCtx> = None;

fn set_kernel_ctx(ctx: KernelCtx) {
    // SAFETY: mono-thread.
    unsafe { KERNEL_RETURN_CTX = Some(ctx); }
}

fn has_kernel_ctx() -> bool {
    // SAFETY: lecture simple.
    unsafe { KERNEL_RETURN_CTX.is_some() }
}

fn take_kernel_ctx() -> Option<KernelCtx> {
    // SAFETY: mono-thread.
    unsafe { KERNEL_RETURN_CTX.take() }
}

/// Lance `schedule_next()` en sauvegardant le contexte kernel courant.
/// Retourne quand tous les process user ont fini. Restaure automatiquement
/// CR3 kernel au retour.
pub fn run_until_all_exit() {
    extern "C" fn trampoline(_arg: u64) -> ! {
        schedule_next();
    }
    // Sauvegarde CR3 kernel pour le restaurer au retour
    let (kernel_p4, _) = crate::memory::paging::current_cr3();

    let mut ctx = KernelCtx::default();
    let ctx_ptr = &mut ctx as *mut KernelCtx;
    unsafe {
        KERNEL_RETURN_CTX_PTR = ctx_ptr;
        let _ = kernel_save_and_run(ctx_ptr, trampoline, 0);
        // Ici seulement si exit_current a appelé kernel_return.
        // Restaure CR3 kernel pour que le shell puisse continuer.
        crate::memory::paging::switch_cr3(kernel_p4);
    }
}

static mut KERNEL_RETURN_CTX_PTR: *mut KernelCtx = core::ptr::null_mut();

// -----------------------------------------------------------------------------
// Scheduler : élit et active le prochain process
// -----------------------------------------------------------------------------

/// Élit le prochain process runnable et jump en ring 3 (iretq).
/// Utilisé à l'init (PID 1 = init) et après exit.
pub fn schedule_next() -> ! {
    let (p4_frame, rip, rsp, rflags, cs, ss, kstack) = {
        let mut table = PROCS.lock();
        let next = match table.pick_next() {
            Some(p) => p,
            None => {
                drop(table);
                // Plus de process runnable → retour au contexte kernel si
                // l'appelant en a sauvegardé un. Sinon hlt_loop.
                // SAFETY: mono-thread, le pointeur est valide tant que
                // run_until_all_exit() n'a pas retourné.
                unsafe {
                    if !KERNEL_RETURN_CTX_PTR.is_null() {
                        let ctx = KERNEL_RETURN_CTX_PTR;
                        KERNEL_RETURN_CTX_PTR = core::ptr::null_mut();
                        crate::serial_println!("[proc] aucun process runnable — retour shell");
                        kernel_return(ctx, 0);
                    }
                }
                crate::serial_println!("[proc] aucun process runnable — idle");
                crate::arch::x86_64::hlt_loop();
            }
        };
        table.current = next;
        let p = table.procs.get_mut(&next).expect("process disparu");
        let tf = &p.trap_frame;
        (
            p.address_space.p4_frame(),
            tf.rip, tf.rsp, tf.rflags, tf.cs, tf.ss,
            p.kernel_stack_top(),
        )
    };

    // Active la page table
    // SAFETY: p4_frame vient d'un Process vivant, kernel half mappé.
    unsafe { crate::memory::paging::switch_cr3(p4_frame); }

    // Installe TSS.rsp0 = kernel stack de ce process
    crate::arch::x86_64::percpu::set_kernel_stack(kstack);
    crate::arch::x86_64::percpu::set_current_process(core::ptr::null_mut());

    // Jump en ring 3 via iretq
    extern "C" {
        fn enter_userspace(rip: u64, rsp: u64, rflags: u64, cs: u64, ss: u64) -> !;
    }
    // SAFETY: paramètres issus d'un Process validé.
    unsafe { enter_userspace(rip, rsp, rflags, cs, ss) }
}

/// Appelé par le dispatcher timer (via preempt::on_timer) pour faire un
/// switch coopératif de process. Modifie le TrapFrame pointé par le stub asm.
pub fn reschedule(frame: &mut TrapFrame) {
    let (next_pid, next_rip, next_rsp, next_rflags, next_cs, next_ss, kstack, p4_frame) = {
        let mut table = PROCS.lock();

        // Sauvegarde l'état courant dans le process courant
        let cur_pid = table.current;
        if cur_pid != 0 {
            if let Some(cur) = table.procs.get_mut(&cur_pid) {
                cur.trap_frame = frame.clone();
                // Re-enqueue s'il est toujours runnable
                if matches!(cur.state, ProcessState::Runnable) {
                    table.runnable.push_back(cur_pid);
                }
            }
        }

        // Élit le prochain
        let next = match table.pick_next() {
            Some(p) => p,
            None => return,  // rien d'autre à exécuter, on continue le courant
        };
        if next == table.current { return; }
        table.current = next;
        let p = table.procs.get_mut(&next).expect("process disparu");
        let tf = &p.trap_frame;
        (
            next,
            tf.rip, tf.rsp, tf.rflags, tf.cs, tf.ss,
            p.kernel_stack_top(),
            p.address_space.p4_frame(),
        )
    };

    let _ = next_pid;

    // Écrit la trap frame du prochain process dans le slot courant (que
    // l'asm va iret-er).
    frame.rip = next_rip;
    frame.rsp = next_rsp;
    frame.rflags = next_rflags;
    frame.cs = next_cs;
    frame.ss = next_ss;
    // Le reste des GPR a été écrasé par cur.trap_frame au save ; on doit
    // les copier depuis la TrapFrame du next :
    {
        let table = PROCS.lock();
        if let Some(p) = table.procs.get(&table.current) {
            let src = &p.trap_frame;
            frame.rax = src.rax; frame.rbx = src.rbx; frame.rcx = src.rcx; frame.rdx = src.rdx;
            frame.rsi = src.rsi; frame.rdi = src.rdi; frame.rbp = src.rbp;
            frame.r8  = src.r8;  frame.r9  = src.r9;  frame.r10 = src.r10; frame.r11 = src.r11;
            frame.r12 = src.r12; frame.r13 = src.r13; frame.r14 = src.r14; frame.r15 = src.r15;
        }
    }

    // SAFETY: p4_frame issue d'un Process vivant.
    unsafe { crate::memory::paging::switch_cr3(p4_frame); }
    crate::arch::x86_64::percpu::set_kernel_stack(kstack);
}

/// Retourne la liste (pid, parent, name, state) des process.
pub fn list() -> Vec<(Pid, Pid, String, ProcessState)> {
    PROCS.lock().list()
}
