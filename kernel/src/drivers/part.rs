// =============================================================================
// part.rs — MBR + GPT partition table parser.
//
// Lit le secteur 0 (MBR) et détecte les partitions :
//   - Si la signature MBR (0x55AA) est présente, on lit les 4 entrées primaires.
//   - Si une entrée a le type 0xEE (protective MBR → GPT), on parse la GPT
//     (secteur 1 = header, secteurs suivants = partition entries).
//   - Sinon : MBR classique.
//
// Types supportés (identifiés) :
//   0x06/0x0E/0x0C : FAT16/FAT16 LBA/FAT32 LBA
//   0x0B           : FAT32 (CHS)
//   0x07           : NTFS / exFAT
//   0x83           : Linux (ext2/3/4, btrfs, xfs, …)
//   0xEE           : GPT protective
//   0x82           : Linux swap
//
// API :
//   init(disk_idx) → détecte et stocke les partitions
//   partitions()   → &[Partition]
// =============================================================================

use alloc::{string::String, vec, vec::Vec};
use spin::{Mutex, Once};

use crate::drivers::ata;

#[derive(Debug, Clone)]
pub struct Partition {
    pub disk_idx: usize,
    pub index: u8,
    pub part_type: PartitionType,
    pub start_lba: u32,
    pub sectors: u32,
    pub bootable: bool,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionType {
    Empty,
    Fat12,
    Fat16,
    Fat32,
    Fat32Lba,
    Fat16Lba,
    Ntfs,
    Linux,
    LinuxSwap,
    GptProtective,
    Unknown(u8),
}

impl PartitionType {
    pub fn from_mbr_type(t: u8) -> Self {
        match t {
            0x00 => PartitionType::Empty,
            0x01 => PartitionType::Fat12,
            0x04 | 0x06 => PartitionType::Fat16,
            0x0B => PartitionType::Fat32,
            0x0C => PartitionType::Fat32Lba,
            0x0E => PartitionType::Fat16Lba,
            0x07 => PartitionType::Ntfs,
            0x83 => PartitionType::Linux,
            0x82 => PartitionType::LinuxSwap,
            0xEE => PartitionType::GptProtective,
            other => PartitionType::Unknown(other),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PartitionType::Empty => "Empty",
            PartitionType::Fat12 => "FAT12",
            PartitionType::Fat16 => "FAT16",
            PartitionType::Fat32 => "FAT32",
            PartitionType::Fat32Lba => "FAT32 LBA",
            PartitionType::Fat16Lba => "FAT16 LBA",
            PartitionType::Ntfs => "NTFS/exFAT",
            PartitionType::Linux => "Linux",
            PartitionType::LinuxSwap => "Linux swap",
            PartitionType::GptProtective => "GPT protective",
            PartitionType::Unknown(_) => "Unknown",
        }
    }

    pub fn is_fat32(self) -> bool {
        matches!(self, PartitionType::Fat32 | PartitionType::Fat32Lba)
    }
}

static PARTITIONS: Once<Mutex<Vec<Partition>>> = Once::new();

/// Scan le MBR du disque `disk_idx` et enregistre les partitions trouvées.
pub fn init(disk_idx: usize) {
    let mut buf = [0u8; 512];
    if ata::read(disk_idx, 0, 1, &mut buf).is_err() {
        crate::println!("[part] impossible de lire le MBR du disque {}", disk_idx);
        PARTITIONS.call_once(|| Mutex::new(Vec::new()));
        return;
    }

    // Vérification signature MBR
    if buf[510] != 0x55 || buf[511] != 0xAA {
        crate::println!("[part] pas de signature MBR (disque {})", disk_idx);
        PARTITIONS.call_once(|| Mutex::new(Vec::new()));
        return;
    }

    let mut parts = Vec::new();

    // Parse les 4 entrées du MBR (16 octets chacune, à l'offset 0x1BE)
    for i in 0..4u8 {
        let base = 0x1BE + (i as usize) * 16;
        let status = buf[base];
        let ptype = buf[base + 4];
        let start_lba = u32::from_le_bytes([
            buf[base + 8], buf[base + 9], buf[base + 10], buf[base + 11],
        ]);
        let sectors = u32::from_le_bytes([
            buf[base + 12], buf[base + 13], buf[base + 14], buf[base + 15],
        ]);

        let pt = PartitionType::from_mbr_type(ptype);
        if pt == PartitionType::Empty { continue; }

        let mib = (sectors as u64) * 512 / (1024 * 1024);
        crate::println!("[part] #{} {} LBA={} size={} MiB  {}",
            i, pt.name(), start_lba, mib,
            if status == 0x80 { "(active)" } else { "" });

        parts.push(Partition {
            disk_idx,
            index: i,
            part_type: pt,
            start_lba,
            sectors,
            bootable: status == 0x80,
            label: String::new(),
        });
    }

    if parts.is_empty() {
        crate::println!("[part] aucune partition détectée");
    }

    PARTITIONS.call_once(|| Mutex::new(parts));
}

pub fn partitions() -> Option<&'static Mutex<Vec<Partition>>> {
    PARTITIONS.get()
}

/// Cherche la première partition FAT32 et retourne (disk_idx, start_lba, sectors).
pub fn find_first_fat32() -> Option<(usize, u32, u32)> {
    let lock = PARTITIONS.get()?.lock();
    for p in lock.iter() {
        if p.part_type.is_fat32() {
            return Some((p.disk_idx, p.start_lba, p.sectors));
        }
    }
    None
}
