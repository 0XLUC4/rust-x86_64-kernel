// =============================================================================
// fs — Virtual File System minimaliste + ramfs.
//
// Modèle UNIX-like ultra-simplifié :
//   - un seul système de fichiers, monté en "/"
//   - structure plate (pas de sous-dossiers, on stocke des chemins "/foo")
//   - fichiers = Vec<u8>
//   - opérations : create, open (lecture), list, remove
//
// Si un initrd (module multiboot) est fourni, on y charge les fichiers.
// Format attendu : archive "flat" maison (header + entries), voir
// `load_initrd` ci-dessous.
// =============================================================================

pub mod ramfs;
pub mod elf;
pub mod userprog;

pub use ramfs::{FS, File, FsError};

use crate::boot_info::BootInfo;

/// Si un module multiboot est présent, on interprète son contenu comme des
/// fichiers à charger dans le ramfs. Format archive *super simple* :
///
///   [4 bytes: nombre d'entrées N]
///   pour chaque entrée :
///     [2 bytes: name_len]
///     [name_len bytes: nom UTF-8]
///     [4 bytes: data_len]
///     [data_len bytes: data]
///
/// Si le format n'est pas reconnu, on stocke le module entier comme "/initrd.bin".
pub fn init(boot_info: &BootInfo) {
    let mut fs = FS.lock();

    // Quelques fichiers système toujours présents
    fs.create("/welcome.txt", b"Bienvenue sur Rust Kernel!\n\nTape 'help' pour voir les commandes.\n");
    fs.create("/motd",        b"Le kernel le plus confortable du monde.\n");

    // /etc/passwd : seed avec root (mdp "root") et un user "luc" (mdp "luc").
    // Les hashes sont calculés dynamiquement au boot — simple et garantit la
    // cohérence avec notre implémentation SHA-256. À terme, l'installateur
    // écrira ce fichier sur disque avec les mots de passe choisis.
    let root_hash = crate::crypto::sha256_hex(b"root");
    let luc_hash  = crate::crypto::sha256_hex(b"luc");
    let passwd = alloc::format!(
        "root:{}:0:0:root:/root:/bin/sh\n\
         luc:{}:1000:1000:Luc:/home/luc:/bin/sh\n",
        root_hash, luc_hash,
    );
    fs.create("/etc/passwd", passwd.as_bytes());
    crate::serial_println!("[fs] /etc/passwd seedé (root, luc)");

    // ELF userspace de démo : "exec /hello_user" pour tester ring 3 + syscalls.
    let hello = userprog::hello_world_elf();
    crate::println!("[fs] /hello_user : ELF64 genere ({} octets)", hello.len());
    fs.create("/hello_user", &hello);

    // Second : boucle qui affiche 20 dots — test de préemption / exec long
    let counter = userprog::counter_elf();
    crate::println!("[fs] /counter    : ELF64 genere ({} octets)", counter.len());
    fs.create("/counter", &counter);

    // Binaires userland "vrais" (Phase V), construits par la crate /userland.
    // Embarqués au build du kernel via include_bytes. Chemin résolu depuis
    // kernel/src/fs/mod.rs → remonte 3 niveaux pour atteindre /userland/target.
    const INIT_ELF: &[u8] = include_bytes!(
        "../../../userland/target/x86_64-user/release/init");
    const SH_ELF: &[u8] = include_bytes!(
        "../../../userland/target/x86_64-user/release/sh");
    fs.create("/sbin/init", INIT_ELF);
    fs.create("/bin/sh",    SH_ELF);
    crate::println!("[fs] /sbin/init  : ELF userland ({} octets)", INIT_ELF.len());
    crate::println!("[fs] /bin/sh     : ELF userland ({} octets)", SH_ELF.len());

    // Charge les modules multiboot
    for (data, name) in boot_info.modules() {
        crate::println!("[fs] module '{}' ({} octets)", name, data.len());
        if !try_load_archive(&mut fs, data) {
            let path = alloc::format!("/{}", if name.is_empty() { "initrd.bin" } else { name });
            fs.create(&path, data);
        }
    }

    crate::println!("[fs] ramfs prêt, {} fichiers", fs.count());
}

fn try_load_archive(fs: &mut ramfs::Ramfs, data: &[u8]) -> bool {
    if data.len() < 4 { return false; }
    let n = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if n > 1024 { return false; }  // heuristique : pas plus de 1024 fichiers

    let mut off = 4;
    for _ in 0..n {
        if off + 2 > data.len() { return false; }
        let name_len = u16::from_le_bytes([data[off], data[off+1]]) as usize;
        off += 2;
        if off + name_len > data.len() { return false; }
        let name = match core::str::from_utf8(&data[off..off+name_len]) {
            Ok(s) => s, Err(_) => return false,
        };
        off += name_len;
        if off + 4 > data.len() { return false; }
        let data_len = u32::from_le_bytes([data[off], data[off+1], data[off+2], data[off+3]]) as usize;
        off += 4;
        if off + data_len > data.len() { return false; }
        let path = if name.starts_with('/') { alloc::string::String::from(name) }
                   else { alloc::format!("/{}", name) };
        fs.create(&path, &data[off..off+data_len]);
        off += data_len;
    }
    true
}
