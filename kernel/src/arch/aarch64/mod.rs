//! The `AArch64` end of the kernel.

pub(crate) mod console;
mod cpu;
mod trap;

use crate::early::{EarlyError, EarlyMemory};

/// Name for log lines.
pub(crate) const NAME: &str = "aarch64";

/// The page table descriptor layout this machine uses.
pub(crate) type PageEncoding = ferrix_paging::aarch64::AArch64;

/// Bring up the early console.
///
/// Unlike x86-64, this has real work to do: the PL011 is `MMIO` and has to be
/// mapped as device memory before a single byte can go out.
pub(crate) fn init_console(memory: &mut EarlyMemory) -> Result<(), EarlyError> {
    console::init(memory)
}

pub(crate) use trap::{TrapFrame, advance_past_breakpoint, breakpoint, classify, report_trap};

/// Install the exception vector table.
///
/// Until this runs the CPU is still pointing at firmware's vectors, which
/// stopped existing at `exit_boot_services`: any fault before it is a jump into
/// reclaimed memory.
///
/// # Safety
///
/// Must be called exactly once, on the boot CPU, before interrupts are
/// unmasked.
pub(crate) unsafe fn init_traps() {
    // SAFETY: called once from `kmain`, before anything faults deliberately.
    unsafe { trap::init() };
}

/// Publish page table writes and invalidate the whole TLB.
pub(crate) fn flush_tlb() {
    cpu::flush_tlb();
}

/// Stop the machine.
pub(crate) fn shutdown() -> ! {
    cpu::psci_system_off();
    halt()
}

/// Stop this CPU permanently.
pub(crate) fn halt() -> ! {
    cpu::disable_interrupts();
    loop {
        cpu::wfi();
    }
}
