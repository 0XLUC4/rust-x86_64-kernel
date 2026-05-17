; =============================================================================
; context_switch.asm — commutation de contexte entre threads kernel.
;
; Convention System V AMD64 :
;   - registres callee-saved : RBX, RBP, R12, R13, R14, R15
;   - caller-saved (RAX, RCX, RDX, RSI, RDI, R8-R11) : déjà sauvés par le compilo
;     au call site, on n'a pas à s'en occuper.
;
; Un "Context" kernel tient donc juste dans la stack : on sauvegarde les
; callee-saved + RFLAGS, on switch RSP, on restaure chez le nouveau thread.
;
; Signature C : void context_switch(usize *old_rsp, usize new_rsp);
;   RDI = &old_rsp (où sauvegarder le RSP courant)
;   RSI = new_rsp  (RSP du thread à reprendre)
; =============================================================================

global context_switch

section .text
bits 64
context_switch:
    ; Sauvegarde des callee-saved sur la stack courante
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    pushfq                      ; sauvegarde RFLAGS

    ; Écrit RSP courant dans *old_rsp
    mov [rdi], rsp

    ; Bascule sur la stack du nouveau thread
    mov rsp, rsi

    ; Restaure depuis la nouvelle stack (ordre inverse)
    popfq
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp

    ; ret pop l'adresse de retour poussée par le call qui nous a amené là
    ; (ou par thread::bootstrap lors de la toute première exécution).
    ret
