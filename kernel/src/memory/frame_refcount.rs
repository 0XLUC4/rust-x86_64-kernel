// =============================================================================
// frame_refcount.rs — comptage de références par frame physique.
//
// Utilisé par CoW (fork) : quand un parent fork, les deux AddressSpace
// pointent sur les mêmes frames en read-only. Refcount = 2. Au premier
// write, on copie la frame ET on décrémente le refcount de l'originale.
// Quand un process meurt, chaque frame détenue voit son refcount diminuer,
// et on ne libère que celles dont le refcount tombe à 0.
//
// Implémentation : table dense (Vec<AtomicU8>). On borne à 256 refs max par
// frame (suffit largement pour une arbo de fork). Si un refcount non-tracké
// (=0 par défaut) se voit dec'd, on traite comme "pas partagée" → free direct.
// =============================================================================

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::structures::paging::{PhysFrame, Size4KiB};

pub struct FrameRefcount {
    /// Indexé par `frame_index = phys_addr / 4096`.
    counts: Vec<u8>,
}

impl FrameRefcount {
    const fn new() -> Self { FrameRefcount { counts: Vec::new() } }

    fn ensure_index(&mut self, idx: usize) {
        if self.counts.len() <= idx {
            self.counts.resize(idx + 1, 0);
        }
    }

    pub fn set(&mut self, frame: PhysFrame<Size4KiB>, n: u8) {
        let idx = (frame.start_address().as_u64() >> 12) as usize;
        self.ensure_index(idx);
        self.counts[idx] = n;
    }

    /// Incrémente, retourne la nouvelle valeur. Sature à 255.
    pub fn inc(&mut self, frame: PhysFrame<Size4KiB>) -> u8 {
        let idx = (frame.start_address().as_u64() >> 12) as usize;
        self.ensure_index(idx);
        if self.counts[idx] < 255 {
            self.counts[idx] = self.counts[idx].saturating_add(1);
        }
        self.counts[idx]
    }

    /// Décrémente, retourne la nouvelle valeur. Borne à 0.
    pub fn dec(&mut self, frame: PhysFrame<Size4KiB>) -> u8 {
        let idx = (frame.start_address().as_u64() >> 12) as usize;
        if idx >= self.counts.len() { return 0; }
        if self.counts[idx] > 0 {
            self.counts[idx] -= 1;
        }
        self.counts[idx]
    }

    pub fn get(&self, frame: PhysFrame<Size4KiB>) -> u8 {
        let idx = (frame.start_address().as_u64() >> 12) as usize;
        self.counts.get(idx).copied().unwrap_or(0)
    }
}

pub static FRAME_REFCOUNT: Mutex<FrameRefcount> = Mutex::new(FrameRefcount::new());
