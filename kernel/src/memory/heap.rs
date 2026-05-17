// =============================================================================
// heap.rs — allocateur global pour le kernel.
//
// Stratégie simplifiée : on utilise une région FIXE de 100 KiB dans les
// premiers 1 GiB que `boot.asm` a déjà identity-mappé en huge pages 2 MiB.
// Pas besoin de frame allocator ni de page table manipulation — parfait
// pour démarrer.
//
// Upgrade naturel ensuite : parser la memory map multiboot2, construire
// un frame allocator, activer des pages à la demande (map_to). Laissé
// comme exercice (ou v2 du kernel).
// =============================================================================

use linked_list_allocator::LockedHeap;

/// Base du heap : on prend une zone inutilisée dans les premiers 1 GiB
/// identity-mappés. 0x_4444_4444_0000 serait hors map — on reste sous 1 GiB.
///
/// Ici on prend 16 MiB (0x0100_0000), bien au-dessus du kernel lui-même
/// (chargé à 1 MiB, taille < quelques MiB).
pub const HEAP_START: usize = 0x0100_0000;

/// 32 MiB — nécessaire pour un backbuffer 1920x1080 (~8 MiB) + réseau + FS.
pub const HEAP_SIZE: usize = 32 * 1024 * 1024;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Initialise le heap. Doit être appelé une seule fois, tôt dans `_start`,
/// avant tout usage de `Box`/`Vec`/`String`.
pub fn init_heap() -> Result<(), &'static str> {
    // SAFETY: la zone [HEAP_START, HEAP_START+HEAP_SIZE) est :
    //   - identity-mappée par boot.asm (dans les 1ers 1 GiB)
    //   - non utilisée par le kernel (chargé autour de 1 MiB, << 16 MiB)
    //   - non utilisée par aucune structure statique
    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }
    Ok(())
}
