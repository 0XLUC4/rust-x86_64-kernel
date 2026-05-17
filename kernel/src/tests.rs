// =============================================================================
// tests.rs — tests de fumée exécutés au boot après init.
//
// Pas de framework `#[test]` (ça nécessite custom_test_frameworks + une infra
// qui poll QEMU via isa-debug-exit). On fait simple : des fn appelées en
// séquence, chacune affiche PASS/FAIL sur le serial et la VGA.
// =============================================================================

use crate::{print, println, serial_println};

pub fn run_all() {
    println!("\n=== Tests de fumée ===");
    let mut passed = 0;
    let mut failed = 0;

    macro_rules! check {
        ($name:expr, $body:expr) => {{
            print!("  [ .. ] {:.<40}", $name);
            let ok: bool = $body;
            if ok {
                println!(" OK");
                serial_println!("[test] {} OK", $name);
                passed += 1;
            } else {
                println!(" FAIL");
                serial_println!("[test] {} FAIL", $name);
                failed += 1;
            }
        }};
    }

    check!("heap alloc", test_heap_alloc());
    check!("heap large", test_heap_large());
    check!("time monotonic", test_time_monotonic());
    check!("fs create/read", test_fs_roundtrip());
    check!("frame alloc", test_frame_alloc());
    check!("breakpoint", test_breakpoint());

    println!("=== {} passés / {} échoués ===\n", passed, failed);
}

fn test_heap_alloc() -> bool {
    use alloc::boxed::Box;
    let b = Box::new(42u64);
    *b == 42
}

fn test_heap_large() -> bool {
    use alloc::vec::Vec;
    // Alloue 10 KiB : doit tenir dans le heap de 100 KiB
    let v: Vec<u8> = alloc::vec![0xab; 10 * 1024];
    v.len() == 10 * 1024 && v[0] == 0xab && v[v.len() - 1] == 0xab
}

fn test_time_monotonic() -> bool {
    let a = crate::time::uptime_ms();
    // Busy wait un petit peu — le timer doit avancer car les interrupts
    // sont activées. Si on attend trop peu, risque de faux négatif. 30ms
    // donne 3 ticks à 100Hz.
    for _ in 0..5_000_000 { core::hint::spin_loop(); }
    let b = crate::time::uptime_ms();
    b > a
}

fn test_fs_roundtrip() -> bool {
    let mut fs = crate::fs::FS.lock();
    fs.create("/test_fs", b"hello world");
    let ok = matches!(fs.read("/test_fs"), Ok(ref v) if v == b"hello world");
    let _ = fs.remove("/test_fs");
    ok
}

fn test_frame_alloc() -> bool {
    use x86_64::structures::paging::FrameAllocator;
    let mut guard = crate::memory::frame_allocator::FRAME_ALLOCATOR.lock();
    let Some(fa) = guard.as_mut() else { return false; };
    // Alloue 4 frames, vérifie qu'elles sont distinctes
    let f1 = fa.allocate_frame();
    let f2 = fa.allocate_frame();
    let f3 = fa.allocate_frame();
    let f4 = fa.allocate_frame();
    let all = [f1, f2, f3, f4];
    if all.iter().any(|f| f.is_none()) { return false; }
    for i in 0..4 {
        for j in (i+1)..4 {
            if all[i] == all[j] { return false; }
        }
    }
    true
}

fn test_breakpoint() -> bool {
    // Si le handler breakpoint ne tue pas le CPU, ce test passe.
    x86_64::instructions::interrupts::int3();
    true
}
