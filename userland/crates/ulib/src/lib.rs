// =============================================================================
// ulib — runtime + libc minimaliste pour les binaires user de d/OS.
//
// Public API :
//   - `syscall::*`              : wrappers pour chaque syscall (write, read, fork…)
//   - `println!` / `print!`     : macros qui écrivent sur fd=1 (VGA)
//   - `eprintln!` / `eprint!`   : macros qui écrivent sur fd=2 (serial)
//   - `#[entry]` (via ulib_rt!) : déclare le `main` user et fournit `_start`
//
// Les numéros de syscall doivent rester synchronisés avec
// `kernel/src/syscall/mod.rs::nr`.
// =============================================================================

#![no_std]

extern crate alloc;

pub mod syscall;
pub mod io;
pub mod process;
pub mod mem;
pub mod errno;

// Re-exports pratiques.
pub use syscall::{
    exit, fork, exec, wait, yield_now, sleep_ms, getpid,
    getuid, geteuid, getgid, getegid, setuid,
    write, read,
};
pub use io::{stdout, stderr, stdin_line};
pub use process::{fork_exec_wait, spawn};
pub use errno::{errno, set_errno, errno_str, perror};

/// Runtime : macro qui plante un `_start` + appelle `main`.
/// Utiliser comme :
///
///     ulib::entry!(main);
///     fn main() { /* ... */ }
#[macro_export]
macro_rules! entry {
    ($main:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn _start() -> ! {
            $main();
            $crate::exit(0);
        }

        #[panic_handler]
        fn __panic(info: &core::panic::PanicInfo) -> ! {
            $crate::eprintln!("[panic user] {}", info);
            $crate::exit(127);
        }
    };
}
