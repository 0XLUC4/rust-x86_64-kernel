// =============================================================================
// persist — stockage persistant simple sur disque ATA.
//
// Idée : on réserve une zone fixe du disque (64 secteurs = 32 KiB au LBA 2048)
// qui contient un blob état sérialisé. Ce blob contient le contenu des
// fichiers que l'on souhaite persister à travers les reboots — pour l'instant
// `/etc/passwd` et `/etc/settings`.
//
// Format binaire :
//   offset 0  : magic "DOSSTATE" (8 octets)
//   offset 8  : version u32 little-endian (actuellement 1)
//   offset 12 : nombre d'entrées u32
//   offset 16 : entrées consécutives
//
// Entrée :
//   u16 path_len | path UTF-8 | u32 data_len | data
//
// Pas de checksum pour l'instant — on veut rester testable et lisible. Un
// CRC32 sera ajouté quand on aura la moindre donnée critique.
// =============================================================================

use alloc::string::{String, ToString};
use alloc::vec::Vec;

const MAGIC: &[u8; 8] = b"DOSSTATE";
const VERSION: u32 = 1;
pub const PERSIST_LBA: u32 = 2048;
pub const PERSIST_SECTORS: u8 = 64;         // 64 × 512 = 32 KiB max
const PERSIST_BYTES: usize = PERSIST_SECTORS as usize * 512;

/// Fichiers que l'on persiste (chemins ramfs).
/// Ajouter ici les paths dont on veut le contenu restauré au boot.
pub const PERSISTED_PATHS: &[&str] = &[
    "/etc/passwd",
    "/etc/settings",
];

/// Disque cible : on utilise le disque 0 par défaut.
const DISK_IDX: usize = 0;

/// État en mémoire (chemin → contenu). Pas un FS : c'est juste le bloc brut.
#[derive(Default)]
struct Blob {
    entries: Vec<(String, Vec<u8>)>,
}

impl Blob {
    fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(PERSIST_BYTES);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (path, data) in &self.entries {
            buf.extend_from_slice(&(path.len() as u16).to_le_bytes());
            buf.extend_from_slice(path.as_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(data);
        }
        if buf.len() < PERSIST_BYTES {
            buf.resize(PERSIST_BYTES, 0);
        }
        buf
    }

    fn deserialize(raw: &[u8]) -> Option<Self> {
        if raw.len() < 16 { return None; }
        if &raw[0..8] != MAGIC { return None; }
        let ver = u32::from_le_bytes(raw[8..12].try_into().ok()?);
        if ver != VERSION { return None; }
        let n = u32::from_le_bytes(raw[12..16].try_into().ok()?) as usize;
        if n > 1024 { return None; }

        let mut entries = Vec::with_capacity(n);
        let mut off = 16;
        for _ in 0..n {
            if off + 2 > raw.len() { return None; }
            let plen = u16::from_le_bytes(raw[off..off+2].try_into().ok()?) as usize;
            off += 2;
            if off + plen > raw.len() { return None; }
            let path = core::str::from_utf8(&raw[off..off+plen]).ok()?.to_string();
            off += plen;
            if off + 4 > raw.len() { return None; }
            let dlen = u32::from_le_bytes(raw[off..off+4].try_into().ok()?) as usize;
            off += 4;
            if off + dlen > raw.len() { return None; }
            let data = raw[off..off+dlen].to_vec();
            off += dlen;
            entries.push((path, data));
        }
        Some(Blob { entries })
    }
}

// -----------------------------------------------------------------------------
// API publique
// -----------------------------------------------------------------------------

/// Charge le blob depuis le disque et restaure les fichiers dans le ramfs.
/// Retourne true si un état valide a été chargé, false sinon (dans ce cas
/// le caller doit seeder les valeurs par défaut).
pub fn load_into_ramfs() -> bool {
    let mut buf = alloc::vec![0u8; PERSIST_BYTES];
    if let Err(e) = crate::drivers::ata::read(DISK_IDX, PERSIST_LBA, PERSIST_SECTORS, &mut buf) {
        crate::serial_println!("[persist] read échec : {} — état par défaut", e);
        return false;
    }
    let blob = match Blob::deserialize(&buf) {
        Some(b) => b,
        None => {
            crate::serial_println!("[persist] pas de magic DOSSTATE — état par défaut");
            return false;
        }
    };

    let mut fs = crate::fs::FS.lock();
    for (path, data) in &blob.entries {
        fs.create(path, data);
    }
    crate::serial_println!("[persist] {} fichiers restaurés depuis le disque", blob.entries.len());
    true
}

/// Sauve sur disque les fichiers listés dans `PERSISTED_PATHS`. Silently skip
/// ceux qui n'existent pas.
pub fn save_from_ramfs() -> Result<(), &'static str> {
    let mut blob = Blob::default();
    {
        let fs = crate::fs::FS.lock();
        for path in PERSISTED_PATHS {
            if let Ok(data) = fs.read(path) {
                blob.entries.push(((*path).to_string(), data));
            }
        }
    }
    let buf = blob.serialize();
    if buf.len() > PERSIST_BYTES {
        return Err("persist: blob trop gros (>32 KiB)");
    }
    crate::drivers::ata::write(DISK_IDX, PERSIST_LBA, PERSIST_SECTORS, &buf)?;
    crate::serial_println!("[persist] {} fichiers sauvés sur disque", blob.entries.len());
    Ok(())
}
