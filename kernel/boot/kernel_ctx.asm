; =============================================================================
; kernel_ctx.asm — sauvegarde/restauration du contexte kernel pour reprendre
; le shell après qu'un process user ait fini (exit).
;
; kernel_save_and_run(ctx_ptr, user_entry_fn, arg) :
;   - Sauvegarde callee-saved + RSP + RBP dans *ctx_ptr
;   - Appelle user_entry_fn(arg) qui normalement ne revient pas (iretq -> ring 3)
;   - Si kernel_return(ctx_ptr) est appelé depuis ailleurs (typiquement
;     depuis exit_current quand plus de process runnable), on restaure le
;     contexte sauvé et on retourne à l'appelant original.
;
; Structure KernelCtx (match src/task/process.rs) :
;   [0x00] rbx
;   [0x08] rbp
;   [0x10] r12
;   [0x18] r13
;   [0x20] r14
;   [0x28] r15
;   [0x30] rsp
;   [0x38] rip (return address)
;
; Signature C :
;   extern "C" fn kernel_save_and_run(ctx: *mut KernelCtx,
;                                     f: extern "C" fn(u64) -> !,
;                                     arg: u64) -> u64;
;   extern "C" fn kernel_return(ctx: *const KernelCtx, retval: u64) -> !;
;
; kernel_save_and_run retourne :
;   - jamais, si f ne revient pas (iretq direct)
;   - via kernel_return, avec retval dans RAX (on atterrit après le call f)
; =============================================================================

global kernel_save_and_run
global kernel_return

section .text
bits 64

; RDI = ctx, RSI = f, RDX = arg
kernel_save_and_run:
    ; Sauvegarde callee-saved + RSP + RBP + RIP dans ctx
    mov [rdi + 0x00], rbx
    mov [rdi + 0x08], rbp
    mov [rdi + 0x10], r12
    mov [rdi + 0x18], r13
    mov [rdi + 0x20], r14
    mov [rdi + 0x28], r15
    ; RSP "restauré" = RSP actuel avant call (donc RSP + 0, car on n'a rien pushé)
    mov [rdi + 0x30], rsp
    ; RIP de retour = adresse après le call (déjà sur la stack à [rsp])
    mov rax, [rsp]
    mov [rdi + 0x38], rax

    ; Appelle f(arg) : RDI = arg
    mov rdi, rdx
    call rsi

    ; Si f revient normalement, on retourne avec RAX = valeur retour de f
    ret

; RDI = ctx, RSI = retval
kernel_return:
    ; Restaure callee-saved + RSP + RBP
    mov rbx, [rdi + 0x00]
    mov rbp, [rdi + 0x08]
    mov r12, [rdi + 0x10]
    mov r13, [rdi + 0x18]
    mov r14, [rdi + 0x20]
    mov r15, [rdi + 0x28]
    mov rsp, [rdi + 0x30]
    ; RAX = valeur de retour
    mov rax, rsi
    ; Override la return address avec celle sauvée
    mov rdx, [rdi + 0x38]
    mov [rsp], rdx
    ret
