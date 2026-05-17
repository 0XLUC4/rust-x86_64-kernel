// =============================================================================
// syscall_nr — numéros de syscalls. **Ne jamais renuméroter.**
//
// Reprend l'existant kernel/src/syscall/mod.rs::nr et ajoute le bloc
// graphique/IPC en plage 40..=59.
// =============================================================================

// --- I/O fondamental (1..9) ---
pub const WRITE:    u64 = 1;
pub const READ:     u64 = 2;
pub const EXIT:     u64 = 3;
pub const GETPID:   u64 = 4;
pub const UPTIME:   u64 = 5;

// --- FS (10..19) ---
pub const FS_READ:  u64 = 10;
pub const FS_LIST:  u64 = 11;

// --- Process (20..29) ---
pub const FORK:     u64 = 20;
pub const EXEC:     u64 = 21;
pub const WAIT:     u64 = 22;
pub const KILL:     u64 = 23;
pub const YIELD:    u64 = 24;
pub const SLEEP_MS: u64 = 25;
pub const BRK:      u64 = 26;

// --- Identité (30..39) ---
pub const GETUID:   u64 = 30;
pub const GETEUID:  u64 = 31;
pub const GETGID:   u64 = 32;
pub const GETEGID:  u64 = 33;
pub const SETUID:   u64 = 34;
pub const SETGID:   u64 = 35;

// --- Graphique / IPC / SHM (40..59) — NOUVEAU ---
/// fb_acquire(out: *mut FbInfo) -> 0 sur succès. Réservé au display-server.
pub const FB_ACQUIRE:    u64 = 40;
/// fb_present(rect: *const Rect) -> 0 sur succès.
pub const FB_PRESENT:    u64 = 41;
/// input_poll(buf: *mut InputEvent, max: u64) -> n events lus (0 si vide).
pub const INPUT_POLL:    u64 = 42;
/// shm_create(size: u64) -> handle (u64), u64::MAX si erreur.
pub const SHM_CREATE:    u64 = 43;
/// shm_map(handle: u64, mode: u64) -> ptr user, 0 si erreur.
pub const SHM_MAP:       u64 = 44;
/// shm_unmap(ptr: u64) -> 0 OK.
pub const SHM_UNMAP:     u64 = 45;
/// ipc_send(target_pid: u64, msg_ptr: u64, msg_len: u64) -> 0 OK / errno.
pub const IPC_SEND:      u64 = 46;
/// ipc_recv(buf: *mut u8, max: u64, out_sender: *mut u64) -> n bytes lus.
pub const IPC_RECV:      u64 = 47;

// --- Helpers ---
pub const fn is_gfx(nr: u64) -> bool { nr >= FB_ACQUIRE && nr <= IPC_RECV }
