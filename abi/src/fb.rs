// =============================================================================
// fb — types ABI du framebuffer scanout (FB_ACQUIRE / FB_PRESENT).
//
// Modèle :
//   * Le kernel possède l'unique scanout MMIO. Le display-server s'octroie
//     un handle exclusif via FB_ACQUIRE (refusé pour tout autre process).
//   * FB_ACQUIRE retourne un FbInfo + la zone shm-mappée (le backbuffer
//     accessible en RAM par le display-server).
//   * FB_PRESENT(rect) demande au kernel de copier la dirty rect du
//     backbuffer mappé vers la scanout (ou TRANSFER+FLUSH virtio-gpu).
// =============================================================================

/// Format pixel. Stocké en u32 pour stabilité ABI.
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 0xAARRGGBB en little-endian = octets {B,G,R,A}.
    Bgra8888 = 0,
    /// 0xRRGGBB packé sur 4 bytes (alpha ignoré).
    Xrgb8888 = 1,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Sortie de FB_ACQUIRE. Le pointeur est valide dans l'espace virtuel
/// du process qui a appelé acquire (le display-server).
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct FbInfo {
    /// Pointeur user vers le backbuffer mappé (RW). 0 si erreur.
    pub buffer_ptr: u64,
    /// Taille totale du backbuffer en bytes.
    pub buffer_len: u64,
    pub width: u32,
    pub height: u32,
    /// Stride en bytes (≥ width * bpp/8, peut être plus grand).
    pub pitch: u32,
    pub format: u32,
    /// Capacités hardware (bitmask CAP_*).
    pub caps: u32,
    /// Réservé (alignement + extension future).
    pub _reserved: u32,
}

// Capabilities du backend de présentation.
pub const CAP_VSYNC:      u32 = 1 << 0;
pub const CAP_DAMAGE:     u32 = 1 << 1;  // honore Rect partiel
pub const CAP_VIRTIO_GPU: u32 = 1 << 2;
pub const CAP_DOUBLE_BUF: u32 = 1 << 3;
