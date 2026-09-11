//! The x86-64 end of the loader.

use core::arch::asm;

use ferrix_bootinfo::Arch;

use super::Handoff;

/// The architecture the kernel is told it booted on.
pub(crate) const ARCH: Arch = Arch::X86_64;

/// The `e_machine` a kernel for this architecture must carry.
pub(crate) const ELF_MACHINE: u16 = ferrix_elf::EM_X86_64;

/// The ELF class a kernel for this architecture must be.
pub(crate) const ELF_CLASS: ferrix_elf::Class = ferrix_elf::Class::Elf64;

/// Extended Feature Enable Register.
const IA32_EFER: u32 = 0xC000_0080;
/// `EFER.NXE` — makes bit 63 of a page table entry mean "no execute".
const EFER_NXE: u64 = 1 << 11;
/// `CR4.PGE` — makes the global bit in a page table entry mean anything.
const CR4_PGE: u64 = 1 << 7;

/// Enable the CPU features the page tables the loader built depend on.
///
/// Both of these must happen **before** `CR3` is loaded. Bit 63 of a page table
/// entry is a *reserved* bit until `EFER.NXE` is set, and a reserved bit that is
/// set does not mean "no execute" — it means every access through that entry
/// takes a page fault.
pub(crate) fn prepare_cpu() -> Result<(), &'static str> {
    // SAFETY: reading IA32_EFER is architecturally defined on every 64-bit x86
    // and has no side effects.
    let efer = unsafe { read_msr(IA32_EFER) };
    // SAFETY: setting NXE only changes the meaning of a bit we are about to
    // start using, and the tables that use it are not installed yet.
    unsafe { write_msr(IA32_EFER, efer | EFER_NXE) };

    let cr4: u64;
    // SAFETY: reading CR4 has no side effects.
    unsafe {
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
    }
    // SAFETY: enabling global pages changes nothing until an entry with the
    // global bit is installed, which happens after this returns.
    unsafe {
        asm!("mov cr4, {}", in(reg) cr4 | CR4_PGE, options(nostack, preserves_flags));
    }

    Ok(())
}

/// Nothing to do: x86-64 caches are coherent with the page table walker, so a
/// table written through the cache is visible to it without being cleaned.
pub(crate) const fn clean_dcache(_start: u64, _len: u64) {}

/// Read a model-specific register.
///
/// # Safety
///
/// `msr` must be a register this CPU implements; reading one it does not raises
/// a general protection fault.
unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: the caller guarantees the register exists.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Write a model-specific register.
///
/// # Safety
///
/// `msr` must exist, and `value` must be one it accepts: an invalid value
/// raises a general protection fault, and a valid but wrong one changes how the
/// CPU interprets memory.
unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: the caller guarantees the register and the value.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Install the loader's page tables and jump to the kernel.
///
/// # Safety
///
/// Boot services must already have been exited, [`prepare_cpu`] must have been
/// called, and `handoff` must describe tables in which the loader's own code is
/// still identity mapped — the instruction after `mov cr3` is fetched through
/// the new tables.
pub(crate) unsafe fn enter_kernel(handoff: Handoff) -> ! {
    // One four-level tree covers both halves of the address space here, so the
    // loader should have handed us the same table twice.
    debug_assert_eq!(
        handoff.identity_table, handoff.root_table,
        "x86-64 has a single root table for both halves"
    );

    // SAFETY: the caller's contract is exactly the set of conditions that make
    // this sequence sound. `cli` first, because firmware's interrupt handlers
    // stopped existing at `exit_boot_services` and its timer has not.
    unsafe {
        asm!(
            "cli",
            "mov cr3, {root}",
            // From here the loader is running out of its identity mapping in
            // the kernel's tables.
            "mov rsp, {stack}",
            // A null frame pointer terminates any backtrace the kernel walks,
            // rather than letting it wander into the loader's dead frames.
            "xor rbp, rbp",
            "jmp {entry}",
            root = in(reg) handoff.root_table,
            stack = in(reg) handoff.stack_top,
            entry = in(reg) handoff.entry,
            // The System V argument register: the kernel entry takes the boot
            // info pointer as its only argument.
            in("rdi") handoff.boot_info,
            options(noreturn),
        );
    }
}
