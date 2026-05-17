// =============================================================================
// drivers/ext2.rs — ext2 read-only minimal.
//
// Couvre :
//   - Superblock (offset 1024 dans la partition)
//   - Block Group Descriptor Table
//   - Inode read par numéro
//   - Lecture d'un fichier régulier (direct blocks + single-indirect)
//   - Listing d'un répertoire (linked list de dirent variables)
//
// Limites volontaires :
//   - Pas de double/triple-indirect (= fichiers > ~4 MiB en block-size=1 KiB
//     non supportés tels quels — easy to extend).
//   - Pas de path-resolution (juste read inode N + ls inode N + read file N).
//   - Pas d'extents (ext4) — ext2 strict, block pointers 32-bit.
//   - Pas d'écriture.
//
// Spec ref : "The Second Extended File System" — David Poirier, et le
// kernel.org doc Documentation/filesystems/ext2.rst.
// =============================================================================

use alloc::string::String;
use alloc::vec::Vec;
use spin::{Mutex, Once};

use crate::drivers::ata;

const EXT2_MAGIC: u16 = 0xEF53;
const SECTOR_SIZE: usize = 512;
const SUPERBLOCK_OFFSET_BYTES: u32 = 1024;
const ROOT_INODE: u32 = 2;

const EXT2_S_IFMT:  u16 = 0xF000;
const EXT2_S_IFDIR: u16 = 0x4000;
const EXT2_S_IFREG: u16 = 0x8000;

#[derive(Debug, Clone)]
pub struct Superblock {
    pub inodes_count: u32,
    pub blocks_count: u32,
    pub free_blocks_count: u32,
    pub free_inodes_count: u32,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub magic: u16,
    pub inode_size: u16,
}

#[derive(Debug, Clone)]
pub struct BlockGroupDesc {
    pub block_bitmap_block: u32,
    pub inode_bitmap_block: u32,
    pub inode_table_block:  u32,
    pub free_blocks_count:  u16,
    pub free_inodes_count:  u16,
    pub used_dirs_count:    u16,
}

#[derive(Debug, Clone, Default)]
pub struct Inode {
    pub mode: u16,
    pub uid: u16,
    pub size: u32,
    pub atime: u32,
    pub ctime: u32,
    pub mtime: u32,
    pub gid: u16,
    pub links_count: u16,
    pub blocks: u32,        // en unités de 512-byte sectors (ext2 historique)
    pub flags: u32,
    pub block: [u32; 15],   // 0..12 direct, 12 single-indirect, 13 double, 14 triple
}

impl Inode {
    pub fn is_dir(&self)  -> bool { self.mode & EXT2_S_IFMT == EXT2_S_IFDIR }
    pub fn is_file(&self) -> bool { self.mode & EXT2_S_IFMT == EXT2_S_IFREG }
}

pub struct Ext2 {
    disk_idx: usize,
    part_start_lba: u32,
    pub sb: Superblock,
    block_size: u32,
}

static MOUNTED: Once<Mutex<Ext2>> = Once::new();

impl Ext2 {
    /// Tente de monter une partition ext2 en lisant son superblock.
    pub fn mount(disk_idx: usize, start_lba: u32) -> Result<Ext2, &'static str> {
        // Lit 2 secteurs à start_lba+2 (SB est à offset 1024 = 2 secteurs).
        let sb_lba = start_lba + SUPERBLOCK_OFFSET_BYTES / SECTOR_SIZE as u32;
        let mut buf = [0u8; 1024];
        ata::read(disk_idx, sb_lba, 2, &mut buf)?;

        let sb = parse_superblock(&buf)?;
        if sb.magic != EXT2_MAGIC {
            return Err("ext2: bad magic");
        }
        let block_size = 1024u32 << sb.log_block_size;
        Ok(Ext2 { disk_idx, part_start_lba: start_lba, sb, block_size })
    }

    pub fn block_size(&self) -> u32 { self.block_size }

    fn block_to_lba(&self, block: u32) -> u32 {
        self.part_start_lba + block * (self.block_size / SECTOR_SIZE as u32)
    }

    fn read_block(&self, block: u32, out: &mut [u8]) -> Result<(), &'static str> {
        if out.len() < self.block_size as usize { return Err("ext2: buf < block_size"); }
        let lba = self.block_to_lba(block);
        let sectors = (self.block_size / SECTOR_SIZE as u32) as u8;
        ata::read(self.disk_idx, lba, sectors, &mut out[..self.block_size as usize])
    }

    /// Charge le descripteur du group `g`.
    fn read_bgd(&self, g: u32) -> Result<BlockGroupDesc, &'static str> {
        // Le BGD table commence au block suivant le superblock.
        let bgdt_block = if self.block_size == 1024 { 2 } else { 1 };
        let bgd_per_block = self.block_size / 32;
        let block = bgdt_block + g / bgd_per_block;
        let off_in_block = (g % bgd_per_block) as usize * 32;

        let mut buf = alloc::vec![0u8; self.block_size as usize];
        self.read_block(block, &mut buf)?;
        Ok(parse_bgd(&buf[off_in_block..off_in_block + 32]))
    }

    /// Charge l'inode `ino` (1-indexé : root = 2).
    pub fn read_inode(&self, ino: u32) -> Result<Inode, &'static str> {
        if ino == 0 { return Err("ext2: inode 0"); }
        let group = (ino - 1) / self.sb.inodes_per_group;
        let index = (ino - 1) % self.sb.inodes_per_group;
        let bgd = self.read_bgd(group)?;

        let inode_size = self.sb.inode_size as u32;
        let inodes_per_block = self.block_size / inode_size;
        let block = bgd.inode_table_block + index / inodes_per_block;
        let off_in_block = (index % inodes_per_block) as usize * inode_size as usize;

        let mut buf = alloc::vec![0u8; self.block_size as usize];
        self.read_block(block, &mut buf)?;
        Ok(parse_inode(&buf[off_in_block..off_in_block + 128]))
    }

    /// Lit le contenu d'un fichier régulier (direct + single-indirect).
    pub fn read_file(&self, ino: u32) -> Result<Vec<u8>, &'static str> {
        let inode = self.read_inode(ino)?;
        if !inode.is_file() { return Err("ext2: pas un fichier"); }

        let mut out = Vec::with_capacity(inode.size as usize);
        let bs = self.block_size as usize;
        let mut remaining = inode.size as usize;
        let mut buf = alloc::vec![0u8; bs];

        // Direct blocks 0..12
        for i in 0..12 {
            if remaining == 0 { break; }
            let bnum = inode.block[i];
            if bnum == 0 { break; }
            self.read_block(bnum, &mut buf)?;
            let take = remaining.min(bs);
            out.extend_from_slice(&buf[..take]);
            remaining -= take;
        }

        // Single-indirect : block[12] contient un tableau de u32 (block_size / 4 entrées)
        if remaining > 0 && inode.block[12] != 0 {
            let mut indir = alloc::vec![0u8; bs];
            self.read_block(inode.block[12], &mut indir)?;
            let n = bs / 4;
            for i in 0..n {
                if remaining == 0 { break; }
                let bnum = u32::from_le_bytes([
                    indir[i*4], indir[i*4+1], indir[i*4+2], indir[i*4+3],
                ]);
                if bnum == 0 { break; }
                self.read_block(bnum, &mut buf)?;
                let take = remaining.min(bs);
                out.extend_from_slice(&buf[..take]);
                remaining -= take;
            }
        }
        // TODO: double/triple-indirect (fichiers > ~4 MiB en bs=1KiB).

        Ok(out)
    }

    /// Liste les entrées de répertoire de l'inode `ino` (direct blocks seulement).
    pub fn ls(&self, ino: u32) -> Result<Vec<(String, u32)>, &'static str> {
        let inode = self.read_inode(ino)?;
        if !inode.is_dir() { return Err("ext2: pas un dir"); }
        let bs = self.block_size as usize;
        let mut out = Vec::new();
        let mut buf = alloc::vec![0u8; bs];

        for i in 0..12 {
            let bnum = inode.block[i];
            if bnum == 0 { break; }
            self.read_block(bnum, &mut buf)?;
            let mut off = 0usize;
            while off + 8 <= bs {
                let inode_n = u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]);
                let rec_len = u16::from_le_bytes([buf[off+4], buf[off+5]]) as usize;
                let name_len = buf[off+6] as usize;
                if rec_len == 0 { break; }
                if inode_n != 0 && off + 8 + name_len <= bs {
                    let name = core::str::from_utf8(&buf[off+8..off+8+name_len])
                        .unwrap_or("?")
                        .into();
                    out.push((name, inode_n));
                }
                off += rec_len;
            }
        }
        Ok(out)
    }
}

// -----------------------------------------------------------------------------
// Parsers
// -----------------------------------------------------------------------------

fn parse_superblock(buf: &[u8]) -> Result<Superblock, &'static str> {
    if buf.len() < 1024 { return Err("ext2: sb buf trop court"); }
    let u32at = |off: usize| u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]);
    let u16at = |off: usize| u16::from_le_bytes([buf[off], buf[off+1]]);

    Ok(Superblock {
        inodes_count: u32at(0),
        blocks_count: u32at(4),
        free_blocks_count: u32at(12),
        free_inodes_count: u32at(16),
        first_data_block: u32at(20),
        log_block_size:   u32at(24),
        blocks_per_group: u32at(32),
        inodes_per_group: u32at(40),
        magic: u16at(56),
        inode_size: { let v = u16at(88); if v == 0 { 128 } else { v } },
    })
}

fn parse_bgd(buf: &[u8]) -> BlockGroupDesc {
    let u32at = |off: usize| u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]);
    let u16at = |off: usize| u16::from_le_bytes([buf[off], buf[off+1]]);
    BlockGroupDesc {
        block_bitmap_block: u32at(0),
        inode_bitmap_block: u32at(4),
        inode_table_block:  u32at(8),
        free_blocks_count:  u16at(12),
        free_inodes_count:  u16at(14),
        used_dirs_count:    u16at(16),
    }
}

fn parse_inode(buf: &[u8]) -> Inode {
    let u32at = |off: usize| u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]);
    let u16at = |off: usize| u16::from_le_bytes([buf[off], buf[off+1]]);
    let mut block = [0u32; 15];
    for i in 0..15 {
        block[i] = u32at(40 + i * 4);
    }
    Inode {
        mode: u16at(0),
        uid:  u16at(2),
        size: u32at(4),
        atime: u32at(8),
        ctime: u32at(12),
        mtime: u32at(16),
        gid:   u16at(24),
        links_count: u16at(26),
        blocks: u32at(28),
        flags:  u32at(32),
        block,
    }
}

// -----------------------------------------------------------------------------
// API globale
// -----------------------------------------------------------------------------

/// Tente de monter la première partition de type Linux (0x83) en ext2.
pub fn mount_first() -> Result<(), &'static str> {
    let (disk_idx, start_lba, _) = crate::drivers::part::find_first_linux()
        .ok_or("ext2: aucune partition Linux trouvée")?;
    let fs = Ext2::mount(disk_idx, start_lba)?;
    crate::println!(
        "[ext2] monté : {} blocks de {} B, {} inodes, magic={:#06x}",
        fs.sb.blocks_count, fs.block_size, fs.sb.inodes_count, fs.sb.magic,
    );
    MOUNTED.call_once(|| Mutex::new(fs));
    Ok(())
}

pub fn mounted() -> Option<&'static Mutex<Ext2>> {
    MOUNTED.get()
}
