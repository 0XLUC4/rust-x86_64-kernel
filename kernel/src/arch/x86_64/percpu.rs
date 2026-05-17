// =============================================================================
// percpu.rs — structure "Per-CPU" référencée via GS_BASE.
//
// À chaque entrée kernel (syscall, interrupt depuis ring 3), on fait `swapgs`
// pour que GS_BASE pointe sur cette structure. Le stub asm utilise
// `[gs:0]` et `[gs:8]` pour lire/écrire user_rsp / kernel_rsp.
//
// Layout FIGÉ (lu par boot/syscall_entry.asm) :
//   offset 0  : user_rsp         u64
//   offset 8  : kernel_rsp       u64
//   offset 16 : current_process  *mut Process (stable ABI avec Rust)
//   offset 24 : syscall_count    u64
//
// Mono-core pour l'instant : un seul PerCpu statique, chargé via MSR
// KERNEL_GS_BASE (swappé par swapgs) — cf init().
// =============================================================================

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::model_specific::{KernelGsBase, GsBase};
use x86_64::VirtAddr;

#[repr(C)]
pub struct PerCpu {
    pub user_rsp: u64,
    pub kernel_rsp: u64,
    pub current_process: *mut (),
    pub syscall_count: AtomicU64,
    pub cpu_id: u32,
    _pad: u32,
}

// SAFETY: un seul CPU actif à la fois écrit sur PER_CPU. Les accès concurrents
// sont sérialisés via swapgs (contexte kernel exclusif par core).
unsafe impl Sync for PerCpu {}

static mut PER_CPU: PerCpu = PerCpu {
    user_rsp: 0,
    kernel_rsp: 0,
    current_process: core::ptr::null_mut(),
    syscall_count: AtomicU64::new(0),
    cpu_id: 0,
    _pad: 0,
};

/// Initialise KernelGsBase et GsBase pour pointer sur PER_CPU.
/// SAFETY: à appeler une seule fois au boot, après que la heap/paging soient OK.
pub unsafe fn init() {
    let addr = VirtAddr::new(core::ptr::addr_of!(PER_CPU) as u64);
    // Côté kernel courant, GS_BASE pointe sur PER_CPU.
    GsBase::write(addr);
    // Après swapgs en ring 3, le CPU swap GS<->KernelGS : on met la même
    // valeur dans KERNEL_GS_BASE pour qu'au premier syscall user→kernel,
    // swapgs réinstalle la bonne base.
    KernelGsBase::write(addr);
    crate::println!("[per-cpu] GS_BASE = {:#x}", addr.as_u64());
}

/// Installe la stack kernel du thread courant dans PerCpu ET TSS.rsp0.
/// Appelé par le scheduler à chaque context switch.
pub fn set_kernel_stack(rsp: VirtAddr) {
    // SAFETY: écriture atomique d'un u64 aligné, swapgs garantit mutex par core.
    unsafe {
        PER_CPU.kernel_rsp = rsp.as_u64();
    }
    crate::arch::x86_64::gdt::set_kernel_stack(rsp);
}

/// Retourne le pointeur vers le process courant (Process*).
pub fn current_process() -> *mut () {
    // SAFETY: simple lecture d'un pointeur aligné.
    unsafe { PER_CPU.current_process }
}

pub fn set_current_process(p: *mut ()) {
    // SAFETY: écriture d'un pointeur aligné, mutex par core via swapgs.
    unsafe { PER_CPU.current_process = p; }
}

pub fn bump_syscall_count() -> u64 {
    // SAFETY: atomic.
    unsafe { PER_CPU.syscall_count.fetch_add(1, Ordering::Relaxed) + 1 }
}

pub fn syscall_count() -> u64 {
    // SAFETY: atomic.
    unsafe { PER_CPU.syscall_count.load(Ordering::Relaxed) }
}
