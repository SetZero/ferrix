//! Single ARMv7-A instructions with no spelling in Rust.
//!
//! On the assembly allow-list as "CPU primitives" (`docs/ASSEMBLY.md`), as
//! AArch64's are. Coprocessor 15 is how ARMv7-A spells every system register,
//! and a coprocessor transfer is not something Rust has syntax for; nor are a
//! barrier, a TLB operation, a mode change or a hypervisor call.

use core::arch::asm;

use ferrix_fdt::PsciConduit;

/// `CPSR.A`, `CPSR.I` and `CPSR.F`: asynchronous aborts, IRQs and FIQs masked.
const CPSR_MASK_BITS: u32 = (1 << 8) | (1 << 7) | (1 << 6);

/// `CPSR.I`: IRQs masked.
pub(crate) const CPSR_IRQ_MASKED: u32 = 1 << 7;

/// `SCTLR.V`: exceptions go to the fixed high vectors at `0xFFFF_0000` rather
/// than to `VBAR`.
const SCTLR_HIGH_VECTORS: u32 = 1 << 13;
/// `SCTLR.TE`: exceptions are taken in Thumb state.
const SCTLR_THUMB_EXCEPTIONS: u32 = 1 << 30;

/// `TTBCR.EPD0`: translations through `TTBR0` fault instead of walking.
const TTBCR_EPD0: u32 = 1 << 7;

/// PSCI `SYSTEM_OFF`, in the 32-bit calling convention.
const PSCI_SYSTEM_OFF: u32 = 0x8400_0008;

/// Wait for an interrupt.
pub(crate) fn wfi() {
    // SAFETY: `wfi` is a hint. With interrupts masked it may still return at
    // any time, which is why every caller loops.
    unsafe {
        asm!("wfi", options(nomem, nostack, preserves_flags));
    }
}

/// Mask asynchronous aborts, IRQs and FIQs on this CPU.
pub(crate) fn disable_interrupts() {
    // SAFETY: `cpsid` only sets mask bits in CPSR.
    unsafe {
        asm!("cpsid aif", options(nomem, nostack));
    }
}

/// Unmask IRQs on this CPU, leaving FIQs and asynchronous aborts masked.
///
/// Only IRQs, for AArch64's reasons: nothing is routed to FIQ, which is
/// conventionally a secure world's, and an asynchronous abort stays masked
/// until there is something that could act on one.
pub(crate) fn enable_interrupts() {
    // SAFETY: `cpsie` only clears mask bits in CPSR. The vector table is
    // installed long before anything calls this.
    unsafe {
        asm!("cpsie i", options(nomem, nostack));
    }
}

/// The current program status.
pub(crate) fn read_cpsr() -> u32 {
    let cpsr: u32;
    // SAFETY: reading CPSR has no side effects.
    unsafe {
        asm!("mrs {}, cpsr", out(reg) cpsr, options(nomem, nostack, preserves_flags));
    }
    cpsr
}

/// Put back the interrupt mask bits a [`read_cpsr`] saw.
///
/// Only the A, I and F bits change: the rest of the value written is the
/// current CPSR's own, so the mode and the endianness cannot be disturbed by a
/// caller that passed back something stale.
///
/// # Safety
///
/// `saved` must have come from [`read_cpsr`] on this CPU. Unmasking what the
/// caller did not mask unmasks exceptions it may not be ready for.
pub(crate) unsafe fn restore_interrupt_mask(saved: u32) {
    // SAFETY: the caller guarantees the bits came from this CPU's own CPSR.
    unsafe {
        asm!(
            "mrs {scratch}, cpsr",
            "bic {scratch}, {scratch}, #{mask}",
            "orr {scratch}, {scratch}, {saved}",
            "msr cpsr_xc, {scratch}",
            scratch = out(reg) _,
            saved = in(reg) saved & CPSR_MASK_BITS,
            mask = const CPSR_MASK_BITS,
            options(nomem, nostack),
        );
    }
}

/// Publish page table writes and invalidate the whole TLB, on every core.
///
/// The first barrier is the one that is easy to leave out, for the reason it
/// is on AArch64: the table walker is a separate observer of memory, and a
/// descriptor still in a store buffer is one it cannot see. The branch
/// predictor goes too, because the 32-bit architecture lets it hold virtual
/// addresses whose translation just changed.
pub(crate) fn flush_tlb() {
    // SAFETY: barriers and maintenance operations have no effect other than
    // ordering and invalidation.
    unsafe {
        asm!(
            "dsb ishst",
            "mcr p15, 0, {zero}, c8, c3, 0",
            "mcr p15, 0, {zero}, c7, c1, 6",
            "dsb ish",
            "isb",
            zero = in(reg) 0u32,
            options(nostack, preserves_flags),
        );
    }
}

/// Ask the platform to power the machine off, through whichever instruction
/// the device tree says its PSCI firmware answers.
///
/// Returns if nothing answers, which is why the caller halts afterwards. As on
/// AArch64 it is made only after the success marker, because the wrong conduit
/// is an undefined-instruction exception rather than an error code.
pub(crate) fn psci_system_off(conduit: PsciConduit) {
    match conduit {
        // SAFETY: `hvc` with r0 = SYSTEM_OFF either powers off or returns an
        // error in r0; the caller halts either way.
        PsciConduit::Hvc => unsafe {
            asm!(
                ".arch_extension virt",
                "hvc #0",
                inout("r0") PSCI_SYSTEM_OFF => _,
                lateout("r1") _,
                lateout("r2") _,
                lateout("r3") _,
                options(nostack),
            );
        },
        // SAFETY: as above, through the secure monitor.
        PsciConduit::Smc => unsafe {
            asm!(
                ".arch_extension sec",
                "smc #0",
                inout("r0") PSCI_SYSTEM_OFF => _,
                lateout("r1") _,
                lateout("r2") _,
                lateout("r3") _,
                options(nostack),
            );
        },
    }
}

/// Install the exception vector table.
///
/// Also clears `SCTLR.V` and `SCTLR.TE`, which firmware may have left set and
/// either of which makes `VBAR` meaningless: one sends exceptions to a fixed
/// address in the kernel image's region where nothing is mapped, the other
/// enters the vectors in the wrong instruction set.
///
/// # Safety
///
/// `table` must be the address of an eight-entry ARM-state vector table,
/// 32-byte aligned, every entry of which is real code.
pub(crate) unsafe fn install_vectors(table: u32) {
    // SAFETY: the caller guarantees the table. The `isb` makes both writes
    // take effect before the next instruction is fetched.
    unsafe {
        asm!(
            "mrc p15, 0, {scratch}, c1, c0, 0",
            "bic {scratch}, {scratch}, #{high}",
            "bic {scratch}, {scratch}, #{thumb}",
            "mcr p15, 0, {scratch}, c1, c0, 0",
            "mcr p15, 0, {table}, c12, c0, 0",
            "isb",
            scratch = out(reg) _,
            table = in(reg) table,
            high = const SCTLR_HIGH_VECTORS,
            thumb = const SCTLR_THUMB_EXCEPTIONS,
            options(nostack, preserves_flags),
        );
    }
}

/// Stop the CPU translating the lower half of the address space at all.
///
/// How ARMv7-A drops the loader's identity map, as AArch64 does: the map is a
/// second regime with its own base register, so it is switched off rather
/// than dismantled. `TTBR0` is zeroed as well, so a later change that clears
/// `EPD0` cannot resurrect the loader's tables, and the TLB is invalidated,
/// because entries the identity map left there would otherwise keep
/// translating until something evicted them.
///
/// # Safety
///
/// Nothing may still be executing or reading through the lower half.
pub(crate) unsafe fn disable_ttbr0() {
    // SAFETY: the caller guarantees nothing needs the lower half.
    unsafe {
        asm!(
            "mrc p15, 0, {scratch}, c2, c0, 2",
            "orr {scratch}, {scratch}, #{epd0}",
            "mcr p15, 0, {scratch}, c2, c0, 2",
            "mcrr p15, 0, {zero}, {zero}, c2",
            "isb",
            "mcr p15, 0, {zero}, c8, c7, 0",
            "dsb",
            "isb",
            scratch = out(reg) _,
            zero = in(reg) 0u32,
            epd0 = const TTBCR_EPD0,
            options(nostack, preserves_flags),
        );
    }
}

/// The frequency of the architected counter, in hertz.
pub(crate) fn read_cntfrq() -> u32 {
    let frequency: u32;
    // SAFETY: reading `CNTFRQ` has no side effects.
    unsafe {
        asm!("mrc p15, 0, {}, c14, c0, 0", out(reg) frequency, options(nomem, nostack, preserves_flags));
    }
    frequency
}

/// The virtual counter.
///
/// The `isb` is not optional, for AArch64's reason: without it the read is
/// free to be satisfied out of order with the instructions around it.
pub(crate) fn read_cntvct() -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: a barrier and a 64-bit system register read.
    unsafe {
        asm!(
            "isb",
            "mrrc p15, 1, {low}, {high}, c14",
            low = out(reg) low,
            high = out(reg) high,
            options(nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Set the instant the virtual timer fires at.
pub(crate) fn write_cntv_cval(instant: u64) {
    // SAFETY: `CNTV_CVAL` is a comparator; writing it changes when the
    // timer's output asserts and nothing else.
    unsafe {
        asm!(
            "mcrr p15, 3, {low}, {high}, c14",
            low = in(reg) instant as u32,
            high = in(reg) (instant >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Enable or mask the virtual timer.
pub(crate) fn write_cntv_ctl(control: u32) {
    // SAFETY: `CNTV_CTL` holds the timer's enable and mask bits. The `isb`
    // makes the write take effect before the next instruction, so a caller
    // that disarms and then returns cannot take one more interrupt.
    unsafe {
        asm!(
            "mcr p15, 0, {}, c14, c3, 1",
            "isb",
            in(reg) control,
            options(nomem, nostack, preserves_flags),
        );
    }
}
