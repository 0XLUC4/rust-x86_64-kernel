// =============================================================================
// syscall — dispatcher Rust de l'instruction `syscall` (Phase II : ring 3 réel).
//
// Côté userspace, un programme fait :
//     mov rax, <nr>
//     mov rdi, <a1>  ...
//     syscall
//
// Le CPU saute à `syscall_entry` (boot/syscall_entry.asm) qui :
//   1. swapgs + switch stack kernel (via GS_BASE percpu area)
//   2. sauvegarde RCX (rip retour), R11 (rflags retour)
//   3. appelle `syscall_dispatch(nr, a1..a6)` — ABI C
//   4. sysretq
//
// Ajouts Phase II :
//   - validation des pointeurs user (range 0..0x0000_8000_0000_0000)
//   - syscalls : fork, exec, wait, kill, getpid, brk, mmap_stub
// =============================================================================

use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

use crate::task::process::{self, Pid};
use crate::task::signal::Signal;

extern "C" {
    fn syscall_entry();
}

/// Numéros de syscall (stable, ne pas renuméroter).
pub mod nr {
    pub const WRITE:    u64 = 1;
    pub const READ:     u64 = 2;
    pub const EXIT:     u64 = 3;
    pub const GETPID:   u64 = 4;
    pub const UPTIME:   u64 = 5;
    pub const FS_READ:  u64 = 10;
    pub const FS_LIST:  u64 = 11;
    // Phase II
    pub const FORK:     u64 = 20;
    pub const EXEC:     u64 = 21;
    pub const WAIT:     u64 = 22;
    pub const KILL:     u64 = 23;
    pub const YIELD:    u64 = 24;
    pub const SLEEP_MS: u64 = 25;
    pub const BRK:      u64 = 26;
    // Phase IV — identité utilisateur
    pub const GETUID:   u64 = 30;
    pub const GETEUID:  u64 = 31;
    pub const GETGID:   u64 = 32;
    pub const GETEGID:  u64 = 33;
    pub const SETUID:   u64 = 34;
    pub const SETGID:   u64 = 35;
    // Phase V — graphique / IPC / shm. Cf. abi/src/syscall_nr.rs (source of truth).
    pub const FB_ACQUIRE: u64 = 40;
    pub const FB_PRESENT: u64 = 41;
    pub const INPUT_POLL: u64 = 42;
    pub const SHM_CREATE: u64 = 43;
    pub const SHM_MAP:    u64 = 44;
    pub const SHM_UNMAP:  u64 = 45;
    pub const IPC_SEND:   u64 = 46;
    pub const IPC_RECV:   u64 = 47;
}

/// Borne supérieure de la moitié basse (userspace canonique).
const USER_SPACE_MAX: u64 = 0x0000_8000_0000_0000;

pub fn init() {
    // SCE (Syscall Enable) dans EFER
    // SAFETY: on écrit un MSR standard.
    unsafe {
        Efer::update(|f| f.insert(EferFlags::SYSTEM_CALL_EXTENSIONS));
    }

    // STAR : sélecteurs de segment. Layout (cf gdt.rs) :
    //   kernel_code = sel 0x08
    //   kernel_data = sel 0x10
    //   user_data   = sel 0x18 + DPL=3 → 0x1b
    //   user_code   = sel 0x20 + DPL=3 → 0x23
    //
    // STAR[63:48] (user base) doit être tel que :
    //   SYSRET charge CS = STAR[63:48] + 16 | 3  → user_code (0x23)
    //   SYSRET charge SS = STAR[63:48] + 8  | 3  → user_data (0x1b)
    // Donc STAR[63:48] = 0x13 (= 0x10 | 3)
    //
    // STAR[47:32] (kernel base) :
    //   SYSCALL charge CS = STAR[47:32] & ~3  → kernel_code (0x08)
    //   SYSCALL charge SS = STAR[47:32] + 8   → kernel_data (0x10)
    // Donc STAR[47:32] = 0x08
    let sel = crate::arch::x86_64::gdt::selectors();
    // SAFETY: configuration standard SYSCALL/SYSRET.
    Star::write(
        sel.user_code,   // CS user (sysret)
        sel.user_data,   // SS user
        sel.kernel_code, // CS kernel (syscall)
        sel.kernel_data, // SS kernel
    ).expect("STAR write");

    LStar::write(VirtAddr::new(syscall_entry as u64));
    SFMask::write(RFlags::INTERRUPT_FLAG);

    unsafe { crate::arch::x86_64::percpu::init(); }

    crate::println!("[sys] syscall armé (LSTAR = {:#x})", syscall_entry as u64);
}

// -----------------------------------------------------------------------------
// Validation user pointers
// -----------------------------------------------------------------------------

/// Réexport pour les modules sœurs (gfx, ipc) qui ont besoin de valider
/// des pointeurs user — même règle que `user_slice_mut` interne.
pub fn syscall_user_slice<'a>(ptr: u64, len: u64) -> Option<&'a [u8]> {
    user_slice(ptr, len)
}
pub fn syscall_user_slice_mut<'a>(ptr: u64, len: u64) -> Option<&'a mut [u8]> {
    user_slice_mut(ptr, len)
}

fn user_slice_mut<'a>(ptr: u64, len: u64) -> Option<&'a mut [u8]> {
    if ptr == 0 || len == 0 { return None; }
    let end = ptr.checked_add(len)?;
    if end >= USER_SPACE_MAX { return None; }
    // SAFETY: sous l'hypothèse que le process a mappé cette zone, ce que la
    // MMU validera à l'accès (page fault → SIGSEGV via handler idt.rs).
    unsafe { Some(core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize)) }
}

fn user_slice<'a>(ptr: u64, len: u64) -> Option<&'a [u8]> {
    if ptr == 0 || len == 0 { return None; }
    let end = ptr.checked_add(len)?;
    if end >= USER_SPACE_MAX { return None; }
    // SAFETY: idem — MMU valide les pages effectivement mappées.
    unsafe { Some(core::slice::from_raw_parts(ptr as *const u8, len as usize)) }
}

#[allow(dead_code)]
fn user_cstr(ptr: u64, max: usize) -> Option<alloc::string::String> {
    if ptr == 0 { return None; }
    let mut s = alloc::string::String::new();
    for i in 0..max {
        let addr = ptr.checked_add(i as u64)?;
        if addr >= USER_SPACE_MAX { return None; }
        // SAFETY: une éventuelle faute déclenche le handler PF qui kill le process.
        let b = unsafe { core::ptr::read_volatile(addr as *const u8) };
        if b == 0 { return Some(s); }
        s.push(b as char);
    }
    None
}

// -----------------------------------------------------------------------------
// Dispatcher
// -----------------------------------------------------------------------------

/// Appelé depuis syscall_entry.asm.
#[no_mangle]
pub extern "C" fn syscall_dispatch(
    nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64,
) -> u64 {
    crate::arch::x86_64::percpu::bump_syscall_count();

    let _ = (a4, a5, a6);
    match nr {
        nr::WRITE    => sys_write(a1, a2, a3),
        nr::READ     => sys_read(a1, a2, a3),
        nr::EXIT     => sys_exit(a1 as i32),
        nr::GETPID   => sys_getpid(),
        nr::UPTIME   => crate::time::uptime_ms(),
        nr::FS_READ  => sys_fs_read(a1, a2, a3, a4),
        nr::FS_LIST  => sys_fs_list(a1, a2),
        nr::FORK     => sys_fork(),
        nr::EXEC     => sys_exec(a1, a2),
        nr::WAIT     => sys_wait(a1),
        nr::KILL     => sys_kill(a1, a2),
        nr::YIELD    => { crate::task::thread::yield_now(); 0 }
        nr::SLEEP_MS => sys_sleep_ms(a1),
        nr::BRK      => sys_brk(a1),
        nr::GETUID   => sys_getuid(),
        nr::GETEUID  => sys_geteuid(),
        nr::GETGID   => sys_getgid(),
        nr::GETEGID  => sys_getegid(),
        nr::SETUID   => sys_setuid(a1 as u32),
        nr::SETGID   => sys_setgid(a1 as u32),
        // Phase V — gfx/IPC/shm
        nr::FB_ACQUIRE => crate::gfx::sys_fb_acquire(a1),
        nr::FB_PRESENT => crate::gfx::sys_fb_present(a1),
        nr::INPUT_POLL => crate::gfx::sys_input_poll(a1, a2),
        nr::SHM_CREATE => crate::ipc::sys_shm_create(a1),
        nr::SHM_MAP    => crate::ipc::sys_shm_map(a1, a2),
        nr::SHM_UNMAP  => crate::ipc::sys_shm_unmap(a1),
        nr::IPC_SEND   => crate::ipc::sys_ipc_send(a1, a2, a3),
        nr::IPC_RECV   => crate::ipc::sys_ipc_recv(a1, a2, a3),
        _ => u64::MAX,  // ENOSYS
    }
}

// -----------------------------------------------------------------------------
// Identité utilisateur
// -----------------------------------------------------------------------------

fn sys_getuid() -> u64 {
    process::PROCS.lock().current()
        .map(|p| p.uid as u64)
        .unwrap_or(u64::MAX)
}

fn sys_geteuid() -> u64 {
    process::PROCS.lock().current()
        .map(|p| p.euid as u64)
        .unwrap_or(u64::MAX)
}

fn sys_getgid() -> u64 {
    process::PROCS.lock().current()
        .map(|p| p.gid as u64)
        .unwrap_or(u64::MAX)
}

fn sys_getegid() -> u64 {
    process::PROCS.lock().current()
        .map(|p| p.egid as u64)
        .unwrap_or(u64::MAX)
}

/// setuid : seul root (euid=0) peut changer d'identité arbitrairement.
/// Un user normal ne peut que revenir à son uid réel.
///
/// Retourne 0 en succès, u64::MAX (EPERM) en refus.
fn sys_setuid(new_uid: u32) -> u64 {
    let mut table = process::PROCS.lock();
    let proc = match table.current() {
        Some(p) => p, None => return u64::MAX,
    };
    if proc.euid == process::ROOT_UID {
        // root → peut tout faire : on set real, effective.
        proc.uid = new_uid;
        proc.euid = new_uid;
        0
    } else if new_uid == proc.uid {
        // user normal : retour à l'uid réel (après un saved-uid par ex).
        proc.euid = new_uid;
        0
    } else {
        u64::MAX  // EPERM
    }
}

fn sys_setgid(new_gid: u32) -> u64 {
    let mut table = process::PROCS.lock();
    let proc = match table.current() {
        Some(p) => p, None => return u64::MAX,
    };
    if proc.euid == process::ROOT_UID {
        proc.gid = new_gid;
        proc.egid = new_gid;
        0
    } else if new_gid == proc.gid {
        proc.egid = new_gid;
        0
    } else {
        u64::MAX
    }
}

// -----------------------------------------------------------------------------
// Syscalls I/O
// -----------------------------------------------------------------------------

/// fd : 1=stdout (VGA), 2=stderr (serial)
fn sys_write(fd: u64, buf: u64, len: u64) -> u64 {
    let slice = match user_slice(buf, len) {
        Some(s) => s, None => return u64::MAX,
    };
    let s = match core::str::from_utf8(slice) {
        Ok(s) => s, Err(_) => return u64::MAX,
    };
    match fd {
        1 => { crate::print!("{}", s); len }
        2 => { crate::serial_print!("{}", s); len }
        _ => u64::MAX,
    }
}

/// Lit des caractères depuis stdin (fd=0). Bloquant "pauvre-homme" : on yield
/// tant que la queue clavier est vide. Sémantique line-buffered : on retourne
/// dès qu'on a un `\n` ou que le buffer user est plein.
///
/// Retourne le nombre d'octets écrits dans `buf`, ou `u64::MAX` sur erreur.
fn sys_read(fd: u64, buf: u64, len: u64) -> u64 {
    if fd != 0 { return u64::MAX; }  // stdin only pour l'instant
    let out = match user_slice_mut(buf, len) {
        Some(s) => s, None => return u64::MAX,
    };
    if out.is_empty() { return 0; }

    let mut written = 0;
    loop {
        match crate::drivers::keyboard::try_read_char() {
            Some(ch) => {
                // Echo VGA pour UX shell (comme un terminal en mode canon).
                if ch == '\x08' || ch == '\x7f' {
                    if written > 0 {
                        written -= 1;
                        crate::print!("\x08 \x08");
                    }
                    continue;
                }
                crate::print!("{}", ch);
                // Traduit CR en LF (entrée = \r côté PC/AT).
                let byte = if ch == '\r' { b'\n' } else { ch as u32 as u8 };
                out[written] = byte;
                written += 1;
                if byte == b'\n' || written >= out.len() {
                    return written as u64;
                }
            }
            None => {
                // Pas de char : yield le CPU. Le timer préempte de toute façon,
                // mais on accélère la remise en queue.
                crate::task::thread::yield_now();
            }
        }
    }
}

fn sys_exit(code: i32) -> u64 {
    process::exit_current(code)
}

fn sys_getpid() -> u64 {
    process::PROCS.lock().current_pid() as u64
}

fn sys_fs_read(path_ptr: u64, path_len: u64, out_buf: u64, out_max: u64) -> u64 {
    let path_bytes = match user_slice(path_ptr, path_len) {
        Some(s) => s, None => return u64::MAX,
    };
    let path = match core::str::from_utf8(path_bytes) {
        Ok(s) => s, Err(_) => return u64::MAX,
    };
    let data = {
        let fs = crate::fs::FS.lock();
        match fs.read(path) {
            Ok(d) => d,
            Err(_) => return u64::MAX,
        }
    };
    let out = match user_slice_mut(out_buf, out_max) {
        Some(s) => s, None => return u64::MAX,
    };
    let n = data.len().min(out.len());
    out[..n].copy_from_slice(&data[..n]);
    n as u64
}

fn sys_fs_list(out_buf: u64, out_max: u64) -> u64 {
    let out = match user_slice_mut(out_buf, out_max) {
        Some(s) => s, None => return u64::MAX,
    };
    let mut written = 0usize;
    let fs = crate::fs::FS.lock();
    for name in fs.list() {
        let bytes = name.as_bytes();
        if written + bytes.len() + 1 > out.len() { break; }
        out[written..written+bytes.len()].copy_from_slice(bytes);
        out[written+bytes.len()] = b'\n';
        written += bytes.len() + 1;
    }
    written as u64
}

// -----------------------------------------------------------------------------
// Syscalls process
// -----------------------------------------------------------------------------

fn sys_fork() -> u64 {
    match process::fork() {
        Ok(child_pid) => child_pid as u64,
        Err(_) => u64::MAX,
    }
}

fn sys_exec(path_ptr: u64, path_len: u64) -> u64 {
    let path_bytes = match user_slice(path_ptr, path_len) {
        Some(s) => s, None => return u64::MAX,
    };
    let path = match core::str::from_utf8(path_bytes) {
        Ok(s) => s, Err(_) => return u64::MAX,
    };
    let parent = process::PROCS.lock().current_pid();
    match process::exec_from_search(path, parent) {
        Ok(new_pid) => new_pid as u64,
        Err(_) => u64::MAX,
    }
}

fn sys_wait(_flags: u64) -> u64 {
    let caller = process::PROCS.lock().current_pid();
    match process::wait_any(caller) {
        Some((pid, code)) => ((pid as u64) << 32) | (code as u32 as u64),
        None => u64::MAX,
    }
}

fn sys_kill(pid: u64, sig: u64) -> u64 {
    let s = match Signal::from_num(sig as u32) {
        Some(s) => s, None => return u64::MAX,
    };
    match process::kill(pid as Pid, s) {
        Ok(()) => 0, Err(_) => u64::MAX,
    }
}

fn sys_sleep_ms(ms: u64) -> u64 {
    // Simplification : busy-wait monotonique (l'ordonnancement tourne via le timer).
    let start = crate::time::uptime_ms();
    while crate::time::uptime_ms() - start < ms {
        x86_64::instructions::hlt();
    }
    0
}

fn sys_brk(_addr: u64) -> u64 {
    // Not implemented — renvoie 0 = "impossible de changer la break".
    0
}
