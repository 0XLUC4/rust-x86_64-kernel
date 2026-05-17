// =============================================================================
// ulib::mem — bump allocator user-space + wrappers C-style malloc/free.
//
// Stratégie : un buffer statique de 256 KiB dans BSS, AtomicUsize comme
// curseur. `free` est no-op (bump pur — pas de free réel). C'est suffisant
// pour les outils userland qui font de petites allocations ponctuelles.
//
// Phase ultérieure : remplacer par un `linked_list_allocator` user backé
// par `brk()` syscall quand le kernel l'implémentera vraiment.
// =============================================================================

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_SIZE: usize = 256 * 1024;

#[repr(align(16))]
struct AlignedHeap([u8; HEAP_SIZE]);

static mut HEAP: AlignedHeap = AlignedHeap([0; HEAP_SIZE]);
static OFFSET: AtomicUsize = AtomicUsize::new(0);

pub struct BumpAlloc;

unsafe impl GlobalAlloc for BumpAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(8);
        loop {
            let cur = OFFSET.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let new = aligned + layout.size();
            if new > HEAP_SIZE { return core::ptr::null_mut(); }
            if OFFSET
                .compare_exchange(cur, new, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                let base = core::ptr::addr_of_mut!(HEAP.0) as *mut u8;
                return base.add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // bump : free n'a aucun effet.
    }
}

#[global_allocator]
static ALLOC: BumpAlloc = BumpAlloc;

/// Octets actuellement alloués (informatif).
pub fn used() -> usize { OFFSET.load(Ordering::Relaxed) }
pub fn capacity() -> usize { HEAP_SIZE }

/// Wrapper C-style. `malloc(0)` retourne un pointeur non-null mais inutilisable.
#[no_mangle]
pub extern "C" fn malloc(size: usize) -> *mut u8 {
    if size == 0 { return 1 as *mut u8; }
    let layout = match Layout::from_size_align(size, 16) {
        Ok(l) => l, Err(_) => return core::ptr::null_mut(),
    };
    unsafe { ALLOC.alloc(layout) }
}

#[no_mangle]
pub extern "C" fn free(_ptr: *mut u8) { /* bump : no-op */ }

#[no_mangle]
pub extern "C" fn calloc(nmemb: usize, size: usize) -> *mut u8 {
    let total = match nmemb.checked_mul(size) { Some(v) => v, None => return core::ptr::null_mut() };
    let p = malloc(total);
    if !p.is_null() {
        unsafe { core::ptr::write_bytes(p, 0, total); }
    }
    p
}

#[no_mangle]
pub extern "C" fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if ptr.is_null() { return malloc(new_size); }
    if new_size == 0 { return 1 as *mut u8; }
    // Bump : on ne sait pas la taille originale → on alloue un nouveau et
    // copie au plus la taille demandée. Le block d'origine est perdu.
    let new = malloc(new_size);
    if !new.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(ptr, new, new_size); }
    }
    new
}
