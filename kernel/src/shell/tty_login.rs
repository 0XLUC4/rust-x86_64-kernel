// =============================================================================
// shell::tty_login — invite TTY avant d'autoriser l'utilisation du shell.
//
// Style "agetty + login" : on lit `<host> login:` puis `Password:` (caché).
// Authentification via `crate::users::authenticate`.
// Si succès :
//   - met à jour la session TTY kernel courante
//   - imprime un MOTD
//   - retourne au shell
// Si échec : compte les tentatives (max 3), puis sleep_ms avant de retry.
//
// Note : on refuse les credentials par défaut (`root/root`, `lunux/lunux`) si
// `/etc/passwd` n'a pas été chargé pour une raison quelconque — sécurité de
// base : pas de session sans auth.
// =============================================================================

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};
use futures_util::StreamExt;
use spin::Mutex;

use crate::{print, println, drivers::keyboard::KeyStream};

const MAX_ATTEMPTS: u32 = 3;
const HOSTNAME: &str = "lunux-os";

static CURRENT_USER: Mutex<Option<crate::users::User>> = Mutex::new(None);
static LOGOUT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Boucle d'auth TTY : ne retourne que quand un compte a été authentifié.
/// Appelée depuis `shell::run()` au démarrage si on est en mode TTY.
pub async fn run() {
    LOGOUT_REQUESTED.store(false, Ordering::Relaxed);
    let mut kb = KeyStream::new();
    print_motd();

    loop {
        let mut attempts = 0u32;
        let user = loop {
            print!("\n{} login: ", HOSTNAME);
            let login = read_line(&mut kb, false).await;
            if login.is_empty() { continue; }

            print!("Password: ");
            let password = read_line(&mut kb, true).await;

            match crate::users::authenticate(&login, &password) {
                Some(u) => break u,
                None => {
                    attempts += 1;
                    println!("\nLogin incorrect.");
                    if attempts >= MAX_ATTEMPTS {
                        println!("Trop d'echecs. Pause 5 secondes...");
                        crate::time::sleep::sleep_ms(5000).await;
                        attempts = 0;
                    }
                }
            }
        };

        // Authentifié : on met à jour la session
        *CURRENT_USER.lock() = Some(user.clone());
        print_welcome(&user);
        return;
    }
}

pub fn logout() {
    *CURRENT_USER.lock() = None;
    LOGOUT_REQUESTED.store(true, Ordering::Relaxed);
}

pub fn logout_requested() -> bool {
    LOGOUT_REQUESTED.load(Ordering::Relaxed)
}

pub fn current_user() -> Option<crate::users::User> {
    CURRENT_USER.lock().clone()
}

pub fn current_username() -> String {
    current_user()
        .map(|u| u.name)
        .unwrap_or_else(|| String::from("?"))
}

async fn read_line(kb: &mut KeyStream, hide: bool) -> String {
    let mut line = String::new();
    while let Some(ch) = kb.next().await {
        match ch {
            '\n' | '\r' => { println!(); return line; }
            '\x08' | '\x7f' => {
                if line.pop().is_some() {
                    print!("\x08 \x08");
                }
            }
            ch if (ch as u32) < 0x20 => {}
            ch => {
                line.push(ch);
                if hide { print!("*"); } else { print!("{}", ch); }
            }
        }
    }
    line
}

fn print_motd() {
    let kernel_ver = env!("CARGO_PKG_VERSION");
    println!();
    println!("{} ({}) tty1", HOSTNAME, kernel_ver);
    println!();
    println!("Comptes par defaut :  root / root     |    lunux / lunux");
    println!();
}

fn print_welcome(user: &crate::users::User) {
    println!();
    println!("Bienvenue sur Rust Kernel, {} !", user.name);
    println!("uid={}  home={}  shell={}",
        user.uid, user.home, user.shell);
    if user.uid == 0 {
        println!("Tu es connecte en tant que ROOT. Fais attention.");
    }
    println!();
    println!("Tape 'help' pour la liste des commandes, 'logout' pour se deconnecter.");
    println!();
}
