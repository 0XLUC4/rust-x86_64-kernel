// =============================================================================
// mouse.rs — driver souris PS/2 (IRQ12).
//
// Pipeline :
//   IRQ12 (ISR)      -> lit un byte depuis 0x60, assemble paquet 3 bytes,
//                       pousse un `MouseEvent` dans EVENT_QUEUE
//   consommateur     -> lit les événements si un mode graphique/souris est
//                       réactivé plus tard
//
// Protocole (standard PS/2, 3 bytes par paquet) :
//   byte 0 : Y_OF | X_OF | Y_SIGN | X_SIGN | 1 | MB | RB | LB
//   byte 1 : delta X (signed, combiné avec X_SIGN)
//   byte 2 : delta Y (signed, combiné avec Y_SIGN)
//
// Initialisation (via 8042 controller ports 0x60/0x64) :
//   1. enable aux port (cmd 0xA8)
//   2. read config, set bit 1 (aux IRQ enable), clear bit 5 (aux disable), write back
//   3. mouse set-defaults (0xF6)
//   4. mouse enable streaming (0xF4)
// =============================================================================

use conquer_once::spin::OnceCell;
use core::sync::atomic::{AtomicI32, Ordering};
use crossbeam_queue::ArrayQueue;
use spin::Mutex;
use x86_64::instructions::port::Port;

// Ports du 8042 keyboard/mouse controller.
const PORT_DATA: u16 = 0x60;
const PORT_CMD:  u16 = 0x64;  // lecture = status, écriture = commande

// Commandes 8042.
const CMD_ENABLE_AUX:    u8 = 0xA8;
const CMD_READ_CONFIG:   u8 = 0x20;
const CMD_WRITE_CONFIG:  u8 = 0x60;
const CMD_WRITE_AUX:     u8 = 0xD4;

// Commandes souris.
const MOUSE_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE:       u8 = 0xF4;
const MOUSE_SET_SAMPLE:   u8 = 0xF3;   // suivi de la sample rate (Hz)
const MOUSE_SET_RESOLUTION: u8 = 0xE8; // suivi de 0..3 (1..8 counts/mm)

#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub dx: i16,
    pub dy: i16,
    /// Bitmask : bit0 = left, bit1 = right, bit2 = middle.
    pub buttons: u8,
}

impl MouseEvent {
    pub const LEFT:   u8 = 0b001;
    pub const RIGHT:  u8 = 0b010;
    pub const MIDDLE: u8 = 0b100;

    pub fn left(&self)   -> bool { self.buttons & Self::LEFT   != 0 }
    pub fn right(&self)  -> bool { self.buttons & Self::RIGHT  != 0 }
    pub fn middle(&self) -> bool { self.buttons & Self::MIDDLE != 0 }
}

static EVENT_QUEUE: OnceCell<ArrayQueue<MouseEvent>> = OnceCell::uninit();

/// Position logique du curseur (en pixels) — maintenue par le compositeur.
/// Le driver souris n'y touche pas ; il envoie juste les deltas bruts.
pub static CURSOR_X: AtomicI32 = AtomicI32::new(0);
pub static CURSOR_Y: AtomicI32 = AtomicI32::new(0);

/// Boutons actuellement pressés (snapshot).
pub static BUTTONS: Mutex<u8> = Mutex::new(0);

/// État d'assemblage du paquet 3 bytes (entre IRQ successives).
struct PacketAssembler {
    bytes: [u8; 3],
    idx: usize,
}

static ASSEMBLER: Mutex<PacketAssembler> = Mutex::new(PacketAssembler {
    bytes: [0; 3],
    idx: 0,
});

// -----------------------------------------------------------------------------
// Init
// -----------------------------------------------------------------------------

/// Attend que l'input buffer (bit 1 du status) soit vide — contrôleur prêt à recevoir.
fn wait_input_empty() {
    let mut status = Port::<u8>::new(PORT_CMD);
    for _ in 0..100_000 {
        if unsafe { status.read() } & 0b10 == 0 { return; }
    }
}

/// Attend que l'output buffer (bit 0) soit plein — donnée dispo à lire.
fn wait_output_full() {
    let mut status = Port::<u8>::new(PORT_CMD);
    for _ in 0..100_000 {
        if unsafe { status.read() } & 0b01 != 0 { return; }
    }
}

fn write_cmd(cmd: u8) {
    wait_input_empty();
    unsafe { Port::<u8>::new(PORT_CMD).write(cmd); }
}

fn write_data(byte: u8) {
    wait_input_empty();
    unsafe { Port::<u8>::new(PORT_DATA).write(byte); }
}

fn read_data() -> u8 {
    wait_output_full();
    unsafe { Port::<u8>::new(PORT_DATA).read() }
}

/// Envoie une commande au device auxiliaire (souris). On doit préfixer par 0xD4
/// pour indiquer au 8042 que le byte suivant va à la souris, pas au clavier.
fn mouse_write(byte: u8) -> Result<(), &'static str> {
    write_cmd(CMD_WRITE_AUX);
    write_data(byte);
    // ACK attendu (0xFA).
    let ack = read_data();
    if ack != 0xFA {
        crate::serial_println!("[mouse] unexpected response {:#x} (attendu 0xFA)", ack);
        return Err("mouse: commande non ACKed");
    }
    Ok(())
}

pub fn init() -> Result<(), &'static str> {
    EVENT_QUEUE
        .try_init_once(|| ArrayQueue::new(512))
        .map_err(|_| "mouse: init appelé deux fois")?;

    // 1. Active le port auxiliaire.
    write_cmd(CMD_ENABLE_AUX);

    // 2. Lit config, active IRQ12 (bit 1), clear aux-disable (bit 5).
    write_cmd(CMD_READ_CONFIG);
    let mut cfg = read_data();
    cfg |= 0b0000_0010;   // IRQ12 enable
    cfg &= !0b0010_0000;  // aux clock enable (clear bit 5)
    write_cmd(CMD_WRITE_CONFIG);
    write_data(cfg);

    // 3. Reset à la config par défaut.
    mouse_write(MOUSE_SET_DEFAULTS)?;

    // 4. Boost : sample rate 200 Hz (max std) + résolution 8 counts/mm (max).
    mouse_write(MOUSE_SET_SAMPLE)?;
    mouse_write(200)?;
    mouse_write(MOUSE_SET_RESOLUTION)?;
    mouse_write(3)?; // 3 = 8 counts/mm

    // 5. Active le streaming.
    mouse_write(MOUSE_ENABLE)?;

    crate::serial_println!("[mouse] PS/2 souris prête (IRQ12, cfg={:#x})", cfg);
    Ok(())
}

// -----------------------------------------------------------------------------
// ISR hook — appelé depuis le handler d'IRQ12 (idt.rs).
// DOIT être rapide et non-bloquant.
// -----------------------------------------------------------------------------

pub(crate) fn on_irq() {
    // Vérifie que le byte dispo vient bien de l'aux (bit 5 du status).
    // Sinon, IRQ12 parasite (QEMU peut grouper plusieurs paquets).
    let status = unsafe { Port::<u8>::new(PORT_CMD).read() };
    if status & 0b0010_0001 != 0b0010_0001 {
        // Pas de byte aux prêt : on sort (EOI sera envoyé par l'appelant).
        return;
    }

    let byte = unsafe { Port::<u8>::new(PORT_DATA).read() };
    let mut asm = ASSEMBLER.lock();

    // Sanity : le byte 0 d'un paquet valide a le bit 3 set (cf spec PS/2).
    if asm.idx == 0 && (byte & 0b1000) == 0 {
        // Resync : ignore ce byte, ne compte rien.
        return;
    }

    let idx = asm.idx;
    asm.bytes[idx] = byte;
    asm.idx = idx + 1;

    if asm.idx < 3 { return; }
    asm.idx = 0;

    let b0 = asm.bytes[0];
    let b1 = asm.bytes[1];
    let b2 = asm.bytes[2];

    // Overflow : drop
    if (b0 & 0b1100_0000) != 0 { return; }

    // Sign extend via les bits 4/5 du byte 0.
    let dx_raw = b1 as i16;
    let dy_raw = b2 as i16;
    let dx = if b0 & 0b0001_0000 != 0 { dx_raw - 256 } else { dx_raw };
    // Y est inversé (souris : +Y = vers le haut ; écran : +Y = vers le bas).
    let dy = if b0 & 0b0010_0000 != 0 { dy_raw - 256 } else { dy_raw };
    let dy = -dy;

    let buttons = b0 & 0b111;
    *BUTTONS.lock() = buttons;

    let ev = MouseEvent { dx, dy, buttons };
    if let Ok(q) = EVENT_QUEUE.try_get() {
        let _ = q.push(ev);
    }
}

/// Récupère le prochain événement, ou None.
pub fn try_recv() -> Option<MouseEvent> {
    EVENT_QUEUE.try_get().ok()?.pop()
}
