// =============================================================================
// fat32.rs — FAT32 read-only filesystem driver.
//
// Supporte les opérations de base pour monter et lire une partition FAT32 :
//   - Lecture du BPB (BIOS Parameter Block) / Boot Sector
//   - Navigation dans la FAT (chain de clusters)
//   - Lecture de répertoires (entrées 8.3 + VFAT long file names)
//   - Lecture de fichiers (suivi de la chaîne FAT, lecture séquentielle)
//
// Pas d'écriture (read-only). Pas de cache FAT intelligent (on relit à chaque
// accès — suffisant pour QEMU).
//
// API :
//   Fat32::mount(disk_idx, start_lba) -> Result<Fat32, &str>
//   fs.ls(path)     -> Vec<DirEntry>
//   fs.read(path)   -> Result<Vec<u8>, &str>
//   fs.stat(path)   -> Option<DirEntry>
// =============================================================================

use alloc::{string::String, vec, vec::Vec};
use spin::{Mutex, Once};

use crate::drivers::ata;

const SECTOR_SIZE: usize = 512;

/// BIOS Parameter Block — champs du boot sector FAT32.
#[derive(Debug, Clone)]
pub struct Bpb {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub total_sectors: u32,
    pub sectors_per_fat: u32,
    pub root_cluster: u32,
    pub fs_info_sector: u16,
    pub volume_label: String,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u32,
    pub cluster: u32,
}

pub struct Fat32 {
    disk_idx: usize,
    part_start_lba: u32,
    pub bpb: Bpb,
    /// Premier secteur de la zone DATA.
    data_start_lba: u32,
    /// Premier secteur de la FAT.
    fat_start_lba: u32,
}

/// Singleton monté.
static MOUNTED: Once<Mutex<Fat32>> = Once::new();

impl Fat32 {
    /// Monte une partition FAT32 depuis le disque donné à l'offset LBA donné.
    pub fn mount(disk_idx: usize, start_lba: u32) -> Result<Fat32, &'static str> {
        let mut buf = [0u8; SECTOR_SIZE];
        ata::read(disk_idx, start_lba, 1, &mut buf)?;

        // Vérification signature
        if buf[510] != 0x55 || buf[511] != 0xAA {
            return Err("FAT32: signature 0x55AA absente");
        }

        let bytes_per_sector = u16::from_le_bytes([buf[11], buf[12]]);
        let sectors_per_cluster = buf[13];
        let reserved_sectors = u16::from_le_bytes([buf[14], buf[15]]);
        let num_fats = buf[16];

        // Total sectors : champ 16-bit (offset 19-20) ou 32-bit (offset 32-35)
        let total16 = u16::from_le_bytes([buf[19], buf[20]]);
        let total32 = u32::from_le_bytes([buf[32], buf[33], buf[34], buf[35]]);
        let total_sectors = if total16 != 0 { total16 as u32 } else { total32 };

        // FAT32 : sectors_per_fat est le champ 32-bit à l'offset 36
        let sectors_per_fat = u32::from_le_bytes([buf[36], buf[37], buf[38], buf[39]]);
        let root_cluster = u32::from_le_bytes([buf[44], buf[45], buf[46], buf[47]]);
        let fs_info_sector = u16::from_le_bytes([buf[48], buf[49]]);

        // Volume label (offset 71, 11 octets dans EBPB FAT32)
        let label_bytes = &buf[71..82];
        let volume_label = core::str::from_utf8(label_bytes)
            .unwrap_or("???")
            .trim()
            .into();

        let fat_start_lba = start_lba + reserved_sectors as u32;
        let data_start_lba = fat_start_lba + (num_fats as u32) * sectors_per_fat;

        let bpb = Bpb {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            num_fats,
            total_sectors,
            sectors_per_fat,
            root_cluster,
            fs_info_sector,
            volume_label,
        };

        Ok(Fat32 {
            disk_idx,
            part_start_lba: start_lba,
            bpb,
            data_start_lba,
            fat_start_lba,
        })
    }

    /// Convertit un numéro de cluster en LBA absolu.
    fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.data_start_lba + (cluster - 2) * self.bpb.sectors_per_cluster as u32
    }

    /// Lit le prochain cluster dans la FAT. Retourne None si fin de chaîne.
    fn next_cluster(&self, cluster: u32) -> Option<u32> {
        let fat_offset = cluster * 4;
        let fat_sector = self.fat_start_lba + fat_offset / SECTOR_SIZE as u32;
        let entry_offset = (fat_offset % SECTOR_SIZE as u32) as usize;

        let mut buf = [0u8; SECTOR_SIZE];
        if ata::read(self.disk_idx, fat_sector, 1, &mut buf).is_err() {
            return None;
        }

        let val = u32::from_le_bytes([
            buf[entry_offset],
            buf[entry_offset + 1],
            buf[entry_offset + 2],
            buf[entry_offset + 3],
        ]) & 0x0FFF_FFFF;

        if val >= 0x0FFF_FFF8 {
            None // End of chain
        } else if val == 0 || val == 1 {
            None // Free / reserved
        } else {
            Some(val)
        }
    }

    /// Lit toute la chaîne de clusters d'un fichier/répertoire.
    fn read_chain(&self, start_cluster: u32) -> Result<Vec<u8>, &'static str> {
        let cluster_bytes = self.bpb.sectors_per_cluster as usize * SECTOR_SIZE;
        let mut data = Vec::new();
        let mut cluster = start_cluster;
        let mut safety = 0u32;

        loop {
            let lba = self.cluster_to_lba(cluster);
            let mut cluster_buf = vec![0u8; cluster_bytes];

            // Lire cluster par secteurs (ata::read accepte max 255 secteurs)
            for s in 0..self.bpb.sectors_per_cluster as u32 {
                let sector_off = (s as usize) * SECTOR_SIZE;
                ata::read(
                    self.disk_idx,
                    lba + s,
                    1,
                    &mut cluster_buf[sector_off..sector_off + SECTOR_SIZE],
                )?;
            }

            data.extend_from_slice(&cluster_buf);

            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => break,
            }

            safety += 1;
            if safety > 1_000_000 {
                return Err("FAT32: chaîne de clusters trop longue");
            }
        }

        Ok(data)
    }

    /// Parse les entrées d'un répertoire depuis les données brutes.
    fn parse_dir_entries(&self, raw: &[u8]) -> Vec<DirEntry> {
        let mut entries = Vec::new();
        let mut lfn_parts: Vec<(u8, String)> = Vec::new();
        let mut i = 0;

        while i + 32 <= raw.len() {
            let entry = &raw[i..i + 32];
            i += 32;

            if entry[0] == 0x00 { break; }  // Fin du répertoire
            if entry[0] == 0xE5 { continue; } // Entrée supprimée

            let attrs = entry[11];

            // Long File Name entry
            if attrs == 0x0F {
                let seq = entry[0] & 0x3F;
                let mut name_part = String::new();

                // Chars 1-5 (offset 1, 5 chars UCS-2)
                for j in [1, 3, 5, 7, 9] {
                    let c = u16::from_le_bytes([entry[j], entry[j + 1]]);
                    if c == 0 || c == 0xFFFF { break; }
                    if let Some(ch) = char::from_u32(c as u32) { name_part.push(ch); }
                }
                // Chars 6-11 (offset 14, 6 chars UCS-2)
                for j in [14, 16, 18, 20, 22, 24] {
                    let c = u16::from_le_bytes([entry[j], entry[j + 1]]);
                    if c == 0 || c == 0xFFFF { break; }
                    if let Some(ch) = char::from_u32(c as u32) { name_part.push(ch); }
                }
                // Chars 12-13 (offset 28, 2 chars UCS-2)
                for j in [28, 30] {
                    let c = u16::from_le_bytes([entry[j], entry[j + 1]]);
                    if c == 0 || c == 0xFFFF { break; }
                    if let Some(ch) = char::from_u32(c as u32) { name_part.push(ch); }
                }

                lfn_parts.push((seq, name_part));
                continue;
            }

            // Hidden/system/volume label → on affiche quand même le volume label
            if attrs & 0x08 != 0 && attrs & 0x10 == 0 {
                lfn_parts.clear();
                continue; // Volume label → skip
            }

            // Short File Name entry (8.3)
            let name = if !lfn_parts.is_empty() {
                // Reconstruit le LFN
                lfn_parts.sort_by_key(|(seq, _)| *seq);
                let long_name: String = lfn_parts.iter()
                    .map(|(_, s)| s.as_str())
                    .collect();
                lfn_parts.clear();
                long_name
            } else {
                // 8.3 format
                let base = core::str::from_utf8(&entry[0..8])
                    .unwrap_or("?")
                    .trim();
                let ext = core::str::from_utf8(&entry[8..11])
                    .unwrap_or("")
                    .trim();
                if ext.is_empty() {
                    String::from(base)
                } else {
                    alloc::format!("{}.{}", base, ext)
                }
            };

            // Skip . et ..
            if name == "." || name == ".." {
                lfn_parts.clear();
                continue;
            }

            let is_dir = attrs & 0x10 != 0;
            let cluster_hi = u16::from_le_bytes([entry[20], entry[21]]) as u32;
            let cluster_lo = u16::from_le_bytes([entry[26], entry[27]]) as u32;
            let cluster = (cluster_hi << 16) | cluster_lo;
            let size = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]);

            entries.push(DirEntry { name, is_dir, size, cluster });
        }

        entries
    }

    /// Liste les entrées du répertoire au cluster donné.
    pub fn ls_cluster(&self, cluster: u32) -> Result<Vec<DirEntry>, &'static str> {
        let raw = self.read_chain(cluster)?;
        Ok(self.parse_dir_entries(&raw))
    }

    /// Liste le répertoire racine.
    pub fn ls_root(&self) -> Result<Vec<DirEntry>, &'static str> {
        self.ls_cluster(self.bpb.root_cluster)
    }

    /// Résout un chemin (ex: "/docs/readme.txt") et retourne le DirEntry.
    pub fn resolve(&self, path: &str) -> Result<DirEntry, &'static str> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Ok(DirEntry {
                name: String::from("/"),
                is_dir: true,
                size: 0,
                cluster: self.bpb.root_cluster,
            });
        }

        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_cluster = self.bpb.root_cluster;

        for (idx, component) in parts.iter().enumerate() {
            let entries = self.ls_cluster(current_cluster)?;
            let is_last = idx == parts.len() - 1;

            let found = entries.iter().find(|e| {
                e.name.eq_ignore_ascii_case(component)
            });

            match found {
                Some(entry) => {
                    if is_last {
                        return Ok(entry.clone());
                    }
                    if !entry.is_dir {
                        return Err("FAT32: composant de chemin n'est pas un répertoire");
                    }
                    current_cluster = entry.cluster;
                }
                None => return Err("FAT32: fichier/répertoire introuvable"),
            }
        }

        Err("FAT32: chemin invalide")
    }

    /// Liste le contenu d'un répertoire par chemin.
    pub fn ls(&self, path: &str) -> Result<Vec<DirEntry>, &'static str> {
        let entry = self.resolve(path)?;
        if !entry.is_dir {
            return Err("FAT32: pas un répertoire");
        }
        self.ls_cluster(entry.cluster)
    }

    /// Lit un fichier entier par chemin. Retourne les octets.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        let entry = self.resolve(path)?;
        if entry.is_dir {
            return Err("FAT32: c'est un répertoire");
        }
        let mut data = self.read_chain(entry.cluster)?;
        data.truncate(entry.size as usize);
        Ok(data)
    }

    // -------------------------------------------------------------------------
    // Write path — Phase Future : alloc cluster, update FAT, write data,
    // patch dir entry. Limité au répertoire racine, noms 8.3 uniquement,
    // mode "truncate" (réécriture complète).
    // -------------------------------------------------------------------------

    /// Met à jour une entrée FAT (sur les deux copies FAT si num_fats=2).
    fn set_fat_entry(&self, cluster: u32, value: u32) -> Result<(), &'static str> {
        let fat_offset = cluster * 4;
        let entry_offset = (fat_offset % SECTOR_SIZE as u32) as usize;
        let fat_sector_rel = fat_offset / SECTOR_SIZE as u32;

        for fat_idx in 0..self.bpb.num_fats as u32 {
            let lba = self.fat_start_lba + fat_idx * self.bpb.sectors_per_fat + fat_sector_rel;
            let mut buf = [0u8; SECTOR_SIZE];
            ata::read(self.disk_idx, lba, 1, &mut buf)?;

            // Préserve les 4 bits hauts (réservés FAT32).
            let prev = u32::from_le_bytes([
                buf[entry_offset], buf[entry_offset+1],
                buf[entry_offset+2], buf[entry_offset+3],
            ]);
            let new_val = (prev & 0xF000_0000) | (value & 0x0FFF_FFFF);
            let b = new_val.to_le_bytes();
            buf[entry_offset]   = b[0];
            buf[entry_offset+1] = b[1];
            buf[entry_offset+2] = b[2];
            buf[entry_offset+3] = b[3];

            ata::write(self.disk_idx, lba, 1, &buf)?;
        }
        Ok(())
    }

    /// Cherche un cluster libre (FAT entry == 0). Marque EOF dans la FAT.
    fn alloc_cluster(&self) -> Result<u32, &'static str> {
        let entries_per_sector = SECTOR_SIZE as u32 / 4;
        let total_clusters = self.bpb.sectors_per_fat * entries_per_sector;
        let mut buf = [0u8; SECTOR_SIZE];

        for sec in 0..self.bpb.sectors_per_fat {
            let lba = self.fat_start_lba + sec;
            ata::read(self.disk_idx, lba, 1, &mut buf)?;
            for i in 0..entries_per_sector {
                let cluster = sec * entries_per_sector + i;
                // Cluster 0 et 1 sont réservés.
                if cluster < 2 { continue; }
                if cluster >= total_clusters { return Err("FAT32: plus de clusters libres"); }
                let off = (i * 4) as usize;
                let val = u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]])
                    & 0x0FFF_FFFF;
                if val == 0 {
                    self.set_fat_entry(cluster, 0x0FFF_FFFF)?; // EOF
                    return Ok(cluster);
                }
            }
        }
        Err("FAT32: scan FAT épuisé")
    }

    /// Écrit `data` dans un cluster (zero-pad le reste).
    fn write_cluster(&self, cluster: u32, data: &[u8]) -> Result<(), &'static str> {
        let cluster_bytes = self.bpb.sectors_per_cluster as usize * SECTOR_SIZE;
        if data.len() > cluster_bytes { return Err("FAT32: data > cluster_size"); }

        let lba = self.cluster_to_lba(cluster);
        let mut buf = alloc::vec![0u8; cluster_bytes];
        buf[..data.len()].copy_from_slice(data);
        ata::write(self.disk_idx, lba, self.bpb.sectors_per_cluster, &buf)?;
        Ok(())
    }

    /// Crée (ou écrase) un fichier nommé `name_83` à la racine.
    /// `name_83` doit être ≤ 11 caractères en majuscules ASCII (8 + 3, sans le
    /// point). Exemples valides : "README", "HELLO   TXT", "BOOT    INI".
    ///
    /// Allocation : autant de clusters que nécessaire, chaînés dans la FAT.
    /// Free de l'ancienne chaîne si le fichier existait déjà.
    pub fn create_root_file(&self, name_83: &str, data: &[u8]) -> Result<(), &'static str> {
        let name_bytes = pack_short_name(name_83)?;

        // Allocation des clusters pour les données.
        let cluster_bytes = self.bpb.sectors_per_cluster as usize * SECTOR_SIZE;
        let n_clusters = if data.is_empty() {
            0
        } else {
            (data.len() + cluster_bytes - 1) / cluster_bytes
        };
        let mut chain: alloc::vec::Vec<u32> = alloc::vec::Vec::with_capacity(n_clusters);
        for _ in 0..n_clusters {
            chain.push(self.alloc_cluster()?);
        }
        // Lier la chaîne dans la FAT (le dernier reste EOF).
        for w in chain.windows(2) {
            self.set_fat_entry(w[0], w[1])?;
        }

        // Écriture des données dans les clusters.
        for (i, c) in chain.iter().enumerate() {
            let off = i * cluster_bytes;
            let end = (off + cluster_bytes).min(data.len());
            self.write_cluster(*c, &data[off..end])?;
        }

        // Patch de l'entrée de répertoire dans la racine.
        let first_cluster = chain.first().copied().unwrap_or(0);
        self.upsert_root_dir_entry(&name_bytes, first_cluster, data.len() as u32)?;
        Ok(())
    }

    /// Insère ou remplace l'entrée 32 bytes correspondant à `name_83` dans la
    /// racine. Format : 11 nom + 1 attr + 8 réservés + 2 hi-cluster + 8 dates
    /// + 2 lo-cluster + 4 size.
    fn upsert_root_dir_entry(
        &self,
        name_83: &[u8; 11],
        first_cluster: u32,
        size: u32,
    ) -> Result<(), &'static str> {
        let mut cur = self.bpb.root_cluster;
        loop {
            let lba_start = self.cluster_to_lba(cur);
            let mut cluster_buf = alloc::vec![0u8;
                self.bpb.sectors_per_cluster as usize * SECTOR_SIZE];
            ata::read(self.disk_idx, lba_start, self.bpb.sectors_per_cluster, &mut cluster_buf)?;

            // Walk les entrées 32-byte, cherche match name OR slot libre (0x00/0xE5).
            let mut chosen_off: Option<usize> = None;
            let mut chosen_is_replace = false;
            for off in (0..cluster_buf.len()).step_by(32) {
                let first = cluster_buf[off];
                if first == 0x00 {
                    chosen_off = Some(off);
                    chosen_is_replace = false;
                    break;
                }
                // Skip LFN entries
                let attr = cluster_buf[off + 11];
                if attr == 0x0F { continue; }
                if first == 0xE5 {
                    if chosen_off.is_none() {
                        chosen_off = Some(off);
                        chosen_is_replace = false;
                    }
                    continue;
                }
                if &cluster_buf[off..off+11] == name_83 {
                    chosen_off = Some(off);
                    chosen_is_replace = true;
                    break;
                }
            }

            if let Some(off) = chosen_off {
                // Si on remplace, libère l'ancienne chaîne.
                if chosen_is_replace {
                    let hi = u16::from_le_bytes([cluster_buf[off+20], cluster_buf[off+21]]);
                    let lo = u16::from_le_bytes([cluster_buf[off+26], cluster_buf[off+27]]);
                    let old_first = ((hi as u32) << 16) | (lo as u32);
                    self.free_chain(old_first)?;
                }

                let mut entry = [0u8; 32];
                entry[..11].copy_from_slice(name_83);
                entry[11] = 0x20; // ATTR_ARCHIVE
                let hi = ((first_cluster >> 16) & 0xFFFF) as u16;
                let lo = (first_cluster & 0xFFFF) as u16;
                entry[20..22].copy_from_slice(&hi.to_le_bytes());
                entry[26..28].copy_from_slice(&lo.to_le_bytes());
                entry[28..32].copy_from_slice(&size.to_le_bytes());
                cluster_buf[off..off+32].copy_from_slice(&entry);
                ata::write(self.disk_idx, lba_start, self.bpb.sectors_per_cluster, &cluster_buf)?;
                return Ok(());
            }

            // Pas de slot — passe au cluster suivant de la racine, ou alloue un
            // nouveau cluster pour la racine.
            match self.next_cluster(cur) {
                Some(n) => cur = n,
                None => {
                    let new_c = self.alloc_cluster()?;
                    self.set_fat_entry(cur, new_c)?;
                    // Le cluster vient d'être alloué (zéroes via write_cluster
                    // dans le prochain tour).
                    self.write_cluster(new_c, &[])?;
                    cur = new_c;
                }
            }
        }
    }

    /// Libère une chaîne de clusters (set chaque entrée FAT à 0).
    fn free_chain(&self, first: u32) -> Result<(), &'static str> {
        if first < 2 { return Ok(()); }
        let mut cur = first;
        loop {
            let next = self.next_cluster(cur);
            self.set_fat_entry(cur, 0)?;
            match next {
                Some(n) => cur = n,
                None => return Ok(()),
            }
        }
    }

    /// Supprime une entrée à la racine et libère sa chaîne.
    pub fn unlink_root(&self, name_83: &str) -> Result<(), &'static str> {
        let name_bytes = pack_short_name(name_83)?;
        let mut cur = self.bpb.root_cluster;
        loop {
            let lba = self.cluster_to_lba(cur);
            let mut cluster_buf = alloc::vec![0u8;
                self.bpb.sectors_per_cluster as usize * SECTOR_SIZE];
            ata::read(self.disk_idx, lba, self.bpb.sectors_per_cluster, &mut cluster_buf)?;
            for off in (0..cluster_buf.len()).step_by(32) {
                if cluster_buf[off] == 0x00 { return Err("FAT32: fichier non trouvé"); }
                if cluster_buf[off + 11] == 0x0F { continue; }
                if &cluster_buf[off..off+11] == &name_bytes {
                    let hi = u16::from_le_bytes([cluster_buf[off+20], cluster_buf[off+21]]);
                    let lo = u16::from_le_bytes([cluster_buf[off+26], cluster_buf[off+27]]);
                    let first = ((hi as u32) << 16) | (lo as u32);
                    self.free_chain(first)?;
                    cluster_buf[off] = 0xE5;
                    ata::write(self.disk_idx, lba, self.bpb.sectors_per_cluster, &cluster_buf)?;
                    return Ok(());
                }
            }
            match self.next_cluster(cur) {
                Some(n) => cur = n,
                None => return Err("FAT32: fichier non trouvé"),
            }
        }
    }

    pub fn info_string(&self) -> String {
        let cluster_bytes = self.bpb.sectors_per_cluster as u32 * self.bpb.bytes_per_sector as u32;
        let data_clusters = (self.bpb.total_sectors
            - self.bpb.reserved_sectors as u32
            - self.bpb.num_fats as u32 * self.bpb.sectors_per_fat) / self.bpb.sectors_per_cluster as u32;
        let total_mib = (data_clusters as u64) * cluster_bytes as u64 / (1024 * 1024);

        alloc::format!(
            "FAT32 '{}' : {} MiB, cluster={} B, {} spc, {} FATs",
            self.bpb.volume_label,
            total_mib,
            cluster_bytes,
            self.bpb.sectors_per_cluster,
            self.bpb.num_fats,
        )
    }
}

// -----------------------------------------------------------------------------
// API globale (singleton monté)
// -----------------------------------------------------------------------------

/// Monte la première partition FAT32 trouvée par le module `part`.
pub fn mount_first() -> Result<(), &'static str> {
    let (disk_idx, start_lba, _sectors) = crate::drivers::part::find_first_fat32()
        .ok_or("FAT32: aucune partition FAT32 trouvée")?;

    let fs = Fat32::mount(disk_idx, start_lba)?;
    crate::println!("[fat32] {}", fs.info_string());

    MOUNTED.call_once(|| Mutex::new(fs));
    Ok(())
}

pub fn mounted() -> Option<&'static Mutex<Fat32>> {
    MOUNTED.get()
}

/// Convertit "README.TXT" / "BOOT.INI" / "HELLO" en buffer 11 bytes 8.3
/// upper-case, padded espaces.
fn pack_short_name(name: &str) -> Result<[u8; 11], &'static str> {
    let upper: String = name.chars().map(|c| c.to_ascii_uppercase()).collect();
    let (base, ext) = match upper.find('.') {
        Some(i) => (&upper[..i], &upper[i+1..]),
        None    => (upper.as_str(), ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return Err("FAT32: nom invalide (8.3 only)");
    }
    let mut buf = [b' '; 11];
    for (i, b) in base.bytes().enumerate() {
        if !is_valid_short_byte(b) { return Err("FAT32: caractère invalide"); }
        buf[i] = b;
    }
    for (i, b) in ext.bytes().enumerate() {
        if !is_valid_short_byte(b) { return Err("FAT32: caractère invalide"); }
        buf[8 + i] = b;
    }
    Ok(buf)
}

fn is_valid_short_byte(b: u8) -> bool {
    matches!(b,
        b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'$' | b'%' | b'&' | b'!' | b'@'
        | b'~' | b'(' | b')' | b'{' | b'}' | b'#' | b'^'
    )
}
