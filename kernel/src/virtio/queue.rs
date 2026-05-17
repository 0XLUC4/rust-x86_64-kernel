// =============================================================================
// virtio::queue — virtqueue split layout (spec v1.0 §2.6).
//
// Structure :
//   - Descriptor table (16 octets × size)
//   - Available ring  : index-based publication par le driver
//   - Used ring       : index-based retour par le device
//
// On utilise l'ABI "packed" legacy mais alignée — suffit pour nos besoins.
// Pour la simplicité on n'utilise pas d'IRQ, on polle le used ring.
// =============================================================================

use alloc::boxed::Box;
use core::ptr;
use core::sync::atomic::{AtomicU16, Ordering};

/// Descripteur virtqueue (spec §2.6.5).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Desc {
    pub addr:  u64,
    pub len:   u32,
    pub flags: u16,
    pub next:  u16,
}

pub const VIRTQ_DESC_F_NEXT:     u16 = 1;
pub const VIRTQ_DESC_F_WRITE:    u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// Available ring : publie des descripteurs vers le device.
#[repr(C)]
pub struct AvailRing {
    pub flags:    u16,
    pub idx:      u16,
    pub ring:     [u16; QUEUE_SIZE],
    pub used_event: u16, // Si VIRTIO_F_EVENT_IDX — non utilisé ici
}

/// Used ring : device signale les descripteurs consommés.
#[repr(C)]
pub struct UsedElem {
    pub id:  u32,  // index du descripteur head rendu
    pub len: u32,  // total bytes écrits dans les descripteurs WRITE
}

#[repr(C)]
pub struct UsedRing {
    pub flags:     u16,
    pub idx:       u16,
    pub ring:      [UsedElem; QUEUE_SIZE],
    pub avail_event: u16,
}

/// Taille fixe de queue. 64 = largement assez pour virtio-gpu en polling.
pub const QUEUE_SIZE: usize = 64;

/// Virtqueue split allouée sur le heap kernel.
///
/// L'identity map kernel garantit que les adresses virtuelles des Box/Vec
/// alloués < 1 GiB sont égales aux adresses physiques. virtio-gpu dans QEMU
/// lit les descripteurs directement en RAM → identity map suffit, pas d'IOMMU.
pub struct VirtQueue {
    pub desc:     Box<[Desc; QUEUE_SIZE]>,
    pub avail:    Box<AvailRing>,
    pub used:     Box<UsedRing>,
    pub next_desc: u16,
    pub last_used: u16,
    pub notify_off: u16,
}

impl VirtQueue {
    pub fn new() -> Self {
        let mut desc = Box::new([Desc::default(); QUEUE_SIZE]);
        // Chaîne initiale pour allocation : chaque desc pointe vers le suivant.
        for i in 0..QUEUE_SIZE - 1 {
            desc[i].next = (i + 1) as u16;
        }
        let avail = Box::new(AvailRing {
            flags: 0, idx: 0,
            ring: [0; QUEUE_SIZE],
            used_event: 0,
        });
        let used = Box::new(UsedRing {
            flags: 0, idx: 0,
            ring: core::array::from_fn(|_| UsedElem { id: 0, len: 0 }),
            avail_event: 0,
        });
        VirtQueue {
            desc, avail, used,
            next_desc: 0,
            last_used: 0,
            notify_off: 0,
        }
    }

    pub fn desc_phys(&self) -> u64 { self.desc.as_ptr() as u64 }
    pub fn avail_phys(&self) -> u64 { self.avail.as_ref() as *const _ as u64 }
    pub fn used_phys(&self) -> u64 { self.used.as_ref() as *const _ as u64 }

    /// Soumet une chaîne de descripteurs (req_bytes en OUT puis resp_bytes en IN).
    /// Bloque en polling sur le used ring jusqu'à la réponse.
    /// Retourne la taille totale écrite par le device.
    pub fn submit_request(&mut self, transport: &super::VirtioTransport,
                          req: &[u8], resp: &mut [u8]) -> Result<u32, &'static str>
    {
        if req.is_empty() || resp.is_empty() {
            return Err("virtqueue: buffers vides");
        }
        // On utilise les descripteurs 0 et 1 en permanence — c'est un MVP
        // mono-threaded sans interruptions. Tant qu'on attend une réponse
        // avant d'en envoyer une autre, c'est sûr.
        let head = 0u16;
        let second = 1u16;

        self.desc[head as usize] = Desc {
            addr: req.as_ptr() as u64,
            len: req.len() as u32,
            flags: VIRTQ_DESC_F_NEXT,
            next: second,
        };
        self.desc[second as usize] = Desc {
            addr: resp.as_mut_ptr() as u64,
            len: resp.len() as u32,
            flags: VIRTQ_DESC_F_WRITE,
            next: 0,
        };

        // Publication dans avail.
        let avail = &mut *self.avail;
        let old_idx = avail.idx;
        let slot = (old_idx as usize) % QUEUE_SIZE;
        avail.ring[slot] = head;
        // Memory barrier avant d'incrémenter idx (le device lit idx pour savoir).
        core::sync::atomic::fence(Ordering::SeqCst);
        let new_idx = old_idx.wrapping_add(1);
        unsafe {
            ptr::write_volatile(&mut avail.idx, new_idx);
        }
        core::sync::atomic::fence(Ordering::SeqCst);

        // Notifie le device.
        transport.notify(self.notify_off);

        // Poll le used ring : on attend que used.idx dépasse last_used.
        let timeout_iters = 50_000_000u64; // ~quelques secondes de spin
        let mut iter: u64 = 0;
        loop {
            let cur_idx = unsafe { ptr::read_volatile(&self.used.idx) };
            if cur_idx != self.last_used { break; }
            iter += 1;
            if iter >= timeout_iters {
                return Err("virtqueue: timeout en attente de la réponse GPU");
            }
            core::hint::spin_loop();
        }
        core::sync::atomic::fence(Ordering::SeqCst);

        // Lit l'élément used.
        let slot = (self.last_used as usize) % QUEUE_SIZE;
        let used_elem = unsafe { ptr::read_volatile(&self.used.ring[slot]) };
        self.last_used = self.last_used.wrapping_add(1);

        Ok(used_elem.len)
    }
}

/// Compteur global pour éviter collision descripteurs entre drivers.
#[allow(dead_code)]
static DESC_COUNTER: AtomicU16 = AtomicU16::new(0);
