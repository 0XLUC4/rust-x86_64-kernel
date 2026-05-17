# Rust Kernel — x86_64 / Multiboot2

Un kernel bare-metal en Rust, bootable via GRUB, avec une architecture comparable
(en miniature) à celle d'un vrai OS. **Phase III** : persistence FAT32 + réseau TCP/IP.

## Ce que ce kernel fait

**Boot & bas niveau**
- Header multiboot2 + pipeline GRUB → ISO bootable
- Boot asm 32 → 64 bits : vérif CPUID, long mode, paging identity 1 GiB via huge pages 2 MiB
- GDT propre + TSS + Interrupt Stack Table (stack dédiée double-fault)
- IDT complète : handlers pour breakpoint, page fault, GPF, double fault + IRQ timer/clavier
- PIC 8259 remappé (IRQ 32..47)

**Mémoire**
- Parser multiboot2 (memory map, cmdline, modules)
- Frame allocator bitmap (détecte la RAM réelle, réserve kernel/heap/bitmap/initrd)
- Paging virtuel : wrapper `OffsetPageTable` avec `map_to()`, `alloc_and_map()`, `translate()`
- Heap allocator (`linked_list_allocator`) : `Box`, `Vec`, `String` disponibles

**Horloge & concurrence**
- PIT à 100 Hz, horloge monotonique atomique
- `sleep_ms(n)` async (Future + liste de timers + wakers)
- Executor async coopératif avec `spawn()` global et `task_count()`
- Threads kernel avec context switch (callee-saved + RFLAGS), scheduler round-robin

**I/O & FS**
- Driver VGA text mode 80×25 avec macros `print!` / `println!`
- Driver port série COM1 (macros `serial_print!` / `serial_println!`) — debug QEMU
- Driver clavier PS/2 : ISR ultra-courte → queue lock-free → `KeyStream` async
- VFS minimal + ramfs (stockage plat `BTreeMap<String, Vec<u8>>`)
- Loader d'initrd (module multiboot) au format archive maison

**Syscalls & userspace (Phase II)**
- Init MSRs `STAR` / `LSTAR` / `SFMASK` + `EFER.SCE`
- Entry stub asm + dispatcher Rust (SYSCALL/SYSRET)
- GDT user segments (DPL=3) + TSS.rsp0 management
- ELF64 loader (PT_LOAD mapping avec USER_ACCESSIBLE)
- Process avec PageTable privée, PID allocator
- `fork()` avec **CoW** (clone P4, mark pages RO + bit OS_9)
- `execve()` (charge ELF depuis ramfs)
- Préemption : yield depuis le timer ISR (sauvegarde *tous* les regs)
- Signaux POSIX minimaux (SIGINT, SIGKILL, SIGTERM, SIGSEGV, SIGILL, SIGBUS)
- Syscalls : `write`, `read`, `exit`, `getpid`, `uptime`, `fs_read`, `fs_list`, `fork`, `exec`, `wait`, `kill`, `yield`, `sleep_ms`, `brk`

**Persistence (Phase III)**
- Parser MBR (4 entrées primaires, détection types FAT12/16/32, NTFS, Linux, GPT)
- Driver FAT32 read-only : BPB, chaîne FAT, répertoires 8.3 + VFAT LFN, lecture de fichiers
- Montage automatique de la première partition FAT32 au boot
- Commandes shell : `lspart`, `fatinfo`, `fatls`, `fatcat`

**Réseau (Phase III)**
- Driver Intel e1000 (82540EM) : PCI BAR0 MMIO, RX/TX descriptor rings, polling
- Stack TCP/IP via `smoltcp` : IPv4, ARP, ICMP, TCP, UDP
- Interface réseau avec IP statique 10.0.2.15/24 (QEMU user networking)
- Sockets TCP/UDP (connect, send, recv, close, bind)
- Commandes shell : `ifconfig`, `netstat`, `ping`

**Observabilité**
- Panic handler avec backtrace (parcours des frames via RBP)
- Tests de fumée au boot (heap, fs, frame alloc, timer, breakpoint)

**Shell interactif**
- `help` `ls` `cat` `echo` `write` `rm` — manipulation du FS
- `mem` `ps` `threads` `uptime` — introspection
- `sleep <ms>` `yield` — test concurrence
- `syscall <n>` — invocation directe
- `exec <path>` `psu` `killu` — process userspace
- `lspart` `fatinfo` `fatls` `fatcat` — disque & FAT32
- `ifconfig` `netstat` `ping` — réseau
- `clear` `panic` `bp` — utilitaires
- `acpi` `lsapic` `lspci` `disk` `read` — hardware

## Architecture

```
┌──────────────────────────────────────────────┐
│ GRUB → kernel à 1 MiB                        │
└─────────────────┬────────────────────────────┘
                  ▼
┌──────────────────────────────────────────────┐   boot/*.asm
│ boot.asm (32 bits)                           │
│   check multiboot2 · CPUID · long mode       │
│   setup P4/P3/P2 identity 1 GiB              │
│   CR4.PAE · EFER.LME · CR0.PG                │
│ boot_64.asm (64 bits)                        │
│   reset segments · call _start               │
└─────────────────┬────────────────────────────┘
                  ▼
┌──────────────────────────────────────────────┐   src/main.rs
│ _start (RDI = mb2 ptr)                       │
│   init par couches (16 étapes)               │
│   tests::run_all()                           │
│   executor.run() ←─ shell + heartbeat        │
└──────────────────────────────────────────────┘

src/
├── arch/x86_64/   gdt, idt, pic, apic, backtrace, percpu
├── memory/        heap, frame_allocator, paging, address_space, cow, frame_refcount
├── drivers/       vga, serial, keyboard, ata, part, fat32, e1000
├── task/          executor (async), thread (ctx switch), process, preempt, signal
├── fs/            ramfs, loader initrd, elf loader, userprog générateur
├── syscall/       MSRs + dispatcher (22 syscalls)
├── time/          PIT, monotonic clock, sleep
├── net/           smoltcp integration, socket API (TCP/UDP/ICMP)
├── shell/         commandes interactives (~35 commandes)
├── acpi/          RSDP, MADT, FADT
├── pci/           PCI enumeration
├── tests.rs       tests de fumée
└── boot_info.rs   parser multiboot2
```

## Build & run

### Prérequis (Debian/Ubuntu)
```bash
sudo apt install nasm xorriso grub-pc-bin grub-common mtools qemu-system-x86
```
Rust nightly + rust-src (géré automatiquement par `rust-toolchain.toml`).

### Lancer
```bash
make run          # build ISO + lance QEMU (avec disque si présent + NIC e1000)
make run-nodisk   # idem, sans disque
make run-fat      # crée un disque FAT32 de test + lance QEMU
make run-gdb      # idem, avec serveur GDB sur :1234
make clean
```

Pour créer un disque FAT32 de test manuellement :
```bash
make build/test.img
```

## Créer un initrd à charger

Le kernel supporte un module multiboot au format archive simple :

```
[4 bytes LE: nombre de fichiers N]
pour chaque fichier :
  [2 bytes LE: name_len]
  [name_len bytes: nom UTF-8]
  [4 bytes LE: data_len]
  [data_len bytes: data]
```

Ajoute-le dans `grub/grub.cfg` avec `module2 /boot/initrd.img "initrd"`.

## Pistes d'extension

**Proches (faisables en une session) :**
- Parser ELF basique pour charger un module
- Couleurs dynamiques VGA (ANSI-like)
- Commande `hexdump` dans le shell
- Éditeur de ligne avec historique dans le shell (flèches, Ctrl-R)

**Intermédiaires (quelques sessions) :**
- Ring 3 réel : user code segment, user stacks via TSS rsp0, mapping USER_ACCESSIBLE
- ELF loader + fork/exec
- Driver ATA PIO pour lire un vrai disque
- APIC au lieu du PIC (prérequis pour SMP)
- Préemption (yield depuis l'ISR timer)

**Grands chantiers :**
- SMP (boot des autres cores, IPIs)
- Réseau (driver e1000 + pile TCP/IP minimale — il existe `smoltcp`)
- FS persistant (FAT32 ou ext2 lecture)
- Signaux POSIX
- Virtual memory avancée (CoW fork, mmap avec swap, shared memory)

## Références

- [Writing an OS in Rust](https://os.phil-opp.com/) — la bible
- [OSDev Wiki](https://wiki.osdev.org/) — encyclopédie des pièges
- [Intel SDM vol 3](https://software.intel.com/en-us/articles/intel-sdm) — spec CPU
- [Multiboot2 spec](https://www.gnu.org/software/grub/manual/multiboot2/multiboot.html)

## Avertissement

Ce kernel est un projet pédagogique substantiel (~2500 lignes) mais **pas testé
sur le hardware réel** (développé et pensé pour QEMU). Il y aura très
probablement des ajustements à faire au premier build (versions de crates,
lints de lifetimes). Utilise-le comme base d'apprentissage et de modification,
pas comme produit fini.

## Phase III livrée. Bilan.

## [CORE] Phase III — Persistence + Réseau (livrée)

**Fichiers ajoutés (~1700 LOC)** :
- `src/drivers/part.rs` (~150 LOC) — MBR partition table parser (4 entrées primaires, types FAT12/16/32/NTFS/Linux/GPT)
- `src/drivers/fat32.rs` (~340 LOC) — FAT32 read-only : BPB, FAT chain traversal, 8.3 + VFAT LFN, path resolution
- `src/drivers/e1000.rs` (~350 LOC) — Intel 82540EM NIC : PCI BAR0 MMIO, RX/TX descriptor rings (64 entries), polling
- `src/net/mod.rs` (~150 LOC) — smoltcp integration, E1000Device trait impl, Interface config
- `src/net/socket.rs` (~120 LOC) — API sockets TCP/UDP (connect, send, recv, close, bind)

**Fichiers modifiés** :
- `src/drivers/mod.rs` — déclarations modules part, fat32, e1000
- `src/main.rs` — séquence d'init étendue (partition scan, FAT32 mount, e1000, net stack, net poll task), banner v0.6
- `src/shell/mod.rs` — commandes `lspart`, `fatinfo`, `fatls`, `fatcat`, `ifconfig`, `netstat`, `ping`
- `Cargo.toml` — ajout smoltcp
- `Makefile` — cibles FAT32 disk image + QEMU flags réseau + disque

## [SYSTEM] Ce qu'il se passera au boot

```
[init] GDT+TSS+IST (user segments OK)
[init] IDT (préemption timer + syscall int 0x80)
[init] PIC (IRQ 32..47)
[time] PIT 100 Hz
[mem]  heap, frames, paging
[init] Keyboard, FS, syscalls, thread scheduler
[acpi] RSDP rev=2 XSDT, 7 tables
[apic] LAPIC v0x50 enabled (id=0) @ 0xfee00000
[pci]  7+ device(s)
[ata]  Primary/Master : QEMU HARDDISK
[part] #0 FAT32 LBA LBA=2048 size=30 MiB  (active)
[fat32] FAT32 'RUSTDISK' : 29 MiB, cluster=4096 B, 8 spc, 2 FATs
[init] FAT32 monté avec succès
[e1000] trouvé à 00:03.0  devid=0x100e
[e1000] MAC 52:54:00:12:34:56  link=up
[net] interface up — IP 10.0.2.15/24 gw 10.0.2.2
[init] Interrupts ON
```

## [NEXT] Pistes d'extension restantes

**Proches (faisables en une session) :**
- FAT32 écriture (create, write, truncate)
- DHCP automatique (smoltcp le supporte)
- DNS résolveur via UDP
- Commande `wget` (TCP GET basique)

**Intermédiaires (quelques sessions) :**
- APIC complet au lieu du PIC (prérequis SMP)
- SMP (boot des autres cores, IPIs, per-core scheduler)
- NVMe driver (remplacement du ATA PIO)
- ext2 read-only

**Grands chantiers :**
- Virtual memory avancée (mmap, shared memory, demand paging)
- Sockets POSIX complets (select/poll/epoll)
- Shell user en ring 3 (avec allocator user + libc minimale)

Tu veux que j'enchaîne sur une fonctionnalité particulière ?
