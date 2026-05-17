# Phase V — ABI graphique / IPC / shm

État : **step 1 — squelette ABI gravé, handlers stub fonctionnels**.

## Frontière kernel ↔ user

| | Kernel (ring 0) | User space (ring 3) |
|---|---|---|
| Possède la MMIO scanout | ✅ | ❌ |
| Reçoit IRQ KB/Souris | ✅ | ❌ |
| `display-server` (process) | ❌ | ✅ — un seul, exclusif via FB_ACQUIRE |
| Compositor / window manager | ❌ | ✅ (souvent fusionné avec display-server) |
| Toolkit / apps | ❌ | ✅ |

## Syscalls ajoutés (numéros stables)

| Nr | Nom | Signature | Auth |
|---|---|---|---|
| 40 | FB_ACQUIRE   | `(out: *mut FbInfo) -> 0/-1`              | display-server only |
| 41 | FB_PRESENT   | `(rect: *const Rect) -> 0/-1`             | display-server only |
| 42 | INPUT_POLL   | `(buf: *mut InputEvent, max) -> n`        | display-server only |
| 43 | SHM_CREATE   | `(size) -> handle (u64)`                  | tout process |
| 44 | SHM_MAP      | `(handle, mode) -> ptr_user`              | tout process |
| 45 | SHM_UNMAP    | `(ptr) -> 0`                              | tout process |
| 46 | IPC_SEND     | `(target_pid, msg_ptr, msg_len) -> 0/-1`  | tout process |
| 47 | IPC_RECV     | `(buf, max, *out_sender) -> n` (bloquant) | tout process |

## Source of truth ABI

Tous les types `repr(C)` sont définis dans la crate `dos-abi` (`/abi`) :

- `abi::syscall_nr` — numéros (gravés).
- `abi::fb` — `FbInfo`, `Rect`, `PixelFormat`, capabilities.
- `abi::input` — `InputEvent`, `InputKind`, masks boutons/modifieurs.
- `abi::shm` — `ShmHandle`, modes RW.
- `abi::ipc` — `IpcHeader` (32 B fixe).
- `abi::gfx` — protocole display-server (au-dessus d'IPC, pas un syscall).
- `abi::errno` — codes d'erreur stables.

Le kernel duplique ponctuellement quelques structs (cf. `gfx::FbInfoAbi`,
`gfx::InputEventAbi`) — toute divergence avec `dos-abi` est un **bug**.

## Roadmap step 2 → step 4

- **step 2** : `sys_shm_map` réel — mapping page-table user (range VA libre,
  flags PRESENT|US|RW, frames du handle), refcount des handles.
- **step 3** : `FB_ACQUIRE` retourne un `buffer_ptr` mappé : le kernel crée
  un SHM interne couvrant son `present_buf` et le mappe chez le caller.
- **step 4** : connecter `drivers::keyboard` et un futur driver souris à
  `gfx::input_queue::push_event` pour que `INPUT_POLL` retourne du vrai trafic.
- **step 5** : sortir le shell du kernel → réécrire en app user qui lit via
  IPC les events du display-server. Le kernel ne contient plus AUCUN code
  de présentation au-delà du driver scanout.

## Anti-patterns à proscrire

- ❌ Tout code kernel qui parle de `Window`, `Focus`, `ZOrder`, `Theme`.
- ❌ Tout process user qui fait du MMIO direct sur le framebuffer.
- ❌ Plus d'un process détenteur de FB simultanément (le test EBUSY garde).
