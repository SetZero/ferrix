//! x86-64's part of running threads.
//!
//! * The thread pointer is the `%fs` base, set with `arch_prctl`. `%fs:0`
//!   holds the pointer itself, so reading it costs one load.
//! * `clone`'s `tls` argument is the thread pointer itself.
//! * The kernel's `clone` takes `flags, stack, parent_tid, child_tid, tls` in
//!   that order on x86-64, which is not every architecture's order.
//!
//! The assembly is adapted from musl's `src/thread/x86_64` (MIT).

use core::ffi::{c_int, c_void};

use crate::syscall::{self, nr};

/// `ARCH_SET_FS`, from `asm/prctl.h`: `arch_prctl`'s request to set the `%fs`
/// base.
const ARCH_SET_FS: usize = 0x1002;

/// A function `clone`'s child calls on its new stack.
pub type Entry = unsafe extern "C" fn(*mut c_void) -> c_int;

/// The calling thread's thread pointer. Zero before one is set, in which case
/// reading it faults.
pub fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reading `%fs:0` reads memory only. Once a thread pointer is set
    // it holds that pointer, and before then the read faults rather than
    // returning garbage.
    unsafe {
        core::arch::asm!(
            "mov {}, qword ptr fs:[0]",
            out(reg) tp,
            options(nostack, readonly, preserves_flags),
        );
    }
    tp
}

/// Makes `tp` the calling thread's thread pointer. Returns the kernel's
/// result: zero, or a negated error number.
///
/// # Safety
///
/// `tp` must be a control block whose first word holds `tp`, and which lives
/// as long as the thread.
pub unsafe fn set_thread_pointer(tp: usize) -> isize {
    // SAFETY: the caller vouches for the control block.
    unsafe { syscall::syscall2(nr::ARCH_PRCTL, ARCH_SET_FS, tp) }
}

/// The `tls` argument `clone` takes for a thread whose thread pointer is `tp`.
pub const fn clone_tls(tp: usize) -> usize {
    tp
}

unsafe extern "C" {
    /// See [`clone`].
    fn ferrousli_clone(
        entry: Entry,
        stack: usize,
        flags: usize,
        arg: *mut c_void,
        parent_tid: *mut c_int,
        tls: usize,
        child_tid: *mut c_int,
    ) -> c_int;

    /// See [`unmap_self`].
    fn ferrousli_unmap_self(base: usize, len: usize) -> !;
}

/// Makes a thread with `clone(flags)`, running `entry(arg)` on `stack`, and
/// returns its id or a negated error number.
///
/// The child's stack pointer is `stack` rounded down to 16 bytes, less the
/// word the trampoline pops `arg` from, so `entry` starts with the stack
/// aligned as the calling convention requires. If `entry` returns, the thread
/// exits with its result.
///
/// # Safety
///
/// `stack` must be the top of memory the child may use, `tls` a control block
/// set up for it, `parent_tid` and `child_tid` what `flags` asks the kernel to
/// write, and `entry` sound to run on the new thread.
pub unsafe fn clone(
    entry: Entry,
    stack: usize,
    flags: usize,
    arg: *mut c_void,
    parent_tid: *mut c_int,
    tls: usize,
    child_tid: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for every argument.
    unsafe { ferrousli_clone(entry, stack, flags, arg, parent_tid, tls, child_tid) }
}

/// Unmaps `len` bytes at `base` and ends the calling thread, touching neither
/// the stack nor the thread pointer after the unmapping, since either may be
/// in the mapping.
///
/// # Safety
///
/// Every signal must be blocked, since a handler would need a stack, and
/// nothing may use the mapping afterwards.
pub unsafe fn unmap_self(base: usize, len: usize) -> ! {
    // SAFETY: the caller vouches for the mapping and the signal mask.
    unsafe { ferrousli_unmap_self(base, len) }
}

// ferrousli_clone(entry, stack, flags, arg, parent_tid, tls, child_tid)
//
// In: rdi = entry, rsi = stack, rdx = flags, rcx = arg, r8 = parent_tid,
// r9 = tls, 8(%rsp) = child_tid. The system call wants rdi = flags,
// rsi = stack, rdx = parent_tid, r10 = child_tid, r8 = tls. `syscall` keeps
// r9, so the child finds `entry` there; it finds `arg` on its stack.
//
// The child clears rbp, ending the frame chain for debuggers.
core::arch::global_asm!(
    ".pushsection .text.ferrousli_clone,\"ax\",@progbits",
    ".p2align 4",
    ".globl ferrousli_clone",
    ".hidden ferrousli_clone",
    ".type ferrousli_clone,@function",
    "ferrousli_clone:",
    "mov ${clone}, %eax",
    "mov %rdi, %r11",
    "mov %rdx, %rdi",
    "mov %r8, %rdx",
    "mov %r9, %r8",
    "mov 8(%rsp), %r10",
    "mov %r11, %r9",
    "and $-16, %rsi",
    "sub $8, %rsi",
    "mov %rcx, (%rsi)",
    "syscall",
    "test %eax, %eax",
    "jnz 1f",
    "xor %ebp, %ebp",
    "pop %rdi",
    "call *%r9",
    "mov %eax, %edi",
    "mov ${exit}, %eax",
    "syscall",
    "hlt",
    "1:",
    "ret",
    ".size ferrousli_clone,.-ferrousli_clone",
    ".popsection",
    clone = const nr::CLONE,
    exit = const nr::EXIT,
    options(att_syntax),
);

// ferrousli_unmap_self(base, len)
//
// `munmap` takes its arguments where the call left them. Neither system call
// uses the stack, which may be gone after the first.
core::arch::global_asm!(
    ".pushsection .text.ferrousli_unmap_self,\"ax\",@progbits",
    ".p2align 4",
    ".globl ferrousli_unmap_self",
    ".hidden ferrousli_unmap_self",
    ".type ferrousli_unmap_self,@function",
    "ferrousli_unmap_self:",
    "mov ${munmap}, %eax",
    "syscall",
    "xor %edi, %edi",
    "mov ${exit}, %eax",
    "syscall",
    "hlt",
    ".size ferrousli_unmap_self,.-ferrousli_unmap_self",
    ".popsection",
    munmap = const nr::MUNMAP,
    exit = const nr::EXIT,
    options(att_syntax),
);
