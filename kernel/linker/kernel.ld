/* Link script for the Ferrix kernel.
 *
 * One script for all three architectures. The layout within the image is the
 * same on each; where the image goes is not -- the 64-bit pair link it in the
 * top 2 GiB, ARMv7-A at 0xf0000000 -- so KERNEL_VIRT_BASE is not defined here.
 * `.cargo/config.toml` defines it per target with `--defsym`, from the same
 * number as KERNEL_VIRT_BASE in libs/bootinfo, and the loader refuses to start
 * a kernel linked anywhere else. A build that forgets the definition fails to
 * link rather than producing an image at address zero.
 *
 * The loader reads the program headers this produces and maps each segment
 * with the permissions declared here, so the PHDRS block below is not
 * decoration: it is what makes the kernel's text unwritable and its data
 * unexecutable. See docs/ARCHITECTURE.md section 4.
 */

ENTRY(_start)

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
     * three above, which is what the loader's segment mapping assumes. The
     * .ARM.exidx and .ARM.extab tables are the 32-bit Arm unwinder's, and left
     * in would become a fourth segment. */
    /DISCARD/ : {
        *(.eh_frame) *(.eh_frame_hdr) *(.eh_frame*)
        *(.ARM.exidx*) *(.ARM.extab*)
        *(.note) *(.note.*)
        *(.comment)
        *(.interp)
        *(.dynamic) *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash)
        *(.rela) *(.rela.*) *(.rel) *(.rel.*)
        *(.gcc_except_table*)
    }
}
