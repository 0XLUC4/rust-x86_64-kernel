// =============================================================================
// ramfs.rs — FS en mémoire pure, structure plate, nommé par chemin absolu.
// =============================================================================

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use spin::Mutex;

#[derive(Debug)]
pub enum FsError {
    NotFound,
    AlreadyExists,
}

#[derive(Clone)]
pub struct File {
    pub path: String,
    pub data: Vec<u8>,
}

pub struct Ramfs {
    files: BTreeMap<String, Vec<u8>>,
}

impl Ramfs {
    const fn new() -> Self { Ramfs { files: BTreeMap::new() } }

    /// Crée (ou remplace) un fichier.
    pub fn create(&mut self, path: &str, data: &[u8]) {
        self.files.insert(String::from(path), Vec::from(data));
    }

    /// Écrit dans un fichier existant (ou le crée).
    pub fn write(&mut self, path: &str, data: &[u8]) {
        self.files.insert(String::from(path), Vec::from(data));
    }

    /// Lit un fichier.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        self.files.get(path).cloned().ok_or(FsError::NotFound)
    }

    /// Supprime un fichier.
    pub fn remove(&mut self, path: &str) -> Result<(), FsError> {
        self.files.remove(path).map(|_| ()).ok_or(FsError::NotFound)
    }

    /// Liste tous les chemins.
    pub fn list(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    pub fn count(&self) -> usize { self.files.len() }

    pub fn size(&self, path: &str) -> Option<usize> {
        self.files.get(path).map(|v| v.len())
    }
}

pub static FS: Mutex<Ramfs> = Mutex::new(Ramfs::new());
