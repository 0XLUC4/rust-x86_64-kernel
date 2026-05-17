// =============================================================================
// fb.rs — driver framebuffer linéaire (RGB 32 bpp).
//
// Obtenu depuis le multiboot2 tag 8. On map la région MMIO (avec WRITE_THROUGH
// pour la vidéo, sinon le CPU cache les écritures et l'écran saccade).
//
// API :
//   Fb::init(info)       — depuis boot_info::FramebufferInfo
//   Fb::put_pixel(x, y, rgb)
//   Fb::fill_rect(x, y, w, h, rgb)
//   Fb::blit_char(x, y, ch, fg, bg)  — rendu 8x16
//   Fb::scroll_up(lines)
//   Fb::clear(bg)
//
// Couleur = u32 encodé 0x00RRGGBB (on assume le format GRUB par défaut XRGB).
// =============================================================================

use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use spin::{Mutex, Once};

use crate::boot_info::FramebufferInfo;
use crate::drivers::font8x16::{self, CHAR_HEIGHT, CHAR_WIDTH};
use crate::memory::paging;

/// Copie rapide src→dst de `count` u32 via `rep movsq` (+ tail `rep movsd`).
///
/// Exige :
///   - `dst` et `src` pointent chacun sur au moins `count` u32 valides
///   - Les deux zones ne se chevauchent pas (comme copy_nonoverlapping)
///   - Alignement 4 octets (garanti pour un framebuffer RGBA32 linéaire)
///
/// rep movsq copie par paquets de 8 octets. Sur les CPU Intel/AMD modernes
/// en mode MMIO write-combining, c'est la primitive de copie mémoire la
/// plus rapide accessible sans SSE.
#[inline]
pub unsafe fn fast_blit_row(src: *const u32, dst: *mut u32, count: usize) {
    if count == 0 { return; }
    let qwords = count / 2;          // 2 u32 par u64
    let tail   = count % 2;          // 0 ou 1 u32 restant

    if qwords > 0 {
        // rep movsq : RSI → RDI, RCX fois, 8 octets à la fois.
        asm!(
            "rep movsq",
            inout("rcx") qwords => _,
            inout("rsi") src    => _,
            inout("rdi") dst    => _,
            options(nostack, preserves_flags),
        );
    }
    if tail > 0 {
        // Un u32 final : write direct.
        let src_tail = src.add(count - 1);
        let dst_tail = dst.add(count - 1);
        core::ptr::write_volatile(dst_tail, *src_tail);
    }
}

pub const BLACK:      u32 = 0x000000;
pub const DARK_GRAY:  u32 = 0x1e1e2e;
pub const BG:         u32 = 0x0a0a14;
pub const WHITE:      u32 = 0xe8e8f0;
pub const GREEN:      u32 = 0x50fa7b;
pub const CYAN:       u32 = 0x8be9fd;
pub const YELLOW:     u32 = 0xf1fa8c;
pub const RED:        u32 = 0xff5555;
pub const BLUE:       u32 = 0x6272a4;
pub const MAGENTA:    u32 = 0xff79c6;

#[derive(Clone, Copy)]
struct DirtyRect {
    any: bool,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

/// Backend de présentation : MMIO direct (legacy) ou virtio-gpu.
#[derive(Clone, Copy, Debug)]
pub enum PresentBackend {
    /// Scanout MMIO classique fourni par multiboot (VBE / -vga std).
    Mmio,
    /// virtio-gpu : le present_buf est copié vers la resource GPU via
    /// TRANSFER_TO_HOST_2D + RESOURCE_FLUSH.
    VirtioGpu,
}

pub struct Fb {
    front: *mut u32,   // MMIO scanout si backend = Mmio, sinon unused
    /// Buffer dans lequel les primitives écrivent (`fill_rect`, `blit_char`).
    /// C'est **toujours** celui-ci qui est muté par les paints.
    back: Vec<u32>,
    /// Buffer "prêt à présenter". Swappé avec `back` au moment de present.
    /// Le memcpy MMIO se fait depuis celui-ci — jamais lu par les paints.
    present_buf: Vec<u32>,
    width: u32,
    height: u32,
    pitch_u32: u32,
    text_scale: u32,
    dirty: DirtyRect,
    frame_ready: bool,
    ready_dirty: DirtyRect,
    backend: PresentBackend,
}

// SAFETY: la MMIO framebuffer n'est accédée que via FB_LOCK.
unsafe impl Send for Fb {}

impl Fb {
    #[inline]
    pub fn width(&self) -> u32 { self.width }
    #[inline]
    pub fn height(&self) -> u32 { self.height }
    /// Stride en bytes (pitch_u32 * 4 octets).
    #[inline]
    pub fn pitch_bytes(&self) -> u32 { self.pitch_u32 * 4 }

    #[inline]
    pub fn glyph_width(&self) -> u32 {
        (CHAR_WIDTH as u32) * self.text_scale
    }

    #[inline]
    pub fn glyph_height(&self) -> u32 {
        (CHAR_HEIGHT as u32) * self.text_scale
    }

    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        (y * self.pitch_u32 + x) as usize
    }

    fn mark_dirty_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        if w == 0 || h == 0 { return; }
        let x0 = x.min(self.width);
        let y0 = y.min(self.height);
        let x1 = x.saturating_add(w).min(self.width);
        let y1 = y.saturating_add(h).min(self.height);
        if x0 >= x1 || y0 >= y1 { return; }

        if !self.dirty.any {
            self.dirty = DirtyRect { any: true, x0, y0, x1, y1 };
            return;
        }

        self.dirty.x0 = self.dirty.x0.min(x0);
        self.dirty.y0 = self.dirty.y0.min(y0);
        self.dirty.x1 = self.dirty.x1.max(x1);
        self.dirty.y1 = self.dirty.y1.max(y1);
    }

    #[inline]
    pub fn put_pixel(&mut self, x: u32, y: u32, rgb: u32) {
        if x >= self.width || y >= self.height { return; }
        let off = self.idx(x, y);
        self.back[off] = rgb;
        self.mark_dirty_rect(x, y, 1, 1);
    }

    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, rgb: u32) {
        let x1 = x.min(self.width);
        let y1 = y.min(self.height);
        let x2 = x.saturating_add(w).min(self.width);
        let y2 = y.saturating_add(h).min(self.height);
        for py in y1..y2 {
            let row = (py * self.pitch_u32) as usize;
            for px in x1..x2 {
                self.back[row + px as usize] = rgb;
            }
        }
        self.mark_dirty_rect(x1, y1, x2.saturating_sub(x1), y2.saturating_sub(y1));
    }

    pub fn clear(&mut self, rgb: u32) {
        self.fill_rect(0, 0, self.width, self.height, rgb);
    }

    /// Rendu d'un caractère 8x16 upscalé. Optimisé pour le cas commun scale=1.
    ///
    /// Robuste aux coordonnées hors écran : si `x` ou `y` dépassent les
    /// dimensions du FB, la fonction retourne sans rien faire.
    pub fn blit_char(&mut self, x: u32, y: u32, ch: char, fg: u32, bg: u32) {
        if x >= self.width || y >= self.height { return; }

        let glyph = font8x16::glyph(ch);
        let scale = self.text_scale;
        let gw = self.glyph_width();
        let gh = self.glyph_height();

        if scale == 1 {
            // Chemin rapide : 8 pixels par ligne, 16 lignes, écriture sans saturating.
            let max_x = (x + 8).min(self.width);
            let cols = (max_x - x) as usize;
            for gy in 0..CHAR_HEIGHT as u32 {
                let py = y + gy;
                if py >= self.height { break; }
                let bits = glyph[gy as usize];
                let row = (py * self.pitch_u32) as usize;
                let base = row + x as usize;
                // Déroule le loop 8 pixels (inlined par compilateur).
                for gx in 0..cols {
                    let on = (bits >> (7 - gx)) & 1 != 0;
                    self.back[base + gx] = if on { fg } else { bg };
                }
            }
        } else {
            // Chemin lent : scale > 1.
            for gy in 0..CHAR_HEIGHT as u32 {
                let bits = glyph[gy as usize];
                for sy in 0..scale {
                    let py = y.saturating_add(gy.saturating_mul(scale)).saturating_add(sy);
                    if py >= self.height { break; }
                    let row = (py * self.pitch_u32) as usize;
                    for gx in 0..CHAR_WIDTH as u32 {
                        let on = (bits >> (7 - gx as usize)) & 1 != 0;
                        let color = if on { fg } else { bg };
                        let px0 = x.saturating_add(gx.saturating_mul(scale));
                        for sx in 0..scale {
                            let px = px0.saturating_add(sx);
                            if px >= self.width { break; }
                            self.back[row + px as usize] = color;
                        }
                    }
                }
            }
        }
        self.mark_dirty_rect(x, y, gw, gh);
    }

    /// Scroll software limité à la zone [0, max_y) pixels (n'affecte pas
    /// les lignes en dessous — typiquement réservées à la status bar).
    pub fn scroll_up_bounded(&mut self, max_y: u32, pixels: u32, clear_color: u32) {
        if pixels == 0 || pixels >= max_y { return; }
        let y_lim = max_y.min(self.height);

        let line_u32 = self.pitch_u32 as usize;
        let src_start = (pixels as usize) * line_u32;
        let to_copy = ((y_lim - pixels) as usize) * line_u32;
        self.back.copy_within(src_start..(src_start + to_copy), 0);

        // Efface la bande basse de la zone scrollée.
        self.fill_rect(0, y_lim - pixels, self.width, pixels, clear_color);
        self.mark_dirty_rect(0, 0, self.width, y_lim);
    }

    /// Scroll plein écran (legacy).
    pub fn scroll_up(&mut self, pixels: u32, clear_color: u32) {
        self.scroll_up_bounded(self.height, pixels, clear_color);
    }

    /// Finalise la frame en cours : swap le backbuffer draw ↔ present_buf.
    /// Après `commit`, les futurs fill_rect/blit_char vont dans le nouveau
    /// draw buffer (qui était l'ancien present_buf), et `present()` va
    /// copier present_buf (= l'ancien draw) vers la scanout.
    ///
    /// C'est le cœur du double buffer : la phase "paint" et la phase
    /// "memcpy MMIO" touchent deux tampons RAM distincts.
    pub fn commit(&mut self) {
        if !self.dirty.any && !self.frame_ready {
            // Rien de neuf et pas de frame en attente : skip.
            return;
        }
        // Avant swap, recopie les zones non-dirty du present_buf vers back
        // pour que la prochaine frame reparte d'un état cohérent.
        // Simplification : on copie intégralement present_buf → back puis on
        // applique le swap. Coût : un memcpy RAM→RAM de la taille du fb.
        // Pour l'instant on accepte ce coût ; plus tard on tracker le delta.
        if self.dirty.any {
            // Promote la zone dirty draw comme ready.
            self.ready_dirty = self.dirty;
            self.dirty = DirtyRect { any: false, x0: 0, y0: 0, x1: 0, y1: 0 };
            // Swap pointeurs (Vec::swap échange les allocs sans copie).
            core::mem::swap(&mut self.back, &mut self.present_buf);
            // Sync : recopie la zone dirty depuis present_buf (qui vient
            // d'être produit) vers back, pour que la prochaine frame parte
            // d'un snapshot visuellement identique.
            let (x0, y0, x1, y1) = (
                self.ready_dirty.x0, self.ready_dirty.y0,
                self.ready_dirty.x1, self.ready_dirty.y1,
            );
            for y in y0..y1 {
                let row = (y * self.pitch_u32) as usize;
                let off = row + x0 as usize;
                let len = (x1 - x0) as usize;
                // SAFETY: offsets déjà clampés ≤ width/height à mark_dirty_rect.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        self.present_buf.as_ptr().add(off),
                        self.back.as_mut_ptr().add(off),
                        len,
                    );
                }
            }
            self.frame_ready = true;
        }
    }

    /// Copie la frame prête (present_buf) vers la scanout.
    /// Route vers MMIO direct ou virtio-gpu selon le backend.
    pub fn present(&mut self) {
        if !self.frame_ready { return; }

        match self.backend {
            PresentBackend::Mmio => self.present_mmio(),
            PresentBackend::VirtioGpu => self.present_virtio_gpu(),
        }

        self.frame_ready = false;
        self.ready_dirty = DirtyRect { any: false, x0: 0, y0: 0, x1: 0, y1: 0 };
    }

    fn present_mmio(&mut self) {
        let (x0, y0, x1, y1) = (
            self.ready_dirty.x0, self.ready_dirty.y0,
            self.ready_dirty.x1, self.ready_dirty.y1,
        );
        let copy_len = (x1 - x0) as usize;
        for y in y0..y1 {
            let row = (y * self.pitch_u32) as usize;
            let off = row + x0 as usize;
            unsafe {
                fast_blit_row(
                    self.present_buf.as_ptr().add(off),
                    self.front.add(off),
                    copy_len,
                );
            }
        }
    }

    fn present_virtio_gpu(&mut self) {
        // 1. Copie dirty region present_buf → fb virtio-gpu (CPU→RAM GPU).
        // 2. TRANSFER_TO_HOST_2D + RESOURCE_FLUSH.
        use crate::virtio::gpu::GPU;
        let g = match GPU.get() { Some(g) => g, None => return };
        let mut gpu = g.lock();
        let (x0, y0, x1, y1) = (
            self.ready_dirty.x0, self.ready_dirty.y0,
            self.ready_dirty.x1, self.ready_dirty.y1,
        );
        let w = x1 - x0;
        let h = y1 - y0;
        if w == 0 || h == 0 { return; }
        // Copie région par région (ligne par ligne si pitch diffère).
        let dst_w = gpu.width as usize;
        for y in y0..y1 {
            let src_off = (y * self.pitch_u32) as usize + x0 as usize;
            let dst_off = y as usize * dst_w + x0 as usize;
            let len = (x1 - x0) as usize;
            // SAFETY: deux Vec<u32> distincts, pas d'overlap possible.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.present_buf.as_ptr().add(src_off),
                    gpu.fb.as_mut_ptr().add(dst_off),
                    len,
                );
            }
        }
        // 3. Transfert GPU + flush.
        let _ = gpu.present_region(x0, y0, w, h);
    }

    /// Change le backend de présentation. À appeler après virtio::gpu::init().
    pub fn set_backend(&mut self, backend: PresentBackend) {
        crate::serial_println!("[fb] backend : {:?}", backend);
        self.backend = backend;
    }

    pub fn backend(&self) -> PresentBackend { self.backend }

    /// Marque manuellement une zone comme dirty sans y écrire.
    pub fn mark_rect_dirty(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.mark_dirty_rect(x, y, w, h);
    }

    /// Lit un pixel dans le backbuffer (celui sur lequel les paints écrivent).
    #[inline]
    pub fn read_back(&self, x: u32, y: u32) -> u32 {
        if x >= self.width || y >= self.height { return 0; }
        let off = (y * self.pitch_u32 + x) as usize;
        self.back[off]
    }

    /// Écrit un pixel dans le backbuffer (sans toucher au dirty).
    /// Utile pour restaurer des pixels sauvegardés par le compositor
    /// (p. ex. sous le curseur).
    #[inline]
    pub fn write_back_raw(&mut self, x: u32, y: u32, color: u32) {
        if x >= self.width || y >= self.height { return; }
        let off = (y * self.pitch_u32 + x) as usize;
        self.back[off] = color;
    }

    /// Force-flush un rectangle spécifique present_buf→front, sans toucher
    /// au dirty. Utile pour le software cursor : on restaure juste la petite
    /// zone curseur depuis le buffer "propre" (présenté à la dernière frame).
    pub fn present_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        let x0 = x.min(self.width);
        let y0 = y.min(self.height);
        if x0 >= x1 || y0 >= y1 { return; }
        let copy_len = (x1 - x0) as usize;
        for yy in y0..y1 {
            let row = (yy * self.pitch_u32) as usize;
            let off = row + x0 as usize;
            unsafe {
                fast_blit_row(
                    self.present_buf.as_ptr().add(off),
                    self.front.add(off),
                    copy_len,
                );
            }
        }
    }

    /// Écrit un pixel **directement** dans la MMIO sans toucher au backbuffer
    /// ni au dirty. Usage : overlay curseur.
    #[inline]
    pub fn write_front_pixel(&mut self, x: u32, y: u32, rgb: u32) {
        if x >= self.width || y >= self.height { return; }
        let off = (y * self.pitch_u32 + x) as usize;
        unsafe { core::ptr::write_volatile(self.front.add(off), rgb); }
    }

    /// Relit un pixel du present_buf (l'état "propre" affiché, sans curseur).
    #[inline]
    pub fn read_back_pixel(&self, x: u32, y: u32) -> u32 {
        if x >= self.width || y >= self.height { return 0; }
        let off = (y * self.pitch_u32 + x) as usize;
        self.present_buf[off]
    }

    /// Restaure une région du front depuis le back (efface overlay).
    pub fn restore_front_region(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.present_rect(x, y, w, h);
    }
}

static FB: Once<Mutex<Fb>> = Once::new();

/// Initialise le driver framebuffer depuis les infos multiboot2.
/// Retourne Err si pas de FB ou format non géré.
pub fn init(info: &FramebufferInfo) -> Result<(), &'static str> {
    if info.fb_type != 1 {
        return Err("FB: type non RGB (type 1 attendu)");
    }
    if info.bpp != 32 {
        return Err("FB: seul 32 bpp supporté pour l'instant");
    }
    if info.pitch % 4 != 0 {
        return Err("FB: pitch non multiple de 4");
    }

    let size = (info.pitch * info.height) as usize;
    paging::map_mmio(info.addr, size)?;

    let pitch_u32 = info.pitch / 4;
    let text_scale = if info.width >= 2560 {
        3
    } else if info.width >= 1920 {
        2
    } else {
        1
    };

    let buf_len = (pitch_u32 * info.height) as usize;
    let mut fb = Fb {
        front: info.addr as *mut u32,
        back: vec![BG; buf_len],
        present_buf: vec![BG; buf_len],
        width: info.width,
        height: info.height,
        pitch_u32,
        text_scale,
        dirty: DirtyRect { any: false, x0: 0, y0: 0, x1: 0, y1: 0 },
        frame_ready: false,
        ready_dirty: DirtyRect { any: false, x0: 0, y0: 0, x1: 0, y1: 0 },
        backend: PresentBackend::Mmio,
    };

    // Clear + clear scanout à la couleur de fond.
    fb.clear(BG);
    fb.commit();
    fb.present();

    crate::serial_println!(
        "[fb] init OK {}x{}@{}bpp pitch={} addr={:#x} text_scale={} glyph={}x{}",
        info.width,
        info.height,
        info.bpp,
        info.pitch,
        info.addr,
        text_scale,
        fb.glyph_width(),
        fb.glyph_height(),
    );

    FB.call_once(|| Mutex::new(fb));
    Ok(())
}

pub fn fb() -> Option<&'static Mutex<Fb>> { FB.get() }

pub fn is_active() -> bool { FB.get().is_some() }

/// Initialise le FB en backend virtio-gpu pur (aucun scanout MMIO).
/// Dimensions lues depuis virtio-gpu. Utile quand -vga none est passé à QEMU.
pub fn init_virtio_gpu(width: u32, height: u32) -> Result<(), &'static str> {
    if FB.get().is_some() {
        return Err("FB déjà initialisé");
    }
    let pitch_u32 = width;
    let text_scale = if width >= 2560 { 3 } else if width >= 1920 { 2 } else { 1 };
    let buf_len = (pitch_u32 * height) as usize;

    let mut fb = Fb {
        front: core::ptr::null_mut(),
        back: alloc::vec![BG; buf_len],
        present_buf: alloc::vec![BG; buf_len],
        width,
        height,
        pitch_u32,
        text_scale,
        dirty: DirtyRect { any: false, x0: 0, y0: 0, x1: 0, y1: 0 },
        frame_ready: false,
        ready_dirty: DirtyRect { any: false, x0: 0, y0: 0, x1: 0, y1: 0 },
        backend: PresentBackend::VirtioGpu,
    };
    fb.clear(BG);
    fb.commit();
    fb.present();

    crate::serial_println!("[fb] init virtio-gpu OK {}x{} text_scale={}",
        width, height, text_scale);

    FB.call_once(|| Mutex::new(fb));
    Ok(())
}
