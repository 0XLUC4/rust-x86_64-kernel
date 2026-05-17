// =============================================================================
// ulib::io — I/O formatté (`print!`, `println!`, `eprintln!`) et lecture ligne.
//
// Pas d'allocateur → on utilise un buffer sur la stack (`ArrayString`-like).
// Pour les écritures : on écrit direct via `syscall::write`.
// =============================================================================

use core::fmt::{self, Write};

use crate::syscall::{write as sys_write, read as sys_read};

/// Writer qui écrit chaque chunk UTF-8 sur le fd donné.
pub struct FdWriter { pub fd: u64 }

impl Write for FdWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut buf = s.as_bytes();
        while !buf.is_empty() {
            let n = sys_write(self.fd, buf);
            if n < 0 { return Err(fmt::Error); }
            if n == 0 { return Err(fmt::Error); }
            buf = &buf[n as usize..];
        }
        Ok(())
    }
}

pub fn stdout() -> FdWriter { FdWriter { fd: 1 } }
pub fn stderr() -> FdWriter { FdWriter { fd: 2 } }

/// Lit une ligne depuis stdin (fd=0), jusqu'à `\n` ou fin de buffer.
/// Retourne le slice (sans le `\n`) ou None si EOF.
pub fn stdin_line<'a>(buf: &'a mut [u8]) -> Option<&'a str> {
    let n = sys_read(0, buf);
    if n <= 0 { return None; }
    let mut len = n as usize;
    if len > 0 && buf[len - 1] == b'\n' { len -= 1; }
    core::str::from_utf8(&buf[..len]).ok()
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = writeln!($crate::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::io::stderr(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! eprintln {
    () => { $crate::eprint!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = writeln!($crate::io::stderr(), $($arg)*);
    }};
}
