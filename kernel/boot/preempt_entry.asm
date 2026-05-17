; =============================================================================
; preempt_entry.asm — ISR timer avec sauvegarde complète du contexte (v2 safe).
;
; Version : pas de swapgs. Sauvegarde full state, appelle timer_tick_rust,
; restaure, iretq. Le scheduler peut modifier la TrapFrame passée en RDI
; pour switch le process utilisateur.
;
; Layout TrapFrame (match src/task/preempt.rs) :
;   poussé par nous (du haut vers le bas) :
;     gs_base    (stub = 0)
;     fs_base    (stub = 0)
;     r15, r14, r13, r12, r11, r10, r9, r8, rbp, rdi, rsi, rdx, rcx, rbx, rax
;     _pad       (0 — alignement)
;   poussé par le CPU (iret frame) :
;     rip, cs, rflags, rsp, ss
; =============================================================================

global timer_preempt_entry
extern timer_tick_rust

section .text
bits 64

%macro PUSH_ALL_GPRS 0
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
%endmacro

%macro POP_ALL_GPRS 0
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
%endmacro

timer_preempt_entry:
    ; padding d'alignement (match le _pad de TrapFrame)
    push 0

    PUSH_ALL_GPRS

    ; Stubs FS/GS (pas de swapgs pour l'instant)
    push 0                            ; fs_base
    push 0                            ; gs_base

    ; rdi = pointeur sur la TrapFrame = RSP actuel
    mov rdi, rsp

    ; Aligne la stack 16 bytes avant call (on est à 18 pushes * 8 = 144, align ok)
    ; 18 pushes * 8 = 144 bytes ; 144 % 16 = 0 donc rsp est aligné 16 mais
    ; call pushera 8 → désalignera. On sub 8 pour compenser.
    sub rsp, 8

    call timer_tick_rust

    add rsp, 8

    ; Pop stubs
    add rsp, 16

    POP_ALL_GPRS

    ; Pop padding
    add rsp, 8

    iretq
