//! The `SYSCALL` entry path, and the way into ring 3.
//!
//! # Why `SYSCALL` and not an interrupt gate
//!
//! An interrupt gate with `DPL=3` would be far less work: the processor
//! switches to the kernel stack out of the TSS by itself, and the existing
//! trap machinery would carry it. Linux still has one, at `int $0x80`.
//!
//! But nothing calls it. Every current libc on x86-64 issues `syscall`, so an
//! interrupt gate would be a path the kernel could test and no real program
//! would take — which is the worst kind of working code.
//!
//! # What `SYSCALL` does not do
//!
//! It does almost nothing, and the gaps are the whole of this file. It loads
//! `CS` and `SS` from `STAR`, puts the return address in `RCX` and the flags
//! in `R11`, masks the flags with `SFMASK`, and jumps to `LSTAR`. It does
//! **not** switch stacks: the first instruction of the kernel runs on the
//! user's stack, at the user's mercy. It does not save a single register
//! beyond the two it clobbers. And `RCX` and `R11` are gone, which is why the
//! ABI puts the fourth argument in `R10` rather than `RCX` as the C one does.
//!
//! So the trampoline's first job is to get off the user stack without using a
//! register, which is what `GS` and the per-CPU record are for. `swapgs`
//! exchanges `GS_BASE` with `KERNEL_GS_BASE`, so one instruction turns a
//! user-controlled `GS` into the kernel's own — and the same instruction on
//! the way out turns it back.
//!
//! # The rule that makes `swapgs` safe
//!
//! `swapgs` is not idempotent and the processor does not tell you which way
//! round `GS` currently is. Get it wrong and the kernel reads its per-CPU
//! record through a pointer the program chose. The rule here is the one Linux
//! uses: **swap exactly when the privilege level changed**, decided by the
//! saved `CS`, and never on a path that cannot have come from user mode.

use core::mem::offset_of;

use crate::smp::PerCpu;

use super::cpu;
use super::gdt;

/// `IA32_EFER`. Bit 0 enables `SYSCALL`/`SYSRET`.
const IA32_EFER: u32 = 0xC000_0080;
/// `IA32_STAR`. Holds the two segment bases.
const IA32_STAR: u32 = 0xC000_0081;
/// `IA32_LSTAR`. The 64-bit entry point.
const IA32_LSTAR: u32 = 0xC000_0082;
/// `IA32_FMASK`. Flags cleared on entry.
const IA32_FMASK: u32 = 0xC000_0084;
/// `IA32_KERNEL_GS_BASE`. What `swapgs` exchanges `GS_BASE` with.
const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;
/// `IA32_FS_BASE`, which `arch_prctl(ARCH_SET_FS)` writes: a program's thread
/// pointer, and the first thing a libc sets up.
const IA32_FS_BASE: u32 = 0xC000_0100;

/// `EFER.SCE`: system call extensions.
const EFER_SCE: u64 = 1;

/// Flags cleared on entry to the kernel.
///
/// `IF` is the one that matters: without it here, the kernel would run the
/// first instructions of every system call with interrupts still enabled on a
/// stack it has not switched to yet. `DF` matters because the System V ABI lets a
/// program leave the direction flag set and every `rep movs` in the kernel
/// assumes it is clear. `AC` is cleared so that a future `SMAP` cannot be left
/// open by the caller.
const FMASK: u64 = (1 << 9) | (1 << 10) | (1 << 18);

// The trampoline reaches the per-CPU record from assembly, so these offsets
// are part of the contract between this file and `crate::smp`. Asserting them
// here means a field reordered over there fails the build rather than sending
// the kernel to a stack made of somebody's `logical` number.
const KERNEL_STACK_OFFSET: usize = offset_of!(PerCpu, kernel_stack);
const USER_STACK_OFFSET: usize = offset_of!(PerCpu, user_stack);
const _: () = assert!(
    KERNEL_STACK_OFFSET == 8,
    "the syscall trampoline loads the kernel stack from gs:8"
);
const _: () = assert!(
    USER_STACK_OFFSET == 16,
    "the syscall trampoline parks the user stack at gs:16"
);

/// A user program's registers, as the trampoline saves them.
///
/// Laid out to match the pushes in `ferrix_syscall_stub` exactly, in reverse:
/// the last thing pushed is the first field. `repr(C)` because assembly is the
/// other half of this type's definition.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(crate) struct SyscallFrame {
    /// Callee-saved, and the ABI's sixth argument register is not among them.
    pub(crate) r15: u64,
    /// Callee-saved.
    pub(crate) r14: u64,
    /// Callee-saved.
    pub(crate) r13: u64,
    /// Callee-saved.
    pub(crate) r12: u64,
    /// Callee-saved.
    pub(crate) rbp: u64,
    /// Callee-saved.
    pub(crate) rbx: u64,
    /// Sixth argument.
    pub(crate) r9: u64,
    /// Fifth argument.
    pub(crate) r8: u64,
    /// Fourth argument. The ABI uses `R10` here and not `RCX`, because
    /// `SYSCALL` destroys `RCX`.
    pub(crate) r10: u64,
    /// Third argument.
    pub(crate) rdx: u64,
    /// Second argument.
    pub(crate) rsi: u64,
    /// First argument.
    pub(crate) rdi: u64,
    /// The system call number on the way in, the result on the way out.
    pub(crate) rax: u64,
    /// The user `RFLAGS`, which `SYSCALL` left here.
    pub(crate) r11: u64,
    /// The user return address, which `SYSCALL` left here.
    pub(crate) rcx: u64,
    /// The user stack pointer, taken out of the per-CPU scratch word.
    pub(crate) user_rsp: u64,
}

core::arch::global_asm!(
    r#"
.section .text
.globl ferrix_syscall_stub
.align 16
ferrix_syscall_stub:
    // Off the user stack first, before anything can be pushed. `swapgs` makes
    // GS the kernel's; the two GS-relative words are the only memory this
    // code can name without a stack.
    swapgs
    movq %rsp, %gs:16
    movq %gs:8, %rsp

    // Rebuild the user's state as a frame, in `SyscallFrame` order.
    pushq %gs:16          // user_rsp
    pushq %rcx            // user rip
    pushq %r11            // user rflags
    pushq %rax
    pushq %rdi
    pushq %rsi
    pushq %rdx
    pushq %r10
    pushq %r8
    pushq %r9
    pushq %rbx
    pushq %rbp
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15

    cld
    movq %rsp, %rdi
    callq ferrix_syscall_entry

    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %rbp
    popq %rbx
    popq %r9
    popq %r8
    popq %r10
    popq %rdx
    popq %rsi
    popq %rdi
    popq %rax
    popq %r11             // user rflags, back where SYSRET wants it
    popq %rcx             // user rip, back where SYSRET wants it
    popq %rsp             // user stack, straight into RSP

    swapgs
    sysretq

// Enter ring 3 for the first time.
//   rdi = entry point, rsi = user stack pointer
// Never returns: a program leaves ring 3 through a system call or a trap, and
// for good through `exit_group`, which ends its task in the scheduler.
.globl ferrix_enter_user
.align 16
ferrix_enter_user:
    // Masked from here to the `sysretq`, which opens them from R11. Between
    // loading the user stack pointer and dropping privilege this is ring 0 on a
    // user stack, and an interrupt taken there would be pushed onto it.
    cli

    // `SYSRET` takes the address from RCX and the flags from R11, which is
    // exactly the shape of a return from a system call that never happened.
    movq %rdi, %rcx
    movq %rsi, %rsp
    // Interrupts open in ring 3: a program is a scheduled task, carrying its
    // own address space, thread pointer and entry stack, so a tick there is an
    // ordinary preemption. Bit 1 is reserved and always set.
    movq $0x202, %r11

    // Nothing of the kernel's may survive into ring 3. A register left holding
    // a kernel pointer is an information leak that no test will ever notice.
    xorq %rax, %rax
    xorq %rbx, %rbx
    xorq %rdx, %rdx
    xorq %rsi, %rsi
    xorq %rdi, %rdi
    xorq %rbp, %rbp
    xorq %r8, %r8
    xorq %r9, %r9
    xorq %r10, %r10
    xorq %r12, %r12
    xorq %r13, %r13
    xorq %r14, %r14
    xorq %r15, %r15

    swapgs
    sysretq
"#,
    options(att_syntax)
);

unsafe extern "C" {
    /// The `LSTAR` entry point, defined in the block above.
    fn ferrix_syscall_stub();
    /// Enter ring 3 at `entry` on `stack`.
    fn ferrix_enter_user(entry: u64, stack: u64) -> !;
}

/// Where the assembly hands a system call to the rest of the kernel.
///
/// The argument order is this architecture's, not the C one: the ABI puts the
/// fourth argument in `R10` because `SYSCALL` destroys `RCX`. Getting that
/// wrong gives every four-argument call a garbage fourth argument, which for
/// `mmap` is the flags word and so fails loudly, and for `openat` is the mode
/// and so does not.
///
/// # Safety
///
/// Called only from `ferrix_syscall_stub`, with `frame` pointing at the frame
/// it just built on this processor's kernel stack.
#[unsafe(no_mangle)]
extern "C" fn ferrix_syscall_entry(frame: &mut SyscallFrame) {
    let args = crate::syscall::SyscallArgs {
        number: frame.rax as usize,
        args: [
            frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
        ],
    };

    // One call is answered before dispatch, because it is a fact about this
    // processor rather than about the process: `arch_prctl(ARCH_SET_FS)` writes
    // an MSR. It exists on no other architecture -- AArch64 writes `TPIDR_EL0`
    // itself and ARMv7-A has `set_tls` -- so there is nothing for an
    // architecture-neutral dispatch table to say about it. The value survives a
    // switch to another task because the scheduler saves `FS_BASE` with the
    // rest of the program's user state.
    if let Some(ferrix_linux_abi::nr::Syscall::ArchPrctl) = super::decode_syscall(args.number) {
        frame.rax = arch_prctl(args.args[0], args.args[1]) as u64;
        return;
    }

    // Open while the call is served: a call may block, and one that spins
    // waiting for input must not keep the processor from switching away.
    // `SFMASK` closed them on entry, and they are closed again before the
    // frame is restored, because the way out swaps `GS` on a live stack.
    super::enable_interrupts();
    let outcome = crate::syscall::dispatch(&args);
    super::disable_interrupts();

    match outcome {
        crate::syscall::Outcome::Return(value) => {
            frame.rax = value as u64;
        }
        crate::syscall::Outcome::Enter { entry, stack } => {
            // `execve` and a fresh `clone` child: the registers this frame
            // holds belong to a program that no longer exists, so they are
            // replaced rather than returned into. Everything else is cleared
            // for the same reason `ferrix_enter_user` clears it -- a register
            // carrying a kernel value into ring 3 is a leak nothing tests.
            *frame = SyscallFrame {
                r15: 0,
                r14: 0,
                r13: 0,
                r12: 0,
                rbp: 0,
                rbx: 0,
                r9: 0,
                r8: 0,
                r10: 0,
                rdx: 0,
                rsi: 0,
                rdi: 0,
                rax: 0,
                // `SYSRET` takes the flags from R11 and the address from RCX,
                // which is why an entry point can be delivered by returning.
                // Interrupts open, as `ferrix_enter_user` leaves them.
                r11: 0x202,
                rcx: entry,
                user_rsp: stack,
            };
        }
    }

    crate::syscall::process::before_return_to_user();
}

/// `ARCH_SET_FS`, and the three requests that are not it.
///
/// Only the first matters: a static binary cannot start without it, because a
/// libc that cannot place its thread-local block aborts before `main`. The
/// others are answered rather than dispatched so that a program asking gets
/// `EINVAL` and not `ENOSYS`, which is the difference between "this kernel
/// does not know that request" and "this kernel has no `arch_prctl`".
fn arch_prctl(code: u64, value: u64) -> isize {
    /// Set the base `FS` resolves against: the thread pointer.
    const ARCH_SET_FS: u64 = 0x1002;

    match code {
        ARCH_SET_FS => {
            // A thread pointer must be a user address. Nothing here follows
            // it, but a program that set it to a kernel address would have
            // every `FS`-relative access it made afterwards resolve there.
            if !ferrix_bootinfo::is_user_address(value) {
                return ferrix_linux_abi::errno::Errno::EPERM.as_return_value();
            }
            // SAFETY: a canonical user address, written to this processor's
            // `FS_BASE`; it changes only how user accesses resolve.
            unsafe { set_thread_pointer(value) };
            0
        }
        // Everything else, `ARCH_GET_FS` included. Reading the thread pointer
        // back means writing it through a user pointer, which needs the
        // caller's address space -- and this path deliberately does not have
        // one, because it is answering a question about the processor. No
        // libc asks at startup; when something does, it belongs in the
        // dispatch table with a `Process` in hand rather than here.
        _ => ferrix_linux_abi::errno::Errno::EINVAL.as_return_value(),
    }
}

/// Turn `SYSCALL` on for this processor.
///
/// Per processor, because every MSR here is per processor. A secondary that
/// skipped this would take `#UD` on the first system call a program made on
/// it, long after boot and nowhere near the cause.
///
/// # Safety
///
/// Must run once per processor, after its GDT is loaded and after its per-CPU
/// record is installed in `GS`.
pub(crate) unsafe fn init() {
    // SAFETY: `IA32_EFER` exists on every 64-bit x86; setting SCE only enables
    // an instruction that faults until `LSTAR` is set, two lines below.
    let efer = unsafe { cpu::read_msr(IA32_EFER) };
    // SAFETY: as above.
    unsafe { cpu::write_msr(IA32_EFER, efer | EFER_SCE) };

    // STAR[47:32] is the kernel selector base, STAR[63:48] the user one.
    //
    // The user base carries RPL 3, and has to. `SYSRET` loads CS from base + 16
    // and forces RPL 3 into it, but loads SS from base + 8 *as written*. With a
    // bare `0x18` a program runs at CPL 3 on SS `0x20`, which nothing checks
    // until an exception pushes that SS and `iretq` back to ring 3 refuses it
    // with `#GP(0x20)` -- on hardware and under KVM, never under `tcg`, which
    // is why it passed the boot test. Linux uses `__USER32_CS | 3` for this.
    let star = (u64::from(gdt::KERNEL_CODE) << 32) | (u64::from(gdt::SYSRET_BASE | 3) << 48);
    // SAFETY: the selectors are this processor's own GDT entries, and the
    // layout `SYSRET` computes from is asserted at their definition.
    unsafe { cpu::write_msr(IA32_STAR, star) };

    // SAFETY: the address of a function in the kernel's own text.
    unsafe { cpu::write_msr(IA32_LSTAR, ferrix_syscall_stub as *const () as usize as u64) };
    // SAFETY: a mask of flag bits.
    unsafe { cpu::write_msr(IA32_FMASK, FMASK) };

    // `swapgs` exchanges the two, so the kernel's base has to be parked in the
    // shadow copy for the *first* swap to find it. Read rather than assumed:
    // `set_cpu_local` put it in `GS_BASE` already.
    // SAFETY: reading the base this processor installed for itself.
    let kernel_gs = unsafe { cpu::read_msr(IA32_GS_BASE) };
    // SAFETY: parking the kernel's own per-CPU address in the shadow MSR.
    unsafe { cpu::write_msr(IA32_KERNEL_GS_BASE, kernel_gs) };
}

/// `IA32_GS_BASE`, duplicated from `mod.rs` rather than re-exported because
/// this file is the other half of the `swapgs` contract and should say so.
const IA32_GS_BASE: u32 = 0xC000_0101;

/// Set the thread pointer a program reads through `FS`.
///
/// What `arch_prctl(ARCH_SET_FS)` does, and the one system call on this
/// architecture that a static binary cannot start without: a libc that cannot
/// place its TLS block aborts before `main`.
///
/// # Safety
///
/// `base` is a user address the program chose; nothing dereferences it here.
pub(crate) unsafe fn set_thread_pointer(base: u64) {
    // SAFETY: `IA32_FS_BASE` accepts any canonical address. Writing it affects
    // only how this processor resolves `FS`-relative user accesses.
    unsafe { cpu::write_msr(IA32_FS_BASE, base) };
}

/// This processor's `FS_BASE`: the running program's thread pointer.
///
/// # Safety
///
/// Reads an MSR; the caller wants the value for the task whose state it is.
pub(crate) unsafe fn thread_pointer() -> u64 {
    // SAFETY: `IA32_FS_BASE` exists on every 64-bit x86 and reading it has no
    // side effects.
    unsafe { cpu::read_msr(IA32_FS_BASE) }
}

/// Point both of this processor's ways in from ring 3 at `top`.
///
/// There are two because `SYSCALL` switches no stack and an interrupt gate
/// does: the trampoline loads `gs:8`, and an exception or interrupt from ring 3
/// loads the TSS's `RSP0`. Both must name the running task's own kernel stack,
/// or two programs' traps land on one stack.
///
/// # Safety
///
/// `top` must be the top of the kernel stack of the task this processor is
/// switching to, and a TSS must be loaded.
pub(crate) unsafe fn set_entry_stack(top: u64) {
    if let Some(cpu) = crate::smp::this_cpu() {
        cpu.kernel_stack
            .store(top, core::sync::atomic::Ordering::Relaxed);
    }
    // SAFETY: the caller guarantees the stack and the TSS.
    unsafe { gdt::set_privilege_stack(top) };
}

/// Enter ring 3 for the first time, at `entry` on `stack`. Does not return.
///
/// # Safety
///
/// Must be called by a user task, on its own kernel stack, with its address
/// space installed and its entry stack set; `entry` and `stack` must be
/// addresses inside that space.
pub(crate) unsafe fn enter_user(entry: u64, stack: u64) -> ! {
    // SAFETY: the caller's guarantee is the assembly's contract.
    unsafe { ferrix_enter_user(entry, stack) }
}
