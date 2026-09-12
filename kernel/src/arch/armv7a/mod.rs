//! The ARMv7-A end of the kernel.
//!
//! Everything the facade asks of an architecture, for a 32-bit Arm CPU with
//! the Large Physical Address Extension — a Cortex-A7 or A15 — described by a
//! device tree rather than by ACPI. The register-level drivers live beside
//! this directory in `kernel/src/arch/`: the GICv2, which AArch64 shares, and
//! the two serial ports, one of which every machine here has. What is in this
//! directory is how this architecture finds them, and everything that is
//! coprocessor 15 rather than a system register.

pub(crate) mod console;
mod cpu;
mod smp;
mod switch;
mod timer;
mod trap;

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ferrix_bootinfo::{Arch, BootView};
use ferrix_fdt::{GicVersion, PsciConduit};
use ferrix_linux_abi::nr::{self, Syscall};

use super::gicv2;
use crate::early::{EarlyError, EarlyMemory};
use crate::irq::Report;

pub(crate) use smp::{CpuStarter, describe_cpus, hardware_id};
pub(crate) use trap::{TrapFrame, advance_past_breakpoint, breakpoint, classify, report_trap};

/// Point this CPU's per-CPU register at `address`.
///
/// # Safety
///
/// `address` must be this processor's own `PerCpu` record, which must live for
/// the rest of the system's life: `cpu_local` hands it back as a reference.
/// It is below 4 GiB, as every address on this architecture is, so the
/// narrowing to the register's width loses nothing.
pub(crate) unsafe fn set_cpu_local(address: u64) {
    cpu::write_tpidrprw(address as u32);
}

/// The address [`set_cpu_local`] installed on this CPU.
///
/// # Safety
///
/// [`set_cpu_local`] must have run on this CPU. Until it has, `TPIDRPRW` holds
/// whatever firmware left there.
pub(crate) unsafe fn cpu_local() -> u64 {
    u64::from(cpu::read_tpidrprw())
}

/// This CPU's per-CPU register, read from the register rather than through it.
///
/// Safe where [`cpu_local`] is not: before [`set_cpu_local`] has run the value
/// is whatever firmware left there, but reading it cannot fault. For a failure
/// report, which has to name the processor without trusting it.
pub(crate) fn cpu_local_register() -> u64 {
    u64::from(cpu::read_tpidrprw())
}

/// Name for log lines.
pub(crate) const NAME: &str = "armv7a";

/// This machine, as the hand-off structure names it.
///
/// The kernel needs it for the same reason the loader does: to refuse an ELF
/// image built for a different architecture. `boot/src/arch/` has carried the
/// same constant since stage 1; this is the kernel's copy, and the two are
/// checked against each other by the image simply booting.
pub(crate) const ARCH: Arch = Arch::Armv7a;

/// The page table descriptor layout this machine uses.
pub(crate) type PageEncoding = ferrix_paging::armv7a::Armv7a;

/// Bring up the early console: the port the device tree names.
///
/// Which port that is, and which of the two drivers it wants, is
/// [`console::init`]'s decision. This is where the tree it decides from comes
/// from, and the only reason the two are separate functions.
pub(crate) fn init_console(
    view: &BootView<'_>,
    memory: &mut EarlyMemory,
) -> Result<(), EarlyError> {
    let tree = crate::fdt::open(view).map_err(|_| EarlyError::NoConsole)?;
    console::init(&tree, memory)
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

/// Publish page table writes and invalidate the whole TLB — every core's.
pub(crate) fn flush_tlb() {
    cpu::flush_tlb();
}

/// Whether [`flush_tlb`] reaches every processor's TLB.
///
/// Yes, as on AArch64: `TLBIALLIS` is broadcast to the inner shareable
/// domain, and the `dsb ish` after it waits for every core to finish.
pub(crate) const TLB_FLUSH_IS_BROADCAST: bool = true;

/// Fold an ARMv7-A (EABI) system call number onto the call it means.
///
/// The EABI table predates the generic one and kept much of what the generic
/// one dropped, so this architecture has `open` and `fork` where AArch64 does
/// not. It also has calls neither 64-bit architecture has: the `64` forms that
/// carry an offset a 32-bit register cannot, and the ARM-private range at
/// `0x0f0000`, which is why a valid number here is *not* bounded by the size
/// of the shared table. This is the only place in the kernel that knows which
/// of the three tables applies; `crate::syscall` dispatches on the answer.
pub(crate) fn decode_syscall(number: usize) -> Option<Syscall> {
    nr::from_arm(number)
}

/// A whole program, in machine code, for the self-check to run in user mode.
///
/// Empty on this architecture because there is nowhere yet to run it: see
/// [`run_user`]. The self-check skips the program when this is empty rather
/// than failing, so the boot test still reports what does exist here.
pub(crate) const USER_TEST_PROGRAM: &[u8] = &[];

/// The status [`USER_TEST_PROGRAM`] exits with.
pub(crate) const USER_TEST_STATUS: i32 = 42;

/// One byte from the console, if one has arrived.
///
/// Always `None` here for now: this architecture's UART drivers are
/// write-only, and nothing reads from the console until a program can run in
/// user mode on it.
#[expect(
    clippy::missing_const_for_fn,
    reason = "one architecture's version of this reads a register"
)]
pub(crate) fn read_console_byte() -> Option<u8> {
    None
}

/// Run a program in ring 3, returning the status it exits with.
///
/// Not built on this architecture yet. The x86-64 transition landed first
/// because that is where a static binary could be produced to test it; the
/// USR one is stage 6's remaining work, and this reports the absence
/// rather than pretending or halting, so the boot test still says what does
/// and does not exist here.
///
/// # Safety
///
/// The same contract the x86-64 body has: a user address space installed on
/// this processor, and `entry` and `stack` inside it.
pub(crate) unsafe fn run_user(_entry: u64, _stack: u64) -> Result<i32, &'static str> {
    Err("user mode is not built on this architecture yet")
}

/// Make a freshly allocated user root usable.
///
/// Nothing to do: the kernel's half is reached through `TTBR1` and a user root is
/// only ever installed in `TTBR0`, so the two never share a tree and a user
/// root has nothing of the kernel's to be given. x86-64, which keeps both
/// halves in one root, is the architecture this exists for.
#[expect(
    clippy::missing_const_for_fn,
    reason = "one architecture's version of this does real work"
)]
pub(crate) fn prepare_user_root(_root: u64) {}

/// Translate this processor's lower half through the tables at `root`.
///
/// AArch64's version of this, in coprocessor 15's spelling and with one extra
/// thing to get right: `TTBCR.EPD0` is *set* on this architecture from the
/// moment the loader's identity map is dropped, so installing a user root is
/// not only a register write but the re-enabling of a translation regime. A
/// change that wrote `TTBR0` and left `EPD0` alone would fault on every user
/// access and look exactly like page tables that are wrong — which they would
/// not be. See [`cpu::write_ttbr0`].
///
/// # Safety
///
/// `root` must root a live set of tables for the lower half, and they must
/// stay live until another root replaces them on this processor.
pub(crate) unsafe fn install_user_root(root: u64) {
    // SAFETY: the caller guarantees the tables are live.
    unsafe { cpu::write_ttbr0(root) };
    cpu::flush_user_tlb();
}

/// Stop translating the lower half at all.
///
/// What a processor picking up a kernel thread does; see AArch64's, whose
/// argument is the same one. `EPD0` governs walks and not the `TLB`, so the
/// cached user entries have to be invalidated as well.
///
/// # Safety
///
/// Nothing may still need a user address on this processor.
pub(crate) unsafe fn uninstall_user_root() {
    // SAFETY: the caller guarantees no user address is wanted; the kernel is
    // reached entirely through `TTBR1`.
    unsafe { cpu::disable_ttbr0() };
    cpu::flush_user_tlb();
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

/// Drop the loader's identity map, wherever the loader had to put it.
///
/// Usually that is the `TTBR0` regime and nothing else, which is switched off
/// rather than dismantled. On a machine whose RAM is above the split — the
/// STM32MP157, whose DDR starts at 3 GiB — `TTBR0` does not translate those
/// addresses at all, so the loader mapped its own image inside the kernel's
/// tree and that mapping is unmapped by hand first. It cannot be left: the
/// memory under it is the loader's, which [`crate::mm::reclaim_boot_memory`]
/// is about to hand to the frame allocator, and an executable mapping of
/// memory somebody else now owns is a worse version of the thing this function
/// exists to remove.
///
/// # Safety
///
/// Nothing may still be executing or reading through the lower half of the
/// address space, or through that mapping. See [`cpu::disable_ttbr0`].
pub(crate) unsafe fn drop_identity_map(view: &BootView<'_>) {
    if let Some((base, len)) = view.loader_alias() {
        // The frames under it are the loader's own image and not this
        // mapping's to free; the memory map is what gives those back. A
        // failure here needs no report of its own, because the mapping is
        // writable and executable and the W^X sweep immediately after this is
        // exactly what notices one that survived.
        let _ = crate::mm::unmap_kernel(base, len, |_, _| {});
    }
    // SAFETY: the caller guarantees the lower half is unused, and the kernel
    // has run entirely in the upper half since its first instruction.
    unsafe { cpu::disable_ttbr0() };
    IDENTITY_DROPPED.store(true, Ordering::Relaxed);
}

/// How this machine's PSCI firmware is called: 0 until the device tree has
/// been read, then 1 for `hvc` and 2 for `smc`.
static PSCI: AtomicU8 = AtomicU8::new(0);

/// How this machine's PSCI firmware is called, once [`init_interrupts`] has
/// read the device tree.
fn psci_conduit() -> Option<PsciConduit> {
    match PSCI.load(Ordering::Relaxed) {
        1 => Some(PsciConduit::Hvc),
        2 => Some(PsciConduit::Smc),
        _ => None,
    }
}

/// Stop the machine.
pub(crate) fn shutdown() -> ! {
    if let Some(conduit) = psci_conduit() {
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
    // And the inter-processor interrupt, whose enable bit is this core's own:
    // every secondary turns on its copy in `gicv2::init_this_cpu`.
    gicv2::enable(gicv2::IPI_SGI);

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

/// Mask every interrupt on this CPU.
pub(crate) fn disable_interrupts() {
    cpu::disable_interrupts();
}

/// With interrupts masked, wait for one and then unmask IRQs, so one that
/// arrived since they were masked wakes the wait instead of being lost before
/// it. Returns with IRQs unmasked.
pub(crate) fn wait_for_work() {
    cpu::wait_then_enable_interrupts();
}

/// The interrupt number inter-processor interrupts arrive on.
pub(crate) const fn ipi_irq() -> u32 {
    gicv2::IPI_SGI
}

/// Interrupt every core but this one.
///
/// The barrier is here rather than in the shared driver because it is an
/// instruction, and each architecture spells its own.
pub(crate) fn send_ipi_to_others() -> Result<(), &'static str> {
    cpu::dsb_ishst();
    gicv2::send_sgi_to_others();
    Ok(())
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

/// The context switch, and the stack layout a new task starts on.
pub(crate) use switch::{prepare_stack, switch_to};
