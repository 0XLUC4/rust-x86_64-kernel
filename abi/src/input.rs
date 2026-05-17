// =============================================================================
// input — événements bruts kernel → user (INPUT_POLL).
//
// Le kernel agrège les producteurs (PS/2 KB, PS/2 souris, USB HID future) en
// une queue unique d'InputEvent. Seul le display-server appelle INPUT_POLL ;
// il rejoue ensuite vers les surfaces clientes via IPC en y attachant la
// sémantique (focus, hit-test, keysym...).
//
// Le format est volontairement plat et sans union : chaque event porte tous
// les champs ; les champs non-pertinents sont à 0. Coût : 32 bytes/event,
// négligeable face à la fréquence (kHz max).
// =============================================================================

#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// Aucun (sentinelle si le buffer est partiellement rempli).
    None       = 0,
    KeyDown    = 1,
    KeyUp      = 2,
    MouseMove  = 3,
    MouseDown  = 4,
    MouseUp    = 5,
    MouseWheel = 6,
}

/// Bouton souris (bitmask dans `mouse_buttons`).
pub const BTN_LEFT:   u32 = 1 << 0;
pub const BTN_RIGHT:  u32 = 1 << 1;
pub const BTN_MIDDLE: u32 = 1 << 2;

/// Modifieurs clavier (bitmask dans `mods`).
pub const MOD_SHIFT: u32 = 1 << 0;
pub const MOD_CTRL:  u32 = 1 << 1;
pub const MOD_ALT:   u32 = 1 << 2;
pub const MOD_META:  u32 = 1 << 3;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct InputEvent {
    /// Discriminant InputKind (stocké u32 pour stabilité ABI).
    pub kind: u32,
    /// Timestamp depuis boot, ms.
    pub timestamp_ms: u64,

    // -- Clavier --
    /// Scancode brut (PS/2 set 1 ou XT-translated USB HID).
    pub scancode: u32,
    /// Keysym/codepoint Unicode après traduction layout (0 si pas applicable).
    pub keysym: u32,
    pub mods: u32,

    // -- Souris --
    /// Position absolue (curseur global), en pixels.
    pub mouse_x: i32,
    pub mouse_y: i32,
    /// Delta (mouvement relatif depuis dernier event).
    pub mouse_dx: i32,
    pub mouse_dy: i32,
    /// État courant des boutons (bitmask BTN_*).
    pub mouse_buttons: u32,
    /// Wheel : +1 / -1 (sinon 0).
    pub wheel: i32,
}

const _: () = assert!(core::mem::size_of::<InputEvent>() <= 64,
    "InputEvent doit rester compact (cache line)");
