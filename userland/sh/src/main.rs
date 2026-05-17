// =============================================================================
// sh — shell ring 3 minimal.
//
// Fonctionnalités :
//   - prompt "$ "
//   - read stdin → parse en tokens (espace blanc)
//   - builtins : exit, echo, pwd (stub), help, whoami, pid, cd (stub)
//   - sinon : fork + exec(argv[0]) + wait
//
// Pas d'allocateur user : on travaille avec des slices dans un buffer stack.
// Pas de redirections, pas de pipes, pas de variables — MVP.
// =============================================================================

#![no_std]
#![no_main]

use ulib::{
    print, println, eprintln,
    io::stdin_line,
    syscall::{fork, exec, wait, exit, getpid, getuid, geteuid},
};

ulib::entry!(main);

const LINE_MAX: usize = 256;

fn main() {
    println!();
    println!("d/OS shell v0.1 — tape 'help' pour la liste des builtins.");

    let mut buf = [0u8; LINE_MAX];

    loop {
        prompt();
        let line = match stdin_line(&mut buf) {
            Some(l) => l,
            None => {
                // EOF / erreur read : on boucle.
                continue;
            }
        };
        let line = line.trim_ascii();
        if line.is_empty() { continue; }

        // Split en tokens (jusqu'à 8 args).
        let mut argv = [""; 8];
        let mut argc = 0;
        for tok in line.split_ascii_whitespace() {
            if argc >= argv.len() { break; }
            argv[argc] = tok;
            argc += 1;
        }
        if argc == 0 { continue; }

        dispatch(&argv[..argc]);
    }
}

fn prompt() {
    let uid = geteuid();
    let sym = if uid == 0 { '#' } else { '$' };
    print!("{} ", sym);
}

fn dispatch(argv: &[&str]) {
    match argv[0] {
        "exit" | "quit" => {
            let code = argv.get(1).and_then(|s| parse_i32(s)).unwrap_or(0);
            exit(code);
        }
        "help" => help(),
        "echo" => {
            for (i, a) in argv[1..].iter().enumerate() {
                if i > 0 { print!(" "); }
                print!("{}", a);
            }
            println!();
        }
        "pid"    => println!("{}", getpid()),
        "whoami" => {
            let (uid, euid) = (getuid(), geteuid());
            if uid == euid { println!("uid={}", uid); }
            else { println!("uid={} (euid={})", uid, euid); }
        }
        "pwd" => println!("/"),
        "cd"  => eprintln!("cd: non implémenté (pas de sous-dossiers)"),
        _ => {
            // Externe : fork + exec.
            match fork() {
                0 => {
                    let _ = exec(argv[0]);
                    eprintln!("sh: {}: commande introuvable", argv[0]);
                    exit(127);
                }
                pid if pid > 0 => { let _ = wait(pid); }
                _ => eprintln!("sh: fork échoué"),
            }
        }
    }
}

fn help() {
    println!("builtins :");
    println!("  help           cette aide");
    println!("  exit [code]    quitte le shell (code défaut = 0)");
    println!("  echo <args>    répète ses arguments");
    println!("  pid            affiche le PID courant");
    println!("  whoami         uid/euid du process");
    println!("  pwd, cd        stubs");
    println!("sinon : fork + exec du binaire nommé (chemin absolu attendu).");
}

fn parse_i32(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    if b.is_empty() { return None; }
    let (neg, rest) = if b[0] == b'-' { (true, &b[1..]) } else { (false, b) };
    if rest.is_empty() { return None; }
    let mut n: i32 = 0;
    for &c in rest {
        if !(b'0'..=b'9').contains(&c) { return None; }
        n = n.checked_mul(10)?.checked_add((c - b'0') as i32)?;
    }
    Some(if neg { -n } else { n })
}
