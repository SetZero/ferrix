//! The x86-64 end of the kernel.

mod apic;
mod clock;
pub(crate) mod console;
mod cpu;
mod gdt;
mod msi;
mod signal;
mod smp;
mod switch;
mod syscall;
mod trap;

pub(crate) use signal::{SIGNAL_RED_ZONE, UserContext, restore_signal_frame, setup_signal_frame};

use ferrix_bootinfo::{Arch, BootView};
use ferrix_linux_abi::nr::{self, Syscall};
use ferrix_linux_abi::types::{self, OpenFlagBits};

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

/// Which `struct stat` the stat calls fill in: x86-64's own, 144 bytes, from
/// `arch/x86/include/uapi/asm/stat.h`. x86-64 kept the layout it grew rather
/// than adopting the generic one, so this is not the AArch64 answer.
pub(crate) const STAT_LAYOUT: crate::syscall::stat::StatLayout =
    crate::syscall::stat::StatLayout::Legacy;

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

    // `SYSCALL` on this processor, now that `GS` names its record -- which
    // `syscall::init` parks for the first `swapgs`, and which the trampoline
    // reaches its stack through. Here because this runs once on every
    // processor, after its GDT: a program is a task that may be resumed on any
    // of them, and the first system call it made on one that skipped this
    // would take `#UD`. It used to run lazily before each program, when a
    // program never left the processor it started on.
    // SAFETY: this processor's per-CPU record is installed in `GS` above. On
    // a secondary this runs before `init_secondary` loads its own GDT, which is
    // fine: the MSRs only record the selectors, and nothing uses them until a
    // program's first `SYSCALL`, long after that GDT is in place.
    unsafe { syscall::init() };
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
pub(crate) use trap::{
    TrapFrame, advance_past_breakpoint, breakpoint, classify, fault_signal, report_trap,
};

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

/// True while anything the loader identity mapped still translates.
///
/// One tree translates both halves here, so a walk of it from software is the
/// hardware's answer. Two addresses: zero, because a null dereference in
/// kernel code must fault rather than find the first page of physical memory,
/// and the kernel's own physical address, which the identity map covered with
/// the rest of RAM.
pub(crate) fn identity_map_live(view: &BootView<'_>) -> bool {
    crate::mm::translate(0).is_some() || crate::mm::translate(view.raw().kernel_phys).is_some()
}

/// `CR0.WP`: ring 0 obeys a read-only page table entry only while it is set.
const CR0_WP: u64 = 1 << 16;

/// True if the kernel faults when it writes through a read-only mapping.
///
/// On x86-64 that is `CR0.WP`, which the loader sets and firmware is free to
/// have left clear. Without it every read-only kernel mapping is writable from
/// ring 0, and the W^X sweep, which reads entries, cannot tell.
pub(crate) fn kernel_write_protected() -> bool {
    cpu::read_cr0() & CR0_WP != 0
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

/// The `open` flag bits that differ between architectures, as this one
/// numbers them: the generic header's, which x86-64 does not override.
pub(crate) const OPEN_FLAGS: OpenFlagBits = types::OPEN_FLAGS_GENERIC;

/// A whole program, in machine code: write a line to file descriptor 1 and
/// exit with a known status.
///
/// Forty-six bytes and a string, because the first thing to cross into ring 3
/// should be something that can be read in full. A compiled test program would
/// need a second crate, a second target and a build step, and when the
/// transition did not work the first question would be whether the program was
/// at fault. Nothing here can be: there is no libc, no relocation, no stack
/// use, and every instruction is listed.
///
/// ```text
///   mov  $1, %rax          ; __NR_write
///   mov  $1, %rdi          ; fd 1
///   lea  0x19(%rip), %rsi  ; the message, just past this code
///   mov  $18, %rdx         ; its length
///   syscall
///   mov  $231, %rax        ; __NR_exit_group
///   mov  $42, %rdi         ; a status nothing else would produce
///   syscall
/// ```
pub(crate) const USER_TEST_PROGRAM: &[u8] = &[
    0x48, 0xc7, 0xc0, 0x01, 0x00, 0x00, 0x00, // mov $1, %rax
    0x48, 0xc7, 0xc7, 0x01, 0x00, 0x00, 0x00, // mov $1, %rdi
    0x48, 0x8d, 0x35, 0x19, 0x00, 0x00, 0x00, // lea 0x19(%rip), %rsi
    0x48, 0xc7, 0xc2, 0x12, 0x00, 0x00, 0x00, // mov $18, %rdx
    0x0f, 0x05, // syscall
    0x48, 0xc7, 0xc0, 0xe7, 0x00, 0x00, 0x00, // mov $231, %rax
    0x48, 0xc7, 0xc7, 0x2a, 0x00, 0x00, 0x00, // mov $42, %rdi
    0x0f, 0x05, // syscall
    b'h', b'e', b'l', b'l', b'o', b' ', b'f', b'r', b'o', b'm', b' ', b'r', b'i', b'n', b'g', b' ',
    b'3', b'\n',
];

/// The status [`USER_TEST_PROGRAM`] exits with.
pub(crate) const USER_TEST_STATUS: i32 = 42;

/// A program that forks, has its child exit with 23, waits for it, and exits
/// with the child's exit code plus one: 24 when `fork`, the child's copy of
/// its parent's registers and `wait4`'s status word are all right, 99 when
/// `wait4` reports the wrong child.
///
/// ```text
///   movl $57, %eax ; syscall                  ; fork
///   testq %rax, %rax ; jnz 1f
///   movl $231, %eax ; movl $23, %edi ; syscall ; the child exits 23
/// 1: subq $16, %rsp ; movq %rax, %rdi ; movq %rsp, %rsi
///   xorl %edx, %edx ; xorl %r10d, %r10d ; movl $61, %eax ; syscall ; wait4
///   cmpq %rdi, %rax ; jne 2f
///   movl (%rsp), %edi ; shrl $8, %edi ; addl $1, %edi
///   movl $231, %eax ; syscall                 ; exit with the child's code + 1
/// 2: movl $231, %eax ; movl $99, %edi ; syscall
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file.
pub(crate) const USER_FORK_PROGRAM: &[u8] = &[
    0xb8, 0x39, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x85, 0xc0, 0x75, 0x0c, 0xb8, 0xe7, 0x00, 0x00,
    0x00, 0xbf, 0x17, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x83, 0xec, 0x10, 0x48, 0x89, 0xc7, 0x48,
    0x89, 0xe6, 0x31, 0xd2, 0x45, 0x31, 0xd2, 0xb8, 0x3d, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x39,
    0xf8, 0x75, 0x10, 0x8b, 0x3c, 0x24, 0xc1, 0xef, 0x08, 0x83, 0xc7, 0x01, 0xb8, 0xe7, 0x00, 0x00,
    0x00, 0x0f, 0x05, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x63, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x0f,
    0x0b,
];

/// A program that signals itself and exits with what its handler did: 77 when
/// the handler ran on a frame of its own, saw the right `siginfo` and mask, and
/// `rt_sigreturn` put back the registers the frame held -- including the one
/// the handler changed through the `ucontext`. 7 if the handler never ran or
/// the change did not come back; 98 if the handler saw something wrong; 99 if
/// a call or the mask after the return was wrong.
///
/// ```text
///   rt_sigaction(SIGUSR1, {handler, SA_SIGINFO | SA_RESTORER, restorer, 0}, NULL, 8)
///   movl $7, %ebx
///   tgkill(getpid(), getpid(), SIGUSR1)         ; must return 0
///   rt_sigprocmask(SIG_BLOCK, NULL, &old, 8)    ; old must be empty
///   exit_group(%ebx)
/// handler:                                      ; rdi = 10, rsi = siginfo, rdx = ucontext
///   cmpl $10, %edi ; cmpl $10, (%rsi)
///   rt_sigprocmask(SIG_BLOCK, NULL, &mask, 8)   ; mask must be SIGUSR1's bit
///   addq $70, 128(%rdx)                         ; uc_mcontext.rbx += 70
///   ret                                         ; into the restorer
/// restorer: movl $15, %eax ; syscall            ; rt_sigreturn
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file.
pub(crate) const USER_SIGNAL_PROGRAM: &[u8] = &[
    0x48, 0x83, 0xec, 0x20, 0x48, 0x8d, 0x05, 0x8f, 0x00, 0x00, 0x00, 0x48, 0x89, 0x04, 0x24, 0x48,
    0xc7, 0x44, 0x24, 0x08, 0x04, 0x00, 0x00, 0x04, 0x48, 0x8d, 0x05, 0xbf, 0x00, 0x00, 0x00, 0x48,
    0x89, 0x44, 0x24, 0x10, 0x48, 0xc7, 0x44, 0x24, 0x18, 0x00, 0x00, 0x00, 0x00, 0xb8, 0x0d, 0x00,
    0x00, 0x00, 0xbf, 0x0a, 0x00, 0x00, 0x00, 0x48, 0x89, 0xe6, 0x31, 0xd2, 0x41, 0xba, 0x08, 0x00,
    0x00, 0x00, 0x0f, 0x05, 0x48, 0x85, 0xc0, 0x75, 0x45, 0xbb, 0x07, 0x00, 0x00, 0x00, 0xb8, 0x27,
    0x00, 0x00, 0x00, 0x0f, 0x05, 0x89, 0xc7, 0x89, 0xc6, 0xba, 0x0a, 0x00, 0x00, 0x00, 0xb8, 0xea,
    0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x85, 0xc0, 0x75, 0x24, 0x31, 0xff, 0x31, 0xf6, 0x48, 0x89,
    0xe2, 0x41, 0xba, 0x08, 0x00, 0x00, 0x00, 0xb8, 0x0e, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x83,
    0x3c, 0x24, 0x00, 0x75, 0x09, 0x89, 0xdf, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xbf, 0x63,
    0x00, 0x00, 0x00, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x83, 0xff, 0x0a, 0x75, 0x33, 0x83,
    0x3e, 0x0a, 0x75, 0x2e, 0x49, 0x89, 0xd4, 0x48, 0x83, 0xec, 0x08, 0x31, 0xff, 0x31, 0xf6, 0x48,
    0x89, 0xe2, 0x41, 0xba, 0x08, 0x00, 0x00, 0x00, 0xb8, 0x0e, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x58,
    0x48, 0x3d, 0x00, 0x02, 0x00, 0x00, 0x75, 0x0a, 0x49, 0x83, 0x84, 0x24, 0x80, 0x00, 0x00, 0x00,
    0x46, 0xc3, 0xbf, 0x62, 0x00, 0x00, 0x00, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xb8, 0x0f,
    0x00, 0x00, 0x00, 0x0f, 0x05, 0x0f, 0x0b,
];

/// A program that `execve`s `/exec-target` and, if that returns, exits with
/// the error number: the target's own status when it exists, 2 (`ENOENT`) when
/// it does not.
///
/// ```text
///   leaq path(%rip), %rdi ; pushq $0 ; pushq %rdi ; movq %rsp, %rsi
///   xorl %edx, %edx ; movl $59, %eax ; syscall ; execve(path, [path], NULL)
///   negl %eax ; movl %eax, %edi ; movl $231, %eax ; syscall ; exit with errno
/// path: "/exec-target\0"
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file.
pub(crate) const USER_EXEC_PROGRAM: &[u8] = &[
    0x48, 0x8d, 0x3d, 0x1c, 0x00, 0x00, 0x00, 0x6a, 0x00, 0x57, 0x48, 0x89, 0xe6, 0x31, 0xd2, 0xb8,
    0x3b, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xf7, 0xd8, 0x89, 0xc7, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0x0f,
    0x05, 0x0f, 0x0b, 0x2f, 0x65, 0x78, 0x65, 0x63, 0x2d, 0x74, 0x61, 0x72, 0x67, 0x65, 0x74, 0x00,
];

/// A program that sets its trap flag and makes a system call: `exit_group`,
/// with a status nothing else produces.
///
/// For the check that `SYSCALL` masks the trap flag. If it did not, the
/// processor would single-step the first instruction of the trampoline, in
/// ring 0 and still on the program's stack, and the `#DB` would stop the
/// kernel before the call was served. `popfq` setting the flag does not trap
/// after itself, only after the instruction that follows it, which is why the
/// call comes directly after it.
///
/// **The call is one that does not return.** `SYSRET` gives the program its
/// flags back, trap flag included, and the processor then traps in ring 3 at
/// the return address before running anything there. That trap raises
/// `SIGTRAP`, which by default ends the program -- a status of its own, and not
/// the one this check wants to read. `exit_group`'s status says only that the
/// call made with the flag set was served.
///
/// ```text
///   movl $231, %eax ; movl $231, %edi
///   pushfq ; orq $0x100, (%rsp) ; popfq   ; the trap flag, from here on
///   syscall                               ; exit_group(231), single-stepped
///   ud2
/// ```
///
/// Assembled by hand and checked with `objdump -D -b binary -mi386:x86-64`.
pub(crate) const USER_STEP_PROGRAM: &[u8] = &[
    0xb8, 0xe7, 0x00, 0x00, 0x00, // movl $231, %eax
    0xbf, 0xe7, 0x00, 0x00, 0x00, // movl $231, %edi
    0x9c, // pushfq
    0x48, 0x81, 0x0c, 0x24, 0x00, 0x01, 0x00, 0x00, // orq $0x100, (%rsp)
    0x9d, // popfq
    0x0f, 0x05, // syscall
    0x0f, 0x0b, // ud2
];

/// The status [`USER_STEP_PROGRAM`] exits with.
pub(crate) const USER_STEP_STATUS: i32 = 231;

/// A program that spins, then writes a tagged line and exits with a status it
/// reads out of its own image.
///
/// For the check that two programs run at once: it spins long enough to be
/// preempted in ring 3, which a program run with interrupts masked never is.
/// The last ten bytes are the layout every architecture's copy shares, so the
/// check patches them without knowing the instruction set -- the tag character
/// and the newline, then the loop count and the exit status as little-endian
/// words, both loaded RIP-relative.
///
/// Its stack pointer is recorded before the loop and compared after, and a
/// program whose stack pointer changed while it was preempted exits with 99
/// instead. The kernel must give every program back its own.
///
/// ```text
///   movq  %rsp, %r8
///   movl  count(%rip), %ecx
/// 1: decq %rcx
///   jnz   1b
///   cmpq  %rsp, %r8 ; jne 2f
///   movl $1, %eax ; movl $1, %edi ; leaq msg(%rip), %rsi ; movl $16, %edx ; syscall
///   movl $231, %eax ; movl status(%rip), %edi ; syscall
/// 2: movl $231, %eax ; movl $99, %edi ; syscall
///   ud2
/// msg: "spinning task ?\n"   count: .long   status: .long
/// ```
///
/// Assembled by rustc's LLVM and read back out of the object file.
pub(crate) const USER_SPIN_PROGRAM: &[u8] = &[
    0x49, 0x89, 0xe0, 0x8b, 0x0d, 0x4d, 0x00, 0x00, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xfb, 0x49, 0x39,
    0xe0, 0x75, 0x25, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xbf, 0x01, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x35,
    0x22, 0x00, 0x00, 0x00, 0xba, 0x10, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xb8, 0xe7, 0x00, 0x00, 0x00,
    0x8b, 0x3d, 0x24, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x63, 0x00,
    0x00, 0x00, 0x0f, 0x05, 0x0f, 0x0b, 0x73, 0x70, 0x69, 0x6e, 0x6e, 0x69, 0x6e, 0x67, 0x20, 0x74,
    0x61, 0x73, 0x6b, 0x20, 0x3f, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
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
    0xeb, 0x22, 0x90, 0x90, 0x63, 0x61, 0x72, 0x72, 0x69, 0x65, 0x64, 0x20, 0x62, 0x79, 0x20, 0x61,
    0x20, 0x68, 0x61, 0x6e, 0x64, 0x6c, 0x65, 0x70, 0x69, 0x6e, 0x67, 0x00, 0x3f, 0x0a, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x44, 0x8b, 0x25, 0xf5, 0xff, 0xff, 0xff, 0x44, 0x0f, 0xb6, 0x2d, 0xe9,
    0xff, 0xff, 0xff, 0x48, 0x81, 0xec, 0x00, 0x01, 0x00, 0x00, 0x48, 0xc7, 0x84, 0x24, 0x98, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41, 0x80, 0xfd, 0x73, 0x0f, 0x85, 0x1e, 0x01, 0x00, 0x00,
    0x41, 0xbf, 0x01, 0x00, 0x00, 0x00, 0xb8, 0x20, 0x10, 0x00, 0x00, 0xbf, 0x40, 0x00, 0x00, 0x00,
    0x0f, 0x05, 0x48, 0x85, 0xc0, 0x0f, 0x8e, 0xbd, 0x01, 0x00, 0x00, 0x41, 0x89, 0xc6, 0x41, 0xbf,
    0x02, 0x00, 0x00, 0x00, 0xb8, 0x22, 0x10, 0x00, 0x00, 0x44, 0x89, 0xf7, 0x48, 0x8d, 0x35, 0x81,
    0xff, 0xff, 0xff, 0xba, 0x13, 0x00, 0x00, 0x00, 0x4c, 0x8d, 0x94, 0x24, 0x98, 0x00, 0x00, 0x00,
    0x0f, 0x05, 0x48, 0x85, 0xc0, 0x0f, 0x85, 0x8d, 0x01, 0x00, 0x00, 0x44, 0x89, 0xb4, 0x24, 0xa0,
    0x00, 0x00, 0x00, 0x41, 0xbf, 0x03, 0x00, 0x00, 0x00, 0xb8, 0x11, 0x10, 0x00, 0x00, 0x44, 0x89,
    0xe7, 0x48, 0x8d, 0x35, 0x5f, 0xff, 0xff, 0xff, 0xba, 0x04, 0x00, 0x00, 0x00, 0x4c, 0x8d, 0x94,
    0x24, 0xa0, 0x00, 0x00, 0x00, 0x41, 0xb8, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x85, 0xc0,
    0x0f, 0x85, 0x52, 0x01, 0x00, 0x00, 0x41, 0xbf, 0x04, 0x00, 0x00, 0x00, 0xb8, 0x08, 0x10, 0x00,
    0x00, 0x44, 0x89, 0xe7, 0xbe, 0x05, 0x00, 0x00, 0x00, 0x31, 0xd2, 0x45, 0x31, 0xd2, 0x0f, 0x05,
    0x48, 0x85, 0xc0, 0x0f, 0x85, 0x2f, 0x01, 0x00, 0x00, 0x41, 0xbf, 0x05, 0x00, 0x00, 0x00, 0xb8,
    0x12, 0x10, 0x00, 0x00, 0x44, 0x89, 0xe7, 0x48, 0x89, 0xe6, 0xba, 0x40, 0x00, 0x00, 0x00, 0x4c,
    0x8d, 0x94, 0x24, 0x80, 0x00, 0x00, 0x00, 0x41, 0xb8, 0x04, 0x00, 0x00, 0x00, 0x4c, 0x8d, 0x8c,
    0x24, 0x90, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x85, 0xc0, 0x0f, 0x85, 0xf8, 0x00, 0x00, 0x00,
    0x41, 0xbf, 0x06, 0x00, 0x00, 0x00, 0x83, 0xbc, 0x24, 0x90, 0x00, 0x00, 0x00, 0x13, 0x0f, 0x85,
    0xe4, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x35, 0xb9, 0xfe, 0xff, 0xff, 0x48, 0x89, 0xe7, 0xb9, 0x13,
    0x00, 0x00, 0x00, 0x8a, 0x06, 0x3a, 0x07, 0x0f, 0x85, 0xcb, 0x00, 0x00, 0x00, 0x48, 0xff, 0xc6,
    0x48, 0xff, 0xc7, 0xff, 0xc9, 0x75, 0xec, 0x31, 0xff, 0xe9, 0xbd, 0x00, 0x00, 0x00, 0x41, 0xbf,
    0x0b, 0x00, 0x00, 0x00, 0xb8, 0x08, 0x10, 0x00, 0x00, 0x44, 0x89, 0xe7, 0xbe, 0x01, 0x00, 0x00,
    0x00, 0x31, 0xd2, 0x45, 0x31, 0xd2, 0x0f, 0x05, 0x48, 0x85, 0xc0, 0x0f, 0x85, 0x97, 0x00, 0x00,
    0x00, 0x41, 0xbf, 0x0c, 0x00, 0x00, 0x00, 0xb8, 0x12, 0x10, 0x00, 0x00, 0x44, 0x89, 0xe7, 0x48,
    0x89, 0xe6, 0xba, 0x40, 0x00, 0x00, 0x00, 0x4c, 0x8d, 0x94, 0x24, 0x80, 0x00, 0x00, 0x00, 0x41,
    0xb8, 0x04, 0x00, 0x00, 0x00, 0x4c, 0x8d, 0x8c, 0x24, 0x90, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48,
    0x85, 0xc0, 0x75, 0x64, 0x41, 0xbf, 0x14, 0x00, 0x00, 0x00, 0x83, 0xbc, 0x24, 0x94, 0x00, 0x00,
    0x00, 0x01, 0x75, 0x54, 0x41, 0xbf, 0x0d, 0x00, 0x00, 0x00, 0xb8, 0x21, 0x10, 0x00, 0x00, 0x8b,
    0xbc, 0x24, 0x80, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x74, 0x24, 0x40, 0xba, 0x13, 0x00, 0x00, 0x00,
    0x4c, 0x8d, 0x94, 0x24, 0x98, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x85, 0xc0, 0x75, 0x29, 0x41,
    0xbf, 0x0e, 0x00, 0x00, 0x00, 0xb8, 0x11, 0x10, 0x00, 0x00, 0x44, 0x89, 0xe7, 0x48, 0x8d, 0x74,
    0x24, 0x40, 0xba, 0x13, 0x00, 0x00, 0x00, 0x45, 0x31, 0xd2, 0x45, 0x31, 0xc0, 0x0f, 0x05, 0x48,
    0x85, 0xc0, 0x75, 0x04, 0x31, 0xff, 0xeb, 0x03, 0x44, 0x89, 0xff, 0xb8, 0xe7, 0x00, 0x00, 0x00,
    0x0f, 0x05, 0xeb, 0xfe,
];

/// One byte from the console, if one has arrived.
pub(crate) fn read_console_byte() -> Option<u8> {
    console::read_byte()
}

/// `AT_HWCAP` and `AT_HWCAP2` for a program started on this machine.
///
/// Linux reports `CPUID` leaf 1's `EDX` as `AT_HWCAP` on x86-64. `AT_HWCAP2`
/// carries only `FSGSBASE` and ring-3 `MWAIT`, which are bits the kernel sets
/// when it has enabled them for user mode, and this one enables neither.
pub(crate) fn user_hwcaps() -> (u64, u64) {
    (u64::from(core::arch::x86_64::__cpuid(1).edx), 0)
}

/// Enter ring 3 for the first time, at `entry` on `stack`. Does not return.
///
/// # Safety
///
/// Must be called by a user task, on its own kernel stack, with its address
/// space installed; `entry` and `stack` must be addresses within it.
pub(crate) unsafe fn enter_user(entry: u64, stack: u64) -> ! {
    // SAFETY: the caller's guarantee, passed straight through.
    unsafe { syscall::enter_user(entry, stack) }
}

/// Service a system call that arrived through the trap vector.
///
/// # Errors
///
/// Always, for now: a system call through the trap vector, where x86-64 uses SYSCALL.
pub(crate) const fn system_call(_frame: &mut TrapFrame) -> Result<(), &'static str> {
    Err("a system call through the trap vector, where x86-64 uses SYSCALL")
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

/// Where a backtrace starts: this function's frame pointer.
#[inline(always)]
pub(crate) fn frame_pointer() -> u64 {
    cpu::frame_pointer()
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

/// Whether this processor is taking interrupts right now.
pub(crate) fn interrupts_enabled() -> bool {
    // Bit 9 of RFLAGS is IF.
    cpu::read_rflags() & (1 << 9) != 0
}

pub(crate) use apic::{ipi_irq, send_ipi_to_others};
pub(crate) use msi::msi_allocate;

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

/// Stop interrupt `number` being delivered until [`unmask_interrupt`] lets
/// it through again.
///
/// # Errors
///
/// Always, for now. A device's interrupts reach x86-64 as MSI-X, and an MSI-X
/// vector is masked in the device's own table entry, which the interrupt
/// controller does not reach; stage 10's `Vector::mask` will route there.
pub(crate) fn mask_interrupt(number: u32) -> Result<(), &'static str> {
    let _ = number;
    Err("x86-64 has no controller line to mask: its device interrupts are MSI-X")
}

/// Let interrupt `number` be delivered again after [`mask_interrupt`].
///
/// # Errors
///
/// Always, for now, for the reason [`mask_interrupt`] gives.
pub(crate) fn unmask_interrupt(number: u32) -> Result<(), &'static str> {
    let _ = number;
    Err("x86-64 has no controller line to mask: its device interrupts are MSI-X")
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
pub(crate) use switch::{
    UserState, prepare_stack, reset_user_state, restore_user_state, save_user_state, switch_to,
};
pub(crate) use syscall::{UserRegs, resume_user};
