// =============================================================================
// shell — petit shell interactif dans le kernel.
//
// Lit les caractères décodés par le driver clavier (via une queue),
// affiche un prompt, exécute des commandes internes.
//
// Commandes :
//   help         - liste les commandes
//   ls           - liste les fichiers du ramfs
//   cat <path>   - affiche le contenu d'un fichier
//   echo <...>   - affiche ses arguments
//   write <path> <...> - écrit dans un fichier
//   rm <path>    - supprime un fichier
//   mem          - info mémoire
//   ps           - liste les tâches
//   uptime       - temps depuis boot
//   clear        - efface l'écran
//   panic        - pour tester le panic handler
//   sleep <ms>   - dort N ms (test async sleep)
//   echo-serial  - loopback série
// =============================================================================

use crate::{print, println, fs::FS, memory::frame_allocator::FRAME_ALLOCATOR};
use alloc::string::String;
use alloc::vec::Vec;

pub mod tty_login;

pub async fn run() {
    use futures_util::StreamExt;
    use crate::drivers::keyboard::{KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_HOME, KEY_END, KEY_DEL};

    loop {
    tty_login::run().await;
    crate::drivers::keyboard::drain_queue();

    let mut keyboard = crate::drivers::keyboard::KeyStream::new();

    println!();
    println!("Rust Kernel Shell v0.2. Tape 'help' pour la liste des commandes.");
    println!("Fleches haut/bas = historique.");
    print_prompt();

    // Historique : ring buffer de 32 commandes max
    let mut history: Vec<String> = Vec::with_capacity(32);
    let mut hist_cursor: Option<usize> = None; // None = ligne courante, Some(i) = history[i]

    let mut line = String::new();
    while let Some(ch) = keyboard.next().await {
        match ch {
            '\n' | '\r' => {
                println!();
                if !line.is_empty() {
                    // Push dans l'historique si différent du dernier
                    let last_matches = history.last().map(|s| s == &line).unwrap_or(false);
                    if !last_matches {
                        history.push(line.clone());
                        if history.len() > 32 { history.remove(0); }
                    }
                    execute(&line);
                    if tty_login::logout_requested() {
                        break;
                    }
                }
                line.clear();
                hist_cursor = None;
                print_prompt();
            }
            '\x08' | '\x7f' => {
                if line.pop().is_some() {
                    print!("\x08 \x08");
                }
            }
            KEY_UP => {
                // Recule dans l'historique
                if history.is_empty() { continue; }
                let new_cursor = match hist_cursor {
                    None => history.len() - 1,
                    Some(0) => 0,
                    Some(i) => i - 1,
                };
                replace_line(&mut line, &history[new_cursor]);
                hist_cursor = Some(new_cursor);
            }
            KEY_DOWN => {
                match hist_cursor {
                    None => {}
                    Some(i) if i + 1 >= history.len() => {
                        replace_line(&mut line, "");
                        hist_cursor = None;
                    }
                    Some(i) => {
                        let ni = i + 1;
                        replace_line(&mut line, &history[ni]);
                        hist_cursor = Some(ni);
                    }
                }
            }
            KEY_LEFT | KEY_RIGHT | KEY_HOME | KEY_END | KEY_DEL => {
                // Pas encore implémenté : édition mid-line
            }
            ch if (ch as u32) < 0x20 => {
                // autres ctrl chars → ignore
            }
            ch => {
                line.push(ch);
                print!("{}", ch);
            }
        }
    }
    }
}

/// Remplace la ligne courante par `new_line` à l'écran : efface la ligne
/// (via backspaces) puis réaffiche la nouvelle.
fn replace_line(line: &mut String, new_line: &str) {
    // Efface l'ancienne ligne
    for _ in 0..line.len() {
        print!("\x08 \x08");
    }
    // Écrit la nouvelle
    print!("{}", new_line);
    line.clear();
    line.push_str(new_line);
}

fn print_prompt() {
    let user = tty_login::current_username();
    // Prompt en vert si le FB est actif
    if crate::drivers::console::is_ready() {
        crate::drivers::console::set_colors(crate::drivers::fb::GREEN, crate::drivers::fb::BG);
        print!("{}", user);
        crate::drivers::console::set_colors(crate::drivers::fb::WHITE, crate::drivers::fb::BG);
        print!("> ");
    } else {
        print!("{}> ", user);
    }
}

fn execute(line: &str) {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let cmd = parts[0];
    let args = &parts[1..];

    match cmd {
        "help" => cmd_help(),
        "ls" => cmd_ls(),
        "cat" => cmd_cat(args),
        "echo" => cmd_echo(args),
        "write" => cmd_write(args),
        "rm" => cmd_rm(args),
        "mem" => cmd_mem(),
        "ps" => cmd_ps(),
        "threads" => cmd_threads(),
        "yield" => cmd_yield(),
        "syscall" => cmd_syscall(args),
        "uptime" => cmd_uptime(),
        "clear" => cmd_clear(),
        "panic" => panic!("panic demandé par l'utilisateur"),
        "sleep" => cmd_sleep(args),
        "bp" => cmd_breakpoint(),
        "acpi" => cmd_acpi(),
        "lsapic" => cmd_lsapic(),
        "lspci" => cmd_lspci(),
        "disk" => cmd_disk(),
        "read" => cmd_read(args),
        "writeblk" => cmd_writeblk(args),
        "run" => cmd_run(args),
        "which" => cmd_which(args),
        "path" => cmd_path(args),
        "exec" => cmd_exec(args),
        "execfat" => cmd_execfat(args),
        "userls" => cmd_userls(),
        "userdemo" => cmd_userdemo(),
        "psu" => cmd_psu(),
        "killu" => cmd_killu(args),
        "sysinfo" => cmd_sysinfo(),
        // Phase III
        "lspart" => cmd_lspart(),
        "fatinfo" => cmd_fatinfo(),
        "fatls" => cmd_fatls(args),
        "fatcat" => cmd_fatcat(args),
        "ifconfig" => cmd_ifconfig(),
        "netstat" => cmd_netstat(),
        "ping" => cmd_ping(args),
        // Phase IV — comptes utilisateurs
        "whoami" => cmd_whoami(),
        "users" => cmd_users(),
        "hash" => cmd_hash(args),
        "auth" => cmd_auth(args),
        "useradd" => cmd_useradd(args),
        "userdel" => cmd_userdel(args),
        "passwd" => cmd_passwd(args),
        "save" => cmd_save(),
        "logout" => {
            tty_login::logout();
            println!("Session fermee.");
        }
        // Phase V — userland
        "init" => cmd_init(),
        _ => println!("commande inconnue: '{}' (tape 'help')", cmd),
    }
}

fn cmd_help() {
    println!("Commandes disponibles:");
    println!("  help         - cette aide");
    println!("  ls           - liste les fichiers");
    println!("  cat <path>   - affiche un fichier");
    println!("  echo <...>   - répète ses arguments");
    println!("  write <path> <...> - écrit du texte dans un fichier");
    println!("  rm <path>    - supprime un fichier");
    println!("  mem          - info mémoire (frames, heap)");
    println!("  ps           - tâches asynchrones actives");
    println!("  threads      - threads kernel (scheduler)");
    println!("  yield        - cède le CPU (context switch)");
    println!("  syscall <n>  - invoque un syscall directement");
    println!("  uptime       - temps depuis le boot");
    println!("  sleep <ms>   - dort N millisecondes");
    println!("  clear        - efface l'écran");
    println!("  bp           - déclenche un breakpoint (test)");
    println!("  panic        - test du panic handler");
    println!("  acpi         - résumé ACPI (CPU, LAPIC, overrides)");
    println!("  lsapic       - détail Local APIC + I/O APICs");
    println!("  lspci        - liste des devices PCI");
    println!("  disk         - liste des disques ATA");
    println!("  read <idx> <lba> - lit 1 secteur (hexdump)");
    println!("  writeblk <idx> <lba> <byte> --force - écrit 1 secteur brut");
    println!("  run <name|path> - lance un binaire user (search: /, /bin, /fat)");
    println!("  which <name> - résout un binaire selon PATH");
    println!("  path [show|set|add|reset] - gère le PATH userspace");
    println!("  exec <path>  - lance un ELF userspace (ramfs puis FAT32)");
    println!("  execfat <path> - lance un ELF userspace depuis FAT32");
    println!("  userls       - liste les binaires user disponibles");
    println!("  userdemo     - lance les programmes user de démo");
    println!("  psu          - liste les process userspace");
    println!("  killu <pid> <sig>  - envoie un signal à un process user");
    println!("  sysinfo      - info système complète");
    println!(" --- Phase III : disque & réseau ---");
    println!("  lspart       - liste les partitions du disque 0");
    println!("  fatinfo      - info sur la partition FAT32 montée");
    println!("  fatls [path] - liste un répertoire FAT32");
    println!("  fatcat <path>- affiche un fichier FAT32");
    println!("  ifconfig     - config réseau (IP, MAC, link)");
    println!("  netstat      - statistiques réseau");
    println!("  ping <ip>    - envoie un ICMP echo (polling)");
    println!(" --- Phase IV : comptes utilisateurs ---");
    println!("  whoami       - affiche l'identité du process courant");
    println!("  users        - liste les comptes dans /etc/passwd");
    println!("  hash <texte> - calcule le SHA-256 d'une chaîne");
    println!("  auth <user> <mdp> - teste l'authentification d'un compte");
    println!("  logout       - ferme la session TTY");
    println!(" --- Phase V : userland ---");
    println!("  init         - lance /sbin/init (userland ring 3)");
}

fn cmd_whoami() {
    // Le shell tourne en ring 0 (PID 0 côté process table), donc pas d'uid
    // associé. On affiche le "contexte kernel" ; à terme cette commande sera
    // disponible côté userland via le syscall getuid.
    let table = crate::task::process::PROCS.lock();
    match table.iter_current() {
        Some((pid, uid, euid)) =>
            println!("pid={} uid={} euid={}", pid, uid, euid),
        None => {
            if let Some(u) = tty_login::current_user() {
                println!("tty user={} uid={} gid={}", u.name, u.uid, u.gid);
            } else {
                println!("(contexte kernel - aucune session TTY)");
            }
        }
    }
}

fn cmd_users() {
    match crate::users::load() {
        Ok(list) => {
            println!("  uid   gid  name        home           shell");
            for u in list {
                println!("  {:>4} {:>4}  {:<12} {:<14} {}",
                    u.uid, u.gid, u.name, u.home, u.shell);
            }
        }
        Err(e) => println!("users: {}", e),
    }
}

fn cmd_hash(args: &[&str]) {
    if args.is_empty() { println!("usage: hash <texte>"); return; }
    let input = args.join(" ");
    println!("{}", crate::crypto::sha256_hex(input.as_bytes()));
}

fn cmd_auth(args: &[&str]) {
    if args.len() < 2 { println!("usage: auth <user> <mdp>"); return; }
    match crate::users::authenticate(args[0], args[1]) {
        Some(u) => println!("OK : {} (uid={}, gid={}, shell={})",
            u.name, u.uid, u.gid, u.shell),
        None => println!("échec : user inconnu ou mauvais mot de passe"),
    }
}

fn cmd_useradd(args: &[&str]) {
    if args.len() < 2 {
        println!("usage: useradd <nom> <mdp>");
        return;
    }
    match crate::users::create_user(args[0], args[1]) {
        Ok(u) => println!("compte créé : {} (uid={})", u.name, u.uid),
        Err(e) => println!("useradd: {}", e.message()),
    }
}

fn cmd_userdel(args: &[&str]) {
    if args.is_empty() {
        println!("usage: userdel <nom>");
        return;
    }
    let target = match crate::users::find(args[0]) {
        Some(u) => u,
        None => { println!("userdel: utilisateur inconnu"); return; }
    };
    match crate::users::delete_user(target.uid) {
        Ok(()) => println!("compte supprimé : {}", args[0]),
        Err(e) => println!("userdel: {}", e.message()),
    }
}

fn cmd_passwd(args: &[&str]) {
    if args.len() < 2 {
        println!("usage: passwd <nom> <nouveau_mdp>");
        return;
    }
    let target = match crate::users::find(args[0]) {
        Some(u) => u,
        None => { println!("passwd: utilisateur inconnu"); return; }
    };
    match crate::users::set_password(target.uid, args[1]) {
        Ok(()) => println!("mot de passe mis à jour"),
        Err(e) => println!("passwd: {}", e.message()),
    }
}

fn cmd_save() {
    match crate::persist::save_from_ramfs() {
        Ok(()) => println!("état persisté sur disque (LBA 2048, 32 KiB)"),
        Err(e) => println!("save: {}", e),
    }
}

/// Lance /sbin/init (PID 1 conceptuel ; reste "premier process user" en
/// pratique tant qu'on n'a pas d'ordonnancement dédié).
/// On cible explicitement le ramfs — pas de fallback FAT32.
fn cmd_init() {
    match crate::task::process::exec_from_fs("/sbin/init", 0) {
        Ok(pid) => println!("init lancé (pid={})", pid),
        Err(e)  => println!("init: {}", e),
    }
}

fn cmd_ls() {
    let fs = FS.lock();
    let mut files = fs.list();
    files.sort();
    for f in files {
        let size = fs.size(&f).unwrap_or(0);
        println!("  {:>8} B  {}", size, f);
    }
}

fn cmd_cat(args: &[&str]) {
    if args.is_empty() { println!("usage: cat <path>"); return; }
    let fs = FS.lock();
    match fs.read(args[0]) {
        Ok(data) => match core::str::from_utf8(&data) {
            Ok(s) => print!("{}", s),
            Err(_) => println!("(binaire, {} octets)", data.len()),
        },
        Err(_) => println!("cat: {}: fichier introuvable", args[0]),
    }
}

fn cmd_echo(args: &[&str]) {
    println!("{}", args.join(" "));
}

fn cmd_write(args: &[&str]) {
    if args.len() < 2 { println!("usage: write <path> <texte...>"); return; }
    let path = args[0];
    let mut content = args[1..].join(" ");
    content.push('\n');
    FS.lock().write(path, content.as_bytes());
    println!("écrit {} octets dans {}", content.len(), path);
}

fn cmd_rm(args: &[&str]) {
    if args.is_empty() { println!("usage: rm <path>"); return; }
    match FS.lock().remove(args[0]) {
        Ok(()) => {}
        Err(_) => println!("rm: {}: introuvable", args[0]),
    }
}

fn cmd_mem() {
    if let Some(alloc) = FRAME_ALLOCATOR.lock().as_ref() {
        let (used, total) = alloc.stats();
        let used_mib = used * 4 / 1024;
        let total_mib = total * 4 / 1024;
        println!("Frames physiques: {} / {} ({} MiB / {} MiB)", used, total, used_mib, total_mib);
    }
    println!("Heap kernel: {} KiB à {:#x}",
        crate::memory::heap::HEAP_SIZE / 1024,
        crate::memory::heap::HEAP_START);
}

fn cmd_ps() {
    // Info exposée par l'executor
    let count = crate::task::executor::task_count();
    println!("Tâches actives: {}", count);
}

fn cmd_uptime() {
    println!("up {}", crate::time::format_uptime());
}

fn cmd_clear() {
    if crate::drivers::console::is_ready() {
        crate::drivers::console::clear();
    } else {
        crate::drivers::vga::WRITER.lock().clear_screen();
    }
}

fn cmd_sleep(args: &[&str]) {
    if args.is_empty() { println!("usage: sleep <ms>"); return; }
    let ms: u64 = match args[0].parse() {
        Ok(n) => n, Err(_) => { println!("sleep: ms invalide"); return; }
    };
    // NOTE: appelé depuis le shell qui est déjà une tâche async, on peut
    // juste await. Mais execute() n'est pas async. Solution : on spawn
    // une tâche qui sleep et affiche. Limite assumée du design.
    crate::task::executor::spawn(async move {
        crate::time::sleep::sleep_ms(ms).await;
        println!("[sleep {} ms terminé]", ms);
    });
    println!("sleep lancé en tâche de fond");
}

fn cmd_breakpoint() {
    x86_64::instructions::interrupts::int3();
    println!("(retour du breakpoint)");
}

fn cmd_threads() {
    let list = crate::task::thread::SCHEDULER.lock().list();
    println!("  TID  NAME                      STATE");
    for (id, name, state) in list {
        println!("  {:>3}  {:<25} {:?}", id, name, state);
    }
}

fn cmd_yield() {
    println!("yield...");
    crate::task::thread::yield_now();
    println!("...de retour");
}

fn cmd_syscall(args: &[&str]) {
    if args.is_empty() {
        println!("usage: syscall <nr> [arg1] [arg2] [arg3]");
        println!("  1 write, 4 getpid, 5 uptime, 11 fs_list");
        return;
    }
    let nr: u64 = args[0].parse().unwrap_or(0);
    match nr {
        4 => println!("getpid -> {}", crate::syscall::syscall_dispatch(4, 0, 0, 0, 0, 0, 0)),
        5 => println!("uptime -> {} ms", crate::syscall::syscall_dispatch(5, 0, 0, 0, 0, 0, 0)),
        _ => println!("syscall {} non testable interactivement ici", nr),
    }
}

// -----------------------------------------------------------------------------
// Nouveaux diagnostics hardware (Phase I)
// -----------------------------------------------------------------------------

fn cmd_acpi() {
    let info = crate::acpi::info();
    if info.lapic_phys_addr == 0 {
        println!("ACPI : non détecté (mode dégradé PIC only)");
        return;
    }
    println!("ACPI rev         : {}", info.revision);
    println!("CPU cores        : {}  (enabled: {})",
        info.cores.len(),
        info.cores.iter().filter(|c| c.enabled).count());
    println!("LAPIC phys       : {:#x}", info.lapic_phys_addr);
    println!("I/O APICs        : {}", info.io_apics.len());
    println!("IRQ overrides    : {}", info.overrides.len());
    println!("SCI interrupt    : {}", info.sci_interrupt);
    if info.reset_reg_addr != 0 {
        println!("RESET_REG        : {:#x} = {:#x}", info.reset_reg_addr, info.reset_value);
    }
}

fn cmd_lsapic() {
    let info = crate::acpi::info();
    println!("-- Local APIC --");
    if info.lapic_phys_addr != 0 {
        let lapic = crate::arch::x86_64::apic::lapic().lock();
        println!("  ID       : {}", lapic.id());
        println!("  version  : {:#x}", lapic.version());
    } else {
        println!("  (absent)");
    }
    println!("-- CPU cores (MADT) --");
    for c in &info.cores {
        println!("  acpi#{:>2}  lapic#{:>2}  enabled={}",
            c.acpi_processor_id, c.lapic_id, c.enabled);
    }
    println!("-- I/O APICs --");
    if let Some(devs) = crate::arch::x86_64::apic::io_apics() {
        let devs = devs.lock();
        for d in devs.iter() {
            let i = d.info();
            println!("  id={}  addr={:#x}  gsi_base={}  entries={}",
                i.id, i.address, i.gsi_base, d.max_redirection());
        }
    }
    println!("-- IRQ overrides --");
    for o in &info.overrides {
        println!("  bus={} irq={} -> gsi={} flags={:#x}",
            o.bus_source, o.irq_source, o.gsi, o.flags);
    }
}

fn cmd_lspci() {
    let lock = match crate::pci::devices() {
        Some(l) => l.lock(),
        None => { println!("PCI non initialisé"); return; }
    };
    if lock.is_empty() {
        println!("Aucun device PCI détecté");
        return;
    }
    println!("  BUS:DEV.FN  VEN:DEV     CLASS                IRQ  VENDOR");
    for d in lock.iter() {
        println!("  {:02x}:{:02x}.{}   {:04x}:{:04x}  {:<20}  {:>3}  {}",
            d.addr.bus, d.addr.dev, d.addr.func,
            d.vendor_id, d.device_id, d.class_name(),
            d.irq_line, crate::pci::vendor_name(d.vendor_id));
    }
}

fn cmd_disk() {
    let lock = match crate::drivers::ata::disks() {
        Some(l) => l.lock(),
        None => { println!("ATA non initialisé"); return; }
    };
    if lock.is_empty() {
        println!("Aucun disque ATA détecté");
        return;
    }
    println!("  IDX  BUS     DRIVE    SECTORS   SIZE        MODEL");
    for (i, d) in lock.iter().enumerate() {
        let mib = d.sectors * 512 / (1024 * 1024);
        println!("  {:>3}  {:?}  {:?}  {:>8}  {:>6} MiB  {}",
            i, d.bus, d.drive, d.sectors, mib, d.model);
    }
}

fn cmd_read(args: &[&str]) {
    if args.len() < 2 {
        println!("usage: read <disk_idx> <lba>");
        return;
    }
    let idx: usize = match args[0].parse() { Ok(n) => n, _ => { println!("idx?"); return; } };
    let lba: u32   = match args[1].parse() { Ok(n) => n, _ => { println!("lba?"); return; } };
    let mut buf = [0u8; 512];
    match crate::drivers::ata::read(idx, lba, 1, &mut buf) {
        Ok(()) => hexdump_first_rows(&buf, 8),
        Err(e) => println!("read: {}", e),
    }
}

fn parse_u8_auto(s: &str) -> Result<u8, ()> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16).map_err(|_| ())
    } else {
        s.parse::<u8>().map_err(|_| ())
    }
}

fn cmd_writeblk(args: &[&str]) {
    if args.len() < 4 {
        println!("usage: writeblk <disk_idx> <lba> <byte|0xHH> --force");
        return;
    }
    if args[3] != "--force" {
        println!("refusé: ajoute --force pour confirmer l'écriture disque brute");
        return;
    }

    let idx: usize = match args[0].parse() {
        Ok(n) => n,
        Err(_) => { println!("idx?"); return; }
    };
    let lba: u32 = match args[1].parse() {
        Ok(n) => n,
        Err(_) => { println!("lba?"); return; }
    };
    let byte = match parse_u8_auto(args[2]) {
        Ok(b) => b,
        Err(_) => { println!("byte?"); return; }
    };

    let buf = [byte; 512];
    match crate::drivers::ata::write(idx, lba, 1, &buf) {
        Ok(()) => println!("writeblk: disk={} lba={} value=0x{:02x} OK", idx, lba, byte),
        Err(e) => println!("writeblk: {}", e),
    }
}

fn cmd_exec(args: &[&str]) {
    if args.is_empty() { println!("usage: exec <path>"); return; }
    let path = args[0];
    match crate::task::process::exec_from_any(path, 0) {
        Ok(pid) => {
            println!("[exec] pid={} — entrée en ring 3", pid);
            crate::task::process::run_until_all_exit();
            println!("[exec] process terminé, retour au shell");
        }
        Err(e) => println!("exec: {}", e),
    }
}

fn cmd_run(args: &[&str]) {
    if args.is_empty() { println!("usage: run <name|path>"); return; }
    let path = args[0];
    match crate::task::process::exec_from_search(path, 0) {
        Ok(pid) => {
            println!("[run] pid={} — entrée en ring 3", pid);
            crate::task::process::run_until_all_exit();
            println!("[run] process terminé, retour au shell");
        }
        Err(e) => println!("run: {}", e),
    }
}

fn cmd_which(args: &[&str]) {
    if args.is_empty() {
        println!("usage: which <name> [...]");
        return;
    }

    for name in args {
        match crate::task::process::resolve_exec_path(name) {
            Some(path) => println!("{} -> {}", name, path),
            None => println!("{}: introuvable", name),
        }
    }
}

fn cmd_path(args: &[&str]) {
    if args.is_empty() || args[0] == "show" {
        let paths = crate::task::process::exec_paths();
        println!("PATH={}", paths.join(":"));
        for (i, p) in paths.iter().enumerate() {
            println!("  [{}] {}", i, p);
        }
        return;
    }

    match args[0] {
        "reset" => {
            crate::task::process::reset_exec_paths();
            println!("PATH réinitialisé");
        }
        "add" => {
            if args.len() < 2 {
                println!("usage: path add <dir>");
                return;
            }
            match crate::task::process::add_exec_path(args[1]) {
                Ok(()) => println!("PATH += {}", args[1]),
                Err(e) => println!("path add: {}", e),
            }
        }
        "set" => {
            if args.len() < 2 {
                println!("usage: path set <dir1:dir2:...>");
                return;
            }
            let spec = args[1..].join(" ");
            match crate::task::process::set_exec_paths_from_spec(&spec) {
                Ok(()) => println!("PATH <- {}", spec),
                Err(e) => println!("path set: {}", e),
            }
        }
        _ => {
            println!("usage: path [show|set|add|reset]");
            println!("  path");
            println!("  path add /apps");
            println!("  path set /:/bin:/fat:/fat/bin");
            println!("  path reset");
        }
    }
}

fn cmd_execfat(args: &[&str]) {
    if args.is_empty() { println!("usage: execfat <path>"); return; }
    let path = args[0];

    match crate::task::process::exec_from_fat(path, 0) {
        Ok(pid) => {
            println!("[execfat] pid={} — entrée en ring 3", pid);
            crate::task::process::run_until_all_exit();
            println!("[execfat] process terminé, retour au shell");
        }
        Err(e) => println!("execfat: {}", e),
    }
}

fn cmd_userls() {
    println!("--- user bins (ramfs) ---");
    let fs = FS.lock();
    let mut files = fs.list();
    files.sort();
    for f in files {
        if f.starts_with("/") {
            let size = fs.size(&f).unwrap_or(0);
            println!("  {:>8} B  {}", size, f);
        }
    }
    drop(fs);

    println!("--- user bins (fat32 /) ---");
    if let Some(m) = crate::drivers::fat32::mounted() {
        let fat = m.lock();
        match fat.ls("/") {
            Ok(entries) => {
                for e in entries {
                    if !e.is_dir {
                        println!("  {:>8} B  {}", e.size, e.name);
                    }
                }
            }
            Err(e) => println!("  fat32: {}", e),
        }
    } else {
        println!("  FAT32 non monté");
    }
}

fn cmd_userdemo() {
    println!("[userdemo] lancement /hello_user");
    match crate::task::process::exec_from_any("/hello_user", 0) {
        Ok(pid) => {
            println!("[userdemo] pid={} /hello_user", pid);
            crate::task::process::run_until_all_exit();
        }
        Err(e) => {
            println!("[userdemo] /hello_user: {}", e);
            return;
        }
    }

    println!("[userdemo] lancement /counter");
    match crate::task::process::exec_from_any("/counter", 0) {
        Ok(pid) => {
            println!("[userdemo] pid={} /counter", pid);
            crate::task::process::run_until_all_exit();
        }
        Err(e) => println!("[userdemo] /counter: {}", e),
    }
}

fn cmd_psu() {
    let list = crate::task::process::list();
    if list.is_empty() {
        println!("aucun process userspace");
        return;
    }
    println!("  PID  PPID  STATE                    NAME");
    for (pid, parent, name, state) in list {
        println!("  {:>3}  {:>4}  {:<24} {}", pid, parent, alloc::format!("{:?}", state), name);
    }
}

fn cmd_sysinfo() {
    let (used, total) = crate::memory::frame_allocator::FRAME_ALLOCATOR
        .lock().as_ref().map(|a| a.stats()).unwrap_or((0,0));
    let mem_used_mib = used * 4 / 1024;
    let mem_total_mib = total * 4 / 1024;
    let up = crate::time::format_uptime();
    let ticks = crate::time::ticks();
    let syscalls = crate::arch::x86_64::percpu::syscall_count();
    let n_tasks = crate::task::executor::task_count();
    let n_procs = crate::task::process::list().len();

    println!("--- system ---");
    println!("  Kernel      : Rust Kernel v0.6 (Phase III)");
    println!("  Arch        : x86_64");
    println!("  Uptime      : {}  ({} ticks @ 100Hz)", up, ticks);
    println!("  Syscalls    : {}", syscalls);
    println!("--- cpu ---");
    let info = crate::acpi::info();
    println!("  CPU cores   : {}  (ACPI-detected)", info.cores.len());
    println!("  LAPIC addr  : {:#x}", info.lapic_phys_addr);
    println!("--- memory ---");
    println!("  RAM         : {} / {} MiB used", mem_used_mib, mem_total_mib);
    println!("  Heap        : {} KiB @ {:#x}",
        crate::memory::heap::HEAP_SIZE / 1024,
        crate::memory::heap::HEAP_START);
    println!("--- tasks ---");
    println!("  async tasks : {}", n_tasks);
    println!("  user procs  : {}", n_procs);
    println!("--- network ---");
    if let Some(mac) = crate::drivers::e1000::mac_address() {
        println!("  NIC MAC     : {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    }
    if let Some(ip) = crate::net::ip_address() {
        println!("  IPv4        : {}", ip);
    }
}

fn cmd_killu(args: &[&str]) {
    if args.len() < 2 { println!("usage: killu <pid> <sig>"); return; }
    let pid: crate::task::process::Pid = match args[0].parse() {
        Ok(n) => n, _ => { println!("pid?"); return; }
    };
    let sig_num: u32 = match args[1].parse() {
        Ok(n) => n, _ => { println!("sig?"); return; }
    };
    let sig = match crate::task::signal::Signal::from_num(sig_num) {
        Some(s) => s, None => { println!("signal inconnu"); return; }
    };
    match crate::task::process::kill(pid, sig) {
        Ok(()) => println!("signal {:?} envoyé à pid={}", sig, pid),
        Err(e) => println!("kill: {}", e),
    }
}

fn hexdump_first_rows(buf: &[u8], rows: usize) {
    for r in 0..rows.min(buf.len() / 16) {
        let off = r * 16;
        print!("  {:04x}  ", off);
        for c in 0..16 { print!("{:02x} ", buf[off + c]); }
        print!(" ");
        for c in 0..16 {
            let b = buf[off + c];
            let ch = if (0x20..0x7F).contains(&b) { b as char } else { '.' };
            print!("{}", ch);
        }
        println!();
    }
}

// =============================================================================
// Phase III commands : partitions, FAT32, réseau
// =============================================================================

fn cmd_lspart() {
    let lock = match crate::drivers::part::partitions() {
        Some(l) => l.lock(),
        None => { println!("Partitions non scannées (pas de disque ?)"); return; }
    };
    if lock.is_empty() {
        println!("Aucune partition détectée");
        return;
    }
    println!("  #  TYPE            START LBA    SECTORS       SIZE    FLAGS");
    for p in lock.iter() {
        let mib = (p.sectors as u64) * 512 / (1024 * 1024);
        println!("  {}  {:14}  {:>10}  {:>10}  {:>5} MiB  {}",
            p.index,
            p.part_type.name(),
            p.start_lba,
            p.sectors,
            mib,
            if p.bootable { "ACTIVE" } else { "" });
    }
}

fn cmd_fatinfo() {
    let lock = match crate::drivers::fat32::mounted() {
        Some(l) => l.lock(),
        None => { println!("Aucun FAT32 monté"); return; }
    };
    println!("{}", lock.info_string());
    println!("  bytes/sector     : {}", lock.bpb.bytes_per_sector);
    println!("  sectors/cluster  : {}", lock.bpb.sectors_per_cluster);
    println!("  reserved sectors : {}", lock.bpb.reserved_sectors);
    println!("  FATs             : {}", lock.bpb.num_fats);
    println!("  sectors/FAT      : {}", lock.bpb.sectors_per_fat);
    println!("  root cluster     : {}", lock.bpb.root_cluster);
    println!("  total sectors    : {}", lock.bpb.total_sectors);
    println!("  volume label     : '{}'", lock.bpb.volume_label);
}

fn cmd_fatls(args: &[&str]) {
    let lock = match crate::drivers::fat32::mounted() {
        Some(l) => l.lock(),
        None => { println!("Aucun FAT32 monté"); return; }
    };
    let path = if args.is_empty() { "/" } else { args[0] };
    match lock.ls(path) {
        Ok(entries) => {
            println!("  TYPE      SIZE  NAME");
            for e in &entries {
                let t = if e.is_dir { "<DIR>" } else { "     " };
                println!("  {}  {:>8}  {}", t, e.size, e.name);
            }
            println!("  ({} entrées)", entries.len());
        }
        Err(e) => println!("fatls: {}", e),
    }
}

fn cmd_fatcat(args: &[&str]) {
    if args.is_empty() { println!("usage: fatcat <path>"); return; }
    let lock = match crate::drivers::fat32::mounted() {
        Some(l) => l.lock(),
        None => { println!("Aucun FAT32 monté"); return; }
    };
    match lock.read_file(args[0]) {
        Ok(data) => {
            match core::str::from_utf8(&data) {
                Ok(s) => print!("{}", s),
                Err(_) => {
                    println!("(binaire, {} octets)", data.len());
                    hexdump_first_rows(&data, 16);
                }
            }
        }
        Err(e) => println!("fatcat: {}", e),
    }
}

fn cmd_ifconfig() {
    // MAC
    if let Some(mac) = crate::drivers::e1000::mac_address() {
        println!("  MAC     : {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    } else {
        println!("  NIC     : aucun e1000 détecté");
        return;
    }
    // Link
    println!("  link    : {}", if crate::net::socket::link_up() { "UP" } else { "DOWN" });
    // IP
    if let Some(ip) = crate::net::ip_address() {
        println!("  IPv4    : {}", ip);
    } else {
        println!("  IPv4    : non configurée");
    }
    println!("  gateway : 10.0.2.2 (QEMU default)");
}

fn cmd_netstat() {
    if let Some(net) = crate::net::stack() {
        let stack = net.lock();
        let n = stack.sockets.iter().count();
        println!("  sockets ouverts : {}", n);
    } else {
        println!("  stack réseau non initialisé");
    }
}

fn cmd_ping(args: &[&str]) {
    if args.is_empty() {
        println!("usage: ping <ip> (ex: ping 10.0.2.2)");
        return;
    }
    let parts: Vec<&str> = args[0].split('.').collect();
    if parts.len() != 4 {
        println!("IP invalide");
        return;
    }
    let octets: Vec<u8> = parts.iter()
        .filter_map(|p| p.parse::<u8>().ok())
        .collect();
    if octets.len() != 4 {
        println!("IP invalide");
        return;
    }

    use smoltcp::wire::{Ipv4Address, IpAddress};
    let target = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);

    println!("PING {} ...", target);

    // Crée un socket ICMP raw, envoie un echo request, poll pour la réponse
    if let Some(net) = crate::net::stack() {
        use smoltcp::socket::icmp;
        use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr};
        use smoltcp::time::Instant;

        let mut stack = net.lock();

        let rx_buf = icmp::PacketBuffer::new(
            alloc::vec![icmp::PacketMetadata::EMPTY; 4],
            alloc::vec![0u8; 1024],
        );
        let tx_buf = icmp::PacketBuffer::new(
            alloc::vec![icmp::PacketMetadata::EMPTY; 4],
            alloc::vec![0u8; 1024],
        );
        let socket = icmp::Socket::new(rx_buf, tx_buf);
        let handle = stack.sockets.add(socket);

        // Bind
        let ident = 0x4321;
        let sock = stack.sockets.get_mut::<icmp::Socket>(handle);
        sock.bind(icmp::Endpoint::Ident(ident)).ok();

        // Construire echo request
        let echo = Icmpv4Repr::EchoRequest {
            ident,
            seq_no: 1,
            data: b"rustkernel",
        };
        let payload_len = echo.buffer_len();
        let sock = stack.sockets.get_mut::<icmp::Socket>(handle);
        let mut send_buf = sock.send(payload_len, IpAddress::Ipv4(target)).unwrap();
        let mut pkt = Icmpv4Packet::new_unchecked(&mut send_buf);
        echo.emit(&mut pkt, &smoltcp::phy::ChecksumCapabilities::default());

        let send_time = crate::time::uptime_ms();

        // Poll en boucle pendant ~3 secondes
        let mut received = false;
        while crate::time::uptime_ms() - send_time < 3000 {
            let now = Instant::from_millis(crate::time::uptime_ms() as i64);
            {
                let crate::net::NetStack { ref mut iface, ref mut device, ref mut sockets, .. } = &mut *stack;
                let _ = iface.poll(now, device, sockets);
            }

            let sock = stack.sockets.get_mut::<icmp::Socket>(handle);
            if sock.can_recv() {
                if let Ok((payload, _addr)) = sock.recv() {
                    let elapsed = crate::time::uptime_ms() - send_time;
                    println!("  réponse de {} : {} octets, temps={} ms",
                        target, payload.len(), elapsed);
                    received = true;
                    break;
                }
            }
            x86_64::instructions::hlt();
        }

        if !received {
            println!("  timeout (pas de réponse en 3s)");
        }

        // Cleanup
        stack.sockets.remove(handle);
    } else {
        println!("stack réseau non initialisé");
    }
}
