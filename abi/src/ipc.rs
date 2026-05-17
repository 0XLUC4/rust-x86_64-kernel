// =============================================================================
// ipc — messages inter-process (IPC_SEND / IPC_RECV).
//
// Modèle :
//   * Chaque process a une mailbox kernel-side : VecDeque<IpcMessage>.
//   * IPC_SEND copie le payload depuis le caller vers la mailbox du target.
//   * IPC_RECV est bloquant : le caller s'endort jusqu'à message dispo.
//   * Limite par message : 4 KiB. Au-delà, le pattern recommandé est
//     "passer un ShmHandle via le payload + lecture côté receiver".
//
// Format wire :
//   [ IpcHeader (32 B) ][ payload (up to MAX_PAYLOAD) ]
//
// Le payload est libre de format : chaque protocole user (display protocol,
// notification protocol...) le réinterprète à sa guise.
// =============================================================================

pub const MAX_PAYLOAD: usize = 4096 - core::mem::size_of::<IpcHeader>();

/// Type de message — discriminant pour multiplexer plusieurs protocoles
/// au-dessus du même canal IPC. Conventions :
///   * 0..0x1000 : réservé système
///   * 0x1000..0x2000 : display-server protocol
///   * 0x2000..0x3000 : notifications
///   * >= 0x8000 : libre user
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct IpcHeader {
    /// Sender PID (rempli par le kernel, ignoré côté SEND).
    pub sender: u64,
    /// Type/protocol id (cf. plages ci-dessus).
    pub kind: u32,
    /// Taille effective du payload qui suit ce header.
    pub payload_len: u32,
    /// Cookie libre (corrélation request/reply, rempli par le sender).
    pub cookie: u64,
    /// Réservé (alignement + extension).
    pub _reserved: u64,
}

const _: () = assert!(core::mem::size_of::<IpcHeader>() == 32,
    "IpcHeader doit faire exactement 32 bytes — ABI gravée");
