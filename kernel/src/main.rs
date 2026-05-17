// =============================================================================
// main.rs — point d'entrée Rust du kernel (v5 — Phase II : userspace réel).
//
// Séquence d'init :
//   1. serial + vga                       (debug immédiat)
//   2. boot_info : parse multiboot2
//   3. GDT + TSS + IST (user segments inclus)
//   4. IDT (exceptions + IRQ + int 0x80 + préemption timer)
//   5. PIC remap 32..47
//   6. PIT à 100 Hz
//   7. heap allocator
//   8. frame allocator (bitmap)
//   9. paging mapper
//   10. clavier
//   11. fs + initrd
//   12. syscall MSRs + PerCpu (GS_BASE)
//   13. thread scheduler init
//   14. ACPI parse
//   15. Local APIC + I/O APICs
//   16. PCI enumeration
//   17. ATA PIO disk scan
//   18. interrupts ON
//   19. tests de fumée
//   20. executor : shell + tâches de démo
//   21. (à la demande via `exec`) : premier process user → iretq ring 3
// =============================================================================

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(const_mut_refs)]
#![allow(dead_code)]

extern crate alloc;

mod acpi;
mod arch;
mod boot_info;
mod crypto;
mod drivers;
mod fs;
mod gfx;
mod ipc;
mod memory;
mod net;
mod pci;
mod persist;
mod shell;
mod virtio;
mod syscall;
mod task;
mod tests;
mod time;
mod users;

use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn _start(multiboot_info_ptr: usize) -> ! {
    drivers::serial::init();
    serial_println!("\n[boot] _start, mb2 @ {:#x}", multiboot_info_ptr);

    serial_println!("[trace] parse mb2");
    let bi = unsafe { boot_info::BootInfo::from_addr(multiboot_info_ptr) };
    serial_println!("[trace] bootloader_name");
    if let Some(name) = bi.bootloader_name() {
        serial_println!("[boot] bootloader : {}", name);
    }
    serial_println!("[trace] cmdline");
    if let Some(cmd) = bi.cmdline() {
        if !cmd.is_empty() { serial_println!("[boot] cmdline   : {}", cmd); }
    }

    serial_println!("[trace] gdt init");
    arch::x86_64::gdt::init();
    serial_println!("[init] GDT+TSS+IST (user segments OK)");
    serial_println!("[trace] idt init");
    arch::x86_64::idt::init();
    serial_println!("[init] IDT (préemption timer + syscall int 0x80)");
    serial_println!("[trace] pic init");
    unsafe {
        let mut pics = arch::x86_64::pic::PICS.lock();
        pics.initialize();
        // Master : IRQ0 (timer), IRQ1 (keyboard), IRQ2 (cascade vers slave) = 0.
        // Slave  : tout masque en mode terminal.
        pics.write_masks(0b1111_1000, 0b1111_1111);
    }
    serial_println!("[init] PIC (IRQ 32..47 : timer+kbd enabled)");

    serial_println!("[trace] time init");
    time::init();

    serial_println!("[trace] heap init");
    memory::heap::init_heap().expect("heap init");
    serial_println!("[init] Heap {} KiB", memory::heap::HEAP_SIZE / 1024);

    serial_println!("[trace] frame allocator init");
    memory::frame_allocator::init(&bi);
    serial_println!("[trace] paging init");
    unsafe { memory::paging::init(); }

    // --- Framebuffer (remplace la VGA text mode dès que possible) ---
    serial_println!("[trace] framebuffer init");
    if let Some(fbi) = bi.framebuffer() {
        if let Err(e) = drivers::fb::init(&fbi) {
            serial_println!("[fb] init failed: {}", e);
        } else {
            drivers::console::init();
            println!("[init] Framebuffer {}x{}@{}bpp — console prête",
                fbi.width, fbi.height, fbi.bpp);
            // Splash
            banner();
        }
    } else {
        serial_println!("[fb] GRUB n'a pas fourni de framebuffer — fallback VGA text");
        println!("[fb] GRUB n'a pas fourni de framebuffer — fallback VGA text");
    }

    serial_println!("[trace] keyboard init");
    drivers::keyboard::init();
    serial_println!("[init] Keyboard queue");

    serial_println!("[trace] mouse init skipped (terminal mode)");

    serial_println!("[trace] fs init");
    fs::init(&bi);

    serial_println!("[trace] syscall init");
    syscall::init();

    serial_println!("[trace] thread scheduler init");
    task::thread::init_as_main();
    serial_println!("[init] Thread scheduler (main = tid 0)");

    serial_println!("[trace] acpi init");
    let _acpi_info = acpi::init();
    serial_println!("[trace] apic init (skipped — PIC reste maître)");
    // arch::x86_64::apic::init(_acpi_info);

    serial_println!("[trace] pci init");
    pci::init();

    // --- Tentative d'init virtio-gpu ---
    // Deux cas :
    //   1. -vga std  + virtio-gpu-pci : fb multiboot déjà OK, on bascule
    //      juste le backend en VirtioGpu pour utiliser la voie accélérée.
    //   2. -vga none + virtio-gpu-pci : pas de fb multiboot, on crée un Fb
    //      from scratch avec init_virtio_gpu (pure GPU).
    serial_println!("[trace] virtio-gpu init (optional)");
    match virtio::gpu::init() {
        Ok(()) => {
            let gpu_lock = virtio::gpu::GPU.get().expect("gpu init OK mais GPU vide");
            let (gw, gh) = { let g = gpu_lock.lock(); (g.width, g.height) };
            if drivers::fb::is_active() {
                // Cas 1 : on bascule juste le backend.
                if let Some(fb_lock) = drivers::fb::fb() {
                    let mut fb = fb_lock.lock();
                    fb.set_backend(drivers::fb::PresentBackend::VirtioGpu);
                }
                println!("[init] virtio-gpu actif (mode hybride, scanout {}x{})", gw, gh);
            } else {
                // Cas 2 : création pure GPU.
                match drivers::fb::init_virtio_gpu(gw, gh) {
                    Ok(()) => {
                        drivers::console::init();
                        println!("[init] virtio-gpu actif (mode pur, scanout {}x{})", gw, gh);
                        banner();
                    }
                    Err(e) => serial_println!("[fb] init_virtio_gpu fail: {}", e),
                }
            }
        }
        Err(e) => serial_println!("[virtio-gpu] skip: {}", e),
    }

    serial_println!("[trace] ata init");
    drivers::ata::init();

    serial_println!("[trace] phase III disk");
    if let Some(disks) = drivers::ata::disks() {
        if !disks.lock().is_empty() {
            drivers::part::init(0);
            match drivers::fat32::mount_first() {
                Ok(()) => println!("[init] FAT32 monté avec succès"),
                Err(e) => serial_println!("[init] FAT32: {}", e),
            }
            // État persistant (/etc/passwd, settings) : écrase les défauts
            // seedés par fs::init si un blob valide existe sur disque.
            serial_println!("[trace] persist load");
            if persist::load_into_ramfs() {
                serial_println!("[persist] état restauré");
            }
        }
    }

    serial_println!("[trace] e1000 init");
    drivers::e1000::init();
    serial_println!("[trace] net init");
    net::init();

    serial_println!("[trace] interrupts on");
    x86_64::instructions::interrupts::enable();
    {
        use x86_64::registers::rflags::{self, RFlags};
        let rf = rflags::read();
        serial_println!("[rflags] IF={} value={:#x}",
            rf.contains(RFlags::INTERRUPT_FLAG), rf.bits());
        // Dump PIC masks APRÈS initialize + unmask
        use x86_64::instructions::port::Port;
        let mut m1: Port<u8> = Port::new(0x21);
        let mut m2: Port<u8> = Port::new(0xA1);
        unsafe {
            serial_println!("[pic] mask1={:#010b} mask2={:#010b} (0 = enabled)",
                m1.read(), m2.read());
        }
    }
    println!("[init] Interrupts ON");

    serial_println!("[trace] tests");
    tests::run_all();

    serial_println!("[trace] executor start");

    let mut exec = task::executor::Executor::new();
    exec.spawn(task::Task::new(heartbeat()));
    exec.spawn(task::Task::new(net_poll_task()));
    exec.spawn(task::Task::new(net::http::serve()));
    exec.spawn(task::Task::new(status_bar_task()));
    exec.spawn(task::Task::new(shell::run()));
    exec.run();
}

/// Task async qui rafraîchit la status bar toutes les secondes.
async fn status_bar_task() {
    loop {
        time::sleep::sleep_ms(1000).await;
        if !drivers::console::is_ready() { continue; }
        let up = time::format_uptime();
        let (used, total) = memory::frame_allocator::FRAME_ALLOCATOR
            .lock().as_ref()
            .map(|a| a.stats()).unwrap_or((0, 0));
        let mem_mib_used = used * 4 / 1024;
        let mem_mib_total = total * 4 / 1024;
        let ip = net::ip_address()
            .map(|a| alloc::format!("{}", a))
            .unwrap_or_else(|| alloc::string::String::from("no-net"));
        let status = alloc::format!(
            "  Rust Kernel v0.6  |  up {}  |  mem {}M/{}M  |  ip {}  |  kernel>",
            up, mem_mib_used, mem_mib_total, ip,
        );
        drivers::console::draw_status_bar(&status);
    }
}

fn banner() {
    if drivers::console::is_ready() {
        drivers::console::set_colors(drivers::fb::CYAN, drivers::fb::BG);
    }
    println!("    ____             __     __ __                    __");
    println!("   / __ \\__  _______/ /_   / //_/__  _________  ___  / /");
    println!("  / /_/ / / / / ___/ __/  / ,< / _ \\/ ___/ __ \\/ _ \\/ /");
    println!(" / _, _/ /_/ (__  ) /_   / /| /  __/ /  / / / /  __/ /");
    println!("/_/ |_|\\__,_/____/\\__/  /_/ |_\\___/_/  /_/ /_/\\___/_/");
    if drivers::console::is_ready() {
        drivers::console::set_colors(drivers::fb::YELLOW, drivers::fb::BG);
    }
    println!("                           v0.6 - x86_64 Phase III");
    if drivers::console::is_ready() {
        drivers::console::set_colors(drivers::fb::WHITE, drivers::fb::BG);
    }
    println!(" multiboot2 paging async acpi apic pci ata");
    println!(" ELF64 fork(CoW) execve preempt signals");
    println!(" FAT32 e1000 TCP/IP (smoltcp) framebuffer");
    println!();
}

async fn heartbeat() {
    loop {
        time::sleep::sleep_ms(30_000).await;
        serial_println!("[hb] up {}", time::format_uptime());
    }
}

/// Tâche async qui poll le stack réseau toutes les ~50 ms.
async fn net_poll_task() {
    let mut last_stat = 0u64;
    loop {
        net::poll();

        let now = time::uptime_ms();
        if now.saturating_sub(last_stat) >= 2000 {
            last_stat = now;
            if let Some(nic) = drivers::e1000::nic() {
                let n = nic.lock();
                let (rctl, rdh, rdt, status, d0s, d0l) = n.rx_debug();
                serial_println!("[net] t={}ms rx={} tx={} link={} dhcp_ok={}",
                    now, n.rx_packets, n.tx_packets, n.link_up(), net::dhcp_configured());
                serial_println!("      RCTL={:#x} RDH={} RDT={} STATUS={:#x} desc[0]: status={:#x} len={}",
                    rctl, rdh, rdt, status, d0s, d0l);
            }
        }

        time::sleep::sleep_ms(50).await;
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // SAFETY: on accepte le risque de double-panic ; on veut pouvoir écrire
    // même si un lock VGA était tenu au moment du panic.
    unsafe {
        drivers::vga::WRITER.force_unlock();
        drivers::serial::SERIAL1.force_unlock();
    }
    serial_println!("\n╔══ PANIC ══════════════════════════════════════╗");
    serial_println!("{}", info);
    println!("\n╔══ PANIC ══════════════════════════════════════╗");
    println!("{}", info);
    arch::x86_64::backtrace::print();
    println!("╚═══════════════════════════════════════════════╝");
    arch::x86_64::hlt_loop();
}

#[alloc_error_handler]
fn alloc_error_handler(layout: alloc::alloc::Layout) -> ! {
    panic!("alloc error: {:?}", layout)
}
