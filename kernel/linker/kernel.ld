/* Link script for the Ferrix kernel.
 *
 * One script for both architectures, because the address space layout is
 * deliberately identical on each -- see docs/ARCHITECTURE.md section 4. The
 * loader reads the program headers this produces and maps each segment with
 * the permissions declared here, so the PHDRS block below is not decoration:
 * it is what makes the kernel's text unwritable and its data unexecutable.
 */

ENTRY(_start)

/* Must equal KERNEL_VIRT_BASE in libs/bootinfo. The loader refuses to start a
 * kernel linked anywhere else rather than mapping it to the wrong place. */
KERNEL_VIRT_BASE = 0xffffffff80000000;

PHDRS
{
    text   PT_LOAD FLAGS(5);  /* read + execute */
    rodata PT_LOAD FLAGS(4);  /* read only */
    data   PT_LOAD FLAGS(6);  /* read + write */
}

SECTIONS
{
    . = KERNEL_VIRT_BASE;
    __kernel_start = .;

    .text : ALIGN(4096) {
        *(.text.entry)
        *(.text .text.*)
    } :text

    . = ALIGN(4096);
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    } :rodata

    . = ALIGN(4096);
    .data : ALIGN(4096) {
        *(.data .data.*)
        *(.sdata .sdata.*)
        *(.got) *(.got.plt)
    } :data

    /* NOLOAD so the file does not carry a megabyte of zeroes: the segment's
     * memsz exceeds its filesz, and both the loader and the architecture
     * expect the difference to read as zero. */
    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(4096);
        __bss_end = .;
    } :data

    . = ALIGN(4096);
    __kernel_end = .;

    /* None of these mean anything without a dynamic linker or an unwinder, and
     * the kernel has neither. Discarding them keeps the program headers to the
     * three above, which is what the loader's segment mapping assumes. */
    /DISCARD/ : {
        *(.eh_frame) *(.eh_frame_hdr) *(.eh_frame*)
        *(.note) *(.note.*)
        *(.comment)
        *(.interp)
        *(.dynamic) *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash)
        *(.rela) *(.rela.*)
        *(.gcc_except_table*)
    }
}
