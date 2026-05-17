// =============================================================================
// fb_info — extraction des paramètres du framebuffer kernel pour FB_ACQUIRE.
//
// Layout : doit matcher *exactement* abi::fb::FbInfo (32 bytes, repr(C)).
// =============================================================================

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct FbInfoAbi {
    pub buffer_ptr: u64,
    pub buffer_len: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub format: u32,
    pub caps: u32,
    pub _reserved: u32,
}

const _: () = assert!(core::mem::size_of::<FbInfoAbi>() == 40,
    "FbInfoAbi doit faire 40 bytes (8+8+4+4+4+4+4+4)");

pub const PIXEL_FORMAT_BGRA8888: u32 = 0;
pub const CAP_DOUBLE_BUF: u32 = 1 << 3;

pub fn query_kernel_fb() -> Option<FbInfoAbi> {
    let fb_mx = crate::drivers::fb::fb()?;
    let fb = fb_mx.lock();
    Some(FbInfoAbi {
        // Step 1 : pas encore de mmap user — buffer_ptr=0, buffer_len=0.
        // Le display-server appellera ensuite SHM_CREATE/SHM_MAP pour son
        // propre backbuffer et présentera via FB_PRESENT.
        buffer_ptr: 0,
        buffer_len: 0,
        width:  fb.width(),
        height: fb.height(),
        pitch:  fb.pitch_bytes(),
        format: PIXEL_FORMAT_BGRA8888,
        caps:   CAP_DOUBLE_BUF,
        _reserved: 0,
    })
}
