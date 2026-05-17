// =============================================================================
// displayd — display-server minimal en ring 3.
//
// Architecture :
//   1. FB_ACQUIRE → kernel mappe le backbuffer partagé dans notre AS à
//      info.buffer_ptr. On en fait notre canvas BGRA8888.
//   2. Boucle principale :
//        - INPUT_POLL → batche jusqu'à N events.
//        - render(canvas, events) → on écrit dans le backbuffer (bureau
//          + un curseur souris simple en suivi des MOUSE_MOVE).
//        - FB_PRESENT(None) → kernel blit + flush scanout.
//        - sleep_ms(16) → ~60 FPS.
//   3. À terme : IPC_RECV des apps (windows, draw commands), composition
//      z-order, focus, fenêtres. Pour l'instant on dessine juste un
//      bureau plein écran avec un curseur.
//
// Process model : displayd doit être lancé en tant qu'unique propriétaire
// du FB. Toute autre app utilisera l'IPC pour parler à displayd.
// =============================================================================

#![no_std]
#![no_main]

use ulib::syscall::{
    fb_acquire, fb_present, input_poll,
    FbInfo, InputEvent,
    KIND_MOUSE_MOVE, KIND_MOUSE_DOWN, KIND_MOUSE_UP,
};
use ulib::{println, eprintln};

ulib::entry!(main);

const BG_RGB: u32 = 0x0a1a2a; // bleu nuit
const FG_RGB: u32 = 0xe6e8eb;
const CURSOR_RGB: u32 = 0xffffff;
const CURSOR_SIZE: u32 = 8;

fn main() {
    println!("[displayd] starting");

    let mut info = FbInfo::default();
    if fb_acquire(&mut info) != 0 {
        eprintln!("[displayd] FB_ACQUIRE failed (already held? not display-server?)");
        return;
    }
    if info.buffer_ptr == 0 || info.buffer_len == 0 {
        eprintln!("[displayd] kernel returned null buffer");
        return;
    }

    println!(
        "[displayd] FB acquired: {}x{}@{} pitch={} buf={:#x} len={}",
        info.width, info.height, info.format, info.pitch, info.buffer_ptr, info.buffer_len,
    );

    let width = info.width;
    let height = info.height;
    let pitch_u32 = (info.pitch / 4) as usize;
    let canvas: &mut [u32] = unsafe {
        core::slice::from_raw_parts_mut(
            info.buffer_ptr as *mut u32,
            (info.buffer_len / 4) as usize,
        )
    };

    fill(canvas, pitch_u32, width, height, BG_RGB);
    draw_label(canvas, pitch_u32, 32, 32, "d/OS  -  displayd v0.1", FG_RGB, BG_RGB);

    let mut cursor_x: i32 = (width / 2) as i32;
    let mut cursor_y: i32 = (height / 2) as i32;
    let mut last_cursor_x = cursor_x;
    let mut last_cursor_y = cursor_y;

    let mut events = [InputEvent::default(); 16];
    loop {
        // Drain les events.
        let n = input_poll(&mut events);
        if n > 0 {
            for ev in &events[..n as usize] {
                match ev.kind {
                    KIND_MOUSE_MOVE => {
                        cursor_x = (cursor_x + ev.mouse_dx).clamp(0, width as i32 - 1);
                        cursor_y = (cursor_y + ev.mouse_dy).clamp(0, height as i32 - 1);
                    }
                    KIND_MOUSE_DOWN | KIND_MOUSE_UP => {
                        // À terme : forward au compositor pour click/focus.
                    }
                    _ => {}
                }
            }
        }

        // Repeint la zone du curseur précédent (efface) et redessine au nouveau.
        if cursor_x != last_cursor_x || cursor_y != last_cursor_y {
            fill_rect(
                canvas, pitch_u32,
                last_cursor_x as u32, last_cursor_y as u32,
                CURSOR_SIZE, CURSOR_SIZE, width, height,
                BG_RGB,
            );
            fill_rect(
                canvas, pitch_u32,
                cursor_x as u32, cursor_y as u32,
                CURSOR_SIZE, CURSOR_SIZE, width, height,
                CURSOR_RGB,
            );
            last_cursor_x = cursor_x;
            last_cursor_y = cursor_y;
        }

        // Present (full screen — pas de tracking dirty rect pour l'instant).
        fb_present(None);
        ulib::sleep_ms(16);
    }
}

fn fill(canvas: &mut [u32], pitch_u32: usize, w: u32, h: u32, rgb: u32) {
    for y in 0..h as usize {
        let row = y * pitch_u32;
        for x in 0..w as usize {
            canvas[row + x] = rgb;
        }
    }
}

fn fill_rect(
    canvas: &mut [u32], pitch_u32: usize,
    x: u32, y: u32, w: u32, h: u32,
    max_w: u32, max_h: u32,
    rgb: u32,
) {
    let x_end = (x + w).min(max_w);
    let y_end = (y + h).min(max_h);
    for py in y..y_end {
        let row = py as usize * pitch_u32;
        for px in x..x_end {
            canvas[row + px as usize] = rgb;
        }
    }
}

/// Label texte 8x8 ultra simple — chaque char dessiné comme une petite case
/// pleine, sans vraie police. Placeholder en attendant qu'un BDF font soit
/// embarqué dans ulib.
fn draw_label(
    canvas: &mut [u32], pitch_u32: usize,
    x: u32, y: u32, s: &str, fg: u32, _bg: u32,
) {
    let mut cx = x;
    for ch in s.bytes() {
        // Caractères invisibles : juste avancer.
        if ch == b' ' || ch < 0x20 { cx += 8; continue; }
        // Bloc 6x8 plein pour matérialiser le caractère (placeholder font).
        for dy in 0..8u32 {
            let row = (y + dy) as usize * pitch_u32;
            for dx in 0..6u32 {
                canvas[row + (cx + dx) as usize] = fg;
            }
        }
        cx += 8;
    }
}
