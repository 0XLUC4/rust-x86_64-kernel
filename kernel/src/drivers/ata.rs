// =============================================================================
// ata.rs — driver ATA PIO (28-bit LBA) pour les disques IDE legacy.
//
// Ce driver est l'ancêtre universel : lent, bloquant, mais supporté partout
// (y compris QEMU `-drive file=...,if=ide` par défaut). Parfait pour lire
// un MBR + un premier secteur FAT32 en Phase III.
//
// Ports standard (Primary / Secondary bus) :
//   Primary    : IO 0x1F0-0x1F7 + Control 0x3F6
//   Secondary  : IO 0x170-0x177 + Control 0x376
//
// Offsets registre (à ajouter à l'IO base) :
//   0  DATA        r/w    (u16, transferts de secteurs)
//   1  ERROR       r
//   1  FEATURES    w
//   2  SECTOR_CNT  r/w
//   3  LBA_LOW     r/w
//   4  LBA_MID     r/w
//   5  LBA_HIGH    r/w
//   6  DRIVE_HEAD  r/w    (bit 4: drive 0/1 ; bit 6: LBA mode)
//   7  STATUS      r      (BSY, DRDY, DRQ, ERR, DF...)
//   7  COMMAND     w
// =============================================================================

use alloc::vec::Vec;
use spin::{Mutex, Once};
use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};

pub const SECTOR_SIZE: usize = 512;

const CMD_IDENTIFY:     u8 = 0xEC;
const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;

// Bits STATUS
const ST_ERR:  u8 = 1 << 0;
const ST_DRQ:  u8 = 1 << 3;
const ST_DF:   u8 = 1 << 5;
const ST_DRDY: u8 = 1 << 6;
const ST_BSY:  u8 = 1 << 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bus { Primary, Secondary }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drive { Master, Slave }

pub struct AtaChannel {
    io_base:   u16,
    ctrl_base: u16,
}

impl AtaChannel {
    pub const fn new(bus: Bus) -> Self {
        match bus {
            Bus::Primary   => AtaChannel { io_base: 0x1F0, ctrl_base: 0x3F6 },
            Bus::Secondary => AtaChannel { io_base: 0x170, ctrl_base: 0x376 },
        }
    }

    // --- Registres ---
    #[inline] fn data(&self)      -> Port<u16>      { Port::new(self.io_base + 0) }
    #[inline] fn error(&self)     -> PortReadOnly<u8>  { PortReadOnly::new(self.io_base + 1) }
    #[inline] fn features(&self)  -> PortWriteOnly<u8> { PortWriteOnly::new(self.io_base + 1) }
    #[inline] fn sector_cnt(&self)-> Port<u8>       { Port::new(self.io_base + 2) }
    #[inline] fn lba_lo(&self)    -> Port<u8>       { Port::new(self.io_base + 3) }
    #[inline] fn lba_mid(&self)   -> Port<u8>       { Port::new(self.io_base + 4) }
    #[inline] fn lba_hi(&self)    -> Port<u8>       { Port::new(self.io_base + 5) }
    #[inline] fn drive_head(&self)-> Port<u8>       { Port::new(self.io_base + 6) }
    #[inline] fn status(&self)    -> PortReadOnly<u8>  { PortReadOnly::new(self.io_base + 7) }
    #[inline] fn command(&self)   -> PortWriteOnly<u8> { PortWriteOnly::new(self.io_base + 7) }
    #[inline] fn ctrl(&self)      -> Port<u8>       { Port::new(self.ctrl_base) }

    /// Délai ~400ns : lire 4× le Alternate Status.
    fn io_wait(&self) {
        let mut p = PortReadOnly::<u8>::new(self.ctrl_base);
        unsafe { let _ = p.read(); let _ = p.read(); let _ = p.read(); let _ = p.read(); }
    }

    /// Poll tant que BSY==1, puis jusqu'à DRQ==1 ou ERR==1.
    fn wait_drq(&self) -> Result<(), &'static str> {
        let mut s = self.status();
        for _ in 0..100_000 {
            let v = unsafe { s.read() };
            if v & ST_BSY != 0 { continue; }
            if v & (ST_ERR | ST_DF) != 0 { return Err("ATA: ERR/DF"); }
            if v & ST_DRQ != 0 { return Ok(()); }
        }
        Err("ATA: timeout wait_drq")
    }

    fn wait_ready(&self) -> Result<(), &'static str> {
        let mut s = self.status();
        for _ in 0..100_000 {
            let v = unsafe { s.read() };
            if v & ST_BSY != 0 { continue; }
            if v & (ST_ERR | ST_DF) != 0 { return Err("ATA: ERR/DF"); }
            if v & ST_DRDY != 0 { return Ok(()); }
        }
        Err("ATA: timeout wait_ready")
    }

    fn select_drive(&self, drive: Drive, lba_high_nibble: u8) {
        let sel = 0xE0 | (((drive == Drive::Slave) as u8) << 4) | (lba_high_nibble & 0x0F);
        let mut h = self.drive_head();
        unsafe { h.write(sel); }
        self.io_wait();
    }

    /// Envoie un IDENTIFY DEVICE. Renvoie Some(words) si OK, None sinon.
    pub fn identify(&self, drive: Drive) -> Option<[u16; 256]> {
        // Sélection + reset features
        self.select_drive(drive, 0);
        let mut sc = self.sector_cnt();
        let mut lo = self.lba_lo();
        let mut mi = self.lba_mid();
        let mut hi = self.lba_hi();
        unsafe {
            sc.write(0); lo.write(0); mi.write(0); hi.write(0);
        }
        let mut cmd = self.command();
        unsafe { cmd.write(CMD_IDENTIFY); }
        self.io_wait();

        let mut st = self.status();
        let v0 = unsafe { st.read() };
        if v0 == 0 { return None; } // pas de drive

        // Spec : si BSY=0 && LBA_MID|LBA_HI != 0 ⇒ device non-ATA (ATAPI/SATA)
        while unsafe { st.read() } & ST_BSY != 0 {}
        if unsafe { mi.read() } != 0 || unsafe { hi.read() } != 0 {
            return None;
        }

        // Wait DRQ / ERR
        loop {
            let v = unsafe { st.read() };
            if v & ST_ERR != 0 { return None; }
            if v & ST_DRQ != 0 { break; }
        }

        let mut buf = [0u16; 256];
        let mut dp = self.data();
        for w in buf.iter_mut() {
            *w = unsafe { dp.read() };
        }
        Some(buf)
    }

    /// Lecture de `count` secteurs consécutifs à partir de `lba` (28-bit).
    /// `buf` doit faire au moins count * 512 octets.
    pub fn read_sectors(
        &self,
        drive: Drive,
        lba: u32,
        count: u8,
        buf: &mut [u8],
    ) -> Result<(), &'static str> {
        if count == 0 { return Ok(()); }
        if buf.len() < (count as usize) * SECTOR_SIZE { return Err("buf trop petit"); }
        if lba & 0xF000_0000 != 0 { return Err("LBA28 overflow"); }

        self.select_drive(drive, ((lba >> 24) & 0x0F) as u8);
        let mut sc = self.sector_cnt();
        let mut lo = self.lba_lo();
        let mut mi = self.lba_mid();
        let mut hi = self.lba_hi();
        let mut cm = self.command();
        unsafe {
            sc.write(count);
            lo.write((lba & 0xFF) as u8);
            mi.write(((lba >> 8) & 0xFF) as u8);
            hi.write(((lba >> 16) & 0xFF) as u8);
            cm.write(CMD_READ_SECTORS);
        }

        let mut dp = self.data();
        for s in 0..count as usize {
            self.wait_drq()?;
            let off = s * SECTOR_SIZE;
            for i in 0..SECTOR_SIZE / 2 {
                let w = unsafe { dp.read() };
                buf[off + i * 2    ] = (w & 0xFF) as u8;
                buf[off + i * 2 + 1] = (w >> 8)   as u8;
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_sectors(
        &self,
        drive: Drive,
        lba: u32,
        count: u8,
        buf: &[u8],
    ) -> Result<(), &'static str> {
        if count == 0 { return Ok(()); }
        if buf.len() < (count as usize) * SECTOR_SIZE { return Err("buf trop petit"); }
        if lba & 0xF000_0000 != 0 { return Err("LBA28 overflow"); }

        self.select_drive(drive, ((lba >> 24) & 0x0F) as u8);
        let mut sc = self.sector_cnt();
        let mut lo = self.lba_lo();
        let mut mi = self.lba_mid();
        let mut hi = self.lba_hi();
        let mut cm = self.command();
        unsafe {
            sc.write(count);
            lo.write((lba & 0xFF) as u8);
            mi.write(((lba >> 8) & 0xFF) as u8);
            hi.write(((lba >> 16) & 0xFF) as u8);
            cm.write(CMD_WRITE_SECTORS);
        }

        let mut dp = self.data();
        for s in 0..count as usize {
            self.wait_drq()?;
            let off = s * SECTOR_SIZE;
            for i in 0..SECTOR_SIZE / 2 {
                let w = u16::from(buf[off + i * 2]) | (u16::from(buf[off + i * 2 + 1]) << 8);
                unsafe { dp.write(w); }
            }
        }
        // Cache flush
        unsafe { cm.write(0xE7); }
        self.wait_ready()?;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Registre global des disques détectés
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Disk {
    pub bus:       Bus,
    pub drive:     Drive,
    pub model:     alloc::string::String,
    pub serial:    alloc::string::String,
    pub sectors:   u64,
}

static DISKS: Once<Mutex<Vec<Disk>>> = Once::new();

fn words_to_string(words: &[u16]) -> alloc::string::String {
    // Chaque u16 encode 2 chars ASCII dans l'ordre "big-endian" (hi puis lo).
    let mut s = alloc::string::String::with_capacity(words.len() * 2);
    for &w in words {
        s.push(((w >> 8) & 0xFF) as u8 as char);
        s.push((w & 0xFF) as u8 as char);
    }
    s.trim().into()
}

/// Scan des 2 bus × 2 drives. Ajoute chaque disque détecté au registre.
pub fn init() {
    let mut found = Vec::new();
    for bus in [Bus::Primary, Bus::Secondary] {
        let ch = AtaChannel::new(bus);
        for drive in [Drive::Master, Drive::Slave] {
            if let Some(id) = ch.identify(drive) {
                // Words intéressants :
                //   10..19 : serial     (20 bytes)
                //   27..46 : model      (40 bytes)
                //   60..61 : LBA28 sector count (u32)
                //   100..103 : LBA48 sector count (u64)
                let serial = words_to_string(&id[10..20]);
                let model  = words_to_string(&id[27..47]);
                let lba28  = (id[60] as u32) | ((id[61] as u32) << 16);
                let lba48  = (id[100] as u64)
                    | ((id[101] as u64) << 16)
                    | ((id[102] as u64) << 32)
                    | ((id[103] as u64) << 48);
                let sectors = if lba48 != 0 { lba48 } else { lba28 as u64 };
                crate::println!("[ata] {:?}/{:?} : {} ({} secteurs, {} MiB)",
                    bus, drive, model, sectors, sectors * 512 / (1024 * 1024));
                found.push(Disk { bus, drive, model, serial, sectors });
            }
        }
    }
    if found.is_empty() {
        crate::println!("[ata] aucun disque détecté");
    }
    DISKS.call_once(|| Mutex::new(found));
}

pub fn disks() -> Option<&'static Mutex<Vec<Disk>>> {
    DISKS.get()
}

/// Lit `count` secteurs depuis le disque n°`idx` à partir du LBA donné.
pub fn read(idx: usize, lba: u32, count: u8, buf: &mut [u8]) -> Result<(), &'static str> {
    let list = DISKS.get().ok_or("ata non initialisé")?.lock();
    let d = list.get(idx).ok_or("disque introuvable")?;
    let ch = AtaChannel::new(d.bus);
    ch.read_sectors(d.drive, lba, count, buf)
}

/// Écrit `count` secteurs sur le disque n°`idx` à partir du LBA donné.
pub fn write(idx: usize, lba: u32, count: u8, buf: &[u8]) -> Result<(), &'static str> {
    let list = DISKS.get().ok_or("ata non initialisé")?.lock();
    let d = list.get(idx).ok_or("disque introuvable")?;
    let ch = AtaChannel::new(d.bus);
    ch.write_sectors(d.drive, lba, count, buf)
}
