<div align="center">

# 🦀 rust-x86_64-kernel

**A from-scratch x86_64 operating system kernel, written in Rust.**

Boots on bare metal via GRUB. Ring 3 userspace, paging with copy-on-write `fork()`,
preemptive scheduler, FAT32, TCP/IP, framebuffer console — all written from zero, no `std`.

[![Rust](https://img.shields.io/badge/Rust-nightly-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Arch](https://img.shields.io/badge/arch-x86__64-blue)](https://en.wikipedia.org/wiki/X86-64)
[![Boot](https://img.shields.io/badge/boot-Multiboot2%20%2F%20GRUB-green)](https://www.gnu.org/software/grub/manual/multiboot2/multiboot.html)
[![License](https://img.shields.io/badge/license-MIT-lightgrey)](LICENSE)
[![LOC](https://img.shields.io/badge/code-~13k%20LOC-purple)]()
[![Status](https://img.shields.io/badge/status-educational-yellow)]()

</div>

---

## ✨ What it actually does

This isn't a toy that just prints "Hello, world". It's a real microkernel that:

- **Boots** from a Multiboot2-compliant bootloader (GRUB) into 64-bit long mode
- **Manages memory** with a bitmap frame allocator, 4-level paging, and a kernel heap
- **Runs userspace processes** in ring 3 with `fork()` (copy-on-write), `execve()`, signals
- **Schedules preemptively** — the timer IRQ rewrites the iret frame to switch tasks
- **Talks to hardware** — PCI enumeration, ATA disks, Intel e1000 NIC, PS/2 keyboard
- **Persists data** — FAT32 read-only over MBR partitions
- **Networks** — full IPv4 stack (ARP, ICMP, TCP, UDP) on top of a real NIC driver
- **Renders** — linear framebuffer console (1024×768×32), 8×16 pixel font, status bar
- **Synchronizes** — async executor + kernel threads + spin locks + atomic timers
- **Sandboxes** — separate page tables per process, USER_ACCESSIBLE bits, TSS rsp0

> ~13,000 lines of Rust, plus a handful of `nasm` boot stubs. No external kernel crates beyond the `x86_64` helpers and `smoltcp` for the network stack.

---

## 🎬 Demo (boot trace)

```text
[boot] _start, mb2 @ 0x9500
[init] GDT+TSS+IST (user segments OK)
[init] IDT (preemption timer + syscall int 0x80)
[init] PIC (IRQ 32..47)
[time] PIT 100 Hz
[mem]  heap 2 MiB, frame bitmap 0x00f00000, paging OK
[init] Keyboard, FS, syscalls, thread scheduler
[acpi] RSDP rev=2 XSDT, 7 tables
[apic] LAPIC v0x50 enabled (id=0) @ 0xfee00000
[pci]  7 device(s)
[ata]  Primary/Master : QEMU HARDDISK
[part] #0 FAT32 LBA=2048 size=30 MiB (active)
[fat32] 'RUSTDISK' : 29 MiB, cluster=4096 B, 8 spc, 2 FATs
[e1000] found @ 00:03.0 devid=0x100e  MAC 52:54:00:12:34:56  link=up
[net]   interface up — IP 10.0.2.15/24 gw 10.0.2.2
[init] Interrupts ON
[ok] tests::run_all passed (heap, fs, frame_alloc, timer, breakpoint)

> _
```

Try it: `make run` — QEMU boots straight to the shell.

---

## 🧠 Why this exists

I'm 16. I wanted to know what *really* happens between the BIOS handing off control and `printf("hello")`. So I wrote it.

Reading [Phil Opp's blog](https://os.phil-opp.com/) gets you a kernel that prints to VGA. This repo goes further: real ring 3, real fork, real disks, real NIC, real TCP — the parts where "toy kernel" stops being a toy and starts looking like the early pages of Tanenbaum.

If you're a recruiter, hiring manager, or systems engineer: skim [`kernel/src/`](kernel/src/). Every module is hand-written from the Intel SDM, the OSDev wiki, and the FAT32 spec — no copy-paste, no boilerplate generator.

---

## 🗺️ Architecture

```
┌──────────────────────────────────────────────────────────┐
│  GRUB → loads kernel ELF at 1 MiB (Multiboot2 header)    │
└─────────────────────────────┬────────────────────────────┘
                              ▼
┌──────────────────────────────────────────────────────────┐
│  boot.asm (32-bit)                                       │
│    verify multiboot2 magic · CPUID · enter long mode     │
│    P4/P3/P2 identity-map 1 GiB via 2 MiB huge pages      │
│  boot_64.asm (64-bit)                                    │
│    reset segments · call Rust _start(mb2_ptr)            │
└─────────────────────────────┬────────────────────────────┘
                              ▼
┌──────────────────────────────────────────────────────────┐
│  Rust kernel (src/main.rs)                               │
│  ┌──────────┬───────────┬──────────┬───────────────────┐ │
│  │  arch    │  memory   │  task    │     drivers       │ │
│  │ gdt/idt  │ heap      │ exec     │ vga,serial,kbd    │ │
│  │ apic/pic │ paging    │ thread   │ ata,part,fat32    │ │
│  │ percpu   │ frames    │ process  │ e1000 (NIC)       │ │
│  │ backtrace│ cow       │ preempt  │ virtio-gpu (WIP)  │ │
│  └──────────┴───────────┴──────────┴───────────────────┘ │
│  ┌──────────┬───────────┬──────────┬───────────────────┐ │
│  │ syscall  │   fs      │   net    │      shell        │ │
│  │ MSR sysc │ ramfs     │ smoltcp  │ ~35 commands      │ │
│  │ int 0x80 │ initrd    │ TCP/UDP  │ ls/cat/ping/...   │ │
│  │ 22 calls │ elf64 ld  │ sockets  │                   │ │
│  └──────────┴───────────┴──────────┴───────────────────┘ │
└──────────────────────────────────────────────────────────┘
                              ▼
                         ring 3 userspace
                         (ELF processes — fork/exec/signals)
```

---

## 🧩 Features in depth

### Boot & low-level
- Multiboot2 header + boot pipeline → bootable ISO via GRUB
- 32 → 64-bit transition: CPUID checks, long mode, identity-paging 1 GiB via 2 MiB huge pages
- Clean GDT + TSS + Interrupt Stack Table (dedicated stack for double faults)
- Full IDT: breakpoint, page fault, GPF, double fault, timer/keyboard IRQs
- 8259 PIC remapped to vectors 32..47

### Memory management
- Multiboot2 parser (memory map, cmdline, modules)
- Bitmap frame allocator (detects real RAM, reserves kernel/heap/bitmap/initrd zones)
- 4-level paging wrapper around `OffsetPageTable` with `map_to()`, `alloc_and_map()`, `translate()`
- Linked-list heap allocator → `Box`, `Vec`, `String`, async tasks all work
- **Copy-on-write `fork()`**: clones P4, marks shared pages read-only with `PT_OS_9` bit, page-fault handler duplicates the frame on first write
- Per-frame refcount (dense `Vec<u8>`)

### Concurrency & time
- PIT @ 100 Hz, atomic monotonic clock
- `sleep_ms(n)` as a real async future (timer queue + wakers)
- Cooperative async executor with `spawn()` and `task_count()`
- Kernel threads with callee-saved + RFLAGS context switch, round-robin scheduler
- **Preemption** from the timer ISR, including ring 3 → reschedules by rewriting the trap frame in place

### I/O & filesystem
- VGA text mode 80×25 + linear framebuffer (1024×768×32) with 8×16 pixel font
- COM1 serial driver (for QEMU debug logs)
- PS/2 keyboard: short ISR → lock-free queue → async `KeyStream`
- VFS + ramfs (`BTreeMap<String, Vec<u8>>`)
- Initrd loader (Multiboot module, custom archive format)
- MBR partition parser (4 primary entries, FAT12/16/32/NTFS/Linux/GPT detection)
- **FAT32 read-only**: BPB, FAT chain traversal, 8.3 + VFAT LFN, path resolution

### Userspace (ring 3)
- MSRs `STAR` / `LSTAR` / `SFMASK` + `EFER.SCE` initialized for SYSCALL/SYSRET
- Asm entry stub + Rust dispatcher
- User GDT segments (DPL=3), `TSS.rsp0` management
- ELF64 loader (`PT_LOAD` mapped with USER_ACCESSIBLE)
- Process struct, private page table, PID allocator
- `fork()` (CoW), `execve()` (loads ELF from ramfs)
- POSIX signals: SIGINT, SIGKILL, SIGTERM, SIGSEGV, SIGILL, SIGBUS
- 22 syscalls: `write`, `read`, `exit`, `getpid`, `uptime`, `fs_read`, `fs_list`, `fork`, `exec`, `wait`, `kill`, `yield`, `sleep_ms`, `brk`, `fb_*`, `shm_*`, `ipc_*`

### Networking
- Intel e1000 (82540EM) driver: PCI BAR0 MMIO, RX/TX descriptor rings (64 entries), polling
- Full IPv4 stack via `smoltcp`: ARP, ICMP, TCP, UDP
- Static IP `10.0.2.15/24` (QEMU user-mode networking)
- Socket API: `connect`, `send`, `recv`, `close`, `bind`
- Shell commands: `ifconfig`, `netstat`, `ping`

### Graphics & IPC (Phase V, in progress)
- Linear framebuffer console with status bar (uptime, mem, tasks)
- Stable ABI crate (`abi/`) for kernel ↔ user contract — `FbInfo`, `Rect`, `InputEvent`, `ShmHandle`, `IpcHeader`
- Syscalls 40..47 reserved for `FB_ACQUIRE`, `FB_PRESENT`, `INPUT_POLL`, `SHM_*`, `IPC_*`
- Long-term plan: move the shell *out* of the kernel, into a ring 3 display-server

### Observability
- Panic handler with backtrace (frame walk via RBP)
- Boot-time smoke tests: heap, FS, frame allocator, timer, breakpoint

### Shell (~35 commands)
| Group | Commands |
|---|---|
| FS | `help` `ls` `cat` `echo` `write` `rm` |
| Sys | `mem` `ps` `threads` `uptime` `clear` `panic` `bp` |
| Concurrency | `sleep <ms>` `yield` `syscall <n>` |
| Userspace | `exec <path>` `psu` `killu` |
| Disk | `lspart` `fatinfo` `fatls` `fatcat` `disk` `read` |
| Network | `ifconfig` `netstat` `ping` |
| Hardware | `acpi` `lsapic` `lspci` |

---

## 🚀 Build & run

### Prerequisites (Debian / Ubuntu / WSL)

```bash
sudo apt install nasm xorriso grub-pc-bin grub-common mtools \
                 qemu-system-x86 dosfstools build-essential
rustup component add rust-src llvm-tools-preview
```

Rust nightly + `rust-src` are pinned in `rust-toolchain.toml` — `rustup` picks them up automatically.

### Run

```bash
make run          # build kernel + userland, boot in QEMU (with disk + e1000 NIC)
make run-nodisk   # same, no FAT32 disk attached
make run-fat      # generate a test FAT32 image, then boot
make run-gpu      # virtio-gpu accelerated framebuffer
make run-gdb      # boot with GDB server on :1234
make clean
```

### Project layout

```
OS/
├── kernel/        bare-metal Rust kernel (boot asm + src/)
├── userland/      ring 3 programs (init, sh) sharing the ulib crate
├── abi/           stable kernel ↔ user ABI (repr(C) types)
├── tools/         build scripts (FAT32 disk image generator, etc.)
└── docs/          design notes, phase write-ups
```

---

## 🛣️ Roadmap

**Done**
- [x] **Phase I** — modern hardware: ACPI + APIC, PCI enum, ATA PIO
- [x] **Phase II** — ring 3 userspace: ELF64 loader, fork (CoW), execve, signals, SYSCALL/SYSRET
- [x] **Phase III** — persistence & network: MBR + FAT32 read-only, e1000, TCP/IP via smoltcp
- [x] **Phase IV** — UX: 1024×768 framebuffer, pixel console, status bar, ring 3 preemption

**In progress**
- [ ] **Phase V** — graphics ABI: SHM, IPC, display-server in ring 3 (shell exits the kernel)

**Future**
- [ ] SMP (boot APs via IPI INIT/SIPI, per-core scheduler)
- [ ] DHCP + DNS resolver, minimal HTTP server
- [ ] FAT32 write support, ext2 read-only
- [ ] Higher-half kernel + KASLR
- [ ] NVMe driver
- [ ] POSIX-ish libc (so real userspace programs compile)

---

## 🐛 Bugs I hit (and what they taught me)

These wrecked entire weekends — leaving them here because the lessons are the whole point of writing a kernel:

| Symptom | Root cause | Fix |
|---|---|---|
| PIT silent on Q35 chipset | HPET wins arbitration | QEMU flag `-machine pc -no-hpet` |
| Triple fault on first paging op | Frame allocator returned non-zeroed memory; LLVM read garbage PTEs | `write_bytes(0)` inside `allocate_frame` |
| Triple fault setting up e1000 RX ring | Allocating 128 KiB on a 64 KiB stack | Use `vec!` instead of `Box::new([_; N])` |
| Random kernel corruption after a few MiB of allocs | Frame bitmap at 2 MiB overlapped kernel image | Move `BITMAP_ADDR` to 15 MiB |
| `"soft-float incompatible with ABI"` | Custom target spec missing `rustc-abi: x86-softfloat` | Added the field |
| Crash on first preempt from ring 3 | `swapgs` before `GS_BASE` was initialized | Preempt entry v2 without `swapgs` until syscall path is stable |

---

## 📚 References

- [Writing an OS in Rust](https://os.phil-opp.com/) — the bible
- [OSDev Wiki](https://wiki.osdev.org/) — every footgun, documented
- [Intel SDM Vol. 3](https://software.intel.com/en-us/articles/intel-sdm) — the CPU spec
- [Multiboot2 spec](https://www.gnu.org/software/grub/manual/multiboot2/multiboot.html)
- [smoltcp](https://github.com/smoltcp-rs/smoltcp) — embedded TCP/IP

---

## ⚠️ Status

Educational. Developed and tested on QEMU. Has not been booted on real hardware — expect adjustments (timer calibration, NIC quirks, ACPI variations). Use it as a base for learning and hacking, not a shipping OS.

---

## 📄 License

[MIT](LICENSE) © 2026 Luca Severino

---

<div align="center">

**Built by a 16-year-old who wanted to know what happens before `main()`.**

If you're hiring systems engineers, get in touch: [severino.luc4@gmail.com](mailto:severino.luc4@gmail.com)

</div>
