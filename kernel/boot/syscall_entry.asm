; =============================================================================
; syscall_entry.asm — point d'entrée de l'instruction SYSCALL (Phase II).
;
; Convention x86_64 (SYSCALL depuis ring 3) :
;   - RAX = numéro de syscall
;   - RDI, RSI, RDX, R10, R8, R9 = arg 1..6
;   - RCX = RIP retour (écrit par SYSCALL)
;   - R11 = RFLAGS retour (écrit par SYSCALL)
;   - RFLAGS masqué par SFMASK (on clear IF)
;   - CS/SS: chargés depuis STAR[47:32]
;   - RSP : INCHANGÉ → on est encore sur la stack user !
;
; Rôle de ce stub :
;   1. swapgs  -> GS_BASE pointe maintenant vers la PerCpu area kernel
;   2. sauvegarde RSP user dans [gs:0]   (slot "user_rsp")
;   3. charge RSP kernel depuis [gs:8]   (slot "kernel_rsp")
;   4. sauvegarde registres caller-saved
;   5. appelle syscall_dispatch(nr, a1..a6) — C ABI
;   6. restaure + swapgs + sysretq
;
; La structure PerCpu (src/arch/x86_64/percpu.rs) est :
;   offset 0 : user_rsp          (u64)
;   offset 8 : kernel_rsp        (u64)
;   offset 16: current_process*  (*mut Process)
;   ...
; =============================================================================

global syscall_entry
extern syscall_dispatch

section .text
bits 64
syscall_entry:
    ; Switch vers le contexte kernel (GS_BASE kernel)
    swapgs

    ; Sauvegarde RSP user, bascule sur RSP kernel
    mov [gs:0], rsp
    mov rsp, [gs:8]

    ; Sauvegarde RCX (RIP retour) et R11 (RFLAGS retour) en PREMIER
    push rcx
    push r11

    ; Sauvegarde des arg registers + RBX pour pouvoir les restaurer
    push rdi
    push rsi
    push rdx
    push r8
    push r9
    push r10
    push rbx

    ; Convention syscall -> C :
    ;   syscall : RAX=nr  RDI=a1 RSI=a2 RDX=a3 R10=a4 R8=a5 R9=a6
    ;   C       : RDI=a1  RSI=a2 RDX=a3 RCX=a4 R8=a5 R9=a6
    ;
    ; On veut appeler syscall_dispatch(nr, a1, a2, a3, a4, a5, a6)
    ;   → RDI=nr RSI=a1 RDX=a2 RCX=a3 R8=a4 R9=a5 stack=a6

    mov rbx, rax                      ; sauvegarde nr
    push r9                           ; a6 sur la stack (7e arg)

    mov r9, r8                        ; a5 -> R9
    mov r8, r10                       ; a4 -> R8
    mov rcx, rdx                      ; a3 -> RCX
    mov rdx, rsi                      ; a2 -> RDX
    mov rsi, rdi                      ; a1 -> RSI
    mov rdi, rbx                      ; nr -> RDI

    ; Alignement stack 16 avant call (CALL va pusher 8 → donc RSP % 16 == 8 ici)
    ; Après les pushes précédentes on a pushé : rcx r11 rdi rsi rdx r8 r9 r10 rbx + a6 = 10 pushes
    ; 10 * 8 = 80, donc on a désaligné par 0 (la stack était alignée) → avant CALL on doit être à 8 mod 16.
    ; Vu qu'on a pushé 10 fois, on est aligné (10 pair). CALL va push 8 de plus → 8 mod 16 = OK.

    call syscall_dispatch

    ; Pop le a6 supplémentaire
    add rsp, 8

    ; Restaure caller-saved
    pop rbx
    pop r10
    pop r9
    pop r8
    pop rdx
    pop rsi
    pop rdi

    ; Restaure R11 (rflags) et RCX (rip)
    pop r11
    pop rcx

    ; Restaure RSP user (et sauvegarde RSP kernel actuel pour le prochain syscall)
    mov [gs:8], rsp
    mov rsp, [gs:0]

    swapgs
    o64 sysret
