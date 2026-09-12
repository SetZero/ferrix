//! The x86-64 end of the kernel.

mod apic;
mod clock;
pub(crate) mod console;
mod cpu;
mod gdt;
mod smp;
mod switch;
mod trap;

use ferrix_bootinfo::{Arch, BootView};
use ferrix_linux_abi::nr::{self, Syscall};

use crate::early::{EarlyError, EarlyMemory};
use crate::irq::Report;

/// Name for log lines.
pub(crate) const NAME: &str = "x86_64";

/// This machine, as the hand-off structure names it.
///
/// The kernel needs it for the same reason the loader does: to refuse an ELF
/// image built for a different architecture. `boot/src/arch/` has carried the
/// same constant since stage 1; this is the kernel's copy, and the two are
/// checked against each other by the image simply booting.
pub(crate) const ARCH: Arch = Arch::X86_64;

/// The page table descriptor layout this machine uses.
pub(crate) type PageEncoding = ferrix_paging::x86_64::X86_64;

/// Bring up the early console.
///
/// Nothing to find and nothing to map: the 16550 is at a fixed port, behind
/// I/O space, which has no page tables of its own. The Arm counterparts have
/// to find a UART and map an `MMIO` window first, which is why this takes two
/// arguments it ignores.
pub(crate) fn init_console(
    _view: &BootView<'_>,
    _memory: &mut EarlyMemory,
) -> Result<(), EarlyError> {
    console::init();
    Ok(())
}

pub(crate) use smp::{CpuStarter, describe_cpus, hardware_id};

/// `IA32_GS_BASE`: the base address `GS`-relative accesses are made from.
///
/// The kernel's, for now. Once there is a user mode this is the register
/// `swapgs` exchanges with `IA32_KERNEL_GS_BASE` on every entry and exit, and
/// the per-CPU record moves to whichever of the two the kernel side holds.
const IA32_GS_BASE: u32 = 0xC000_0101;

/// Point this CPU's per-CPU register at `address`.
///
/// # Safety
///
/// `address` must be this processor's own `PerCpu` record, which must live for
/// the rest of the system's life: `cpu_local` hands it back as a reference.
pub(crate) unsafe fn set_cpu_local(address: u64) {
    // SAFETY: `IA32_GS_BASE` exists on every 64-bit x86 and accepts any
    // canonical address, which a kernel pointer is.
    unsafe { cpu::write_msr(IA32_GS_BASE, address) };
}

/// The address [`set_cpu_local`] installed on this CPU.
///
/// # Safety
///
/// [`set_cpu_local`] must have run on this CPU. Before it has, `GS` points
/// wherever firmware left it and the load below reads from there.
pub(crate) unsafe fn cpu_local() -> u64 {
    // SAFETY: the caller guarantees `GS` points at a per-CPU record, whose
    // first word is its own address.
    unsafe { cpu::read_gs_word() }
}

/// This CPU's per-CPU register, read from the register rather than through it.
///
/// Safe where [`cpu_local`] is not: before [`set_cpu_local`] has run the value
/// is whatever firmware left there, but reading it cannot fault. For a failure
/// report, which has to name the processor without trusting it.
pub(crate) fn cpu_local_register() -> u64 {
    // SAFETY: `IA32_GS_BASE` exists on every 64-bit x86.
    unsafe { cpu::read_msr(IA32_GS_BASE) }
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

/// Invalidate the whole TLB — this processor's, global entries included.
pub(crate) fn flush_tlb() {
    cpu::flush_tlb_including_global();
}

/// Whether [`flush_tlb`] reaches every processor's TLB.
///
/// No: reloading `CR3` and `invlpg` are both local. Another processor's
/// stale translations are dropped by that processor, told to by an
/// interrupt, which is what a TLB shootdown is.
pub(crate) const TLB_FLUSH_IS_BROADCAST: bool = false;

/// Root of the loader's identity map, while it still exists.
///
/// Always `None` on x86-64: there is only one root table, and the identity map
/// is the lower half of it. A sweep that walks the kernel's tables has
/// therefore already seen it — which is what makes the W^X check on this
/// architecture find the identity map without being told where it is.
pub(crate) const fn identity_root(_view: &BootView<'_>) -> Option<u64> {
    None
}

/// The first root-table slot belonging to the upper half.
///
/// A 48-bit address space has 512 top-level slots, and the upper half starts
/// at slot 256. Everything below it is the identity map the loader built and
/// nothing else: the direct map begins at slot 256, the `vmap` area at 510 and
/// the kernel image at 511.
const UPPER_HALF_SLOT: usize = 256;

/// Top-level slots in a four-level root table.
const ROOT_SLOTS: usize = 512;

/// Fold an x86-64 system call number onto the call it means.
///
/// x86-64 kept the table it grew rather than adopting the generic one every
/// architecture added after 2011 uses, so `read` is 0 here and 63 on AArch64.
/// This is the only place in the kernel that knows which of the three tables
/// applies; `crate::syscall` dispatches on the answer.
pub(crate) fn decode_syscall(number: usize) -> Option<Syscall> {
    nr::from_x86_64(number)
}

/// Make a freshly allocated user root usable.
///
/// x86-64 keeps both halves of the address space in one root, so a user root
/// that did not name the kernel's tables would fault on the first instruction
/// of the trap handler it entered — including the page fault handler, which is
/// a triple fault and a silent reset. The kernel's top-level slots are shared
/// rather than copied, so a later kernel mapping appears in every address
/// space without any of them being walked.
pub(crate) fn prepare_user_root(root: u64) {
    crate::mm::share_kernel_slots(root, UPPER_HALF_SLOT..ROOT_SLOTS);
}

/// Translate this processor's user half through the tables at `root`.
///
/// One write to `CR3`, and nothing else. The flush is the architecture's: a
/// write to `CR3` drops every cached translation that is not marked global,
/// which is precisely the set that belongs to the address space being left.
/// The kernel's own translations are global — `CR4.PGE` is set by the loader
/// and the kernel keeps it — so kernel text, the direct map and the device
/// windows survive the switch and are not re-walked.
///
/// It is therefore emphatically *not* [`flush_tlb`], which is the global
/// flush: using it here would throw away exactly the entries that must be
/// kept, on every switch, for nothing.
///
/// # Safety
///
/// `root` must be a root table [`prepare_user_root`] has made, so that it
/// still names the kernel's upper half — the instruction after this one is a
/// kernel instruction, and the trap taken if it did not translate would be a
/// triple fault. The tables it roots must also stay alive until another root
/// replaces this one on this processor.
pub(crate) unsafe fn install_user_root(root: u64) {
    // SAFETY: the caller guarantees the root carries the kernel's half, which
    // is what maps the code and stack this returns onto.
    unsafe { cpu::write_cr3(root) };
}

/// Go back to translating nothing but the kernel's own tables.
///
/// What a processor picking up a kernel thread does, so that no user address
/// translates while one runs. The alternative — leaving the outgoing process's
/// root installed, because a kernel thread has no user addresses to get wrong
/// — is Linux's lazy TLB, and it is an optimisation that has to keep the
/// address space alive underneath a thread that does not reference it. Stage 6
/// takes the plain version.
///
/// # Safety
///
/// Nothing may still need a user address on this processor.
pub(crate) unsafe fn uninstall_user_root() {
    // SAFETY: the kernel's own root maps everything the kernel runs on, and
    // the caller guarantees no user address is wanted.
    unsafe { cpu::write_cr3(crate::mm::root_table()) };
}

/// Drop the loader's identity map by clearing the lower half of the root
/// table.
///
/// The frames those tables occupied are *not* given back. They are part of the
/// loader's page table pool, which the memory map reports as
/// `MemKind::PageTables` and which the kernel is still running on — the upper
/// half's tables came out of the same pool. Reclaiming the lower half's share
/// would mean tracking which frame of the pool belongs to which half, and the
/// pool is a few dozen frames.
///
/// # Safety
///
/// Nothing may still be executing or reading through the lower half. The
/// kernel runs entirely in the upper half from its first instruction.
pub(crate) unsafe fn drop_identity_map(_view: &BootView<'_>) {
    crate::mm::clear_root_slots(0..UPPER_HALF_SLOT);
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

/// Bring up the local APIC, the counter and the timer.
///
/// Order matters twice over. The counter has to exist before the local APIC
/// timer can be calibrated against it, and the interrupt descriptor table has
/// to exist before the local APIC is enabled — an interrupt delivered to a
/// vector with no gate is a fault the CPU cannot report.
///
/// # Safety
///
/// Must be called exactly once, on the boot CPU, after [`init_traps`] and
/// while interrupts are still masked.
pub(crate) unsafe fn init_interrupts(view: &BootView<'_>) -> Result<Report, &'static str> {
    let firmware =
        crate::acpi::Firmware::open(view).map_err(|_| "the machine has no readable ACPI tables")?;
    let acpi = firmware.acpi();

    let counter = clock::init(&acpi)?;
    // SAFETY: called once from `kmain`, on the boot CPU, after `init_traps`
    // filled the IDT and with interrupts masked.
    unsafe { apic::init(&acpi)? };

    Ok(Report {
        counter,
        counter_hz: clock::counter_hz(),
        controller: "APIC",
        timer: "local APIC timer",
        timer_hz: apic::timer_hz(),
    })
}

/// Unmask interrupts on this CPU.
pub(crate) fn enable_interrupts() {
    cpu::enable_interrupts();
}

/// Mask interrupts on this CPU.
pub(crate) fn disable_interrupts() {
    cpu::disable_interrupts();
}

/// With interrupts masked, unmask them and wait for one, atomically: an
/// interrupt that arrived since they were masked wakes the wait instead of
/// being taken just before it. Returns with interrupts unmasked.
pub(crate) fn wait_for_work() {
    cpu::enable_interrupts_and_halt();
}

pub(crate) use apic::{ipi_irq, send_ipi_to_others};

/// How `ferrix_sync`'s interrupt-masking lock masks interrupts here.
#[derive(Debug)]
pub(crate) struct Irq;

// SAFETY: `disable` masks interrupts on this CPU with `cli` and reports
// whether they were unmasked beforehand; `restore` unmasks only if they were,
// so nesting two critical sections leaves the inner one unable to unmask
// halfway out of the outer one. Neither touches any other state.
unsafe impl ferrix_sync::IrqControl for Irq {
    fn disable() -> usize {
        let was_enabled = cpu::read_rflags() & cpu::RFLAGS_INTERRUPT != 0;
        cpu::disable_interrupts();
        usize::from(was_enabled)
    }

    fn restore(state: usize) {
        if state != 0 {
            cpu::enable_interrupts();
        }
    }
}

/// Wait until an interrupt arrives.
pub(crate) fn wait_for_interrupt() {
    cpu::hlt();
}

/// The free-running counter.
pub(crate) fn counter_now() -> u64 {
    clock::counter_now()
}

/// How fast it counts.
pub(crate) fn counter_hz() -> u64 {
    clock::counter_hz()
}

/// Fire the timer interrupt once, `nanos` from now.
pub(crate) fn timer_arm(nanos: u64) {
    apic::arm(nanos);
}

/// Stop the timer.
pub(crate) fn timer_disarm() {
    apic::disarm();
}

/// The interrupt number the timer arrives on.
pub(crate) fn timer_irq() -> u32 {
    apic::timer_irq()
}

/// Dispatch the interrupt that arrived and retire it at the controller.
///
/// x86-64 puts the vector in the frame, so there is nothing to claim: the
/// work here is deciding what *not* to acknowledge. The spurious vector is
/// raised by the local APIC when an interrupt is withdrawn between being
/// signalled and being taken, and it is the one vector that must never be
/// given an end-of-interrupt — doing so retires a different interrupt that
/// was genuinely in service.
pub(crate) fn service_interrupts(frame: &mut TrapFrame, handle: fn(u32)) {
    if frame.vector == apic::SPURIOUS_VECTOR {
        return;
    }
    handle((frame.vector - trap::IRQ_BASE) as u32);
    apic::end_of_interrupt();
}

/// The context switch, and the stack layout a new task starts on.
pub(crate) use switch::{prepare_stack, switch_to};
