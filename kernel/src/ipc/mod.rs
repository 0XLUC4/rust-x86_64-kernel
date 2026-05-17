// =============================================================================
// ipc — mémoire partagée + mailboxes inter-process.
//
// Trois pièces :
//   * shm     : table de handles → liste de frames physiques (Vec<u64>).
//               shm_create alloue, shm_map mappe dans le caller au VA libre
//               suivant, shm_unmap démappe et décrémente la refcount.
//   * mailbox : par-process VecDeque de IpcFrame (header+payload).
//   * waiters : si un process appelle ipc_recv sur mailbox vide, il passe en
//               Sleeping et un sender le wake.
//
// Phase V — step 2 : sys_shm_map fait du *vrai* mapping page-table dans
// l'AddressSpace du caller (frames partagées, refcount). Le VA est alloué
// par un bump per-pid depuis SHM_BASE = 0x0000_6000_0000_0000.
// =============================================================================

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use spin::Mutex;
use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};

use crate::task::process::{self, Pid};

// -----------------------------------------------------------------------------
// SHM
// -----------------------------------------------------------------------------

const PAGE_SIZE: u64 = 4096;
const SHM_MAX_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB par handle, garde-fou.

/// Base d'allocation des VA pour SHM (entre heap user et stack user).
/// Stack user top = 0x7fff_ffff_f000 ; on prend 0x6000_0000_0000.
const SHM_VA_BASE: u64 = 0x0000_6000_0000_0000;

struct ShmRegion {
    /// Frames physiques (PA) qui composent cette région.
    frames: Vec<u64>,
    size:   u64,
    /// Process créateur (information seulement — n'impose pas de droits).
    creator: Pid,
    /// Refcount : combien de mappings vivants pointent dessus.
    refs: u32,
}

/// Un mapping vivant d'un handle dans un process.
struct ShmMapping {
    pid: Pid,
    handle: u64,
    base_va: u64,
    pages: u64,
}

static SHM_TABLE: Mutex<BTreeMap<u64, ShmRegion>> = Mutex::new(BTreeMap::new());
static SHM_MAPPINGS: Mutex<Vec<ShmMapping>> = Mutex::new(Vec::new());
/// Bump par-pid (en pages, multiplié × 4096 pour obtenir l'offset depuis BASE).
static SHM_VA_NEXT: Mutex<BTreeMap<Pid, u64>> = Mutex::new(BTreeMap::new());
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Mode de mapping (lecture seule, lecture/écriture). Le bit 0 = WRITABLE.
pub const SHM_MODE_R:  u64 = 0;
pub const SHM_MODE_RW: u64 = 1;

pub fn sys_shm_create(size: u64) -> u64 {
    if size == 0 || size > SHM_MAX_SIZE { return u64::MAX; }
    let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;

    let mut frames: Vec<u64> = Vec::with_capacity(pages as usize);
    for _ in 0..pages {
        match crate::memory::paging::alloc_zeroed_frame() {
            Ok(pf) => frames.push(pf.start_address().as_u64()),
            Err(_) => {
                // Rollback : on relâche les frames déjà prises.
                for &pa in &frames {
                    if let Some(pf) = PhysFrame::<Size4KiB>::from_start_address(
                        PhysAddr::new(pa)
                    ).ok() {
                        crate::memory::paging::free_frame(pf);
                    }
                }
                return u64::MAX;
            }
        }
    }

    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let creator = process::PROCS.lock().current_pid();
    SHM_TABLE.lock().insert(handle, ShmRegion {
        frames, size, creator, refs: 0,
    });
    handle
}

pub fn sys_shm_map(handle: u64, mode: u64) -> u64 {
    let mut table = SHM_TABLE.lock();
    let region = match table.get_mut(&handle) {
        Some(r) => r, None => return u64::MAX,
    };
    let pages = region.frames.len() as u64;
    let frames_copy: Vec<u64> = region.frames.clone();
    let size = region.size;
    region.refs += 1;
    drop(table);

    // Alloue un VA libre dans le caller en avançant un bump per-pid.
    let pid = process::PROCS.lock().current_pid();
    let base_va = {
        let mut map = SHM_VA_NEXT.lock();
        let next = map.entry(pid).or_insert(0);
        let va = SHM_VA_BASE + *next * PAGE_SIZE;
        *next += pages;
        va
    };

    // Map chaque frame du handle dans l'AS du process courant.
    let flags = if mode & SHM_MODE_RW != 0 {
        PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
    } else {
        PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE
    };

    let mut table = process::PROCS.lock();
    let proc = match table.current() {
        Some(p) => p, None => {
            // rollback refs
            if let Some(r) = SHM_TABLE.lock().get_mut(&handle) { r.refs -= 1; }
            return u64::MAX;
        }
    };

    for i in 0..pages {
        let va = base_va + i * PAGE_SIZE;
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
        let frame_pa = frames_copy[i as usize];
        let frame = match PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(frame_pa)) {
            Ok(f) => f, Err(_) => {
                // rollback partiel: démappe ce qui a déjà été mappé.
                for j in 0..i {
                    let va_j = base_va + j * PAGE_SIZE;
                    let _ = unmap_page(proc, va_j);
                }
                if let Some(r) = SHM_TABLE.lock().get_mut(&handle) { r.refs -= 1; }
                return u64::MAX;
            }
        };
        if let Err(_) = proc.address_space.map_to(page, frame, flags) {
            for j in 0..i {
                let va_j = base_va + j * PAGE_SIZE;
                let _ = unmap_page(proc, va_j);
            }
            if let Some(r) = SHM_TABLE.lock().get_mut(&handle) { r.refs -= 1; }
            return u64::MAX;
        }
    }

    SHM_MAPPINGS.lock().push(ShmMapping {
        pid, handle, base_va, pages,
    });

    crate::serial_println!(
        "[ipc] shm_map handle={} pid={} va={:#x} size={} pages={}",
        handle, pid, base_va, size, pages,
    );
    base_va
}

pub fn sys_shm_unmap(ptr: u64) -> u64 {
    let pid = process::PROCS.lock().current_pid();

    let mapping = {
        let mut maps = SHM_MAPPINGS.lock();
        let idx = match maps.iter().position(|m| m.pid == pid && m.base_va == ptr) {
            Some(i) => i, None => return u64::MAX,
        };
        maps.swap_remove(idx)
    };

    {
        let mut table = process::PROCS.lock();
        if let Some(proc) = table.current() {
            for i in 0..mapping.pages {
                let _ = unmap_page(proc, mapping.base_va + i * PAGE_SIZE);
            }
        }
    }

    // Décrémente la refcount du handle, libère le storage si plus personne.
    let mut shm = SHM_TABLE.lock();
    if let Some(region) = shm.get_mut(&mapping.handle) {
        if region.refs > 0 { region.refs -= 1; }
        if region.refs == 0 {
            // Libère les frames physiques.
            for &pa in &region.frames {
                if let Ok(pf) = PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(pa)) {
                    crate::memory::paging::free_frame(pf);
                }
            }
            shm.remove(&mapping.handle);
        }
    }
    0
}

fn unmap_page(proc: &mut process::Process, va: u64) -> Result<(), &'static str> {
    proc.address_space.unmap_va(va)
}

// -----------------------------------------------------------------------------
// Mailboxes IPC
// -----------------------------------------------------------------------------

const MAX_MSG: usize = 4096;
const MAX_QUEUED_PER_PID: usize = 64;

#[derive(Clone)]
struct IpcFrame {
    sender: Pid,
    bytes: Vec<u8>,
}

static MAILBOXES: Mutex<BTreeMap<Pid, VecDeque<IpcFrame>>> = Mutex::new(BTreeMap::new());

pub fn sys_ipc_send(target_pid: u64, msg_ptr: u64, msg_len: u64) -> u64 {
    if msg_len == 0 || msg_len as usize > MAX_MSG { return u64::MAX; }
    let src = match crate::syscall::syscall_user_slice(msg_ptr, msg_len) {
        Some(s) => s, None => return u64::MAX,
    };
    let sender = process::PROCS.lock().current_pid();
    let target = target_pid as Pid;

    let mut map = MAILBOXES.lock();
    let q = map.entry(target).or_insert_with(VecDeque::new);
    if q.len() >= MAX_QUEUED_PER_PID {
        return u64::MAX; // EAGAIN — backpressure
    }
    q.push_back(IpcFrame { sender, bytes: src.to_vec() });
    0
}

pub fn sys_ipc_recv(buf_ptr: u64, max: u64, out_sender_ptr: u64) -> u64 {
    if max == 0 { return u64::MAX; }
    let me = process::PROCS.lock().current_pid();

    // Non-blocking peek + sleep si vide.
    loop {
        let frame_opt = {
            let mut map = MAILBOXES.lock();
            map.get_mut(&me).and_then(|q| q.pop_front())
        };
        if let Some(frame) = frame_opt {
            let n = frame.bytes.len().min(max as usize);
            let dst = match crate::syscall::syscall_user_slice_mut(buf_ptr, n as u64) {
                Some(s) => s, None => return u64::MAX,
            };
            dst.copy_from_slice(&frame.bytes[..n]);

            if out_sender_ptr != 0 {
                let s_dst = match crate::syscall::syscall_user_slice_mut(out_sender_ptr, 8) {
                    Some(s) => s, None => return u64::MAX,
                };
                s_dst.copy_from_slice(&(frame.sender as u64).to_le_bytes());
            }
            return n as u64;
        }
        // Mailbox vide : on cède la main au scheduler (cooperative wait).
        // Le wake passera implicitement par le timer round-robin → on rescan.
        crate::task::thread::yield_now();
    }
}

/// Hook process::exit : nettoie mailbox + démappe toutes les régions SHM
/// que ce process avait mappées, décrémente les refs, libère les régions
/// orphelines.
pub fn cleanup_pid(pid: Pid) {
    MAILBOXES.lock().remove(&pid);

    // Récupère les mappings de ce pid.
    let owned: Vec<ShmMapping> = {
        let mut maps = SHM_MAPPINGS.lock();
        let mut keep = Vec::new();
        let mut taken = Vec::new();
        for m in maps.drain(..) {
            if m.pid == pid { taken.push(m); } else { keep.push(m); }
        }
        *maps = keep;
        taken
    };

    let mut shm = SHM_TABLE.lock();
    for m in owned {
        if let Some(region) = shm.get_mut(&m.handle) {
            if region.refs > 0 { region.refs -= 1; }
            if region.refs == 0 {
                for &pa in &region.frames {
                    if let Ok(pf) = PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(pa)) {
                        crate::memory::paging::free_frame(pf);
                    }
                }
                shm.remove(&m.handle);
            }
        }
    }

    SHM_VA_NEXT.lock().remove(&pid);
}
