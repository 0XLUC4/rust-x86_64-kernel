// =============================================================================
// keyboard.rs — driver clavier PS/2 (v2).
//
// Pipeline :
//   IRQ1 (ISR)  -> SCANCODE_QUEUE (u8, lock-free)
//   KeyStream  -> lit la queue, décode via pc-keyboard, yield `char`
//
// L'ISR pousse les scancodes bruts. Le décodage (scancode set 1 -> codepoint
// Unicode) est fait côté consommateur pour garder l'ISR ultra-rapide.
// =============================================================================

use conquer_once::spin::OnceCell;
use core::{
    pin::Pin,
    task::{Context, Poll},
};
use crossbeam_queue::ArrayQueue;
use futures_util::{stream::Stream, task::AtomicWaker};
use pc_keyboard::{layouts, DecodedKey, HandleControl, KeyCode, Keyboard, ScancodeSet1};
use spin::Mutex;

/// Codes "control chars" maison pour les flèches, vu que DecodedKey::RawKey
/// ne rentre pas dans un char. Le shell les décode pour gérer l'historique.
pub const KEY_UP:    char = '\u{11}';
pub const KEY_DOWN:  char = '\u{12}';
pub const KEY_LEFT:  char = '\u{13}';
pub const KEY_RIGHT: char = '\u{14}';
pub const KEY_HOME:  char = '\u{15}';
pub const KEY_END:   char = '\u{16}';
pub const KEY_DEL:   char = '\u{17}';

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();
static DECODER: OnceCell<Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>>> = OnceCell::uninit();

const LOG_SCANCODES: bool = false;

/// Initialise la queue. À appeler tôt dans le boot.
pub fn init() {
    SCANCODE_QUEUE
        .try_init_once(|| ArrayQueue::new(128))
        .expect("keyboard::init appelé deux fois");
    DECODER
        .try_init_once(|| {
            Mutex::new(Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::Ignore,
            ))
        })
        .expect("keyboard decoder init appelé deux fois");
}

/// Appelé depuis l'ISR clavier (idt.rs). DOIT être non-bloquant et rapide.
pub(crate) fn add_scancode(scancode: u8) {
    if LOG_SCANCODES {
        crate::serial_println!("[kbd] sc={:#x}", scancode);
    }
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if queue.push(scancode).is_ok() {
            WAKER.wake();
        }
    }
}

/// Lit un caractère décodé si disponible (mode polling synchrone).
pub fn try_read_char() -> Option<char> {
    let queue = SCANCODE_QUEUE.try_get().ok()?;
    let decoder_mx = DECODER.try_get().ok()?;
    let scancode = queue.pop()?;
    let mut decoder = decoder_mx.lock();
    decode(&mut *decoder, scancode)
}

/// Vide la queue clavier pour éviter de rejouer des touches anciennes.
pub fn drain_queue() {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        while queue.pop().is_some() {}
    }
}

/// Stream de caractères décodés (niveau user).
pub struct KeyStream {
    keyboard: Keyboard<layouts::Us104Key, ScancodeSet1>,
}

impl KeyStream {
    pub fn new() -> Self {
        KeyStream {
            keyboard: Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::Ignore,
            ),
        }
    }
}

impl Stream for KeyStream {
    type Item = char;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<char>> {
        let queue = SCANCODE_QUEUE.try_get().expect("keyboard pas init");

        loop {
            if let Some(scancode) = queue.pop() {
                if let Some(ch) = decode(&mut self.keyboard, scancode) {
                    return Poll::Ready(Some(ch));
                }
                // Scancode qui ne produit pas de char (ex: shift, relâchement) -> reboucle
                continue;
            }

            // Rien dispo : register waker et retest (évite race ISR)
            WAKER.register(cx.waker());
            match queue.pop() {
                Some(sc) => {
                    WAKER.take();
                    if let Some(ch) = decode(&mut self.keyboard, sc) {
                        return Poll::Ready(Some(ch));
                    }
                    // reboucle dans la loop
                }
                None => return Poll::Pending,
            }
        }
    }
}

fn decode(kbd: &mut Keyboard<layouts::Us104Key, ScancodeSet1>, scancode: u8) -> Option<char> {
    let Ok(Some(event)) = kbd.add_byte(scancode) else { return None; };
    match kbd.process_keyevent(event) {
        Some(DecodedKey::Unicode(ch)) => Some(ch),
        Some(DecodedKey::RawKey(kc)) => match kc {
            KeyCode::ArrowUp    => Some(KEY_UP),
            KeyCode::ArrowDown  => Some(KEY_DOWN),
            KeyCode::ArrowLeft  => Some(KEY_LEFT),
            KeyCode::ArrowRight => Some(KEY_RIGHT),
            KeyCode::Home       => Some(KEY_HOME),
            KeyCode::End        => Some(KEY_END),
            KeyCode::Delete     => Some(KEY_DEL),
            _ => None,
        },
        _ => None,
    }
}
