//! The ARMv7-A end of the kernel.
//!
//! Everything the facade asks of an architecture, for a 32-bit Arm CPU with
//! the Large Physical Address Extension — a Cortex-A7 or A15 — described by a
//! device tree rather than by ACPI. The register-level drivers it shares with
//! AArch64, the GICv2 and the PL011, are `super::gicv2` and `super::pl011`;
//! what is here is how this architecture finds them, and everything that is
//! coprocessor 15 rather than a system register.

mod cpu;
mod timer;
mod trap;

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ferrix_bootinfo::BootView;
use ferrix_fdt::{GicVersion, PsciConduit};

use super::gicv2;
use crate::early::{EarlyError, EarlyMemory};
use crate::irq::Report;

pub(crate) use super::pl011 as console;
pub(crate) use trap::{TrapFrame, advance_past_breakpoint, breakpoint, classify, report_trap};

/// Name for log lines.
pub(crate) const NAME: &str = "armv7a";

/// The page table descriptor layout this machine uses.
pub(crate) type PageEncoding = ferrix_paging::armv7a::Armv7a;

/// Bring up the early console: the UART the device tree names.
///
/// `/chosen`'s `stdout-path` first, which is the machine saying which of its
/// UARTs is the console; the first PL011 in the tree if it does not say, or
/// says something this kernel cannot drive.
pub(crate) fn init_console(
    view: &BootView<'_>,
    memory: &mut EarlyMemory,
) -> Result<(), EarlyError> {
    const PL011: &str = "arm,pl011";

    let tree = crate::fdt::open(view).map_err(|_| EarlyError::NoConsole)?;
    let node = tree
        .console()
        .filter(|node| node.is_compatible(PL011))
        .or_else(|| tree.find_compatible(PL011))
        .ok_or(EarlyError::NoConsole)?;
    let registers = node.reg().next().ok_or(EarlyError::NoConsole)?;
    console::init(memory, registers.address)
}

/// Install the exception vector table.
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

/// Root of the loader's identity map, while it still exists.
///
/// `Some` for AArch64's reason: the identity map is a second translation
/// regime, under `TTBR0`, which nothing walking the kernel's own tables would
/// see — and the W^X sweep has to see everything the hardware can translate.
pub(crate) fn identity_root(view: &BootView<'_>) -> Option<u64> {
    let ttbr0 = view.raw().ttbr0_phys;
    (ttbr0 != 0 && !IDENTITY_DROPPED.load(Ordering::Relaxed)).then_some(ttbr0)
}

/// Set once [`drop_identity_map`] has run.
static IDENTITY_DROPPED: AtomicBool = AtomicBool::new(false);

/// Drop the loader's identity map.
///
/// # Safety
///
/// Nothing may still be executing or reading through the lower half of the
/// address space. See [`cpu::disable_ttbr0`].
pub(crate) unsafe fn drop_identity_map(_view: &BootView<'_>) {
    // SAFETY: the caller guarantees the lower half is unused, and the kernel
    // has run entirely in the upper half since its first instruction.
    unsafe { cpu::disable_ttbr0() };
    IDENTITY_DROPPED.store(true, Ordering::Relaxed);
}

/// How this machine's PSCI firmware is called: 0 until the device tree has
/// been read, then 1 for `hvc` and 2 for `smc`.
static PSCI: AtomicU8 = AtomicU8::new(0);

/// Stop the machine.
pub(crate) fn shutdown() -> ! {
    let conduit = match PSCI.load(Ordering::Relaxed) {
        1 => Some(PsciConduit::Hvc),
        2 => Some(PsciConduit::Smc),
        _ => None,
    };
    if let Some(conduit) = conduit {
        cpu::psci_system_off(conduit);
    }
    halt()
}

/// Stop this CPU permanently.
pub(crate) fn halt() -> ! {
    cpu::disable_interrupts();
    loop {
        cpu::wfi();
    }
}

/// Bring up the interrupt controller and the generic timer, from the device
/// tree.
///
/// # Safety
///
/// Must be called exactly once, on the boot CPU, after [`init_traps`] and
/// while interrupts are masked.
pub(crate) unsafe fn init_interrupts(view: &BootView<'_>) -> Result<Report, &'static str> {
    let tree = crate::fdt::open(view)?;

    let gic = tree
        .interrupt_controller()
        .ok_or("the device tree describes no interrupt controller")?;
    if gic.version != GicVersion::V2 {
        return Err("this GIC is not a GICv2, and GICv3 support is not written yet");
    }
    let distributor = gic
        .distributor()
        .ok_or("the device tree's GIC has no distributor")?;
    let cpu_interface = gic
        .cpu_interface()
        .ok_or("the device tree's GICv2 has no CPU interface")?;

    // SAFETY: called once from `kmain`, on the boot CPU, after the vector
    // table is installed and with interrupts masked.
    unsafe { gicv2::init(distributor.address, cpu_interface.address)? };
    timer::init(&tree)?;
    gicv2::enable(timer::irq());

    // Read now, used at the very end: `shutdown` must not have to parse
    // anything, since it is also what a panic ends in.
    let conduit = match tree.psci_conduit() {
        Some(PsciConduit::Hvc) => 1,
        Some(PsciConduit::Smc) => 2,
        None => 0,
    };
    PSCI.store(conduit, Ordering::Relaxed);

    Ok(Report {
        counter: "generic timer",
        counter_hz: timer::counter_hz(),
        controller: "GICv2",
        timer: "virtual timer",
        timer_hz: timer::counter_hz(),
    })
}

/// Unmask IRQs on this CPU.
pub(crate) fn enable_interrupts() {
    cpu::enable_interrupts();
}

/// How `ferrix_sync`'s interrupt-masking lock masks interrupts here.
#[derive(Debug)]
pub(crate) struct Irq;

// SAFETY: `disable` masks every interrupt on this CPU and returns the CPSR it
// found; `restore` puts back exactly that value's mask bits and nothing else,
// so nesting two critical sections cannot unmask halfway out of the outer one.
unsafe impl ferrix_sync::IrqControl for Irq {
    fn disable() -> usize {
        let previous = cpu::read_cpsr();
        cpu::disable_interrupts();
        previous as usize
    }

    fn restore(state: usize) {
        // SAFETY: `state` is a CPSR this CPU's `disable` read a moment ago,
        // which is exactly `restore_interrupt_mask`'s contract.
        unsafe { cpu::restore_interrupt_mask(state as u32) };
    }
}

/// Wait until an interrupt arrives.
pub(crate) fn wait_for_interrupt() {
    cpu::wfi();
}

/// The free-running counter.
pub(crate) fn counter_now() -> u64 {
    timer::counter_now()
}

/// How fast it counts.
pub(crate) fn counter_hz() -> u64 {
    timer::counter_hz()
}

/// Fire the timer interrupt once, `nanos` from now.
pub(crate) fn timer_arm(nanos: u64) {
    timer::arm(nanos);
}

/// Stop the timer.
pub(crate) fn timer_disarm() {
    timer::disarm();
}

/// The interrupt number the timer arrives on.
pub(crate) fn timer_irq() -> u32 {
    timer::irq()
}

/// Claim every pending interrupt, dispatch it, and retire it — a loop, for
/// the reason AArch64's is one: the exception is taken once however many are
/// pending.
pub(crate) fn service_interrupts(_frame: &mut TrapFrame, handle: fn(u32)) {
    while let Some((id, acknowledgement)) = gicv2::claim() {
        handle(id);
        gicv2::complete(acknowledgement);
    }
}
