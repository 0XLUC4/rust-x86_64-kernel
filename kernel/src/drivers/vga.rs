// =============================================================================
// vga.rs — driver VGA text mode 80×25 @ 0xB8000.
//
// Chaque "caractère" est un u16 :
//   bits  0..7  = ASCII
//   bits  8..11 = foreground color
//   bits 12..14 = background color
//   bit  15     = blink (ignoré en mode moderne)
// =============================================================================

use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use volatile::Volatile;

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;
const VGA_BUFFER_ADDR: usize = 0xb8000;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0, Blue = 1, Green = 2, Cyan = 3, Red = 4, Magenta = 5,
    Brown = 6, LightGray = 7, DarkGray = 8, LightBlue = 9,
    LightGreen = 10, LightCyan = 11, LightRed = 12, Pink = 13,
    Yellow = 14, White = 15,
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(fg: Color, bg: Color) -> Self { Self((bg as u8) << 4 | (fg as u8)) }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ScreenChar {
    ascii: u8,
    color: ColorCode,
}

#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

pub struct Writer {
    column: usize,
    color: ColorCode,
    buffer: &'static mut Buffer,
}

impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            0x08 => {
                // Backspace : recule le curseur d'une colonne (sans effacer).
                // Le code appelant enchaîne typiquement '\x08 \x08' pour effacer.
                if self.column > 0 { self.column -= 1; }
            }
            byte => {
                if self.column >= BUFFER_WIDTH { self.new_line(); }
                let row = BUFFER_HEIGHT - 1;
                self.buffer.chars[row][self.column].write(ScreenChar {
                    ascii: byte,
                    color: self.color,
                });
                self.column += 1;
            }
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // Caractères VGA imprimables + contrôles gérés
                0x20..=0x7e | b'\n' | 0x08 => self.write_byte(byte),
                _ => self.write_byte(0xfe),  // ■
            }
        }
    }

    fn new_line(&mut self) {
        // Scroll : on décale toutes les lignes vers le haut
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let c = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(c);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column = 0;
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar { ascii: b' ', color: self.color };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }

    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT { self.clear_row(row); }
        self.column = 0;
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

lazy_static! {
    /// Writer global, protégé par un Mutex spin (pas de std::Mutex en no_std).
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column: 0,
        color: ColorCode::new(Color::LightGreen, Color::Black),
        // SAFETY: 0xb8000 est le buffer VGA text mode, identity-mappé par le boot.
        buffer: unsafe { &mut *(VGA_BUFFER_ADDR as *mut Buffer) },
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::drivers::vga::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // Chemin framebuffer: lock unique + present unique, sans bloquer les IRQ.
    if crate::drivers::console::is_ready() {
        let _ = crate::drivers::console::write_fmt(args);
        return;
    }

    // Fallback VGA: section critique courte comme avant.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _ = WRITER.lock().write_fmt(args);
    });
}
