// =============================================================================
// shm — handles de mémoire partagée entre processes.
//
// Modèle :
//   * shm_create(size) → ShmHandle (u64). Le kernel alloue N frames physiques
//     contiguës (au sens VMM, pas obligatoirement physiques contiguës) et
//     associe le handle à la liste de frames.
//   * shm_map(handle, mode) → ptr user. Le kernel mappe les frames dans
//     l'espace virtuel du caller à un VA libre choisi par le kernel.
//   * shm_unmap(ptr) libère les mappings sans détruire les frames.
//   * Le handle est retourné en passant via IPC : un client envoie son
//     ShmHandle au display-server, qui le map à son tour et lit/écrit les
//     pixels du surface buffer.
//
// Sécurité : le kernel maintient un compteur de refs par handle ; le handle
// reste valide tant qu'au moins un process le détient ou l'a mappé.
// =============================================================================

/// Handle opaque. 0 et u64::MAX sont des sentinelles (invalide / erreur).
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ShmHandle(pub u64);

impl ShmHandle {
    pub const INVALID: ShmHandle = ShmHandle(0);
    pub fn is_valid(self) -> bool { self.0 != 0 && self.0 != u64::MAX }
}

/// Mode de map (bitmask).
pub const SHM_READ:  u64 = 1 << 0;
pub const SHM_WRITE: u64 = 1 << 1;
pub const SHM_RW:    u64 = SHM_READ | SHM_WRITE;
