# Design notes — OS

## Décisions architecturales

### Kernel : identity-map low + private P3[0] par process

Le kernel vit à partir de 1 MiB, identity-mapé sur 1 GiB via huge pages
2 MiB (boot.asm). Pour chaque process userspace :

- `AddressSpace::new_user()` alloue une P4 privée
- P4[0] reçoit une **P3 privée deep-cloned** depuis la P3 kernel
- P4[1..512] shallow-clone (kernel high-half, pas utilisé pour l'instant)

Conséquence : userland doit être mappé à des adresses qui ne rentrent
pas dans la plage kernel identity (donc ≥ 1 GiB). Entry ELF à
`0x4000_0000` (1 GiB), stack user à `0x7fff_ffff_f000`.

Alternative : higher-half kernel à 0xFFFF_8000_0000_0000. Non fait —
préalable SMP/KASLR.

### Préemption

Timer PIT à 100 Hz → IRQ0 → `preempt_entry.asm` (full state save,
pas de swapgs) → `timer_tick_rust` → `preempt::on_timer(frame)`.
Si `cs & 3 == 3` (ring 3) → `process::reschedule(frame)` qui modifie la
trap frame en place pour pointer sur le process suivant + switch CR3.

### Exit + retour shell

Quand un process user `exit()` et qu'il n'y a plus de runnable,
`schedule_next()` restaure un contexte kernel sauvegardé via
`kernel_save_and_run` (asm) — idée `setjmp/longjmp`.

### CoW via bit OS_9

`PageTableFlags::BIT_9` (COW_MARKER) + refcount par frame
(`FrameRefcount` dense Vec<u8>). Le handler page fault détecte
`PROTECTION_VIOLATION | WRITE`, check COW_MARKER → duplique la frame si
refcount > 1, sinon juste unset le marker.

## Liste des bugs subtils rencontrés pendant Phase II/III

1. **PIT silencieux en Q35** → flag Makefile `-machine pc -no-hpet`
2. **Frame allocator non-zéro** → LLVM lit des entries garbage de page
   tables → #PF → triple fault. Fix : `write_bytes(0)` au moment de
   `allocate_frame`.
3. **Alloc 128 KiB sur stack 64 KiB** (rx ring e1000) → stack overflow
   triple fault. Fix : utiliser `vec!` au lieu de `Box::new([array])`.
4. **Bitmap frame allocator à 2 MiB** → écrase code kernel chargé à
   1 MiB quand il grossit. Fix : BITMAP_ADDR à 15 MiB.
5. **Target spec Rust sans `rustc-abi: x86-softfloat`** → "soft-float
   incompatible with ABI". Fix : ajouter ce champ.
6. **Preempt_entry avec swapgs avant GS_BASE init** → crash. Fix :
   version v2 sans swapgs, à réactiver quand syscall ring 3 stable.

## Prochains chantiers

- Historique commandes (ring buffer + ANSI up/down)
- DHCP (smoltcp `dhcpv4::Socket`)
- HTTP server minimal sur :8080
- Shell user en ring 3 (libc mini + fork/exec réel)
- SMP (AP boot via IPI + INIT/SIPI)
- Higher-half kernel
