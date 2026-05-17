// =============================================================================
// ipc — mémoire partagée + mailboxes inter-process.
//
// Trois pièces :
//   * shm : table de handles → liste de frames physiques (Vec<u64>).
//           shm_create alloue, shm_map mappe dans le caller, shm_unmap libère.
//   * mailbox : par-process VecDeque de IpcFrame (header+payload).
//   * waiters : si un process appelle ipc_recv sur mailbox vide, il dort
//               jusqu'à ce qu'un sender le réveille.
//
// Note Phase V step 1 : les wakers/sleep cross-process sont stub (busy-yield).
// Le bon pattern arrivera avec un futex-like, mais l'ABI est déjà gravée.
// =============================================================================

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use spin::Mutex;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::task::process::{self, Pid};

// -----------------------------------------------------------------------------
// SHM
// -----------------------------------------------------------------------------

const PAGE_SIZE: u64 = 4096;
const SHM_MAX_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB par handle, garde-fou.

struct ShmRegion {
    /// Frames physiques (PA) qui composent cette région.
    frames: Vec<u64>,
    size:   u64,
    /// Process créateur (information seulement — n'impose pas de droits).
    creator: Pid,
    /// Refcount : combien de process l'ont mappé.
    refs: u32,
}

static SHM_TABLE: Mutex<BTreeMap<u64, ShmRegion>> = Mutex::new(BTreeMap::new());
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub fn sys_shm_create(size: u64) -> u64 {
    if size == 0 || size > SHM_MAX_SIZE { return u64::MAX; }
    let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;

    // Allocation des frames physiques via l'API paging.
    let mut frames: Vec<u64> = Vec::with_capacity(pages as usize);
    for _ in 0..pages {
        match crate::memory::paging::alloc_frame() {
            Ok(pf) => frames.push(pf.start_address().as_u64()),
            Err(_) => {
                // Rollback : on relâche les frames déjà prises.
                for &pa in &frames {
                    if let Some(pf) = x86_64::structures::paging::PhysFrame::from_start_address(
                        x86_64::PhysAddr::new(pa)
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

pub fn sys_shm_map(handle: u64, _mode: u64) -> u64 {
    // Phase V step 1 : on ne fait pas encore de mapping page-table user.
    // On retourne 0 (= pas-encore-implémenté) avec une trace claire pour
    // que les tests userland sachent que c'est attendu.
    let mut table = SHM_TABLE.lock();
    let region = match table.get_mut(&handle) {
        Some(r) => r, None => return 0,
    };
    region.refs += 1;
    crate::serial_println!(
        "[ipc] shm_map handle={} size={} pages={} (mapping non implémenté en step 1)",
        handle, region.size, region.frames.len(),
    );
    0
}

pub fn sys_shm_unmap(_ptr: u64) -> u64 {
    // Step 1 : no-op ; le release viendra avec le map réel.
    0
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

    // Boucle de wait coopératif (Phase V step 1).
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
        crate::task::thread::yield_now();
    }
}

/// Hook appelé quand un process meurt : nettoie sa mailbox et décrémente
/// les refs SHM (Phase V step 2 — pour l'instant on vide la mailbox).
pub fn cleanup_pid(pid: Pid) {
    MAILBOXES.lock().remove(&pid);
    // SHM cleanup : pour l'instant on ne libère pas (refcount manuel).
    // À implémenter quand sys_shm_map fera le vrai mapping page-table.
}
