//! Single `AArch64` instructions with no spelling in Rust.
//!
//! On the assembly allow-list as "CPU primitives" (`docs/ASSEMBLY.md`): a
//! barrier, a TLB maintenance operation and a hypervisor call are not things
//! Rust has syntax for.

use core::arch::asm;

/// Wait for an interrupt.
pub(crate) fn wfi() {
    // SAFETY: `wfi` is a hint. With interrupts masked it may still return at
    // any time, which is why every caller loops.
    unsafe {
        asm!("wfi", options(nomem, nostack, preserves_flags));
    }
}

/// Mask every interrupt on this CPU: debug, `SError`, `IRQ` and `FIQ`.
pub(crate) fn disable_interrupts() {
    // SAFETY: `daifset` only sets mask bits in `PSTATE`.
    unsafe {
        asm!("msr daifset, #0xf", options(nomem, nostack));
    }
}

/// Publish page table writes and invalidate the whole TLB.
///
/// The first barrier is the one that matters and the one that is easy to leave
/// out: the table walker is a separate observer of memory, and a descriptor
/// still sitting in a store buffer is a descriptor it cannot see. Without
/// `dsb ishst` the mapping is invalidated *before* it exists, and the fault
/// that follows points at the access rather than at the omission.
pub(crate) fn flush_tlb() {
    // SAFETY: barriers and TLB maintenance have no effect other than ordering
    // and invalidation.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags),
        );
    }
}

/// PSCI `SYSTEM_OFF`, in the 32-bit calling convention.
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;

/// Ask the platform to power the machine off.
///
/// Returns if there is no PSCI implementation to answer, which is why the
/// caller halts afterwards rather than assuming this diverges.
///
/// The call is made *after* the boot test's success marker has been printed, on
/// purpose: `hvc` on a machine with no EL2 raises an exception, and until
/// stage 3 installs a vector table there is nothing to take it. Ordering it
/// last means the worst case is an untidy shutdown rather than a lost result.
pub(crate) fn psci_system_off() {
    // SAFETY: `hvc` with x0 = SYSTEM_OFF either powers the machine off or
    // returns an error in x0. Both are fine; the caller halts either way.
    unsafe {
        asm!(
            "hvc #0",
            in("x0") PSCI_SYSTEM_OFF,
            lateout("x0") _,
            lateout("x1") _,
            lateout("x2") _,
            lateout("x3") _,
            options(nostack),
        );
    }
}

/// Install the exception vector table.
///
/// # Safety
///
/// `table` must be the address of a sixteen-entry vector table aligned to 2048
/// bytes, every entry of which is real code. Until this is called the CPU is
/// still using firmware's table, which stopped existing at
/// `exit_boot_services` — so the window between the hand-off and this call is
/// one where any fault is unrecoverable.
pub(crate) unsafe fn write_vbar(table: u64) {
    // SAFETY: the caller guarantees the table's contents and alignment. The
    // `isb` is what makes the write take effect before the next instruction is
    // fetched.
    unsafe {
        asm!(
            "msr vbar_el1, {}",
            "isb",
            in(reg) table,
            options(nostack, preserves_flags),
        );
    }
}
