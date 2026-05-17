// =============================================================================
// crypto — primitives cryptographiques minimales (no_std, from-scratch).
//
// Portée : rien de "production-grade". On couvre juste ce qu'il faut pour
// l'auth locale et l'intégrité (vérification de hash).
// Ne pas utiliser pour TLS, signatures, etc. — il faudrait alors importer
// `sha2` / `ring-style` crates.
// =============================================================================

pub mod sha256;

pub use sha256::{sha256, sha256_hex};
