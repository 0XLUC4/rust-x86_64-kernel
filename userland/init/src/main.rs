// =============================================================================
// init — PID 1.
//
// Rôle : spawn un shell user puis le respawn en boucle (comme un vrai init),
// en attendant que tous ses enfants se terminent.
//
// Plus tard : parse /etc/inittab, gère SIGCHLD, reap les zombies orphelins.
// Pour l'instant : un seul enfant, respawn si exit.
// =============================================================================

#![no_std]
#![no_main]

use ulib::{println, eprintln, syscall::{fork, exec, wait, exit}};

ulib::entry!(main);

const SHELL: &str = "/bin/sh";

fn main() {
    println!("[init] bonjour depuis le PID 1. lancement de {}", SHELL);

    loop {
        match fork() {
            0 => {
                // Child : devient le shell.
                let _ = exec(SHELL);
                eprintln!("[init] impossible d'exec {} — halte.", SHELL);
                exit(1);
            }
            pid if pid > 0 => {
                // Parent : attend le shell. Quand il sort, on relance.
                let _terminated = wait(pid);
                println!("[init] le shell (pid={}) s'est terminé, redémarrage...", pid);
            }
            _ => {
                eprintln!("[init] fork échoué — halte.");
                exit(1);
            }
        }
    }
}
