// =============================================================================
// ulib::errno — errno simplifié (single-threaded user pour l'instant).
//
// Plus tard quand on aura des threads user, ceci deviendra du TLS (FS_BASE).
// Les codes sont alignés sur Linux x86_64 (1=EPERM, 2=ENOENT, …).
// =============================================================================

pub const EPERM:   i32 = 1;
pub const ENOENT:  i32 = 2;
pub const ESRCH:   i32 = 3;
pub const EINTR:   i32 = 4;
pub const EIO:     i32 = 5;
pub const EBADF:   i32 = 9;
pub const ECHILD:  i32 = 10;
pub const EAGAIN:  i32 = 11;
pub const ENOMEM:  i32 = 12;
pub const EACCES:  i32 = 13;
pub const EFAULT:  i32 = 14;
pub const EBUSY:   i32 = 16;
pub const EEXIST:  i32 = 17;
pub const ENODEV:  i32 = 19;
pub const ENOTDIR: i32 = 20;
pub const EISDIR:  i32 = 21;
pub const EINVAL:  i32 = 22;
pub const ENFILE:  i32 = 23;
pub const EMFILE:  i32 = 24;
pub const ENOSPC:  i32 = 28;
pub const EPIPE:   i32 = 32;

use core::sync::atomic::{AtomicI32, Ordering};
static ERRNO: AtomicI32 = AtomicI32::new(0);

pub fn errno() -> i32 { ERRNO.load(Ordering::Relaxed) }
pub fn set_errno(v: i32) { ERRNO.store(v, Ordering::Relaxed) }

pub fn errno_str(e: i32) -> &'static str {
    match e {
        0       => "Success",
        EPERM   => "Operation not permitted",
        ENOENT  => "No such file or directory",
        EBADF   => "Bad file descriptor",
        EAGAIN  => "Try again",
        ENOMEM  => "Out of memory",
        EACCES  => "Permission denied",
        EFAULT  => "Bad address",
        EBUSY   => "Device or resource busy",
        EEXIST  => "File exists",
        ENOTDIR => "Not a directory",
        EISDIR  => "Is a directory",
        EINVAL  => "Invalid argument",
        ENOSPC  => "No space left on device",
        EPIPE   => "Broken pipe",
        _       => "Unknown error",
    }
}

/// perror — affiche `prefix: msg` sur stderr (style C).
pub fn perror(prefix: &str) {
    let e = errno();
    crate::eprintln!("{}: {}", prefix, errno_str(e));
}
