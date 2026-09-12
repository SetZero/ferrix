//! The `AArch64` end of the kernel.

pub(crate) mod console;
mod cpu;
mod gic;
mod smp;
mod switch;
mod timer;
mod trap;

use core::sync::atomic::{AtomicBool, Ordering};

use ferrix_bootinfo::{Arch, BootView};
use ferrix_linux_abi::nr::{self, Syscall};
use ferrix_linux_abi::types::{self, OpenFlagBits};

use super::gicv2;
use crate::early::{EarlyError, EarlyMemory};
use crate::irq::Report;

/// Name for log lines.
pub(crate) const NAME: &str = "aarch64";

/// This machine, as the hand-off structure names it.
///
/// The kernel needs it for the same reason the loader does: to refuse an ELF
/// image built for a different architecture. `boot/src/arch/` has carried the
/// same constant since stage 1; this is the kernel's copy, and the two are
/// checked against each other by the image simply booting.
pub(crate) const ARCH: Arch = Arch::AArch64;

/// Which `struct stat` the stat calls fill in: the generic one, 128 bytes,
/// from `include/uapi/asm-generic/stat.h`, as on every architecture added
/// after 2011.
pub(crate) const STAT_LAYOUT: crate::syscall::stat::StatLayout =
    crate::syscall::stat::StatLayout::Generic;

/// The page table descriptor layout this machine uses.
pub(crate) type PageEncoding = ferrix_paging::aarch64::AArch64;

/// Bring up the early console.
///
/// Unlike x86-64, this has real work to do: the PL011 is `MMIO` and has to be
/// mapped as device memory before a single byte can go out. Its address is
/// still the `virt` machine's, rather than the SPCR's or the device tree's.
pub(crate) fn init_console(
    _view: &BootView<'_>,
    memory: &mut EarlyMemory,
) -> Result<(), EarlyError> {
    console::init(memory)
}

pub(crate) use smp::{CpuStarter, describe_cpus, hardware_id};

/// Point this CPU's per-CPU register at `address`.
///
/// # Safety
///
/// `address` must be this processor's own `PerCpu` record, which must live for
/// the rest of the system's life: `cpu_local` hands it back as a reference.
pub(crate) unsafe fn set_cpu_local(address: u64) {
    cpu::write_tpidr_el1(address);
}

/// The address [`set_cpu_local`] installed on this CPU.
///
/// # Safety
///
/// [`set_cpu_local`] must have run on this CPU. Until it has, `TPIDR_EL1`
/// holds whatever it held at reset, which the architecture leaves unknown.
pub(crate) unsafe fn cpu_local() -> u64 {
    cpu::read_tpidr_el1()
}

/// This CPU's per-CPU register, read from the register rather than through it.
///
/// Safe where [`cpu_local`] is not: before [`set_cpu_local`] has run the value
/// is whatever reset left there, but reading it cannot fault. For a failure
/// report, which has to name the processor without trusting it.
pub(crate) fn cpu_local_register() -> u64 {
    cpu::read_tpidr_el1()
}
pub(crate) use trap::{
    TrapFrame, advance_past_breakpoint, breakpoint, classify, enter_user, report_trap, system_call,
};

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

/// Publish page table writes and invalidate the whole TLB — every core's.
pub(crate) fn flush_tlb() {
    cpu::flush_tlb();
}

/// Whether [`flush_tlb`] reaches every processor's TLB.
///
/// Yes: `tlbi vmalle1is` is broadcast to the inner shareable domain, which
/// every core of the machine is in, and the `dsb ish` after it does not
/// complete until every one of them has done it. The TLB shootdown x86-64
/// needs interrupts for, this architecture does in hardware.
pub(crate) const TLB_FLUSH_IS_BROADCAST: bool = true;

/// Fold an AArch64 system call number onto the call it means.
///
/// AArch64 uses the generic table from `include/uapi/asm-generic/unistd.h`,
/// which has no `open`, no `fork` and no `dup2` -- musl reaches all three
/// through the `*at` or flag-taking forms. This is the only place in the
/// kernel that knows which of the three tables applies; `crate::syscall`
/// dispatches on the answer.
pub(crate) fn decode_syscall(number: usize) -> Option<Syscall> {
    nr::from_aarch64(number)
}

/// The `open` flag bits that differ between architectures, as this one
/// numbers them. AArch64 uses the generic *call* table but not the generic
/// flags: `arch/arm64/include/uapi/asm/fcntl.h` keeps 32-bit Arm's, so that a
/// compat program's flags mean the same thing to both kernels.
pub(crate) const OPEN_FLAGS: OpenFlagBits = types::OPEN_FLAGS_ARM;

/// A whole program, in machine code, for the self-check to run at EL0.
///
/// `write(1, "hello from EL0\n", 15)` then `exit_group(42)`, using the generic
/// table's numbers — `write` is 64 and `exit_group` 94 on this architecture, not
/// x86-64's 1 and 231. Position-independent: the string is found with `adr`, so
/// the page can be mapped anywhere.
///
/// Assembled by rustc's own LLVM from `global_asm!` and read back out of the
/// object file, rather than encoded by hand, so the bytes cannot disagree with
/// the assembler that builds the rest of the kernel.
pub(crate) const USER_TEST_PROGRAM: &[u8] = &[
    0x08, 0x08, 0x80, 0xd2, // mov  x8, #64      // write
    0x20, 0x00, 0x80, 0xd2, // mov  x0, #1       // fd 1
    0xe1, 0x00, 0x00, 0x10, // adr  x1, msg
    0xe2, 0x01, 0x80, 0xd2, // mov  x2, #15      // length
    0x01, 0x00, 0x00, 0xd4, // svc  #0
    0xc8, 0x0b, 0x80, 0xd2, // mov  x8, #94      // exit_group
    0x40, 0x05, 0x80, 0xd2, // mov  x0, #42      // status
    0x01, 0x00, 0x00, 0xd4, // svc  #0
    0x00, 0x00, 0x00, 0x14, // b    .            // never reached
    // "hello from EL0\n"
    0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x66, 0x72, 0x6f, 0x6d, 0x20, 0x45, 0x4c, 0x30, 0x0a,
];

/// The status [`USER_TEST_PROGRAM`] exits with.
pub(crate) const USER_TEST_STATUS: i32 = 42;

/// A program that spins, then writes a tagged line and exits with a status it
/// reads out of its own image.
///
/// For the check that two programs run at once: it spins long enough to be
/// preempted in EL0, which a program run with interrupts masked never is. The
/// last ten bytes are the layout every architecture's copy shares, so the check
/// patches them without knowing the instruction set -- the tag character and
/// the newline, then the loop count and the exit status as little-endian
/// words, both loaded PC-relative with `ldr`.
///
/// ```text
///   ldr   w9, count
/// 1: subs x9, x9, #1
///   b.ne  1b
///   mov   x8, #64 ; mov x0, #1 ; adr x1, msg ; mov x2, #16 ; svc #0
///   mov   x8, #94 ; ldr w0, status ; svc #0
///   b     .
/// msg: "spinning task ?\n"   count: .word   status: .word
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file, as
/// [`USER_TEST_PROGRAM`] was.
pub(crate) const USER_SPIN_PROGRAM: &[u8] = &[
    0x09, 0x02, 0x00, 0x18, 0x29, 0x05, 0x00, 0xf1, 0xe1, 0xff, 0xff, 0x54, 0x08, 0x08, 0x80, 0xd2,
    0x20, 0x00, 0x80, 0xd2, 0xe1, 0x00, 0x00, 0x10, 0x02, 0x02, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
    0xc8, 0x0b, 0x80, 0xd2, 0x00, 0x01, 0x00, 0x18, 0x01, 0x00, 0x00, 0xd4, 0x00, 0x00, 0x00, 0x14,
    b's', b'p', b'i', b'n', b'n', b'i', b'n', b'g', b' ', b't', b'a', b's', b'k', b' ', b'?',
    b'\n', 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// The stage 9 exit test's program: one side of a conversation over a channel,
/// spoken entirely in the native ABI.
///
/// Two copies run as two processes, told apart by a role byte the check
/// patches in. The sender (`s`) creates a VMO, writes a secret into it, sends
/// `ping` with the VMO's handle over its bootstrap channel, waits for the
/// reply, and exits 0 only if the reply is the secret. The receiver (any other
/// role) waits, reads the message and the handle, reads the secret back out of
/// the VMO through the handle it was given, and sends that as the reply.
///
/// A failure exits with the number of the step that failed, so the status is
/// the diagnosis: the sender's calls are 1 to 5 and its comparison 6; the
/// receiver's calls are 11 to 14, and a message without exactly one handle is
/// 20. The number is kept in a register the kernel preserves across a call and
/// no call takes as an argument.
///
/// The layout every architecture's copy shares, so the check patches it
/// without knowing the instruction set: a branch padded to four bytes, the
/// secret at 4, `ping` at 23, and eight bytes at 28 -- the role, a newline, two
/// zero bytes, and the bootstrap handle as a little-endian word. The data comes
/// first rather than last because ARM state's `adr` reaches only what an 8-bit
/// rotated immediate can express, and data after the code was out of that
/// reach.
///
/// ```text
///   b start ; secret: "carried by a handle" ; ping: "ping" ; role: '?' '\n' 0 0 ; handle: .long
/// start:
///   load the handle and the role; reserve 256 bytes of stack for buffers
///   sender:   vmo_create(64) ; vmo_write(vmo, secret, 19, &0)
///             channel_write(h, ping, 4, &vmo, 1)
///             object_wait_one(h, READABLE | PEER_CLOSED, null, null)
///             channel_read(h, buf, 64, handles, 4, &actual)
///             exit(actual.bytes == 19 && buf == secret ? 0 : 1)
///   receiver: object_wait_one(h, READABLE, null, null)
///             channel_read(h, buf, 64, handles, 4, &actual)
///             vmo_read(handles[0], reply, 19, &0) ; channel_write(h, reply, 19, null, 0)
///             exit(0)
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file, as
/// [`USER_SPIN_PROGRAM`] was.
pub(crate) const USER_NATIVE_PROGRAM: &[u8] = &[
    0x09, 0x00, 0x00, 0x14, 0x63, 0x61, 0x72, 0x72, 0x69, 0x65, 0x64, 0x20, 0x62, 0x79, 0x20, 0x61,
    0x20, 0x68, 0x61, 0x6e, 0x64, 0x6c, 0x65, 0x70, 0x69, 0x6e, 0x67, 0x00, 0x3f, 0x0a, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xc9, 0xff, 0xff, 0x10, 0x34, 0x01, 0x40, 0x39, 0x33, 0x05, 0x40, 0xb9,
    0xff, 0x03, 0x04, 0xd1, 0xff, 0x4f, 0x00, 0xf9, 0x9f, 0xce, 0x01, 0x71, 0x61, 0x07, 0x00, 0x54,
    0x36, 0x00, 0x80, 0xd2, 0x08, 0x04, 0x82, 0xd2, 0x00, 0x08, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
    0x1f, 0x00, 0x00, 0xf1, 0xcd, 0x0b, 0x00, 0x54, 0xf5, 0x03, 0x00, 0x2a, 0x56, 0x00, 0x80, 0xd2,
    0x48, 0x04, 0x82, 0xd2, 0xe0, 0x03, 0x15, 0xaa, 0xe1, 0xfc, 0xff, 0x10, 0x62, 0x02, 0x80, 0xd2,
    0xe3, 0x63, 0x02, 0x91, 0x01, 0x00, 0x00, 0xd4, 0xa0, 0x0a, 0x00, 0xb5, 0xf5, 0xa3, 0x00, 0xb9,
    0x76, 0x00, 0x80, 0xd2, 0x28, 0x02, 0x82, 0xd2, 0xe0, 0x03, 0x13, 0xaa, 0x41, 0xfc, 0xff, 0x70,
    0x82, 0x00, 0x80, 0xd2, 0xe3, 0x83, 0x02, 0x91, 0x24, 0x00, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
    0x60, 0x09, 0x00, 0xb5, 0x96, 0x00, 0x80, 0xd2, 0x08, 0x01, 0x82, 0xd2, 0xe0, 0x03, 0x13, 0xaa,
    0xa1, 0x00, 0x80, 0xd2, 0x02, 0x00, 0x80, 0xd2, 0x03, 0x00, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
    0x60, 0x08, 0x00, 0xb5, 0xb6, 0x00, 0x80, 0xd2, 0x48, 0x02, 0x82, 0xd2, 0xe0, 0x03, 0x13, 0xaa,
    0xe1, 0x03, 0x00, 0x91, 0x02, 0x08, 0x80, 0xd2, 0xe3, 0x03, 0x02, 0x91, 0x84, 0x00, 0x80, 0xd2,
    0xe5, 0x43, 0x02, 0x91, 0x01, 0x00, 0x00, 0xd4, 0x20, 0x07, 0x00, 0xb5, 0xd6, 0x00, 0x80, 0xd2,
    0xe9, 0x93, 0x40, 0xb9, 0x3f, 0x4d, 0x00, 0x71, 0xa1, 0x06, 0x00, 0x54, 0x4a, 0xf8, 0xff, 0x10,
    0xeb, 0x03, 0x00, 0x91, 0x6c, 0x02, 0x80, 0x52, 0x4d, 0x15, 0x40, 0x38, 0x6e, 0x15, 0x40, 0x38,
    0xbf, 0x01, 0x0e, 0x6b, 0xc1, 0x05, 0x00, 0x54, 0x8c, 0x05, 0x00, 0x71, 0x61, 0xff, 0xff, 0x54,
    0x00, 0x00, 0x80, 0xd2, 0x2b, 0x00, 0x00, 0x14, 0x76, 0x01, 0x80, 0xd2, 0x08, 0x01, 0x82, 0xd2,
    0xe0, 0x03, 0x13, 0xaa, 0x21, 0x00, 0x80, 0xd2, 0x02, 0x00, 0x80, 0xd2, 0x03, 0x00, 0x80, 0xd2,
    0x01, 0x00, 0x00, 0xd4, 0x40, 0x04, 0x00, 0xb5, 0x96, 0x01, 0x80, 0xd2, 0x48, 0x02, 0x82, 0xd2,
    0xe0, 0x03, 0x13, 0xaa, 0xe1, 0x03, 0x00, 0x91, 0x02, 0x08, 0x80, 0xd2, 0xe3, 0x03, 0x02, 0x91,
    0x84, 0x00, 0x80, 0xd2, 0xe5, 0x43, 0x02, 0x91, 0x01, 0x00, 0x00, 0xd4, 0x00, 0x03, 0x00, 0xb5,
    0x96, 0x02, 0x80, 0xd2, 0xe9, 0x97, 0x40, 0xb9, 0x3f, 0x05, 0x00, 0x71, 0x81, 0x02, 0x00, 0x54,
    0xb6, 0x01, 0x80, 0xd2, 0x28, 0x04, 0x82, 0xd2, 0xe0, 0x83, 0x40, 0xb9, 0xe1, 0x03, 0x01, 0x91,
    0x62, 0x02, 0x80, 0xd2, 0xe3, 0x63, 0x02, 0x91, 0x01, 0x00, 0x00, 0xd4, 0x80, 0x01, 0x00, 0xb5,
    0xd6, 0x01, 0x80, 0xd2, 0x28, 0x02, 0x82, 0xd2, 0xe0, 0x03, 0x13, 0xaa, 0xe1, 0x03, 0x01, 0x91,
    0x62, 0x02, 0x80, 0xd2, 0x03, 0x00, 0x80, 0xd2, 0x04, 0x00, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4,
    0x60, 0x00, 0x00, 0xb5, 0x00, 0x00, 0x80, 0xd2, 0x02, 0x00, 0x00, 0x14, 0xe0, 0x03, 0x16, 0xaa,
    0xc8, 0x0b, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4, 0x00, 0x00, 0x00, 0x14,
];

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

/// Make a freshly allocated user root usable.
///
/// Nothing to do: the kernel's half is reached through `TTBR1_EL1` and a user root is
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
/// A user root goes in `TTBR0_EL1` and the kernel stays in `TTBR1_EL1`, so
/// unlike x86-64 there is no moment at which the kernel's own translations are
/// in question: the register being written is not the one the code doing the
/// writing is translated through.
///
/// What Arm does not do for free is the invalidation. A write to `CR3` drops
/// the non-global entries as a side effect; a write to `TTBR0_EL1` drops
/// nothing at all, and without [`cpu::flush_user_tlb`] the next user access
/// would be answered out of the *previous* address space's entries. That is
/// the bug this pairing exists to make impossible.
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
/// What a processor picking up a kernel thread does. `EPD0` makes a walk
/// through `TTBR0_EL1` fault rather than merely find nothing, so a stray user
/// address in the kernel is a fault at the instruction that made it — but
/// `EPD0` governs walks and not the `TLB`, so the cached entries have to go as
/// well, which is the second call.
///
/// # Safety
///
/// Nothing may still need a user address on this processor.
pub(crate) unsafe fn uninstall_user_root() {
    // SAFETY: the caller guarantees no user address is wanted; the kernel is
    // reached entirely through `TTBR1_EL1`.
    unsafe { cpu::disable_ttbr0() };
    cpu::flush_user_tlb();
}

/// Root of the loader's identity map, while it still exists.
///
/// `Some` on `AArch64` because the identity map is a second translation regime
/// with its own root, which nothing walking the kernel's tables would ever
/// see. The W^X sweep asks for this so that it sweeps the whole of what the
/// hardware can translate rather than the half the kernel happens to own.
pub(crate) fn identity_root(view: &BootView<'_>) -> Option<u64> {
    let ttbr0 = view.raw().ttbr0_phys;
    // SAFETY-free: this is a read of a `u64` the loader filled in, and zero is
    // how the loader says "the identity map is in the kernel's own table".
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
    // has been running entirely in the upper half since its first instruction.
    unsafe { cpu::disable_ttbr0() };
    IDENTITY_DROPPED.store(true, Ordering::Relaxed);
}

/// Where a backtrace starts: this function's frame pointer.
#[inline(always)]
pub(crate) fn frame_pointer() -> u64 {
    cpu::frame_pointer()
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

/// Bring up the interrupt controller and the generic timer.
///
/// # Safety
///
/// Must be called exactly once, on the boot CPU, after [`init_traps`] and
/// while interrupts are masked.
pub(crate) unsafe fn init_interrupts(view: &BootView<'_>) -> Result<Report, &'static str> {
    let firmware =
        crate::acpi::Firmware::open(view).map_err(|_| "the machine has no readable ACPI tables")?;
    let acpi = firmware.acpi();

    // SAFETY: called once from `kmain`, on the boot CPU, after the vector
    // table is installed and with interrupts masked.
    let version = unsafe { gic::init(&acpi)? };
    timer::init(&acpi)?;

    // The timer is a private peripheral interrupt, so enabling it is a
    // distributor operation like any other — it is only *private* in that
    // each core has its own copy of the number.
    gicv2::enable(timer::irq());
    // And the inter-processor interrupt, whose enable bit is this core's
    // own: every secondary turns on its copy in `gicv2::init_this_cpu`.
    gicv2::enable(gicv2::IPI_SGI);

    Ok(Report {
        counter: "generic timer",
        counter_hz: timer::counter_hz(),
        controller: match version {
            2 => "GICv2",
            _ => "GIC",
        },
        timer: "virtual timer",
        timer_hz: timer::counter_hz(),
    })
}

/// Unmask `IRQ` on this CPU.
pub(crate) fn enable_interrupts() {
    cpu::enable_interrupts();
}

/// Mask every interrupt on this CPU.
pub(crate) fn disable_interrupts() {
    cpu::disable_interrupts();
}

/// With interrupts masked, wait for one and then unmask, so one that arrived
/// since they were masked wakes the wait instead of being lost before it.
/// Returns with `IRQ` unmasked.
pub(crate) fn wait_for_work() {
    cpu::wait_then_enable_interrupts();
}

/// The interrupt number inter-processor interrupts arrive on.
pub(crate) const fn ipi_irq() -> u32 {
    gicv2::IPI_SGI
}

/// Interrupt every core but this one.
///
/// Cannot fail here — it is one write to the distributor — but can on x86-64,
/// where the local APIC has to accept the command. The barrier is here rather
/// than in the shared driver because it is an instruction, and each
/// architecture spells its own.
pub(crate) fn send_ipi_to_others() -> Result<(), &'static str> {
    cpu::dsb_ishst();
    gicv2::send_sgi_to_others();
    Ok(())
}

/// How `ferrix_sync`'s interrupt-masking lock masks interrupts here.
#[derive(Debug)]
pub(crate) struct Irq;

// SAFETY: `disable` masks every interrupt on this CPU and returns the `DAIF`
// value it found; `restore` puts exactly that value back, so nesting two
// critical sections cannot unmask halfway out of the outer one. `DAIF` is
// four bits wide and fits a `usize` on every target this kernel builds for.
unsafe impl ferrix_sync::IrqControl for Irq {
    fn disable() -> usize {
        let previous = cpu::read_daif();
        cpu::disable_interrupts();
        previous as usize
    }

    fn restore(state: usize) {
        // SAFETY: `state` is a `DAIF` value this CPU's `disable` returned a
        // moment ago, which is exactly `write_daif`'s contract.
        unsafe { cpu::write_daif(state as u64) };
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

/// Claim every pending interrupt, dispatch it, and retire it.
///
/// A loop rather than a single claim: the exception is taken once however many
/// interrupts are pending, so returning after one would leave the rest
/// asserted and take the exception again immediately. Reading `GICC_IAR` until
/// it reports a special identifier is how the controller says it has no more.
pub(crate) fn service_interrupts(_frame: &mut TrapFrame, handle: fn(u32)) {
    while let Some((id, acknowledgement)) = gicv2::claim() {
        handle(id);
        gicv2::complete(acknowledgement);
    }
}

/// The context switch, and the stack layout a new task starts on.
pub(crate) use switch::{UserState, prepare_stack, restore_user_state, save_user_state, switch_to};
