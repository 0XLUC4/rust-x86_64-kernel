// =============================================================================
// serial.rs — driver UART 16550 sur le port COM1 (0x3F8).
//
// Super utile pour debug : QEMU redirige le port série vers stdin/stdout
// avec `-serial stdio`, ce qui donne un log hôte-lisible même si la VGA
// est cassée.
// =============================================================================

use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

lazy_static! {
    pub static ref SERIAL1: Mutex<SerialPort> = {
        // SAFETY: 0x3F8 est bien le port COM1 standard.
        let mut port = unsafe { SerialPort::new(0x3F8) };
        port.init();
        Mutex::new(port)
    };
}

pub fn init() {
    // Forcer l'init lazy_static — pas strictement nécessaire, mais ça
    // évite qu'un premier print silencieux perde des caractères.
    lazy_static::initialize(&SERIAL1);
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    x86_64::instructions::interrupts::without_interrupts(|| {
        SERIAL1.lock().write_fmt(args).expect("serial write failed");
    });
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::drivers::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
