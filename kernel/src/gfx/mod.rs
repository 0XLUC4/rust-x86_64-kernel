// =============================================================================
// gfx — interface kernel ↔ display-server.
//
// Trois syscalls exposés :
//   FB_ACQUIRE — réserve l'unique handle scanout au caller (display-server)
//   FB_PRESENT — flush dirty rect du backbuffer caller-mappé vers la scanout
//   INPUT_POLL — drain la queue events kernel vers un buffer user
//
// Garantie d'exclusivité : un seul process à la fois peut détenir le FB.
// Toute autre tentative renvoie -EBUSY.
//
// Frontière nette : ce module ne sait rien de "fenêtre", "focus", "z-order".
// Il fournit juste un linear framebuffer + une queue d'events bruts, et
// laisse au display-server (user space) toute la sémantique au-dessus.
// =============================================================================

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::task::process::{self, Pid};

mod fb_info;
mod input_queue;

pub use fb_info::FbInfoAbi;
pub use input_queue::{push_event, InputEventAbi};

/// PID du process qui détient actuellement le FB (0 = libre).
static FB_OWNER: AtomicU64 = AtomicU64::new(0);

/// Lock d'arbitrage pour acquire/release atomiques + bookkeeping.
static FB_LOCK: Mutex<()> = Mutex::new(());

// -----------------------------------------------------------------------------
// FB_ACQUIRE
// -----------------------------------------------------------------------------
//
// Sortie : remplit le FbInfoAbi pointé par `out_ptr`. Sur succès retourne 0.
// Sémantique : le caller obtient l'exclusivité du scanout. Le kernel n'expose
// PAS le pointeur MMIO direct ; il expose le backbuffer RAM (le `present_buf`
// du driver fb) mappé dans l'espace virtuel du caller via une fenêtre user.
//
// Stub : pour Phase V step 1, on remplit FbInfoAbi avec width/height/pitch
// du framebuffer kernel mais buffer_ptr=0 (pas encore de mmap user). Le mmap
// arrivera quand on aura sys_shm_map fonctionnel + une frame physique stable
// pour le backbuffer.
pub fn sys_fb_acquire(out_ptr: u64) -> u64 {
    let _g = FB_LOCK.lock();

    let caller = process::PROCS.lock().current_pid() as u64;
    let prev = FB_OWNER.load(Ordering::Acquire);
    if prev != 0 && prev != caller {
        return u64::MAX; // EBUSY — un autre process détient déjà le FB
    }
    FB_OWNER.store(caller, Ordering::Release);

    let info = match fb_info::query_kernel_fb() {
        Some(i) => i,
        None => return u64::MAX,
    };

    // Validation user pointer.
    let dst = match crate::syscall::syscall_user_slice_mut(out_ptr, core::mem::size_of::<FbInfoAbi>() as u64) {
        Some(s) => s,
        None => return u64::MAX,
    };
    // SAFETY: dst.len() == size_of::<FbInfoAbi>(), repr(C), aligné u32 — OK
    // pour un write par memcpy.
    let bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(
            &info as *const _ as *const u8,
            core::mem::size_of::<FbInfoAbi>(),
        )
    };
    dst.copy_from_slice(bytes);
    0
}

// -----------------------------------------------------------------------------
// FB_PRESENT
// -----------------------------------------------------------------------------
//
// Le caller a écrit dans le backbuffer mappé ; il demande la copie vers la
// scanout. Pour Phase V step 1 (pas encore de mmap user), on flushe la
// console kernel : le display-server prendra le relais une fois le mmap fait.
pub fn sys_fb_present(rect_ptr: u64) -> u64 {
    let caller = process::PROCS.lock().current_pid() as u64;
    if FB_OWNER.load(Ordering::Acquire) != caller {
        return u64::MAX; // EPERM
    }

    // Lire la Rect (16 bytes : 4×u32). 0 = full present.
    let _rect: (u32, u32, u32, u32) = if rect_ptr == 0 {
        (0, 0, 0, 0)
    } else {
        let bytes = match crate::syscall::syscall_user_slice(rect_ptr, 16) {
            Some(b) => b, None => return u64::MAX,
        };
        let r = |o: usize| u32::from_le_bytes([bytes[o], bytes[o+1], bytes[o+2], bytes[o+3]]);
        (r(0), r(4), r(8), r(12))
    };

    // Pipeline FB : commit + present.
    if let Some(fb_mx) = crate::drivers::fb::fb() {
        let mut fb = fb_mx.lock();
        fb.commit();
        fb.present();
    }
    0
}

// -----------------------------------------------------------------------------
// INPUT_POLL
// -----------------------------------------------------------------------------
//
// Drain jusqu'à `max` événements vers `buf`. Retourne le nombre effectif lu.
// Non-bloquant : si la queue est vide, retourne 0 immédiatement.
pub fn sys_input_poll(buf_ptr: u64, max: u64) -> u64 {
    let caller = process::PROCS.lock().current_pid() as u64;
    if FB_OWNER.load(Ordering::Acquire) != caller {
        return u64::MAX; // EPERM — réservé au display-server
    }
    let evt_size = core::mem::size_of::<InputEventAbi>() as u64;
    let total = match max.checked_mul(evt_size) {
        Some(t) => t, None => return u64::MAX,
    };
    let dst = match crate::syscall::syscall_user_slice_mut(buf_ptr, total) {
        Some(s) => s, None => return u64::MAX,
    };

    let mut written = 0u64;
    while written < max {
        let Some(evt) = input_queue::pop_event() else { break };
        let off = (written * evt_size) as usize;
        let bytes: &[u8] = unsafe {
            core::slice::from_raw_parts(
                &evt as *const _ as *const u8,
                evt_size as usize,
            )
        };
        dst[off..off + evt_size as usize].copy_from_slice(bytes);
        written += 1;
    }
    written
}

/// Hook process::exit : si le process qui meurt détenait le FB, on libère.
pub fn release_if_owner(pid: Pid) {
    let _g = FB_LOCK.lock();
    if FB_OWNER.load(Ordering::Acquire) == pid as u64 {
        FB_OWNER.store(0, Ordering::Release);
    }
}
