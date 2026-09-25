//! The edges of the loader the machine defines: the image header the Android
//! bootloader reads, the first instructions it jumps to, the drop from EL2 to
//! EL1, the vector table, and the system registers Rust has no spelling for.
//!
//! The UEFI loader in `boot/` needs none of the first three, because firmware
//! calls its `efi_main` with a stack and the MMU on. ABL does not: it follows
//! the Linux arm64 boot protocol (`Documentation/arch/arm64/booting.rst`),
//! which enters the image at its first byte with the MMU off, no stack, the
//! device tree's address in `x0`, and -- on this phone -- at EL2. Everything
//! between that and a Rust function is here.

use core::arch::{asm, global_asm};

global_asm!(
    // The 64-byte header of an arm64 `Image`. ABL looks for the magic at
    // 0x38 and reads the size and flags to decide where the image may go.
    ".section .text.head, \"ax\"",
    ".global _head",
    "_head:",
    "    b       ferrix_pixel7_entry",
    "    .long   0",
    // text_offset: the image wants its first byte at the 2 MiB-aligned base.
    "    .quad   0",
    // image_size: the whole footprint, .bss and stack included, from the
    // linker script.
    "    .quad   _image_size",
    // flags: little endian (bit 0 clear), 4 KiB pages (bits 1-2 = 1), and
    // placed as close to the base of DRAM as possible (bit 3 clear).
    "    .quad   0x2",
    "    .quad   0",
    "    .quad   0",
    "    .quad   0",
    "    .ascii  \"ARM\\x64\"",
    "    .long   0",
    "",
    "ferrix_pixel7_entry:",
    "    mov     x19, x0",
    "    adrp    x9, __stack_top",
    "    add     x9, x9, :lo12:__stack_top",
    "    mov     sp, x9",
    // .bss is not in the image ABL copied, so it holds whatever was in RAM.
    "    adrp    x9, __bss_start",
    "    add     x9, x9, :lo12:__bss_start",
    "    adrp    x10, __bss_end",
    "    add     x10, x10, :lo12:__bss_end",
    "1:  cmp     x9, x10",
    "    b.hs    2f",
    "    str     xzr, [x9], #8",
    "    b       1b",
    // Report what ABL handed over while still at the level it handed over at.
    "2:  mov     x0, x19",
    "    mrs     x1, CurrentEL",
    "    adr     x2, _head",
    "    bl      {early}",
    // At EL2, make EL1 a plain AArch64 kernel's level and drop to it: no
    // traps, the physical timer and counter reachable, the GIC's system
    // registers enabled, and the MMU and caches off as they are here.
    "    mrs     x9, CurrentEL",
    "    cmp     x9, #8",
    "    b.ne    3f",
    "    mov     x9, #0x80000000",
    "    msr     hcr_el2, x9",
    "    isb",
    "    mov     x9, #3",
    "    msr     cnthctl_el2, x9",
    "    msr     cntvoff_el2, xzr",
    "    mov     x9, #0x33ff",
    "    msr     cptr_el2, x9",
    "    msr     hstr_el2, xzr",
    "    mrs     x9, midr_el1",
    "    msr     vpidr_el2, x9",
    "    mrs     x9, mpidr_el1",
    "    msr     vmpidr_el2, x9",
    "    mrs     x9, icc_sre_el2",
    // SRE (bit 0) and Enable (bit 3): two bits no single logical immediate
    // encodes.
    "    orr     x9, x9, #0x1",
    "    orr     x9, x9, #0x8",
    "    msr     icc_sre_el2, x9",
    "    isb",
    "    movz    x9, #0x0800",
    "    movk    x9, #0x30d0, lsl #16",
    "    msr     sctlr_el1, x9",
    "    mov     x9, sp",
    "    msr     sp_el1, x9",
    "    adr     x9, 3f",
    "    msr     elr_el2, x9",
    "    mov     x9, #0x3c5",
    "    msr     spsr_el2, x9",
    "    eret",
    "3:  adr     x9, ferrix_pixel7_vectors",
    "    msr     vbar_el1, x9",
    "    isb",
    "    mov     x0, x19",
    "    bl      {main}",
    "4:  wfe",
    "    b       4b",
    "",
    // Sixteen entries at fixed 128-byte offsets, each passing which one it is
    // and the syndrome to Rust. Nothing here returns: an exception in a loader
    // is reported and the phone is reset.
    ".section .text",
    ".balign 2048",
    "ferrix_pixel7_vectors:",
    ".irp kind, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15",
    "    .balign 128",
    "    mov     x0, #\\kind",
    "    mrs     x1, esr_el1",
    "    mrs     x2, elr_el1",
    "    mrs     x3, far_el1",
    "    b       {trap}",
    ".endr",
    early = sym crate::early,
    main = sym crate::main,
    trap = sym crate::trap,
);

/// `HCR_EL2` and `SCTLR_EL2` as ABL left them.
///
/// # Safety
///
/// Only at EL2: below it, reading either register is undefined.
pub(crate) unsafe fn el2_state() -> (u64, u64) {
    let hcr: u64;
    let sctlr: u64;
    // SAFETY: the caller is at EL2, where both reads are defined and have no
    // side effects.
    unsafe {
        asm!(
            "mrs {hcr}, hcr_el2",
            "mrs {sctlr}, sctlr_el2",
            hcr = out(reg) hcr,
            sctlr = out(reg) sctlr,
            options(nomem, nostack, preserves_flags),
        );
    }
    (hcr, sctlr)
}

/// The exception level the loader is running at.
pub(crate) fn current_el() -> u64 {
    let level: u64;
    // SAFETY: CurrentEL is readable at every exception level.
    unsafe {
        asm!("mrs {}, CurrentEL", out(reg) level, options(nomem, nostack, preserves_flags));
    }
    (level >> 2) & 0b11
}

/// The generic timer's count and frequency.
pub(crate) fn counter() -> (u64, u64) {
    let count: u64;
    let frequency: u64;
    // SAFETY: both are readable at EL1 once `CNTHCTL_EL2` allows it, which
    // the entry sequence set, and at EL2 always.
    unsafe {
        asm!(
            "isb",
            "mrs {count}, cntpct_el0",
            "mrs {frequency}, cntfrq_el0",
            count = out(reg) count,
            frequency = out(reg) frequency,
            options(nomem, nostack, preserves_flags),
        );
    }
    (count, frequency)
}

/// Reset the phone through PSCI, which the device tree says is reached by
/// `smc`. ABL then boots whatever the active slot holds.
pub(crate) fn system_reset() -> ! {
    /// `SYSTEM_RESET`, PSCI 0.2's function 9.
    const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
    // SAFETY: SYSTEM_RESET does not return; if firmware refuses it, the loop
    // after keeps this function's promise.
    unsafe {
        asm!("smc #0", in("x0") PSCI_SYSTEM_RESET, options(nostack));
    }
    loop {
        // SAFETY: `wfe` only waits.
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}
