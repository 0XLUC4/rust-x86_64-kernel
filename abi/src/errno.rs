// =============================================================================
// errno — codes d'erreur retournés par les syscalls.
//
// Convention : un syscall "i64" retourne soit n>=0 (succès), soit -errno.
// Pour les syscalls "u64" qui retournent un handle/ptr, sentinelle = u64::MAX.
//
// Les valeurs sont stables et alignées sur Linux quand l'analogie tient,
// pour faciliter le portage de code. Plage 1..127 réservée POSIX-like.
// =============================================================================

pub type Errno = i32;

pub const EPERM:    Errno = 1;
pub const ENOENT:   Errno = 2;
pub const ESRCH:    Errno = 3;
pub const EBADF:    Errno = 9;
pub const EAGAIN:   Errno = 11;
pub const ENOMEM:   Errno = 12;
pub const EACCES:   Errno = 13;
pub const EFAULT:   Errno = 14;
pub const EBUSY:    Errno = 16;
pub const EINVAL:   Errno = 22;
pub const ENOSYS:   Errno = 38;
pub const EMSGSIZE: Errno = 90;
