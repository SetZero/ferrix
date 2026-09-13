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
mod signal;
mod smp;
mod switch;
mod timer;
mod trap;

pub(crate) use signal::{SIGNAL_RED_ZONE, UserContext, restore_signal_frame, setup_signal_frame};

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use ferrix_bootinfo::{Arch, BootView};
use ferrix_fdt::{GicVersion, PsciConduit};
use ferrix_linux_abi::nr::{self, Syscall};
use ferrix_linux_abi::types::{self, OpenFlagBits};

use super::gicv2;
use crate::early::{EarlyError, EarlyMemory};
use crate::irq::Report;

pub(crate) use smp::{CpuStarter, describe_cpus, hardware_id};
pub(crate) use trap::{
    TrapFrame, UserRegs, advance_past_breakpoint, breakpoint, classify, enter_user, fault_signal,
    report_trap, resume_user, system_call,
};

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

/// Which `struct stat` the stat calls fill in: `struct stat64`, 104 bytes,
/// from `arch/arm/include/uapi/asm/stat.h`. This architecture's plain
/// `struct stat` cannot hold a 64-bit size, so musl calls only the `64` forms,
/// and the table carries no `newfstatat` for the plain one to be filled by.
pub(crate) const STAT_LAYOUT: crate::syscall::stat::StatLayout =
    crate::syscall::stat::StatLayout::Stat64;

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

/// The `open` flag bits that differ between architectures, as this one
/// numbers them: `arch/arm/include/uapi/asm/fcntl.h`'s.
pub(crate) const OPEN_FLAGS: OpenFlagBits = types::OPEN_FLAGS_ARM;

/// A whole program, in machine code, for the self-check to run in USR mode.
///
/// `write(1, "hello from USR\n", 15)` then `exit_group(42)`, with the EABI's
/// numbers — `write` is 4 and `exit_group` 248, in `r7` — and the string found
/// with `adr`, so the page can be mapped anywhere. ARM state, not Thumb.
///
/// Assembled by rustc's own LLVM from `global_asm!` and read back out of the
/// object file rather than encoded by hand. The length is a literal because
/// that assembler will not take a label difference as a `mov` immediate in ARM
/// state; the extraction asserts it against the string.
pub(crate) const USER_TEST_PROGRAM: &[u8] = &[
    0x04, 0x70, 0xa0, 0xe3, // mov  r7, #4       // write
    0x01, 0x00, 0xa0, 0xe3, // mov  r0, #1       // fd 1
    0x14, 0x10, 0x8f, 0xe2, // adr  r1, msg
    0x0f, 0x20, 0xa0, 0xe3, // mov  r2, #15      // length
    0x00, 0x00, 0x00, 0xef, // svc  #0
    0xf8, 0x70, 0xa0, 0xe3, // mov  r7, #248     // exit_group
    0x2a, 0x00, 0xa0, 0xe3, // mov  r0, #42      // status
    0x00, 0x00, 0x00, 0xef, // svc  #0
    0xfe, 0xff, 0xff, 0xea, // b    .            // never reached
    // "hello from USR\n"
    0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x66, 0x72, 0x6f, 0x6d, 0x20, 0x55, 0x53, 0x52, 0x0a,
];

/// The status [`USER_TEST_PROGRAM`] exits with.
pub(crate) const USER_TEST_STATUS: i32 = 42;

/// A program that forks, has its child exit with 23, waits for it, and exits
/// with the child's exit code plus one: 24 when `fork`, the child's copy of
/// its parent's registers and `wait4`'s status word are all right, 99 when
/// `wait4` reports the wrong child.
///
/// ```text
///   mov r7, #2 ; svc #0                            ; fork
///   cmp r0, #0 ; bne 1f
///   mov r7, #248 ; mov r0, #23 ; svc #0            ; the child exits 23
/// 1: mov r4, r0 ; sub sp, sp, #8 ; mov r1, sp ; mov r2, #0 ; mov r3, #0
///   mov r7, #114 ; svc #0                          ; wait4
///   cmp r0, r4 ; bne 2f
///   ldr r0, [sp] ; lsr r0, r0, #8 ; add r0, r0, #1 ; mov r7, #248 ; svc #0
/// 2: mov r7, #248 ; mov r0, #99 ; svc #0
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file.
pub(crate) const USER_FORK_PROGRAM: &[u8] = &[
    0x02, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3, 0x02, 0x00, 0x00, 0x1a,
    0xf8, 0x70, 0xa0, 0xe3, 0x17, 0x00, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x40, 0xa0, 0xe1,
    0x08, 0xd0, 0x4d, 0xe2, 0x0d, 0x10, 0xa0, 0xe1, 0x00, 0x20, 0xa0, 0xe3, 0x00, 0x30, 0xa0, 0xe3,
    0x72, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x04, 0x00, 0x50, 0xe1, 0x04, 0x00, 0x00, 0x1a,
    0x00, 0x00, 0x9d, 0xe5, 0x20, 0x04, 0xa0, 0xe1, 0x01, 0x00, 0x80, 0xe2, 0xf8, 0x70, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0xf8, 0x70, 0xa0, 0xe3, 0x63, 0x00, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
    0xfe, 0xff, 0xff, 0xea,
];

/// A program that signals itself twice and exits with what its handlers did:
/// 77 when both ran on frames of their own and `rt_sigreturn` and `sigreturn`
/// each put back the registers their frame held -- including the one each
/// handler changed through the frame's `ucontext`. ARMv7-A has two frames, and
/// a C library uses both: `SA_SIGINFO` gets the `rt` one, anything else the
/// plain one. 7 if a handler never ran or its change did not come back; 98 if
/// a handler saw something wrong; 99 if a call or the final mask was wrong.
///
/// ```text
///   rt_sigaction(SIGUSR1, {handler, SA_SIGINFO | SA_RESTORER, restorer, 0}, NULL, 8)
///   rt_sigaction(SIGUSR2, {handler2, SA_RESTORER, restorer2, 0}, NULL, 8)
///   mov r4, #7
///   tgkill(getpid(), getpid(), SIGUSR1) ; kill(getpid(), SIGUSR2)   ; both must return 0
///   rt_sigprocmask(SIG_BLOCK, NULL, &old, 8)    ; old must be empty
///   exit_group(r4)
/// handler:                                      ; r0 = 10, r1 = siginfo, r2 = ucontext
///   cmp r0, #10 ; ldr r3, [r1] ; cmp r3, #10
///   rt_sigprocmask(SIG_BLOCK, NULL, &mask, 8)   ; mask must be SIGUSR1's bit
///   uc_mcontext.arm_r4 ([r2, #48]) += 30 ; bx lr
/// handler2:                                     ; r0 = 12, the ucontext at sp
///   cmp r0, #12 ; uc_mcontext.arm_r4 ([sp, #48]) += 40 ; bx lr
/// restorer:  mov r7, #173 ; svc #0              ; rt_sigreturn
/// restorer2: mov r7, #119 ; svc #0              ; sigreturn
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file. ARM state.
pub(crate) const USER_SIGNAL_PROGRAM: &[u8] = &[
    0x18, 0xd0, 0x4d, 0xe2, 0xf8, 0x00, 0x8f, 0xe2, 0x00, 0x00, 0x8d, 0xe5, 0x01, 0x03, 0xa0, 0xe3,
    0x04, 0x00, 0x80, 0xe3, 0x04, 0x00, 0x8d, 0xe5, 0x57, 0x0f, 0x8f, 0xe2, 0x08, 0x00, 0x8d, 0xe5,
    0x00, 0x00, 0xa0, 0xe3, 0x0c, 0x00, 0x8d, 0xe5, 0x10, 0x00, 0x8d, 0xe5, 0x0a, 0x00, 0xa0, 0xe3,
    0x0d, 0x10, 0xa0, 0xe1, 0x00, 0x20, 0xa0, 0xe3, 0x08, 0x30, 0xa0, 0xe3, 0xae, 0x70, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3, 0x2a, 0x00, 0x00, 0x1a, 0x41, 0x0f, 0x8f, 0xe2,
    0x00, 0x00, 0x8d, 0xe5, 0x01, 0x03, 0xa0, 0xe3, 0x04, 0x00, 0x8d, 0xe5, 0x12, 0x0e, 0x8f, 0xe2,
    0x08, 0x00, 0x8d, 0xe5, 0x0c, 0x00, 0xa0, 0xe3, 0x0d, 0x10, 0xa0, 0xe1, 0x00, 0x20, 0xa0, 0xe3,
    0x08, 0x30, 0xa0, 0xe3, 0xae, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3,
    0x1c, 0x00, 0x00, 0x1a, 0x07, 0x40, 0xa0, 0xe3, 0x14, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
    0x00, 0x60, 0xa0, 0xe1, 0x00, 0x10, 0xa0, 0xe1, 0x0a, 0x20, 0xa0, 0xe3, 0x0c, 0x71, 0x00, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3, 0x12, 0x00, 0x00, 0x1a, 0x06, 0x00, 0xa0, 0xe1,
    0x0c, 0x10, 0xa0, 0xe3, 0x25, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3,
    0x0c, 0x00, 0x00, 0x1a, 0x00, 0x00, 0xa0, 0xe3, 0x00, 0x10, 0xa0, 0xe3, 0x0d, 0x20, 0xa0, 0xe1,
    0x08, 0x30, 0xa0, 0xe3, 0xaf, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x9d, 0xe5,
    0x04, 0x10, 0x9d, 0xe5, 0x01, 0x00, 0x90, 0xe1, 0x02, 0x00, 0x00, 0x1a, 0x04, 0x00, 0xa0, 0xe1,
    0xf8, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x63, 0x00, 0xa0, 0xe3, 0xf8, 0x70, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0x0a, 0x00, 0x50, 0xe3, 0x18, 0x00, 0x00, 0x1a, 0x00, 0x30, 0x91, 0xe5,
    0x0a, 0x00, 0x53, 0xe3, 0x15, 0x00, 0x00, 0x1a, 0x02, 0x50, 0xa0, 0xe1, 0x08, 0xd0, 0x4d, 0xe2,
    0x00, 0x00, 0xa0, 0xe3, 0x00, 0x10, 0xa0, 0xe3, 0x0d, 0x20, 0xa0, 0xe1, 0x08, 0x30, 0xa0, 0xe3,
    0xaf, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x9d, 0xe5, 0x08, 0xd0, 0x8d, 0xe2,
    0x02, 0x0c, 0x50, 0xe3, 0x09, 0x00, 0x00, 0x1a, 0x30, 0x00, 0x95, 0xe5, 0x1e, 0x00, 0x80, 0xe2,
    0x30, 0x00, 0x85, 0xe5, 0x1e, 0xff, 0x2f, 0xe1, 0x0c, 0x00, 0x50, 0xe3, 0x03, 0x00, 0x00, 0x1a,
    0x30, 0x00, 0x9d, 0xe5, 0x28, 0x00, 0x80, 0xe2, 0x30, 0x00, 0x8d, 0xe5, 0x1e, 0xff, 0x2f, 0xe1,
    0x62, 0x00, 0xa0, 0xe3, 0xf8, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0xad, 0x70, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0x77, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
];

/// A program that `execve`s `/exec-target` and, if that returns, exits with
/// the error number: the target's own status when it exists, 2 (`ENOENT`) when
/// it does not.
///
/// ```text
///   adr r0, path ; mov r1, #0 ; push {r0, r1} ; mov r1, sp ; mov r2, #0
///   mov r7, #11 ; svc #0                           ; execve(path, [path], NULL)
///   rsb r0, r0, #0 ; mov r7, #248 ; svc #0         ; exit with errno
/// path: "/exec-target\0"
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file.
pub(crate) const USER_EXEC_PROGRAM: &[u8] = &[
    0x24, 0x00, 0x8f, 0xe2, 0x00, 0x10, 0xa0, 0xe3, 0x03, 0x00, 0x2d, 0xe9, 0x0d, 0x10, 0xa0, 0xe1,
    0x00, 0x20, 0xa0, 0xe3, 0x0b, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x60, 0xe2,
    0xf8, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0xfe, 0xff, 0xff, 0xea, 0x2f, 0x65, 0x78, 0x65,
    0x63, 0x2d, 0x74, 0x61, 0x72, 0x67, 0x65, 0x74, 0x00,
];

/// A program that spins, then writes a tagged line and exits with a status it
/// reads out of its own image.
///
/// For the check that two programs run at once: it spins long enough to be
/// preempted in USR mode, which a program run with interrupts masked never is.
/// The last ten bytes are the layout every architecture's copy shares, so the
/// check patches them without knowing the instruction set -- the tag character
/// and the newline, then the loop count and the exit status as little-endian
/// words, both loaded PC-relative with `ldr`. ARM state.
///
/// Its stack pointer is recorded before the loop and compared after, and a
/// program whose stack pointer changed while it was preempted exits with 99
/// instead. That is the check this architecture needs most: USR's stack
/// pointer is banked, and no trap saves it.
///
/// ```text
///   mov   r5, sp
///   ldr   r4, count
/// 1: subs r4, r4, #1
///   bne   1b
///   cmp   sp, r5 ; bne 2f
///   mov   r7, #4 ; mov r0, #1 ; adr r1, msg ; mov r2, #16 ; svc #0
///   mov   r7, #248 ; ldr r0, status ; svc #0
/// 2: mov exit_group ; status 99 ; svc #0
///   b     .
/// msg: "spinning task ?\n"   count: .word   status: .word
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file, as
/// [`USER_TEST_PROGRAM`] was.
pub(crate) const USER_SPIN_PROGRAM: &[u8] = &[
    0x0d, 0x50, 0xa0, 0xe1, 0x4c, 0x40, 0x9f, 0xe5, 0x01, 0x40, 0x54, 0xe2, 0xfd, 0xff, 0xff, 0x1a,
    0x05, 0x00, 0x5d, 0xe1, 0x07, 0x00, 0x00, 0x1a, 0x04, 0x70, 0xa0, 0xe3, 0x01, 0x00, 0xa0, 0xe3,
    0x20, 0x10, 0x8f, 0xe2, 0x10, 0x20, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0xf8, 0x70, 0xa0, 0xe3,
    0x24, 0x00, 0x9f, 0xe5, 0x00, 0x00, 0x00, 0xef, 0xf8, 0x70, 0xa0, 0xe3, 0x63, 0x00, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0xfe, 0xff, 0xff, 0xea, 0x73, 0x70, 0x69, 0x6e, 0x6e, 0x69, 0x6e, 0x67,
    0x20, 0x74, 0x61, 0x73, 0x6b, 0x20, 0x3f, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
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
    0x07, 0x00, 0x00, 0xea, 0x63, 0x61, 0x72, 0x72, 0x69, 0x65, 0x64, 0x20, 0x62, 0x79, 0x20, 0x61,
    0x20, 0x68, 0x61, 0x6e, 0x64, 0x6c, 0x65, 0x70, 0x69, 0x6e, 0x67, 0x00, 0x3f, 0x0a, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x10, 0x90, 0x4f, 0xe2, 0x00, 0xa0, 0xd9, 0xe5, 0x04, 0xb0, 0x99, 0xe5,
    0x01, 0xdc, 0x4d, 0xe2, 0x00, 0x00, 0xa0, 0xe3, 0x98, 0x00, 0x8d, 0xe5, 0x9c, 0x00, 0x8d, 0xe5,
    0x73, 0x00, 0x5a, 0xe3, 0x3d, 0x00, 0x00, 0x1a, 0x01, 0x60, 0xa0, 0xe3, 0x20, 0x70, 0x01, 0xe3,
    0x40, 0x00, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3, 0x64, 0x00, 0x00, 0xda,
    0x00, 0x80, 0xa0, 0xe1, 0x02, 0x60, 0xa0, 0xe3, 0x22, 0x70, 0x01, 0xe3, 0x08, 0x00, 0xa0, 0xe1,
    0x74, 0x10, 0x4f, 0xe2, 0x13, 0x20, 0xa0, 0xe3, 0x98, 0x30, 0x8d, 0xe2, 0x00, 0x00, 0x00, 0xef,
    0x00, 0x00, 0x50, 0xe3, 0x5a, 0x00, 0x00, 0x1a, 0xa0, 0x80, 0x8d, 0xe5, 0x03, 0x60, 0xa0, 0xe3,
    0x11, 0x70, 0x01, 0xe3, 0x0b, 0x00, 0xa0, 0xe1, 0x89, 0x10, 0x4f, 0xe2, 0x04, 0x20, 0xa0, 0xe3,
    0xa0, 0x30, 0x8d, 0xe2, 0x01, 0x40, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3,
    0x4f, 0x00, 0x00, 0x1a, 0x04, 0x60, 0xa0, 0xe3, 0x08, 0x70, 0x01, 0xe3, 0x0b, 0x00, 0xa0, 0xe1,
    0x05, 0x10, 0xa0, 0xe3, 0x00, 0x20, 0xa0, 0xe3, 0x00, 0x30, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
    0x00, 0x00, 0x50, 0xe3, 0x46, 0x00, 0x00, 0x1a, 0x05, 0x60, 0xa0, 0xe3, 0x12, 0x70, 0x01, 0xe3,
    0x0b, 0x00, 0xa0, 0xe1, 0x0d, 0x10, 0xa0, 0xe1, 0x40, 0x20, 0xa0, 0xe3, 0x80, 0x30, 0x8d, 0xe2,
    0x04, 0x40, 0xa0, 0xe3, 0x90, 0x50, 0x8d, 0xe2, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3,
    0x3b, 0x00, 0x00, 0x1a, 0x06, 0x60, 0xa0, 0xe3, 0x90, 0x10, 0x9d, 0xe5, 0x13, 0x00, 0x51, 0xe3,
    0x37, 0x00, 0x00, 0x1a, 0x46, 0x1f, 0x4f, 0xe2, 0x0d, 0x20, 0xa0, 0xe1, 0x13, 0x30, 0xa0, 0xe3,
    0x01, 0x40, 0xd1, 0xe4, 0x01, 0x50, 0xd2, 0xe4, 0x05, 0x00, 0x54, 0xe1, 0x30, 0x00, 0x00, 0x1a,
    0x01, 0x30, 0x53, 0xe2, 0xf9, 0xff, 0xff, 0x1a, 0x00, 0x00, 0xa0, 0xe3, 0x2d, 0x00, 0x00, 0xea,
    0x0b, 0x60, 0xa0, 0xe3, 0x08, 0x70, 0x01, 0xe3, 0x0b, 0x00, 0xa0, 0xe1, 0x01, 0x10, 0xa0, 0xe3,
    0x00, 0x20, 0xa0, 0xe3, 0x00, 0x30, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3,
    0x23, 0x00, 0x00, 0x1a, 0x0c, 0x60, 0xa0, 0xe3, 0x12, 0x70, 0x01, 0xe3, 0x0b, 0x00, 0xa0, 0xe1,
    0x0d, 0x10, 0xa0, 0xe1, 0x40, 0x20, 0xa0, 0xe3, 0x80, 0x30, 0x8d, 0xe2, 0x04, 0x40, 0xa0, 0xe3,
    0x90, 0x50, 0x8d, 0xe2, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3, 0x18, 0x00, 0x00, 0x1a,
    0x14, 0x60, 0xa0, 0xe3, 0x94, 0x10, 0x9d, 0xe5, 0x01, 0x00, 0x51, 0xe3, 0x14, 0x00, 0x00, 0x1a,
    0x0d, 0x60, 0xa0, 0xe3, 0x21, 0x70, 0x01, 0xe3, 0x80, 0x00, 0x9d, 0xe5, 0x40, 0x10, 0x8d, 0xe2,
    0x13, 0x20, 0xa0, 0xe3, 0x98, 0x30, 0x8d, 0xe2, 0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3,
    0x0b, 0x00, 0x00, 0x1a, 0x0e, 0x60, 0xa0, 0xe3, 0x11, 0x70, 0x01, 0xe3, 0x0b, 0x00, 0xa0, 0xe1,
    0x40, 0x10, 0x8d, 0xe2, 0x13, 0x20, 0xa0, 0xe3, 0x00, 0x30, 0xa0, 0xe3, 0x00, 0x40, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xef, 0x00, 0x00, 0x50, 0xe3, 0x01, 0x00, 0x00, 0x1a, 0x00, 0x00, 0xa0, 0xe3,
    0x00, 0x00, 0x00, 0xea, 0x06, 0x00, 0xa0, 0xe1, 0xf8, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef,
    0xfe, 0xff, 0xff, 0xea,
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

/// Where a backtrace starts: this function's frame pointer.
#[inline(always)]
pub(crate) fn frame_pointer() -> u64 {
    u64::from(cpu::frame_pointer())
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
    // As on AArch64, a frame that cannot be used costs MSI vectors, not boot.
    if let Some(frame) = tree.gicv2m_frames().next() {
        let _ = gicv2::init_msi_frame(frame.region.address, frame.spi_base.zip(frame.spi_count));
    }
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

/// Take an interrupt a device can raise by message, from the `GICv2m` frame.
///
/// # Errors
///
/// No usable frame, or every SPI it has already taken.
pub(crate) fn msi_allocate() -> Result<crate::irq::Msi, &'static str> {
    gicv2::msi_allocate()
}

/// Stop interrupt `number` being delivered until [`unmask_interrupt`] lets
/// it through again.
///
/// # Errors
///
/// If `number` is not a line the interrupt controller has.
pub(crate) fn mask_interrupt(number: u32) -> Result<(), &'static str> {
    if number >= gicv2::FIRST_SPECIAL_ID {
        return Err("not a line the interrupt controller has");
    }
    gicv2::disable(number);
    Ok(())
}

/// Let interrupt `number` be delivered again after [`mask_interrupt`].
///
/// # Errors
///
/// If `number` is not a line the interrupt controller has.
pub(crate) fn unmask_interrupt(number: u32) -> Result<(), &'static str> {
    if number >= gicv2::FIRST_SPECIAL_ID {
        return Err("not a line the interrupt controller has");
    }
    gicv2::enable(number);
    Ok(())
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
pub(crate) use switch::{
    UserState, prepare_stack, reset_user_state, restore_user_state, save_user_state, switch_to,
};
