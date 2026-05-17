// =============================================================================
// input_queue — anneau lock-free pour InputEvents bruts.
//
// Producteurs : ISRs clavier / souris (peuvent appeler push_event en
// contexte interruption).
// Consommateur unique : sys_input_poll appelé par le display-server.
//
// Implémentation : ArrayQueue<InputEventAbi> (crossbeam) — déjà utilisé par
// l'executor. Capacité 256, drop le plus ancien en cas de saturation.
//
// Layout InputEventAbi : doit matcher abi::input::InputEvent.
// =============================================================================

use crossbeam_queue::ArrayQueue;
use spin::Once;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct InputEventAbi {
    pub kind: u32,
    pub _pad0: u32,             // alignement timestamp_ms u64
    pub timestamp_ms: u64,
    pub scancode: u32,
    pub keysym: u32,
    pub mods: u32,
    pub _pad1: u32,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub mouse_dx: i32,
    pub mouse_dy: i32,
    pub mouse_buttons: u32,
    pub wheel: i32,
}

// L'ABI userland (abi::input::InputEvent) ne porte pas de _pad explicites ;
// c'est le compilateur qui les insère pour aligner u64. On force ici pour
// rester en contrôle au cas où Rust changerait la stratégie.
const _: () = assert!(core::mem::size_of::<InputEventAbi>() == 56,
    "InputEventAbi doit faire 56 bytes (cf abi::input::InputEvent)");

pub const KIND_NONE:        u32 = 0;
pub const KIND_KEY_DOWN:    u32 = 1;
pub const KIND_KEY_UP:      u32 = 2;
pub const KIND_MOUSE_MOVE:  u32 = 3;
pub const KIND_MOUSE_DOWN:  u32 = 4;
pub const KIND_MOUSE_UP:    u32 = 5;
pub const KIND_MOUSE_WHEEL: u32 = 6;

const QUEUE_CAP: usize = 256;
static QUEUE: Once<ArrayQueue<InputEventAbi>> = Once::new();

fn queue() -> &'static ArrayQueue<InputEventAbi> {
    QUEUE.call_once(|| ArrayQueue::new(QUEUE_CAP))
}

/// Push un event. Si la queue est pleine, on **drop l'ancien** (force) — la
/// politique « lose oldest » est saine pour des inputs : on préfère perdre un
/// scroll qu'un click récent.
pub fn push_event(evt: InputEventAbi) {
    let q = queue();
    if q.push(evt).is_err() {
        let _ = q.pop();   // libère un slot
        let _ = q.push(evt);
    }
}

pub fn pop_event() -> Option<InputEventAbi> {
    queue().pop()
}

pub fn len() -> usize {
    queue().len()
}
