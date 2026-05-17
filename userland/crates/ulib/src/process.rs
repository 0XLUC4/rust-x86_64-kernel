// =============================================================================
// ulib::process — helpers de haut niveau pour fork/exec/wait.
// =============================================================================

use crate::syscall::{fork, exec, wait};

/// Pattern standard "lance un binaire et attends-le".
/// Retourne le pid du child (non le code de sortie — on ne l'expose pas encore).
pub fn fork_exec_wait(path: &str) -> i64 {
    match fork() {
        0 => {
            // Child : exec. Si ça revient, c'est une erreur.
            let _ = exec(path);
            crate::eprintln!("exec('{}') a échoué", path);
            crate::exit(127);
        }
        pid if pid > 0 => {
            wait(pid)
        }
        _ => -1,
    }
}

/// Fork + exec sans attendre. Retourne le pid du child, ou -1.
pub fn spawn(path: &str) -> i64 {
    match fork() {
        0 => {
            let _ = exec(path);
            crate::exit(127);
        }
        pid if pid > 0 => pid,
        _ => -1,
    }
}
