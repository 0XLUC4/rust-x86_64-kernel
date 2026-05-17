// =============================================================================
// virtio::gpu — driver 2D accéléré pour virtio-gpu (QEMU -vga virtio).
//
// Pipeline :
//   1. Probe : PCI vendor 0x1af4, device 0x1050 → VirtioTransport
//   2. Init : reset, features, setup queue 0 (controlq), driver_ok
//   3. GET_DISPLAY_INFO → obtient width/height scanout 0
//   4. RESOURCE_CREATE_2D (id=1, R8G8B8A8_UNORM, WxH)
//   5. RESOURCE_ATTACH_BACKING : fournit nos pages de framebuffer RAM
//   6. SET_SCANOUT 0 → attache resource 1 comme sortie
//
// Pour chaque frame :
//   7. TRANSFER_TO_HOST_2D (copie RAM → GPU)
//   8. RESOURCE_FLUSH (affiche)
//
// C'est équivalent au "present" MMIO, mais la copie RAM→GPU est gérée par
// le device côté QEMU : pas de write MMIO uncached/write-combine, le
// transport passe par des DMA vers la zone device-side qui peut être
// accélérée par le GPU hôte (OpenGL/Vulkan ANGLE).
// =============================================================================

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem;
use spin::{Mutex, Once};

use crate::virtio::{
    VirtioTransport, queue::VirtQueue,
    VIRTIO_VENDOR_ID, VIRTIO_DEVICE_ID_GPU, VIRTIO_DEVICE_ID_GPU_LEGACY,
};

// --- Commandes virtio-gpu (spec virtio-gpu v1.0 §5.7) ---
const VIRTIO_GPU_CMD_GET_DISPLAY_INFO:   u32 = 0x0100;
const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const VIRTIO_GPU_CMD_RESOURCE_UNREF:     u32 = 0x0102;
const VIRTIO_GPU_CMD_SET_SCANOUT:        u32 = 0x0103;
const VIRTIO_GPU_CMD_RESOURCE_FLUSH:     u32 = 0x0104;
const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D:u32 = 0x0105;
const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;

const VIRTIO_GPU_RESP_OK_NODATA:         u32 = 0x1100;
const VIRTIO_GPU_RESP_OK_DISPLAY_INFO:   u32 = 0x1101;

// Format pixel : R8G8B8A8_UNORM correspond à notre backbuffer BGRA little-endian
// (en fait B8G8R8A8 sur x86 LE). virtio-gpu utilise VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM = 2
// ou R8G8B8A8 = 67. On pick B8G8R8X8 car c'est ce que notre framebuffer écrit.
const VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: u32 = 2;

const RESOURCE_ID: u32 = 1;
const SCANOUT_ID: u32 = 0;

// --- Structures de commande (toutes packed via #[repr(C)]) ---
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CtrlHdr {
    cmd_type: u32,
    flags:    u32,
    fence_id: u64,
    ctx_id:   u32,
    padding:  u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct GpuRect {
    x: u32, y: u32, width: u32, height: u32,
}

#[repr(C)]
struct ResourceCreate2d {
    hdr:      CtrlHdr,
    resource_id: u32,
    format:   u32,
    width:    u32,
    height:   u32,
}

#[repr(C)]
struct ResourceAttachBacking {
    hdr: CtrlHdr,
    resource_id: u32,
    nr_entries: u32,
    // Suivi par nr_entries × MemEntry
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct MemEntry {
    addr: u64,
    length: u32,
    padding: u32,
}

#[repr(C)]
struct SetScanout {
    hdr: CtrlHdr,
    r: GpuRect,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
struct TransferToHost2d {
    hdr: CtrlHdr,
    r: GpuRect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
struct ResourceFlush {
    hdr: CtrlHdr,
    r: GpuRect,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayOne {
    r: GpuRect,
    enabled: u32,
    flags: u32,
}

#[repr(C)]
struct RespDisplayInfo {
    hdr: CtrlHdr,
    pmodes: [DisplayOne; 16],
}

// -----------------------------------------------------------------------------
// Driver
// -----------------------------------------------------------------------------

pub struct VirtioGpu {
    pub transport: VirtioTransport,
    pub queue: VirtQueue,
    pub width: u32,
    pub height: u32,
    /// Framebuffer logique attaché à la resource GPU.
    /// Les paint() du compositor écrivent dedans ; commit() → transfer+flush.
    pub fb: Box<[u32]>,
}

impl VirtioGpu {
    /// Probe le bus PCI, initialise le device s'il existe.
    pub fn probe() -> Result<Self, &'static str> {
        let pci_dev = crate::virtio::detect_virtio_devices().into_iter()
            .find(|d| d.device_id == VIRTIO_DEVICE_ID_GPU
                   || d.device_id == VIRTIO_DEVICE_ID_GPU_LEGACY)
            .ok_or("virtio-gpu: device PCI non trouvé")?;

        crate::serial_println!("[virtio-gpu] trouvé PCI {:02x}:{:02x}.{} vendor={:#x} device={:#x}",
            pci_dev.addr.bus, pci_dev.addr.dev, pci_dev.addr.func,
            pci_dev.vendor_id, pci_dev.device_id);

        let transport = VirtioTransport::probe(pci_dev)?;
        transport.init_base()?;

        // Setup queue 0 (controlq) pour les commandes 2D.
        let mut queue = VirtQueue::new();
        queue.notify_off = transport.setup_queue(
            0, crate::virtio::queue::QUEUE_SIZE as u16,
            queue.desc_phys(), queue.avail_phys(), queue.used_phys(),
        );

        transport.driver_ok();
        crate::serial_println!("[virtio-gpu] transport init OK, queue 0 prête");

        let mut gpu = VirtioGpu {
            transport,
            queue,
            width: 0,
            height: 0,
            fb: Box::new([]),
        };

        // Récupère la géométrie scanout 0.
        let (w, h) = gpu.get_display_info()?;
        crate::serial_println!("[virtio-gpu] scanout 0 : {}x{}", w, h);
        gpu.width = w;
        gpu.height = h;

        // Alloue le framebuffer aligné sur une page.
        let size = (w * h) as usize;
        let fb_vec: Vec<u32> = alloc::vec![0u32; size];
        gpu.fb = fb_vec.into_boxed_slice();

        // Crée resource + attache backing.
        gpu.create_resource_2d()?;
        gpu.attach_backing()?;
        gpu.set_scanout()?;

        crate::serial_println!("[virtio-gpu] pipeline 2D prêt");
        Ok(gpu)
    }

    fn submit<T>(&mut self, cmd: &T, expected_resp: u32) -> Result<(), &'static str>
    where T: Sized
    {
        let req = unsafe {
            core::slice::from_raw_parts(
                cmd as *const T as *const u8,
                mem::size_of::<T>(),
            )
        };
        let mut resp = [0u8; 256];
        let len = self.queue.submit_request(&self.transport, req, &mut resp)? as usize;
        if len < mem::size_of::<CtrlHdr>() {
            return Err("virtio-gpu: réponse trop courte");
        }
        let hdr: &CtrlHdr = unsafe { &*(resp.as_ptr() as *const CtrlHdr) };
        if hdr.cmd_type != expected_resp {
            crate::serial_println!("[virtio-gpu] cmd response inattendue: {:#x} (attendu {:#x})",
                hdr.cmd_type, expected_resp);
            return Err("virtio-gpu: réponse inattendue");
        }
        Ok(())
    }

    fn get_display_info(&mut self) -> Result<(u32, u32), &'static str> {
        let req = CtrlHdr {
            cmd_type: VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
            ..Default::default()
        };
        let req_bytes = unsafe {
            core::slice::from_raw_parts(
                &req as *const _ as *const u8,
                mem::size_of::<CtrlHdr>(),
            )
        };
        let mut resp_buf = [0u8; mem::size_of::<RespDisplayInfo>()];
        let _len = self.queue.submit_request(&self.transport, req_bytes, &mut resp_buf)?;
        let resp: &RespDisplayInfo = unsafe { &*(resp_buf.as_ptr() as *const RespDisplayInfo) };
        if resp.hdr.cmd_type != VIRTIO_GPU_RESP_OK_DISPLAY_INFO {
            return Err("virtio-gpu: réponse GET_DISPLAY_INFO inattendue");
        }
        let p = &resp.pmodes[0];
        if p.enabled == 0 || p.r.width == 0 || p.r.height == 0 {
            return Err("virtio-gpu: scanout 0 désactivé");
        }
        Ok((p.r.width, p.r.height))
    }

    fn create_resource_2d(&mut self) -> Result<(), &'static str> {
        let cmd = ResourceCreate2d {
            hdr: CtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                ..Default::default()
            },
            resource_id: RESOURCE_ID,
            format: VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
            width: self.width,
            height: self.height,
        };
        self.submit(&cmd, VIRTIO_GPU_RESP_OK_NODATA)
    }

    fn attach_backing(&mut self) -> Result<(), &'static str> {
        // Pour simplifier : on passe la totalité du framebuffer comme une
        // seule entrée mémoire contiguë. Comme le kernel utilise un heap
        // linéaire bump-like et que identity map couvre < 1 GiB, l'allocation
        // du Box<[u32]> au heap est déjà physiquement contiguë tant qu'elle
        // tient dans un seul chunk. À l'échelle 1024×768×4 = 3 Mo, c'est OK.
        #[repr(C)]
        struct Full {
            hdr: ResourceAttachBacking,
            entry: MemEntry,
        }
        let cmd = Full {
            hdr: ResourceAttachBacking {
                hdr: CtrlHdr {
                    cmd_type: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                    ..Default::default()
                },
                resource_id: RESOURCE_ID,
                nr_entries: 1,
            },
            entry: MemEntry {
                addr: self.fb.as_ptr() as u64,
                length: (self.fb.len() * mem::size_of::<u32>()) as u32,
                padding: 0,
            },
        };
        self.submit(&cmd, VIRTIO_GPU_RESP_OK_NODATA)
    }

    fn set_scanout(&mut self) -> Result<(), &'static str> {
        let cmd = SetScanout {
            hdr: CtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_SET_SCANOUT,
                ..Default::default()
            },
            r: GpuRect { x: 0, y: 0, width: self.width, height: self.height },
            scanout_id: SCANOUT_ID,
            resource_id: RESOURCE_ID,
        };
        self.submit(&cmd, VIRTIO_GPU_RESP_OK_NODATA)
    }

    /// Transfert la zone `(x, y, w, h)` du framebuffer RAM vers la resource GPU,
    /// puis flush pour affichage.
    pub fn present_region(&mut self, x: u32, y: u32, w: u32, h: u32) -> Result<(), &'static str> {
        if w == 0 || h == 0 { return Ok(()); }
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let w = w.min(self.width - x);
        let h = h.min(self.height - y);

        // offset dans le backing = (y * width + x) * 4
        let offset = ((y * self.width + x) as u64) * 4;

        let xfer = TransferToHost2d {
            hdr: CtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
                ..Default::default()
            },
            r: GpuRect { x, y, width: w, height: h },
            offset,
            resource_id: RESOURCE_ID,
            padding: 0,
        };
        self.submit(&xfer, VIRTIO_GPU_RESP_OK_NODATA)?;

        let flush = ResourceFlush {
            hdr: CtrlHdr {
                cmd_type: VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                ..Default::default()
            },
            r: GpuRect { x, y, width: w, height: h },
            resource_id: RESOURCE_ID,
            padding: 0,
        };
        self.submit(&flush, VIRTIO_GPU_RESP_OK_NODATA)?;
        Ok(())
    }
}

pub static GPU: Once<Mutex<VirtioGpu>> = Once::new();

/// Tente d'initialiser le driver. Ne fail pas si virtio-gpu est absent —
/// l'appelant doit simplement fallback sur le chemin MMIO classique.
pub fn init() -> Result<(), &'static str> {
    let gpu = VirtioGpu::probe()?;
    GPU.call_once(|| Mutex::new(gpu));
    Ok(())
}

pub fn present_full() -> Result<(), &'static str> {
    let g = GPU.get().ok_or("virtio-gpu: non initialisé")?;
    let mut gpu = g.lock();
    let (w, h) = (gpu.width, gpu.height);
    gpu.present_region(0, 0, w, h)
}
