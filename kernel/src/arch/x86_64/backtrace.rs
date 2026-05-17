// =============================================================================
// backtrace.rs — parcours de la stack pour afficher les adresses de retour.
//
// Fonctionne UNIQUEMENT si le code est compilé avec frame pointers (-Cforce-frame-pointers).
// En debug c'est généralement le cas par défaut. En release, il faut
// forcer l'option (voir Cargo.toml).
//
// Layout d'une stack avec frame pointers :
//
//   haute mémoire
//   ┌──────────────────┐
//   │ return addr N    │ ← [rbp+8]
//   ├──────────────────┤
//   │ saved rbp N      │ ← rbp pointe ici
//   ├──────────────────┤
//   │ locals de N      │
//   │      ...         │
//   │ return addr N+1  │ ← [nouveau rbp+8]
//   ├──────────────────┤
//   │ saved rbp N+1    │ ← [rbp]=ancien rbp → nouveau rbp
//   └──────────────────┘
//   basse mémoire
//
// On suit la chaîne des `saved rbp` jusqu'à 0 (ou jusqu'à une adresse bogus).
// =============================================================================

use crate::println;

/// Affiche un backtrace à partir de la frame courante.
#[inline(never)]
pub fn print() {
    let mut rbp: usize;
    // SAFETY: lit simplement RBP
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack)); }
    walk(rbp, 16);
}

/// Affiche un backtrace à partir d'un RBP donné (utile depuis un handler
/// d'exception où on a un stack frame fourni par le CPU).
pub fn walk(mut rbp: usize, max_depth: usize) {
    println!("--- backtrace ---");
    for i in 0..max_depth {
        if rbp == 0 || rbp < 0x1000 {
            break;
        }
        // SAFETY: on essaie de lire [rbp] et [rbp+8] qui DOIT être dans une stack
        // kernel valide. Si c'est du garbage on lira quelque chose de random, mais
        // un triple fault est très improbable tant qu'on reste identity-mappé.
        let return_addr = unsafe { *((rbp + 8) as *const usize) };
        let saved_rbp = unsafe { *(rbp as *const usize) };
        if return_addr == 0 { break; }
        println!("  #{:<2} {:#018x}", i, return_addr);
        // Garde-fou : si la chaîne ne décroît pas, on sort (corruption)
        if saved_rbp <= rbp { break; }
        rbp = saved_rbp;
    }
    println!("-----------------");
}
