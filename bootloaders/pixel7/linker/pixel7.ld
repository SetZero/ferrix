/*
 * The Pixel 7 loader's layout: one flat image, linked where the Android
 * bootloader places a kernel whose header asks to sit near the base of DRAM.
 *
 * ABL copies the image's bytes and jumps to its first instruction with the
 * MMU off, so everything here is physical. The header's `image_size` covers
 * the whole footprint -- `.bss` and the stack included -- because that is how
 * much memory ABL must leave alone past the load address, and the header is
 * the only way to tell it.
 */

ENTRY(_head)

LOAD_ADDRESS = 0x80000000;
STACK_SIZE = 0x10000;

SECTIONS
{
    . = LOAD_ADDRESS;

    .text : {
        KEEP(*(.text.head))
        *(.text .text.*)
    }

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(16) {
        *(.data .data.*)
    }

    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    . = ALIGN(4096);
    __stack_bottom = .;
    . += STACK_SIZE;
    __stack_top = .;

    _end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
    }
}

_image_size = _end - _head;
