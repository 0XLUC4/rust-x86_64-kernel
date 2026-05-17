; =============================================================================
; boot_64.asm — on est maintenant en long mode 64 bits.
; On reset les segments, active SSE, puis on saute dans _start (Rust).
;
; Pourquoi activer SSE : les compilateurs récents (rustc/LLVM) émettent des
; instructions SSE même quand on demande "softfloat" (ex: moves 128-bit pour
; memcpy, registres XMM pour passer des structs). Il faut donc :
;   1. clear CR0.EM (bit 2 : Emulation), set CR0.MP (bit 1 : Monitor coProc)
;   2. set CR4.OSFXSR (bit 9) + CR4.OSXMMEXCPT (bit 10)
; Sans ça, chaque insn SSE → #UD (invalid opcode).
; =============================================================================

global long_mode_start
extern _start

section .text
bits 64
long_mode_start:
    mov ax, 0
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    ; --- Enable SSE ---
    mov rax, cr0
    and ax, 0xFFFB       ; clear EM (bit 2)
    or ax, 0x2           ; set MP (bit 1)
    mov cr0, rax

    mov rax, cr4
    or ax, 3 << 9        ; set OSFXSR (9) + OSXMMEXCPT (10)
    mov cr4, rax

    call _start

.halt:
    cli
    hlt
    jmp .halt
