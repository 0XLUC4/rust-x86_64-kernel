// =============================================================================
// ulib::syscall — wrappers inline-asm pour l'ABI `syscall` de d/OS.
//
// Convention (alignée sur System V / Linux x86_64) :
//   RAX = numéro de syscall
//   RDI, RSI, RDX, R10, R8, R9 = args 1..6
//   RCX et R11 sont clobbered par l'instruction `syscall`
//   Retour dans RAX.
//
// Les numéros doivent rester strictement synchronisés avec
// `kernel/src/syscall/mod.rs::nr`.
// =============================================================================

use core::arch::asm;

// --- Numéros (mirror de kernel::syscall::nr) ---
pub const NR_WRITE:    u64 = 1;
pub const NR_READ:     u64 = 2;
pub const NR_EXIT:     u64 = 3;
pub const NR_GETPID:   u64 = 4;
pub const NR_UPTIME:   u64 = 5;
pub const NR_FS_READ:  u64 = 10;
pub const NR_FS_LIST:  u64 = 11;
pub const NR_FORK:     u64 = 20;
pub const NR_EXEC:     u64 = 21;
pub const NR_WAIT:     u64 = 22;
pub const NR_KILL:     u64 = 23;
pub const NR_YIELD:    u64 = 24;
pub const NR_SLEEP_MS: u64 = 25;
pub const NR_BRK:      u64 = 26;
pub const NR_GETUID:   u64 = 30;
pub const NR_GETEUID:  u64 = 31;
pub const NR_GETGID:   u64 = 32;
pub const NR_GETEGID:  u64 = 33;
pub const NR_SETUID:   u64 = 34;
pub const NR_SETGID:   u64 = 35;

// --- Phase V — graphique / IPC / shm ---
pub const NR_FB_ACQUIRE: u64 = 40;
pub const NR_FB_PRESENT: u64 = 41;
pub const NR_INPUT_POLL: u64 = 42;
pub const NR_SHM_CREATE: u64 = 43;
pub const NR_SHM_MAP:    u64 = 44;
pub const NR_SHM_UNMAP:  u64 = 45;
pub const NR_IPC_SEND:   u64 = 46;
pub const NR_IPC_RECV:   u64 = 47;

// -----------------------------------------------------------------------------
// Raw helpers : 0..3 args (assez pour tous nos syscalls actuels).
// -----------------------------------------------------------------------------

#[inline(always)]
pub unsafe fn syscall0(nr: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        in("rax") nr,
        lateout("rax") ret,
        out("rcx") _, out("r11") _,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall1(nr: u64, a1: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        in("rax") nr, in("rdi") a1,
        lateout("rax") ret,
        out("rcx") _, out("r11") _,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall2(nr: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        in("rax") nr, in("rdi") a1, in("rsi") a2,
        lateout("rax") ret,
        out("rcx") _, out("r11") _,
        options(nostack),
    );
    ret
}

#[inline(always)]
pub unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        in("rax") nr, in("rdi") a1, in("rsi") a2, in("rdx") a3,
        lateout("rax") ret,
        out("rcx") _, out("r11") _,
        options(nostack),
    );
    ret
}

// -----------------------------------------------------------------------------
// High-level wrappers
// -----------------------------------------------------------------------------

pub fn write(fd: u64, buf: &[u8]) -> i64 {
    unsafe { syscall3(NR_WRITE, fd, buf.as_ptr() as u64, buf.len() as u64) as i64 }
}

/// Lit jusqu'à buf.len() octets depuis fd. Retour = bytes lus (>=0) ou -1.
pub fn read(fd: u64, buf: &mut [u8]) -> i64 {
    let n = unsafe { syscall3(NR_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if n == u64::MAX { -1 } else { n as i64 }
}

pub fn exit(code: i32) -> ! {
    unsafe { syscall1(NR_EXIT, code as u64); }
    loop { unsafe { core::arch::asm!("hlt"); } }
}

pub fn getpid() -> u32 { unsafe { syscall0(NR_GETPID) as u32 } }
pub fn uptime_ms() -> u64 { unsafe { syscall0(NR_UPTIME) } }
pub fn yield_now() { unsafe { syscall0(NR_YIELD); } }
pub fn sleep_ms(ms: u64) { unsafe { syscall1(NR_SLEEP_MS, ms); } }

/// Retour = 0 dans le child, >0 (pid du child) dans le parent, u64::MAX sur erreur.
pub fn fork() -> i64 {
    let r = unsafe { syscall0(NR_FORK) };
    if r == u64::MAX { -1 } else { r as i64 }
}

/// exec : remplace l'image courante par `path`. Chemin null-terminé **pas** requis —
/// on passe la longueur. Revient seulement sur erreur.
pub fn exec(path: &str) -> i64 {
    let r = unsafe { syscall2(NR_EXEC, path.as_ptr() as u64, path.len() as u64) };
    if r == u64::MAX { -1 } else { r as i64 }
}

/// wait(pid) : attend la fin d'un fils. `pid = u64::MAX` ou `-1` → n'importe quel fils.
/// Retour = pid du fils terminé.
pub fn wait(pid: i64) -> i64 {
    let r = unsafe { syscall1(NR_WAIT, pid as u64) };
    if r == u64::MAX { -1 } else { r as i64 }
}

pub fn kill(pid: u32, sig: u32) -> i64 {
    let r = unsafe { syscall2(NR_KILL, pid as u64, sig as u64) };
    if r == u64::MAX { -1 } else { r as i64 }
}

pub fn getuid() -> u32  { unsafe { syscall0(NR_GETUID)  as u32 } }
pub fn geteuid() -> u32 { unsafe { syscall0(NR_GETEUID) as u32 } }
pub fn getgid() -> u32  { unsafe { syscall0(NR_GETGID)  as u32 } }
pub fn getegid() -> u32 { unsafe { syscall0(NR_GETEGID) as u32 } }
pub fn setuid(uid: u32) -> i64 {
    let r = unsafe { syscall1(NR_SETUID, uid as u64) };
    if r == u64::MAX { -1 } else { 0 }
}

// -----------------------------------------------------------------------------
// Phase V — wrappers graphique / IPC / shm
// -----------------------------------------------------------------------------

/// Layout binaire de FbInfo (cf. abi::fb::FbInfo). Doit matcher exactement
/// la struct kernel-side `gfx::fb_info::FbInfoAbi`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct FbInfo {
    pub buffer_ptr: u64,
    pub buffer_len: u64,
    pub width:  u32,
    pub height: u32,
    pub pitch:  u32,
    pub format: u32,
    pub caps:   u32,
    pub _reserved: u32,
}

/// fb_acquire — réservé au display-server. Retourne 0 OK / -1 erreur.
pub fn fb_acquire(out: &mut FbInfo) -> i64 {
    let r = unsafe { syscall1(NR_FB_ACQUIRE, out as *mut _ as u64) };
    if r == u64::MAX { -1 } else { 0 }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Rect { pub x: u32, pub y: u32, pub w: u32, pub h: u32 }

pub fn fb_present(rect: Option<&Rect>) -> i64 {
    let ptr = rect.map(|r| r as *const _ as u64).unwrap_or(0);
    let r = unsafe { syscall1(NR_FB_PRESENT, ptr) };
    if r == u64::MAX { -1 } else { 0 }
}

/// Layout binaire d'InputEvent côté user. Mirror exact de
/// `kernel::gfx::input_queue::InputEventAbi` (56 bytes).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct InputEvent {
    pub kind: u32,
    pub _pad0: u32,
    pub timestamp_ms: u64,
    pub scancode: u32,
    pub keysym: u32,
    pub mods: u32,
    pub _pad1: u32,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub mouse_dx: i32,
    pub mouse_dy: i32,
    pub mouse_buttons: u32,
    pub wheel: i32,
}

pub const KIND_KEY_DOWN:    u32 = 1;
pub const KIND_KEY_UP:      u32 = 2;
pub const KIND_MOUSE_MOVE:  u32 = 3;
pub const KIND_MOUSE_DOWN:  u32 = 4;
pub const KIND_MOUSE_UP:    u32 = 5;
pub const KIND_MOUSE_WHEEL: u32 = 6;

/// Lit jusqu'à `buf.len()` events. Retour = nombre d'events lus (0 si vide).
pub fn input_poll(buf: &mut [InputEvent]) -> i64 {
    let n = unsafe { syscall2(NR_INPUT_POLL, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if n == u64::MAX { -1 } else { n as i64 }
}

pub fn shm_create(size: u64) -> i64 {
    let r = unsafe { syscall1(NR_SHM_CREATE, size) };
    if r == u64::MAX { -1 } else { r as i64 }
}

pub const SHM_READ:  u64 = 1 << 0;
pub const SHM_WRITE: u64 = 1 << 1;
pub const SHM_RW:    u64 = SHM_READ | SHM_WRITE;

pub fn shm_map(handle: u64, mode: u64) -> u64 {
    unsafe { syscall2(NR_SHM_MAP, handle, mode) }
}

pub fn shm_unmap(ptr: u64) -> i64 {
    let r = unsafe { syscall1(NR_SHM_UNMAP, ptr) };
    if r == u64::MAX { -1 } else { 0 }
}

pub fn ipc_send(target_pid: u32, msg: &[u8]) -> i64 {
    let r = unsafe {
        syscall3(NR_IPC_SEND, target_pid as u64, msg.as_ptr() as u64, msg.len() as u64)
    };
    if r == u64::MAX { -1 } else { 0 }
}

/// Bloquant. Retourne nb d'octets lus dans `buf`. `out_sender` reçoit le PID expéditeur.
pub fn ipc_recv(buf: &mut [u8], out_sender: &mut u32) -> i64 {
    let mut sender_u64: u64 = 0;
    let n = unsafe {
        syscall3(NR_IPC_RECV,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            &mut sender_u64 as *mut u64 as u64,
        )
    };
    *out_sender = sender_u64 as u32;
    if n == u64::MAX { -1 } else { n as i64 }
}
