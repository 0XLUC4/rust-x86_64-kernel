; =============================================================================
; user_enter.asm — premier saut en ring 3 via iretq.
;
; Signature C :
;   fn enter_userspace(rip: u64, rsp: u64, rflags: u64,
;                      user_cs: u64, user_ss: u64) -> !;
;
; Args SysV :  RDI=rip  RSI=rsp  RDX=rflags  RCX=user_cs  R8=user_ss
;
; On construit sur la stack kernel courante une "iret frame" :
;   [SS][RSP][RFLAGS][CS][RIP]
; puis iretq fait la transition ring 0 -> ring 3 + flush des segments.
;
; On zéroïse tous les registres généraux pour ne pas laisser de données
; kernel côté user (anti-leak ABI).
; =============================================================================

global enter_userspace

section .text
bits 64

enter_userspace:
    ; Avant iretq vers ring 3 : swapgs pour que le "kernel GS" soit
    ; disponible au retour du prochain syscall.
    swapgs

    ; Push en sens inverse (iret pop dans l'ordre RIP, CS, RFLAGS, RSP, SS)
    push r8                           ; SS user
    push rsi                          ; RSP user
    push rdx                          ; RFLAGS user
    push rcx                          ; CS user
    push rdi                          ; RIP user

    ; Zéroise les GPR pour ne pas fuiter des données kernel côté user.
    ; On utilise xor reg, reg qui set aussi les flags, mais iretq charge
    ; un RFLAGS frais depuis la stack, donc c'est OK.
    xor rax, rax
    xor rbx, rbx
    xor rcx, rcx
    xor rdx, rdx
    xor rsi, rsi
    xor rdi, rdi
    xor rbp, rbp
    xor r8,  r8
    xor r9,  r9
    xor r10, r10
    xor r11, r11
    xor r12, r12
    xor r13, r13
    xor r14, r14
    xor r15, r15

    iretq

section .note.GNU-stack noalloc noexec nowrite progbits
