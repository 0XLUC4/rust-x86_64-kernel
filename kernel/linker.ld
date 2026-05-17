/* =============================================================================
 * linker.ld — layout mémoire du kernel.
 * Le kernel est chargé à 1 MiB physique (convention, conflits < 1 MiB avec BIOS).
 * ============================================================================= */

ENTRY(start)

PHDRS {
    text PT_LOAD FLAGS(5); /* R-X */
    data PT_LOAD FLAGS(6); /* RW- */
}

SECTIONS {
    . = 1M;

    .boot :
    {
        /* Le header multiboot DOIT être dans les 32 KiB premiers du fichier */
        KEEP(*(.multiboot_header))
    } :text

    .text :
    {
        *(.text .text.*)
    } :text

    .rodata :
    {
        *(.rodata .rodata.*)
    } :text

    .data :
    {
        *(.data .data.*)
    } :data

    .bss :
    {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.note.*)
    }
}
