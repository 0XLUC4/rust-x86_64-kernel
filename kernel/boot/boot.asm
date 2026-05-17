; =============================================================================
; boot.asm — point d'entrée 32 bits après GRUB.
;
; GRUB nous laisse en mode protégé 32 bits, interrupts off, paging off.
; Notre mission : vérifier le CPU, setup paging identity-mapped sur 1 GiB,
; passer en long mode (64 bits), et jumper sur le code Rust.
; =============================================================================

global start
extern long_mode_start           ; défini dans boot_64.asm

section .text
bits 32
start:
    mov esp, stack_top           ; setup stack
    mov edi, ebx                 ; sauvegarde pointeur info multiboot (arg1)

    call check_multiboot
    call check_cpuid
    call check_long_mode

    call setup_page_tables
    call enable_paging

    ; Charge notre GDT 64-bit et saute en long mode
    lgdt [gdt64.pointer]
    jmp gdt64.code:long_mode_start

    ; Ne devrait jamais arriver
    hlt

; -----------------------------------------------------------------------------
; Vérifie qu'on a bien été chargé par un bootloader multiboot2
; -----------------------------------------------------------------------------
check_multiboot:
    cmp eax, 0x36d76289
    jne .no_multiboot
    ret
.no_multiboot:
    mov al, "0"
    jmp error

; -----------------------------------------------------------------------------
; Vérifie que CPUID est dispo (test du flip du bit 21 de EFLAGS)
; -----------------------------------------------------------------------------
check_cpuid:
    pushfd
    pop eax
    mov ecx, eax
    xor eax, 1 << 21
    push eax
    popfd
    pushfd
    pop eax
    push ecx
    popfd
    cmp eax, ecx
    je .no_cpuid
    ret
.no_cpuid:
    mov al, "1"
    jmp error

; -----------------------------------------------------------------------------
; Vérifie que le CPU supporte le long mode
; -----------------------------------------------------------------------------
check_long_mode:
    mov eax, 0x80000000
    cpuid
    cmp eax, 0x80000001
    jb .no_long_mode

    mov eax, 0x80000001
    cpuid
    test edx, 1 << 29            ; bit LM
    jz .no_long_mode
    ret
.no_long_mode:
    mov al, "2"
    jmp error

; -----------------------------------------------------------------------------
; Setup des page tables : identity map des 1ers 1 GiB avec huge pages (2 MiB)
;   P4[0] -> P3
;   P3[0] -> P2
;   P2[i] -> huge page 2 MiB identity
; -----------------------------------------------------------------------------
setup_page_tables:
    ; P4[0] = P3 | present | writable
    mov eax, p3_table
    or eax, 0b11
    mov [p4_table], eax

    ; P3[0] = P2 | present | writable
    mov eax, p2_table
    or eax, 0b11
    mov [p3_table], eax

    ; P2[i] = (i * 2 MiB) | present | writable | huge
    mov ecx, 0
.map_p2_table:
    mov eax, 0x200000            ; 2 MiB
    mul ecx
    or eax, 0b10000011           ; present + writable + huge
    mov [p2_table + ecx * 8], eax
    inc ecx
    cmp ecx, 512
    jne .map_p2_table

    ret

; -----------------------------------------------------------------------------
; Active PAE, charge CR3, active long mode dans EFER, puis paging dans CR0
; -----------------------------------------------------------------------------
enable_paging:
    ; CR3 = adresse de P4
    mov eax, p4_table
    mov cr3, eax

    ; PAE bit dans CR4
    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax

    ; LME bit dans EFER (MSR 0xC0000080)
    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8
    wrmsr

    ; Active paging : CR0.PG = 1
    mov eax, cr0
    or eax, 1 << 31
    mov cr0, eax

    ret

; -----------------------------------------------------------------------------
; Gestion d'erreur : écrit "ERR: <code>" sur la VGA et halt
; -----------------------------------------------------------------------------
error:
    mov dword [0xb8000], 0x4f524f45
    mov dword [0xb8004], 0x4f3a4f52
    mov dword [0xb8008], 0x4f204f20
    mov byte  [0xb800a], al
    hlt

; =============================================================================
; BSS — page tables alignées 4 KiB, stack
; =============================================================================
section .bss
align 4096
p4_table:
    resb 4096
p3_table:
    resb 4096
p2_table:
    resb 4096
stack_bottom:
    resb 4096 * 16               ; 64 KiB de stack
stack_top:

; =============================================================================
; GDT 64-bit minimale (null, code, data)
; =============================================================================
section .rodata
gdt64:
    dq 0                                         ; entrée null
.code: equ $ - gdt64
    dq (1<<43) | (1<<44) | (1<<47) | (1<<53)     ; code segment
.data: equ $ - gdt64
    dq (1<<44) | (1<<47) | (1<<41)               ; data segment
.pointer:
    dw $ - gdt64 - 1
    dq gdt64
