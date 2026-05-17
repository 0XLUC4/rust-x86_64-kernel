// =============================================================================
// gdt.rs — Global Descriptor Table (Phase II : ring 3 + syscall).
//
// Layout imposé par l'instruction SYSRET : le CPU, en sortie de syscall,
// force les sélecteurs à (STAR[63:48] + 16 | 3) pour CS user et
// (STAR[63:48] + 8 | 3) pour SS user. Il faut donc un ordre GDT tel que :
//
//   STAR[63:48] = sélecteur de user_data - 8
//   user_data   = user_code - 8
//   user_code   = STAR[63:48] + 16
//
// L'ordre natural chez x86_64 crate (ring-3 code APRES ring-3 data) est :
//
//   0x08  kernel_code    (ring 0)
//   0x10  kernel_data    (ring 0)
//   0x1b  user_data      (ring 3, sel base = 0x18)
//   0x23  user_code      (ring 3, sel base = 0x20)
//   ...   tss
//
// On expose aussi :
//   - `set_kernel_stack(rsp)` pour TSS.rsp0 (stack kernel sur transition 3→0)
//   - les sélecteurs `KERNEL_CS`, `KERNEL_DS`, `USER_CS`, `USER_DS`
//
// Interrupt Stack Table :
//   IST[0] = double fault
//   IST[1] = general protection fault / page fault (utile si stack user corrompue)
// =============================================================================

use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const GENERAL_FAULT_IST_INDEX: u16 = 1;

const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5;
const GENERAL_FAULT_STACK_SIZE: usize = 4096 * 5;
const KERNEL_ENTRY_STACK_SIZE: usize = 4096 * 4;

// Stacks statiques pour les handlers d'exception critiques.
// Les accès passent par UnsafeCell pour permettre l'écriture côté TSS
// tout en gardant l'adresse connue à la compilation.
#[repr(align(16))]
struct AlignedStack<const N: usize>([u8; N]);

static mut DOUBLE_FAULT_STACK: AlignedStack<DOUBLE_FAULT_STACK_SIZE> =
    AlignedStack([0; DOUBLE_FAULT_STACK_SIZE]);
static mut GENERAL_FAULT_STACK: AlignedStack<GENERAL_FAULT_STACK_SIZE> =
    AlignedStack([0; GENERAL_FAULT_STACK_SIZE]);

/// Stack initiale pour les entrées syscall / IRQ depuis ring 3.
/// Le scheduler la remplace par la stack kernel du thread courant
/// via `set_kernel_stack()` à chaque commutation.
static mut KERNEL_ENTRY_STACK: AlignedStack<KERNEL_ENTRY_STACK_SIZE> =
    AlignedStack([0; KERNEL_ENTRY_STACK_SIZE]);

/// TSS global (singleton). On le loggue en `static mut` pour pouvoir
/// modifier `rsp[0]` à la volée. L'accès est sérialisé par `KSTACK_LOCK`.
static mut TSS: TaskStateSegment = TaskStateSegment::new();
static KSTACK_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy)]
pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub user_data: SegmentSelector,
    pub user_code: SegmentSelector,
    pub tss: SegmentSelector,
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        // SAFETY: initialisation du TSS avant chargement. On fixe les 3 stacks
        // connues, rsp0 sera réécrit par `set_kernel_stack()` au premier spawn.
        unsafe {
            let ds_start = VirtAddr::from_ptr(&DOUBLE_FAULT_STACK);
            TSS.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
                ds_start + DOUBLE_FAULT_STACK_SIZE;

            let gf_start = VirtAddr::from_ptr(&GENERAL_FAULT_STACK);
            TSS.interrupt_stack_table[GENERAL_FAULT_IST_INDEX as usize] =
                gf_start + GENERAL_FAULT_STACK_SIZE;

            let ke_start = VirtAddr::from_ptr(&KERNEL_ENTRY_STACK);
            TSS.privilege_stack_table[0] = ke_start + KERNEL_ENTRY_STACK_SIZE;
        }

        let mut gdt = GlobalDescriptorTable::new();
        let kernel_code = gdt.add_entry(Descriptor::kernel_code_segment());
        let kernel_data = gdt.add_entry(Descriptor::kernel_data_segment());
        let user_data   = gdt.add_entry(Descriptor::user_data_segment());
        let user_code   = gdt.add_entry(Descriptor::user_code_segment());
        // SAFETY: réf immutable au TSS static mut ; il est rempli ci-dessus et
        // les seuls écrivains ensuite sont `set_kernel_stack()` sous KSTACK_LOCK.
        let tss = gdt.add_entry(Descriptor::tss_segment(unsafe { &TSS }));
        (gdt, Selectors { kernel_code, kernel_data, user_data, user_code, tss })
    };
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    // SAFETY: sélecteurs issus de la GDT chargée à la ligne précédente.
    unsafe {
        CS::set_reg(GDT.1.kernel_code);
        DS::set_reg(GDT.1.kernel_data);
        ES::set_reg(GDT.1.kernel_data);
        SS::set_reg(GDT.1.kernel_data);
        load_tss(GDT.1.tss);
    }
}

/// Retourne les sélecteurs. Utilisé par syscall::init() et par l'exec
/// userspace pour construire l'iretq frame.
pub fn selectors() -> &'static Selectors { &GDT.1 }

/// Remplace TSS.rsp0 — la stack utilisée par le CPU lors d'une transition
/// ring 3 → ring 0 (interrupt ou exception ; SYSCALL, lui, ne change pas
/// automatiquement de stack — on le fait à la main dans syscall_entry.asm).
///
/// À appeler par le scheduler à chaque context switch entre threads qui
/// ont une "kernel stack" distincte.
pub fn set_kernel_stack(rsp: VirtAddr) {
    let _g = KSTACK_LOCK.lock();
    // SAFETY: sérialisé par KSTACK_LOCK ; écriture d'un u64 aligné.
    unsafe {
        TSS.privilege_stack_table[0] = rsp;
    }
}

/// Valeur courante de TSS.rsp0 (utile pour swapgs / re-entry).
pub fn kernel_stack() -> VirtAddr {
    let _g = KSTACK_LOCK.lock();
    // SAFETY: lecture sérialisée par KSTACK_LOCK.
    unsafe { TSS.privilege_stack_table[0] }
}
