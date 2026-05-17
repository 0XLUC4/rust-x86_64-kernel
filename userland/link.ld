/* Linker script pour les binaires user de d/OS.
 *
 * Le kernel charge les ELF à p_vaddr (cf fs/elf.rs + userprog.rs). On aligne
 * sur 0x40_0000 (4 MiB) — bien au-dessus de l'identity map kernel bas (<= 1 GiB
 * réservé au kernel, puis user au-dessus) mais compatible avec le code-model
 * `small` (tout le binaire rentre dans les 2 GiB autour de 0).
 */

ENTRY(_start)

SECTIONS {
    . = 0x400000;

    .text : ALIGN(0x1000) {
        *(.text .text.*)
    }

    .rodata : ALIGN(0x1000) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(0x1000) {
        *(.data .data.*)
    }

    .bss : ALIGN(0x1000) {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.gcc_except_table*)
    }
}
