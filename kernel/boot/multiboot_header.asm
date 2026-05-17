; =============================================================================
; Header Multiboot2 — demande à GRUB un framebuffer 1280×720×32 (mode VESA).
; Spec: https://www.gnu.org/software/grub/manual/multiboot2/multiboot.html
; =============================================================================

section .multiboot_header
header_start:
    dd 0xe85250d6                ; magic multiboot2
    dd 0                         ; architecture 0 = i386 protected
    dd header_end - header_start
    dd 0x100000000 - (0xe85250d6 + 0 + (header_end - header_start))

    ; --- Tag "framebuffer" (type 5) ---
    ; Demande à GRUB de switcher dans un mode graphique linéaire 32 bpp.
    ; Si le matériel ne le supporte pas, GRUB choisit le plus proche.
align 8
fb_tag_start:
    dw 5                         ; type = framebuffer
    dw 0                         ; flags (0 = obligatoire)
    dd fb_tag_end - fb_tag_start ; size
    dd 1280                      ; width
    dd 720                       ; height
    dd 32                        ; bpp
fb_tag_end:

    ; --- Tag "end" (type 0, obligatoire en dernier) ---
align 8
    dw 0
    dw 0
    dd 8
header_end:
