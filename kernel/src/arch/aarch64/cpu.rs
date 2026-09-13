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

/// Wait for an interrupt with `IRQ` masked, then unmask it.
///
/// `wfi` wakes on a *pending* interrupt whether or not it is masked, so one
/// that became pending after the caller masked is not lost: the wait returns
/// at once, and the unmask after it lets the interrupt be taken.
pub(crate) fn wait_then_enable_interrupts() {
    // SAFETY: `wfi` is a hint and `daifclr` only clears the `IRQ` mask bit;
    // the vector table is installed long before this is used.
    unsafe {
        asm!("wfi", "msr daifclr, #0x2", options(nomem, nostack));
    }
}

/// Order every store before this against the next write to device memory.
///
/// For sending a software-generated interrupt: the interrupt is a write to the
/// distributor, and the core that takes it must see what this core wrote
/// before asking — a `dmb` orders memory against memory, and this is memory
/// against a device.
pub(crate) fn dsb_ishst() {
    // SAFETY: a barrier has no effect beyond ordering.
    unsafe {
        asm!("dsb ishst", options(nostack, preserves_flags));
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

/// Unmask `IRQ` on this CPU, leaving `FIQ`, `SError` and debug masked.
///
/// Only `IRQ`: Ferrix routes nothing to `FIQ` — it is conventionally reserved
/// for a secure world the kernel does not own — and an `SError` is an
/// asynchronous abort that stays masked until there is something that could
/// act on one.
pub(crate) fn enable_interrupts() {
    // SAFETY: `daifclr` only clears mask bits in `PSTATE`. The vector table is
    // installed long before anything calls this.
    unsafe {
        asm!("msr daifclr, #0x2", options(nomem, nostack));
    }
}

/// The interrupt mask bits of `PSTATE`.
///
/// Read for the same reason x86-64 reads `RFLAGS`: a lock that masks
/// interrupts has to put back the state it found, not unconditionally unmask.
pub(crate) fn read_daif() -> u64 {
    let daif: u64;
    // SAFETY: reading `DAIF` has no side effects.
    unsafe {
        asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack, preserves_flags));
    }
    daif
}

/// Restore the interrupt mask bits `read_daif` returned.
///
/// # Safety
///
/// `daif` must be a value a previous [`read_daif`] returned. Writing an
/// arbitrary value unmasks exceptions the caller may not be ready for — an
/// `SError` in particular, which Ferrix keeps masked until there is something
/// that could act on one.
pub(crate) unsafe fn write_daif(daif: u64) {
    // SAFETY: the caller guarantees the value came from `read_daif`.
    unsafe {
        asm!("msr daif, {}", in(reg) daif, options(nomem, nostack));
    }
}

/// `TCR_EL1.EPD0` — translations through `TTBR0_EL1` fault instead of walking.
pub(crate) const TCR_EPD0: u64 = 1 << 7;

/// Memory attribute indirection: what each attribute index in a descriptor
/// means.
pub(crate) fn read_mair() -> u64 {
    let value: u64;
    // SAFETY: reading `MAIR_EL1` has no side effects.
    unsafe {
        asm!("mrs {}, mair_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Translation control: sizes, granules and walk attributes of both halves.
pub(crate) fn read_tcr() -> u64 {
    let value: u64;
    // SAFETY: reading `TCR_EL1` has no side effects.
    unsafe {
        asm!("mrs {}, tcr_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// The lower half's translation table base: the root address in bits 47 to 1,
/// the `ASID` above it.
pub(crate) fn read_ttbr0() -> u64 {
    let value: u64;
    // SAFETY: reading `TTBR0_EL1` has no side effects.
    unsafe {
        asm!("mrs {}, ttbr0_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// The table address bits of a `TTBR0_EL1` value.
pub(crate) const TTBR_ADDRESS: u64 = 0x0000_FFFF_FFFF_FFFE;

/// System control: the MMU, the caches, alignment checking.
pub(crate) fn read_sctlr() -> u64 {
    let value: u64;
    // SAFETY: reading `SCTLR_EL1` has no side effects.
    unsafe {
        asm!("mrs {}, sctlr_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Write the data cache lines covering `start..start + len` back to the
/// point of coherency.
///
/// For memory a core with its caches off is about to read: it reads RAM, and
/// a line still dirty in this core's cache is a line it does not see.
pub(crate) fn clean_to_poc(start: u64, len: u64) {
    let cache_type: u64;
    // SAFETY: reading `CTR_EL0` has no side effects.
    unsafe {
        asm!("mrs {}, ctr_el0", out(reg) cache_type, options(nomem, nostack, preserves_flags));
    }
    // `DminLine` is log2 of the smallest line in words.
    let line = 4u64 << ((cache_type >> 16) & 0xF);

    let mut at = start - start % line;
    let end = start.saturating_add(len);
    while at < end {
        // SAFETY: `dc cvac` writes one line back by virtual address and
        // changes no data; the caller's range is mapped.
        unsafe {
            asm!("dc cvac, {}", in(reg) at, options(nostack, preserves_flags));
        }
        at += line;
    }
    // SAFETY: a barrier, completing the maintenance above before anything
    // after it — in particular the call that starts the core that reads it.
    unsafe {
        asm!("dsb sy", options(nostack, preserves_flags));
    }
}

/// A PSCI call through the hypervisor conduit.
///
/// # Safety
///
/// `function` must be a PSCI function taking `a`, `b` and `c`, and what it
/// does must be what the caller intends: `CPU_ON` starts a core executing at
/// an address the caller chose.
pub(crate) unsafe fn hvc_call(function: u64, a: u64, b: u64, c: u64) -> u64 {
    let result: u64;
    // SAFETY: the caller guarantees the function and its arguments. The SMC
    // calling convention lets the callee corrupt every register the C ABI
    // does, which is what the clobber says.
    unsafe {
        asm!(
            "hvc #0",
            inlateout("x0") function => result,
            in("x1") a,
            in("x2") b,
            in("x3") c,
            clobber_abi("C"),
            options(nostack),
        );
    }
    result
}

/// A PSCI call through the secure monitor conduit.
///
/// # Safety
///
/// As [`hvc_call`].
pub(crate) unsafe fn smc_call(function: u64, a: u64, b: u64, c: u64) -> u64 {
    let result: u64;
    // SAFETY: as `hvc_call`.
    unsafe {
        asm!(
            "smc #0",
            inlateout("x0") function => result,
            in("x1") a,
            in("x2") b,
            in("x3") c,
            clobber_abi("C"),
            options(nostack),
        );
    }
    result
}

/// Stop the CPU translating the lower half of the address space at all.
///
/// This is how `AArch64` drops the loader's identity map. Unlike x86-64, where
/// the identity map is a set of entries in the same table as everything else
/// and is dropped by clearing them, here it is a whole second translation
/// regime with its own base register — so the way to drop it is to switch the
/// regime off.
///
/// `TTBR0_EL1` is zeroed as well as disabled. With `EPD0` set the register is
/// not consulted, so this changes no behaviour; it is here so that a later
/// change that clears `EPD0` — stage 6, giving the lower half to a user
/// process — cannot accidentally resurrect the loader's tables.
///
/// # Safety
///
/// Nothing may still be executing or reading through the lower half. The
/// kernel runs entirely in the upper half from its first instruction, so this
/// holds once the boot stack and the hand-off are being reached through the
/// direct map, which they are.
pub(crate) unsafe fn disable_ttbr0() {
    // SAFETY: the caller guarantees nothing needs the lower half. The `isb`
    // makes both writes take effect before the next instruction is fetched;
    // without it the CPU is permitted to walk the old tables for a while yet.
    unsafe {
        asm!(
            "mrs {scratch}, tcr_el1",
            "orr {scratch}, {scratch}, {epd0}",
            "msr tcr_el1, {scratch}",
            "msr ttbr0_el1, xzr",
            "isb",
            scratch = out(reg) _,
            epd0 = in(reg) TCR_EPD0,
            options(nostack, preserves_flags),
        );
    }
}

/// Translate the lower half through the tables at `root`, with `ASID` zero.
///
/// The inverse of [`disable_ttbr0`], and the two writes are in the order that
/// order matters in: the root first and `EPD0` second. Between them the
/// processor is walking the new tables through a regime that is still
/// disabled, which faults; the other order leaves a window in which the regime
/// is live and the register still holds whatever was there before — which for
/// the first user process is the zero [`disable_ttbr0`] wrote, and for every
/// switch after it is *another process's tables*.
///
/// The `ASID` field of `TTBR0_EL1` is left zero, because stage 6 does not
/// allocate address space identifiers: every address space is `ASID` zero and
/// the switch invalidates all of them. See [`flush_user_tlb`].
///
/// # Safety
///
/// `root` must be the physical address of a live translation table for the
/// lower half, and it must stay live until another root replaces it here.
pub(crate) unsafe fn write_ttbr0(root: u64) {
    // SAFETY: the caller guarantees the tables. The `isb` makes both writes
    // take effect before the next instruction is fetched.
    unsafe {
        asm!(
            "msr ttbr0_el1, {root}",
            "mrs {scratch}, tcr_el1",
            "bic {scratch}, {scratch}, {epd0}",
            "msr tcr_el1, {scratch}",
            "isb",
            root = in(reg) root,
            scratch = out(reg) _,
            epd0 = in(reg) TCR_EPD0,
            options(nostack, preserves_flags),
        );
    }
}

/// Drop this processor's cached user translations, and keep the kernel's.
///
/// `TLBI ASIDE1` invalidates the entries matching an `ASID` and by definition
/// not the global ones, so the kernel's — its text, the direct map, the device
/// windows, all mapped global — survive. That is the whole reason this is not
/// [`flush_tlb`]: that one is `vmalle1is`, which throws away the global
/// entries too *and* broadcasts, and an address space switch neither needs nor
/// can afford either.
///
/// `nsh` rather than `ish` on the barrier: the invalidation is this
/// processor's business. Another processor running another thread of the same
/// process must keep its translations, and one that is about to run this
/// address space will invalidate as it installs the root.
pub(crate) fn flush_user_tlb() {
    // SAFETY: invalidating translations can only cost a re-walk. The `dsb`
    // waits for the invalidation and the `isb` keeps the next instruction from
    // being fetched through an entry it removed.
    unsafe {
        asm!(
            "tlbi aside1, {asid}",
            "dsb nsh",
            "isb",
            asid = in(reg) 0_u64,
            options(nostack, preserves_flags),
        );
    }
}

/// This processor's multiprocessor affinity register.
///
/// Read-only and fixed at reset: it is how the processor is named to PSCI and
/// to the interrupt controller, and nothing the kernel does can change it.
pub(crate) fn read_mpidr() -> u64 {
    let mpidr: u64;
    // SAFETY: reading `MPIDR_EL1` has no side effects.
    unsafe {
        asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    mpidr
}

/// `ID_AA64PFR0_EL1`: whether the core has floating point and Advanced SIMD.
pub(crate) fn read_id_aa64pfr0() -> u64 {
    let value: u64;
    // SAFETY: reading an identification register has no side effects.
    unsafe {
        asm!("mrs {}, id_aa64pfr0_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// `ID_AA64ISAR0_EL1`: the first instruction set attribute register.
pub(crate) fn read_id_aa64isar0() -> u64 {
    let value: u64;
    // SAFETY: reading an identification register has no side effects.
    unsafe {
        asm!("mrs {}, id_aa64isar0_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// `ID_AA64ISAR1_EL1`: the second instruction set attribute register.
pub(crate) fn read_id_aa64isar1() -> u64 {
    let value: u64;
    // SAFETY: reading an identification register has no side effects.
    unsafe {
        asm!("mrs {}, id_aa64isar1_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// `CPACR_EL1.FPEN`, both bits: floating point and SIMD do not trap at EL0 or
/// EL1.
const CPACR_FPEN_NO_TRAP: u64 = 0b11 << 20;

/// Let EL0 use floating point and SIMD, on this core.
///
/// musl's `memcpy` and every AArch64 Linux program's string handling use SIMD
/// registers, so without this a program's first copy traps. Firmware usually
/// leaves `FPEN` open, which is why busybox ran before this existed; the
/// architecture resets it to an unknown value, and a kernel that depends on
/// firmware for a register it could set itself fails on the first board whose
/// firmware does not.
///
/// Nothing is saved or restored: the kernel is built soft-float and one
/// program runs at a time, as ARMv7-A's version of this explains.
pub(crate) fn enable_user_fpu() {
    // SAFETY: `FPEN` only controls trapping; the kernel does not rely on
    // floating point trapping, and the `isb` makes the change take effect
    // before the next instruction.
    unsafe {
        asm!(
            "mrs {scratch}, cpacr_el1",
            "orr {scratch}, {scratch}, #{fpen}",
            "msr cpacr_el1, {scratch}",
            "isb",
            scratch = out(reg) _,
            fpen = const CPACR_FPEN_NO_TRAP,
            options(nostack, preserves_flags),
        );
    }
}

/// Set `TPIDR_EL1`, the software thread ID register the kernel keeps its
/// per-CPU record in.
///
/// A scratch register with no architectural meaning, which is the point: the
/// hardware never reads it, and EL0 cannot see it.
pub(crate) fn write_tpidr_el1(value: u64) {
    // SAFETY: writing a scratch register has no effect beyond the register.
    unsafe {
        asm!("msr tpidr_el1, {}", in(reg) value, options(nomem, nostack, preserves_flags));
    }
}

/// Read `TPIDR_EL1`.
pub(crate) fn read_tpidr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading a scratch register has no side effects.
    unsafe {
        asm!("mrs {}, tpidr_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// This function's frame pointer.
///
/// With `force-frame-pointers` on (see `.cargo/config.toml`) this register
/// holds the address of this function's frame record: the caller's frame
/// pointer, and the address it will return to. Walking that chain is how a
/// panic reports who called what.
#[inline(always)]
pub(crate) fn frame_pointer() -> u64 {
    let value: u64;
    // SAFETY: reading a register has no side effects.
    unsafe {
        asm!("mov {}, x29", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// The frequency of the architected counter, in hertz.
///
/// Firmware programs this register at reset and it is read-only thereafter, so
/// a zero here means firmware did not do its job rather than that the counter
/// is stopped.
pub(crate) fn read_cntfrq() -> u64 {
    let frequency: u64;
    // SAFETY: reading `CNTFRQ_EL0` has no side effects.
    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) frequency, options(nomem, nostack, preserves_flags));
    }
    frequency
}

/// The virtual counter.
///
/// The `isb` is not optional: the counter read is otherwise free to be
/// satisfied out of order with respect to the instructions around it, and a
/// timing loop built on that measures something other than what it thinks.
pub(crate) fn read_cntvct() -> u64 {
    let count: u64;
    // SAFETY: a barrier and a system register read, neither of which changes
    // any state.
    unsafe {
        asm!(
            "isb",
            "mrs {}, cntvct_el0",
            out(reg) count,
            options(nostack, preserves_flags),
        );
    }
    count
}

/// Set the instant the virtual timer fires at.
pub(crate) fn write_cntv_cval(instant: u64) {
    // SAFETY: `CNTV_CVAL_EL0` is a comparator. Writing it changes when the
    // timer's output asserts and nothing else.
    unsafe {
        asm!("msr cntv_cval_el0, {}", in(reg) instant, options(nomem, nostack, preserves_flags));
    }
}

/// Enable or mask the virtual timer.
pub(crate) fn write_cntv_ctl(control: u64) {
    // SAFETY: `CNTV_CTL_EL0` holds the timer's enable and mask bits. The `isb`
    // makes the write take effect before the next instruction, so a caller
    // that disarms and then returns cannot take one more interrupt.
    unsafe {
        asm!(
            "msr cntv_ctl_el0, {}",
            "isb",
            in(reg) control,
            options(nomem, nostack, preserves_flags),
        );
    }
}
