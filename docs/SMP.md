# SMP — Symmetric Multi-Processing

État : **design figé, implémentation hors-scope sans boucle d'itération QEMU**.
Ce doc capture le plan pour SMP afin que la suite soit codable d'une traite.

## Pourquoi pas tout de suite

Booter les AP (Application Processors) demande :
- un trampoline 16-bit en mémoire basse (sous 1 MiB),
- l'envoi d'IPI INIT puis SIPI au LAPIC,
- la synchro entre BSP (Bootstrap Processor) et chaque AP via spinlock.

Tout dépend de timings réels (timers, ICR, READ_ICR-busy, etc.). Sans
boucle QEMU pour vérifier "tel AP est-il vivant ?", on écrit du code à
l'aveugle, prêt à triple-fault. Plan ci-dessous, code après QEMU setup.

## Architecture cible

```
                     ┌─────────────────────────────────────┐
                     │             BSP (CPU 0)             │
                     │  boot.asm + init kernel complet     │
                     │  smp::start_aps()                   │
                     └──────────────┬──────────────────────┘
                                    │ IPI INIT / SIPI
                                    ▼
              ┌──────────────────────────────────────────────┐
              │ APs (CPU 1..N) : trampoline 16b → 32b → 64b  │
              │   load shared kernel P4, set up per-CPU GS,  │
              │   call smp::ap_main() ─→ scheduler local     │
              └──────────────────────────────────────────────┘
```

## Étapes d'implémentation

### 1. Découverte (déjà partiellement en place)
- `acpi::madt()` énumère les LAPIC IDs des AP — déjà parsé dans
  `kernel/src/acpi/mod.rs`.
- Stocker dans `static AP_IDS: Mutex<Vec<u8>>`.

### 2. Trampoline AP
- Petit blob 16-bit positionné à une adresse fixe sub-1 MiB (typique :
  0x8000). Compilé par `nasm`, embarqué dans le kernel via `include_bytes!`.
- Le trampoline fait : passage 16→32→64 bits, chargement de la P4 kernel,
  saut vers `ap_entry` Rust.
- Convention : un struct `ApBootInfo` partagé en mémoire basse contient
  `(stack_top, cr3, ap_index)` pour chaque AP.

### 3. Envoi des IPIs
- BSP écrit dans le ICR (Interrupt Command Register) du LAPIC :
  `ICR_HI = (target_id << 24)`
  `ICR_LO = INIT | LEVEL | ASSERT` puis `INIT | LEVEL | DEASSERT`,
  puis `SIPI | (trampoline_page)` deux fois (spec Intel).
- Délais : 10 ms après INIT, 200 µs après chaque SIPI.

### 4. Per-CPU data
- `arch::x86_64::percpu` doit devenir per-cpu : tableau indexé par
  `cpu_id` (lu depuis `LAPIC.id` ou `cpuid leaf 0x0B`).
- `GS_BASE` chargé via `WRMSR(IA32_KERNEL_GS_BASE, &PERCPU[i])` à
  l'init de chaque AP.

### 5. Scheduler multi-CPU
- Aujourd'hui `process::runnable` est un `VecDeque<Pid>` unique.
- Cible : un runqueue par CPU (`Vec<Mutex<VecDeque<Pid>>>`), avec
  work-stealing simple : si runqueue locale vide, peek dans les autres.
- Le timer ISR appelle `reschedule()` sur le CPU courant uniquement.

### 6. Synchronisation
- `spin::Mutex` (déjà utilisé) reste correct car spinlocks.
- Sections critiques courtes : ok. Pour les longues sections (CR3 switch,
  paging table edits), envisager `irq-disabled` + `cli/sti`.
- IPI TLB shootdown : quand un CPU modifie une page partagée, broadcast
  un IPI vers les autres CPUs pour qu'ils invalident leur TLB.

## Files & functions à créer

| Fichier | Rôle |
|---|---|
| `kernel/src/smp/mod.rs` | API `start_aps()`, `ap_main()`, `cpu_count()`, `current_cpu()` |
| `kernel/src/smp/trampoline.asm` | Code 16→64-bit pour AP |
| `kernel/src/smp/ipi.rs` | Helpers `send_init(target)`, `send_sipi(target, vec)` |
| `kernel/src/arch/x86_64/percpu.rs` | étendre en `[PerCpu; MAX_CPUS]` |
| `kernel/src/task/process.rs` | runqueue par CPU + work stealing |

## Tests prévus

- `smp` shell cmd : liste les CPUs détectés + leur runqueue size.
- Lancer 8 process CPU-bound, vérifier répartition sur N cores.
- TLB shootdown : test qui mmap/unmap depuis CPU0 et vérifie que CPU1
  fault correctement.

## Pré-requis qui manquent

1. **Boucle QEMU** : `qemu-system-x86_64 -smp 4 ...` pour itérer.
2. **`nasm`** disponible pour compiler le trampoline.
3. **Une session de debugging dédiée** (avec `-s -S` + GDB) — minimum 4h
   pour le premier "AP en ring 0 répond au heartbeat".

Reporté tant qu'on développe sans hardware/QEMU stable.
