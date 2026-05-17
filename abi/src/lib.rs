// =============================================================================
// dos-abi — Source of truth de l'ABI binaire entre kernel d/OS et userland.
//
// Règles d'or :
//   * `#![no_std]`, zéro dépendance.
//   * Tous les types qui traversent la frontière sont `#[repr(C)]` ou
//     `#[repr(u32)]`/`u64`. Pas d'enum non-repr, pas de Vec, pas de String.
//   * Les numéros de syscall et discriminants sont gravés : on n'en
//     renumérote jamais. On ajoute en queue.
//   * Layout pensé pour x86_64 (alignements 8-bytes).
//
// Cette crate est consommée par :
//   - kernel/src/syscall/  (côté ring 0 : valider, dispatcher)
//   - userland/crates/ulib (côté ring 3 : wrappers `syscall N`)
//   - le display-server (qui parle l'IPC au-dessus de l'ABI)
// =============================================================================

#![no_std]
#![allow(non_camel_case_types)]

pub mod syscall_nr;
pub mod fb;
pub mod input;
pub mod shm;
pub mod ipc;
pub mod gfx;
pub mod errno;
