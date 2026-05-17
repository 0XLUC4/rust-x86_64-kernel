// =============================================================================
// console.rs — terminal texte au-dessus du framebuffer.
//
// Maintient une grille virtuelle (cols × rows) calculée depuis la taille FB :
//   cols = width / 8
//   rows = height / 16   (moins 2 pour la status bar en bas)
//
// Gère : écriture caractère, saut de ligne, scroll vertical, backspace,
// couleur FG/BG par chunk. Lock-protected.
// =============================================================================

use core::fmt;
use spin::{Mutex, Once};

use crate::drivers::fb::{self, Fb, BG, WHITE};

pub struct Console {
    cols: u32,
    rows: u32,
    glyph_w: u32,
    glyph_h: u32,
    cx: u32,
    cy: u32,
    fg: u32,
    bg: u32,
}

impl Console {
    fn new(fb: &Fb) -> Self {
        let glyph_w = fb.glyph_width();
        let glyph_h = fb.glyph_height();
        // On réserve 2 lignes en bas pour la status bar
        let rows_total = fb.height() / glyph_h;
        let rows = rows_total.saturating_sub(2);
        Console {
            cols: fb.width() / glyph_w,
            rows,
            glyph_w,
            glyph_h,
            cx: 0,
            cy: 0,
            fg: WHITE,
            bg: BG,
        }
    }

    fn newline(&mut self, fb: &mut Fb) {
        self.cx = 0;
        self.cy += 1;
        if self.cy >= self.rows {
            // Scroll borné à la zone console — laisse intacte la status bar
            let max_y = self.rows * self.glyph_h;
            fb.scroll_up_bounded(max_y, self.glyph_h, self.bg);
            self.cy = self.rows - 1;
        }
    }

    fn put_char(&mut self, fb: &mut Fb, ch: char) {
        match ch {
            '\n' => self.newline(fb),
            '\r' => self.cx = 0,
            '\x08' => {
                // Backspace : recule curseur et efface cellule
                if self.cx > 0 {
                    self.cx -= 1;
                    let x = self.cx * self.glyph_w;
                    let y = self.cy * self.glyph_h;
                    fb.fill_rect(x, y, self.glyph_w, self.glyph_h, self.bg);
                }
            }
            '\t' => {
                // Tab = aligne sur prochain multiple de 4
                let next = (self.cx / 4 + 1) * 4;
                while self.cx < next && self.cx < self.cols {
                    self.put_char(fb, ' ');
                }
            }
            ch => {
                if self.cx >= self.cols { self.newline(fb); }
                let x = self.cx * self.glyph_w;
                let y = self.cy * self.glyph_h;
                fb.blit_char(x, y, ch, self.fg, self.bg);
                self.cx += 1;
            }
        }
    }

    fn write_string(&mut self, fb: &mut Fb, s: &str) {
        for ch in s.chars() { self.put_char(fb, ch); }
    }

    fn clear(&mut self, fb: &mut Fb) {
        fb.clear(self.bg);
        self.cx = 0;
        self.cy = 0;
    }
}

static CONSOLE: Once<Mutex<Console>> = Once::new();

pub fn init() {
    if let Some(fb_mutex) = fb::fb() {
        let fb = fb_mutex.lock();
        let cons = Console::new(&*fb);
        let cols = cons.cols;
        let rows = cons.rows;
        CONSOLE.call_once(|| Mutex::new(cons));
        crate::serial_println!("[cons] {}x{} caractères", cols, rows);
    }
}

pub fn is_ready() -> bool { CONSOLE.get().is_some() }

/// Écrit une string sur la console FB si elle est initialisée.
/// Retourne true si écrit, false sinon (fallback VGA).
pub fn write_str(s: &str) -> bool {
    let Some(cons_mx) = CONSOLE.get() else { return false; };
    let Some(fb_mx) = fb::fb() else { return false; };
    let mut fb = fb_mx.lock();
    let mut c = cons_mx.lock();
    c.write_string(&mut *fb, s);
    fb.commit();
    fb.present();
    true
}

pub fn clear() {
    if let (Some(cons_mx), Some(fb_mx)) = (CONSOLE.get(), fb::fb()) {
        let mut fb = fb_mx.lock();
        let mut c = cons_mx.lock();
        c.clear(&mut *fb);
        fb.commit();
        fb.present();
    }
}

pub fn set_colors(fg: u32, bg: u32) {
    if let Some(cons_mx) = CONSOLE.get() {
        let mut c = cons_mx.lock();
        c.fg = fg;
        c.bg = bg;
    }
}

/// Dessine la status bar dans les 2 lignes du bas (pas gérées par la console).
pub fn draw_status_bar(text: &str) {
    let Some(cons_mx) = CONSOLE.get() else { return; };
    let Some(fb_mx) = fb::fb() else { return; };
    let mut fb = fb_mx.lock();
    let cons = cons_mx.lock();
    let fb_width = fb.width();

    let bar_y = cons.rows * cons.glyph_h;
    let bar_h = 2 * cons.glyph_h;
    // Fond bleu nuit
    fb.fill_rect(0, bar_y, fb_width, bar_h, 0x282a36);
    // Bordure haute
    fb.fill_rect(0, bar_y, fb_width, 1, fb::CYAN);

    // Texte blanc centré verticalement
    let text_y = bar_y + (bar_h - cons.glyph_h) / 2;
    let mut x = 8u32;
    for ch in text.chars() {
        if x + cons.glyph_w > fb_width { break; }
        fb.blit_char(x, text_y, ch, fb::WHITE, 0x282a36);
        x += cons.glyph_w;
    }
    fb.commit();
    fb.present();
}

// -----------------------------------------------------------------------------
// Impl fmt::Write pour intégration avec `write!` / `println!`
// -----------------------------------------------------------------------------

pub struct ConsoleFmt;

impl fmt::Write for ConsoleFmt {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_str(s);
        Ok(())
    }
}

struct LockedConsoleWriter<'a> {
    console: &'a mut Console,
    fb: &'a mut Fb,
}

impl fmt::Write for LockedConsoleWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.console.write_string(self.fb, s);
        Ok(())
    }
}

/// Écrit des arguments formatés en lockant une seule fois console + fb,
/// puis flush un unique present().
pub fn write_fmt(args: fmt::Arguments) -> bool {
    let Some(cons_mx) = CONSOLE.get() else { return false; };
    let Some(fb_mx) = fb::fb() else { return false; };

    let mut fb = fb_mx.lock();
    let mut c = cons_mx.lock();
    let mut writer = LockedConsoleWriter {
        console: &mut *c,
        fb: &mut *fb,
    };
    let _ = fmt::write(&mut writer, args);
    fb.commit();
    fb.present();
    true
}
