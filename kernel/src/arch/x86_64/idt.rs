// =============================================================================
// idt.rs — Interrupt Descriptor Table (Phase II).
//
// Évolutions vs Phase I :
//   - handlers page fault / GPF sur IST[1] (stack dédiée → survit à stack
//     overflow côté user)
//   - breakpoint INT3 accessible depuis ring 3 (DPL=3) pour debug user
//   - timer ISR naked → sauvegarde complète des registers → préemption
//   - vecteur 0x80 (128) = syscall "legacy" (INT 0x80) pour tests
// =============================================================================

use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::arch::x86_64::gdt::{DOUBLE_FAULT_IST_INDEX, GENERAL_FAULT_IST_INDEX};
use crate::arch::x86_64::pic::{InterruptIndex, PICS};
use crate::{print, println, serial_println};

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // --- Exceptions CPU ---
        // Breakpoint accessible depuis ring 3 : DPL=3
        idt.breakpoint.set_handler_fn(breakpoint_handler)
            .set_privilege_level(x86_64::PrivilegeLevel::Ring3);

        // SAFETY: index IST valide, stacks init dans gdt.rs
        unsafe {
            idt.page_fault
                .set_handler_fn(page_fault_handler)
                .set_stack_index(GENERAL_FAULT_IST_INDEX);

            idt.general_protection_fault
                .set_handler_fn(gpf_handler)
                .set_stack_index(GENERAL_FAULT_IST_INDEX);

            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(DOUBLE_FAULT_IST_INDEX);
        }

        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);

        // --- IRQ hardware (offset +32 après remap du PIC) ---
        // Timer : ISR préemptive via preempt_entry.asm (full state save).
        // Quand aucun process user n'est actif, timer_tick_rust fait juste
        // time::tick + EOI. Quand un process tourne, il peut reschedule.
        extern "C" { fn timer_preempt_entry(); }
        unsafe {
            idt[InterruptIndex::Timer.as_usize()]
                .set_handler_addr(x86_64::VirtAddr::new(timer_preempt_entry as u64));
        }
        idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Mouse.as_usize()].set_handler_fn(mouse_interrupt_handler);

        // --- INT 0x80 : syscall legacy (fallback / debug) ---
        idt[0x80].set_handler_fn(int80_handler)
            .set_privilege_level(x86_64::PrivilegeLevel::Ring3);

        idt
    };
}

pub fn init() {
    IDT.load();
}

// -----------------------------------------------------------------------------
// Handlers d'exceptions
// -----------------------------------------------------------------------------

extern "x86-interrupt" fn breakpoint_handler(stack: InterruptStackFrame) {
    println!("[EXCEPTION] BREAKPOINT @ {:#x}", stack.instruction_pointer.as_u64());
}

extern "x86-interrupt" fn double_fault_handler(
    stack: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack);
}

extern "x86-interrupt" fn gpf_handler(stack: InterruptStackFrame, error_code: u64) {
    // Si GPF vient de ring 3, on kill le process au lieu de panic.
    let from_user = (stack.code_segment & 3) == 3;
    if from_user {
        serial_println!("[gpf] ring 3 : {:#x} code={:#x} — SIGSEGV",
            stack.instruction_pointer.as_u64(), error_code);
        crate::task::process::current_fault_kill("GPF");
    }
    panic!("EXCEPTION: GENERAL PROTECTION FAULT (ring 0)\ncode={:#x}\n{:#?}",
        error_code, stack);
}

extern "x86-interrupt" fn page_fault_handler(
    stack: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    let addr = Cr2::read();
    let from_user = (stack.code_segment & 3) == 3;

    // Tentative de résolution CoW si la page est marquée read-only et que
    // l'erreur est une WRITE sur une page PRESENT.
    if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE)
        && error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION)
    {
        if crate::memory::cow::try_resolve_cow(addr).is_ok() {
            return;
        }
    }

    if from_user {
        serial_println!("[pf] ring 3 addr={:?} code={:?} rip={:#x} — SIGSEGV",
            addr, error_code, stack.instruction_pointer.as_u64());
        crate::task::process::current_fault_kill("PAGE_FAULT");
    }

    println!("[EXCEPTION] PAGE FAULT (ring 0)");
    println!("  addr accédée : {:?}", addr);
    println!("  code         : {:?}", error_code);
    println!("  {:#?}", stack);
    crate::arch::x86_64::hlt_loop();
}

extern "x86-interrupt" fn invalid_opcode_handler(stack: InterruptStackFrame) {
    let from_user = (stack.code_segment & 3) == 3;
    if from_user {
        serial_println!("[ud] ring 3 rip={:#x} — SIGILL", stack.instruction_pointer.as_u64());
        crate::task::process::current_fault_kill("INVALID_OPCODE");
    }
    panic!("EXCEPTION: INVALID OPCODE (ring 0) rip={:#x}",
        stack.instruction_pointer.as_u64());
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    stack: InterruptStackFrame,
    error_code: u64,
) {
    let from_user = (stack.code_segment & 3) == 3;
    if from_user {
        crate::task::process::current_fault_kill("STACK_SEGMENT_FAULT");
    }
    panic!("STACK SEGMENT FAULT code={:#x}\n{:#?}", error_code, stack);
}

// -----------------------------------------------------------------------------
// Handlers d'IRQ hardware
// -----------------------------------------------------------------------------

/// Hook Rust appelé par preempt_entry.asm à chaque IRQ0 (timer).
/// Responsabilités :
///   1. bump l'horloge monotonique + wake sleepers
///   2. si un process user tourne → déclenche reschedule()
///   3. EOI au PIC
#[no_mangle]
pub extern "C" fn timer_tick_rust(frame: *mut crate::task::preempt::TrapFrame) {
    crate::time::tick();
    crate::task::preempt::on_timer(frame);
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    crate::drivers::keyboard::add_scancode(scancode);
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack: InterruptStackFrame) {
    crate::drivers::mouse::on_irq();
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Mouse.as_u8());
    }
}

extern "x86-interrupt" fn int80_handler(stack: InterruptStackFrame) {
    // Récupère les args depuis les registres via x86_64 registers model_specific
    // n'est pas possible avec cette signature. On expose un chemin simplifié
    // pour debug depuis ring 0 uniquement.
    serial_println!("[int80] rip={:#x}", stack.instruction_pointer.as_u64());
}

// -----------------------------------------------------------------------------
// Test utilitaire
// -----------------------------------------------------------------------------

#[allow(dead_code)]
pub fn trigger_breakpoint() {
    serial_println!("[test] déclenchement breakpoint");
    x86_64::instructions::interrupts::int3();
}
