// =============================================================================
// userprog.rs — génération programmatique d'un ELF64 "hello_user" minimal.
//
// On construit un binaire ELF64 complet en mémoire, avec :
//   - Ehdr (elf header)
//   - 1 Phdr (PT_LOAD) couvrant le segment [.text+.data]
//   - Code asm équivalent à :
//       write(1, "Hello from ring 3!\n", 19)
//       exit(0)
//
// Syscalls utilisés (voir syscall/mod.rs) :
//   nr=1 (WRITE) : rdi=fd, rsi=buf, rdx=len
//   nr=3 (EXIT)  : rdi=code
//
// Code x86_64 (placé à virtual addr 0x40_0000, 4 KiB donc 1 page) :
//   mov rax, 1               ; B8 01 00 00 00  (en 32-bit) -> on utilise mov r64
//   mov rdi, 1               ; BF 01 00 00 00
//   lea rsi, [rip + msg]
//   mov rdx, <len>
//   syscall
//   mov rax, 3
//   xor rdi, rdi
//   syscall
//   jmp $                    ; EB FE (safety)
//
// On émet le bytecode directement (opcodes déterminés à la main).
// =============================================================================

use alloc::vec::Vec;

const ELF_CLASS64: u8 = 2;
const ELF_DATA_LSB: u8 = 1;
const ELF_TYPE_EXEC: u16 = 2;
const ELF_MACHINE_X86_64: u16 = 62;

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

const EHDR_SIZE: u64 = 64;
const PHDR_SIZE: u64 = 56;

/// Adresse de chargement — >= 1 GiB pour ne pas collisionner l'identity kernel
/// bas-niveau. Voir commentaire dans address_space::new_user.
const LOAD_ADDR: u64 = 0x4000_0000;

pub fn hello_world_elf() -> Vec<u8> {
    let msg: &[u8] = b"Hello from ring 3!\n";
    let msg_len = msg.len() as u32;

    // --- Code machine x86_64 (SysV ABI, syscall nr in RAX) ---
    // Instructions :
    //   mov rax, 1                      B8 01 00 00 00
    //   mov rdi, 1                      BF 01 00 00 00
    //   lea rsi, [rip + MSG_OFFSET]     48 8D 35 xx xx xx xx
    //   mov rdx, <msg_len>              BA xx xx xx xx
    //   syscall                         0F 05
    //   mov rax, 3                      B8 03 00 00 00
    //   xor rdi, rdi                    48 31 FF
    //   syscall                         0F 05
    //   hlt / jmp $                     EB FE
    //
    // On calcule MSG_OFFSET dynamiquement : msg placé juste après le code.

    let mut code = Vec::<u8>::new();
    // mov eax, 1
    code.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
    // mov edi, 1
    code.extend_from_slice(&[0xBF, 0x01, 0x00, 0x00, 0x00]);
    // lea rsi, [rip + disp32]  (REX.W + 8D /r, disp32 = MSG_OFFSET - (lea_end))
    let lea_start = code.len();
    code.extend_from_slice(&[0x48, 0x8D, 0x35, 0, 0, 0, 0]);
    let lea_end = code.len();
    let lea_disp_offset = lea_start + 3; // offset of disp32 within the instr
    let _ = (lea_end, lea_disp_offset);

    // mov edx, msg_len
    code.push(0xBA);
    code.extend_from_slice(&msg_len.to_le_bytes());

    // syscall
    code.extend_from_slice(&[0x0F, 0x05]);

    // mov eax, 3
    code.extend_from_slice(&[0xB8, 0x03, 0x00, 0x00, 0x00]);
    // xor edi, edi
    code.extend_from_slice(&[0x31, 0xFF]);
    // syscall
    code.extend_from_slice(&[0x0F, 0x05]);
    // jmp $ (infinite loop safety)
    code.extend_from_slice(&[0xEB, 0xFE]);

    // Place le message à la fin du code
    let msg_offset_in_segment = code.len();
    code.extend_from_slice(msg);

    // Patch LEA disp32 : msg est à code[msg_offset_in_segment], la lea est à
    // code[lea_start], lea_end = lea_start+7. disp32 = msg_offset - lea_end.
    let disp = (msg_offset_in_segment as i32) - (lea_end as i32);
    code[lea_disp_offset..lea_disp_offset+4].copy_from_slice(&disp.to_le_bytes());

    // --- Assemble le fichier ELF ---
    let ph_off = EHDR_SIZE;
    let code_off = EHDR_SIZE + PHDR_SIZE;
    let entry_vaddr = LOAD_ADDR + code_off;
    let seg_vaddr = LOAD_ADDR;
    let seg_filesz = code_off as usize + code.len();

    let mut out = Vec::<u8>::with_capacity(seg_filesz);

    // Ehdr
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
    out.push(ELF_CLASS64);
    out.push(ELF_DATA_LSB);
    out.push(1);                 // EI_VERSION = 1
    out.push(0);                 // EI_OSABI = 0 (System V)
    out.push(0);                 // EI_ABIVERSION
    out.extend_from_slice(&[0u8; 7]); // padding
    out.extend_from_slice(&ELF_TYPE_EXEC.to_le_bytes());        // e_type
    out.extend_from_slice(&ELF_MACHINE_X86_64.to_le_bytes());   // e_machine
    out.extend_from_slice(&1u32.to_le_bytes());                 // e_version
    out.extend_from_slice(&entry_vaddr.to_le_bytes());          // e_entry
    out.extend_from_slice(&ph_off.to_le_bytes());               // e_phoff
    out.extend_from_slice(&0u64.to_le_bytes());                 // e_shoff
    out.extend_from_slice(&0u32.to_le_bytes());                 // e_flags
    out.extend_from_slice(&(EHDR_SIZE as u16).to_le_bytes());   // e_ehsize
    out.extend_from_slice(&(PHDR_SIZE as u16).to_le_bytes());   // e_phentsize
    out.extend_from_slice(&1u16.to_le_bytes());                 // e_phnum
    out.extend_from_slice(&0u16.to_le_bytes());                 // e_shentsize
    out.extend_from_slice(&0u16.to_le_bytes());                 // e_shnum
    out.extend_from_slice(&0u16.to_le_bytes());                 // e_shstrndx

    // Phdr (PT_LOAD, rwx)
    out.extend_from_slice(&PT_LOAD.to_le_bytes());                          // p_type
    out.extend_from_slice(&(PF_R | PF_W | PF_X).to_le_bytes());             // p_flags
    out.extend_from_slice(&0u64.to_le_bytes());                             // p_offset
    out.extend_from_slice(&seg_vaddr.to_le_bytes());                        // p_vaddr
    out.extend_from_slice(&seg_vaddr.to_le_bytes());                        // p_paddr
    out.extend_from_slice(&(seg_filesz as u64).to_le_bytes());              // p_filesz
    out.extend_from_slice(&(seg_filesz as u64).to_le_bytes());              // p_memsz
    out.extend_from_slice(&0x1000u64.to_le_bytes());                        // p_align

    // Code + data
    out.extend_from_slice(&code);

    out
}

/// Second binaire user : boucle qui affiche un `.` toutes les ~50M cycles
/// jusqu'à ce qu'il ait écrit 20 caractères, puis exit(0).
/// Utile pour voir la préemption timer à l'œuvre : le shell doit rester réactif
/// pendant que ce process tourne.
pub fn counter_elf() -> Vec<u8> {
    let msg: &[u8] = b".";

    // Registres utilisés :
    //   RBX = compteur restant (20)
    //   RCX = compteur interne boucle busy-wait
    let mut code = Vec::<u8>::new();

    // mov rbx, 20       (48 C7 C3 14 00 00 00)
    code.extend_from_slice(&[0x48, 0xC7, 0xC3, 0x14, 0x00, 0x00, 0x00]);

    let loop_start = code.len();

    // busy-wait : mov rcx, 5000000 ; loop: dec rcx ; jnz loop
    // mov rcx, 0x004c4b40 = 5M
    code.extend_from_slice(&[0x48, 0xC7, 0xC1, 0x40, 0x4B, 0x4C, 0x00]);
    let busy_start = code.len();
    // dec rcx : 48 FF C9
    code.extend_from_slice(&[0x48, 0xFF, 0xC9]);
    // jnz busy_start : 75 fb (court)
    let jmp_offset = (busy_start as i32 - (code.len() as i32 + 2)) as i8;
    code.push(0x75);
    code.push(jmp_offset as u8);

    // write(1, msg, 1)
    //   mov eax, 1
    code.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
    //   mov edi, 1
    code.extend_from_slice(&[0xBF, 0x01, 0x00, 0x00, 0x00]);
    //   lea rsi, [rip + msg]  — patch à la fin
    let lea_start = code.len();
    code.extend_from_slice(&[0x48, 0x8D, 0x35, 0, 0, 0, 0]);
    let lea_end = code.len();
    //   mov edx, 1
    code.extend_from_slice(&[0xBA, 0x01, 0x00, 0x00, 0x00]);
    //   syscall
    code.extend_from_slice(&[0x0F, 0x05]);

    // dec rbx
    code.extend_from_slice(&[0x48, 0xFF, 0xCB]);
    // jnz loop_start — utilise une jump 32-bit car offset peut être loin
    // jnz rel32 : 0F 85 xx xx xx xx
    let jnz_end = code.len() + 6;
    let rel32 = (loop_start as i32) - (jnz_end as i32);
    code.extend_from_slice(&[0x0F, 0x85]);
    code.extend_from_slice(&rel32.to_le_bytes());

    // newline : write(1, "\n", 1)
    code.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
    code.extend_from_slice(&[0xBF, 0x01, 0x00, 0x00, 0x00]);
    // lea rsi, [rip + nl_offset] — patch plus tard (pointera sur '\n' après msg)
    let nl_lea_start = code.len();
    code.extend_from_slice(&[0x48, 0x8D, 0x35, 0, 0, 0, 0]);
    let nl_lea_end = code.len();
    code.extend_from_slice(&[0xBA, 0x01, 0x00, 0x00, 0x00]);
    code.extend_from_slice(&[0x0F, 0x05]);

    // exit(0)
    code.extend_from_slice(&[0xB8, 0x03, 0x00, 0x00, 0x00]);
    code.extend_from_slice(&[0x48, 0x31, 0xFF]);
    code.extend_from_slice(&[0x0F, 0x05]);
    code.extend_from_slice(&[0xEB, 0xFE]);

    // Data : "." puis "\n"
    let msg_off = code.len();
    code.extend_from_slice(msg);
    let nl_off = code.len();
    code.push(b'\n');

    // Patch LEA de "."
    let disp = (msg_off as i32) - (lea_end as i32);
    code[lea_start+3..lea_start+7].copy_from_slice(&disp.to_le_bytes());
    // Patch LEA de "\n"
    let disp_nl = (nl_off as i32) - (nl_lea_end as i32);
    code[nl_lea_start+3..nl_lea_start+7].copy_from_slice(&disp_nl.to_le_bytes());

    // --- ELF wrapper (identique à hello_world_elf) ---
    let ph_off = EHDR_SIZE;
    let code_off = EHDR_SIZE + PHDR_SIZE;
    let entry_vaddr = LOAD_ADDR + code_off;
    let seg_vaddr = LOAD_ADDR;
    let seg_filesz = code_off as usize + code.len();

    let mut out = Vec::<u8>::with_capacity(seg_filesz);
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F']);
    out.push(ELF_CLASS64);
    out.push(ELF_DATA_LSB);
    out.push(1);
    out.push(0);
    out.push(0);
    out.extend_from_slice(&[0u8; 7]);
    out.extend_from_slice(&ELF_TYPE_EXEC.to_le_bytes());
    out.extend_from_slice(&ELF_MACHINE_X86_64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&entry_vaddr.to_le_bytes());
    out.extend_from_slice(&ph_off.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(EHDR_SIZE as u16).to_le_bytes());
    out.extend_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    out.extend_from_slice(&PT_LOAD.to_le_bytes());
    out.extend_from_slice(&(PF_R | PF_W | PF_X).to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&seg_vaddr.to_le_bytes());
    out.extend_from_slice(&seg_vaddr.to_le_bytes());
    out.extend_from_slice(&(seg_filesz as u64).to_le_bytes());
    out.extend_from_slice(&(seg_filesz as u64).to_le_bytes());
    out.extend_from_slice(&0x1000u64.to_le_bytes());

    out.extend_from_slice(&code);
    out
}
