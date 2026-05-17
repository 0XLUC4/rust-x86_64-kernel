// =============================================================================
// gfx — protocole display-server (au-dessus de IPC, pas un syscall).
//
// Le display-server est un process user normal. Il parle ce protocole avec
// ses clients (apps, panel, launcher...). Tous les messages sont envoyés
// via IPC_SEND avec `IpcHeader.kind` dans la plage 0x1000..0x2000.
//
// Convention :
//   * Requests   : client → server (IDs 0x1000..0x10FF)
//   * Replies    : server → client (IDs 0x1100..0x11FF)
//   * Events     : server → client (IDs 0x1200..0x12FF)
//
// Le payload après l'IpcHeader est la struct correspondante en repr(C).
// =============================================================================

// ---- IDs de message ----
pub const KIND_REQ_SURFACE_CREATE:  u32 = 0x1000;
pub const KIND_REQ_SURFACE_DESTROY: u32 = 0x1001;
pub const KIND_REQ_SURFACE_ATTACH:  u32 = 0x1002;
pub const KIND_REQ_SURFACE_COMMIT:  u32 = 0x1003;
pub const KIND_REQ_SURFACE_TITLE:   u32 = 0x1004;
pub const KIND_REQ_FRAME_CALLBACK:  u32 = 0x1005;

pub const KIND_REP_SURFACE_CREATED: u32 = 0x1100;
pub const KIND_REP_ERROR:           u32 = 0x11FF;

pub const KIND_EVT_INPUT:           u32 = 0x1200;
pub const KIND_EVT_RESIZE:          u32 = 0x1201;
pub const KIND_EVT_FOCUS:           u32 = 0x1202;
pub const KIND_EVT_CLOSED:          u32 = 0x1203;
pub const KIND_EVT_FRAME_DONE:      u32 = 0x1204;

/// Identifiant local-au-server d'une surface. Opaque côté client.
pub type SurfaceId = u64;

// ---- Requests ----

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ReqSurfaceCreate {
    pub w: u32,
    pub h: u32,
    /// PixelFormat (cf. fb::PixelFormat).
    pub format: u32,
    pub flags: u32,
}

pub const SURFACE_FLAG_OPAQUE:     u32 = 1 << 0;
pub const SURFACE_FLAG_DECORATED:  u32 = 1 << 1;
pub const SURFACE_FLAG_FULLSCREEN: u32 = 1 << 2;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ReqSurfaceDestroy {
    pub surface: SurfaceId,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ReqSurfaceAttach {
    pub surface: SurfaceId,
    /// Handle SHM côté client (le server fera shm_map de son côté).
    pub shm_handle: u64,
    pub offset: u64,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ReqSurfaceCommit {
    pub surface: SurfaceId,
    /// Damage region. (0,0,0,0) = "tout" (full surface).
    pub damage_x: u32,
    pub damage_y: u32,
    pub damage_w: u32,
    pub damage_h: u32,
}

// ---- Replies ----

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct RepSurfaceCreated {
    pub surface: SurfaceId,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct RepError {
    /// errno-style (cf. abi::errno).
    pub code: i32,
    pub _pad: u32,
}

// ---- Events ----

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct EvtInput {
    pub surface: SurfaceId,
    /// Event brut tel que reçu du kernel (déjà filtré pour cette surface).
    pub event: super::input::InputEvent,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct EvtResize {
    pub surface: SurfaceId,
    pub new_w: u32,
    pub new_h: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct EvtFocus {
    pub surface: SurfaceId,
    pub focused: u32, // bool packé
    pub _pad: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct EvtClosed {
    pub surface: SurfaceId,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct EvtFrameDone {
    pub surface: SurfaceId,
    pub timestamp_ms: u64,
}
