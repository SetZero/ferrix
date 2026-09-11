//! The x86-64 end of the kernel.

pub(crate) mod console;
mod cpu;
mod gdt;
mod trap;

use crate::early::{EarlyError, EarlyMemory};

/// Name for log lines.
pub(crate) const NAME: &str = "x86_64";

/// The page table descriptor layout this machine uses.
pub(crate) type PageEncoding = ferrix_paging::x86_64::X86_64;

/// Bring up the early console.
///
/// Nothing to map: the 16550 is behind I/O ports, which are a separate address
/// space with no page tables of their own. The `AArch64` counterpart has to map
/// an `MMIO` window first, which is why this takes an argument it ignores.
pub(crate) fn init_console(_memory: &mut EarlyMemory) -> Result<(), EarlyError> {
    console::init();
    Ok(())
}

pub(crate) use trap::{TrapFrame, advance_past_breakpoint, breakpoint, classify, report_trap};

/// Install the descriptor tables and the trap handlers.
///
/// Until this runs the kernel is executing on firmware's tables: a fault would
/// enter a handler that stopped existing at `exit_boot_services`, which is a
/// triple fault and a silent reset.
///
/// # Safety
///
/// Must be called exactly once, on the boot CPU, before interrupts are enabled.
pub(crate) unsafe fn init_traps() {
    // SAFETY: called once from `kmain`, before anything can fault deliberately.
    unsafe { gdt::init() };
    // SAFETY: after `gdt::init`, whose kernel code selector every gate names.
    unsafe { trap::init() };
}

/// Invalidate the whole TLB.
pub(crate) fn flush_tlb() {
    cpu::reload_cr3();
}

/// Stop the machine, and QEMU with it.
pub(crate) fn shutdown() -> ! {
    cpu::debug_exit();
    halt()
}

/// Stop this CPU permanently.
pub(crate) fn halt() -> ! {
    cpu::disable_interrupts();
    loop {
        cpu::hlt();
    }
}
