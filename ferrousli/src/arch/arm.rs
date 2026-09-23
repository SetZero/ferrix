//! ARMv7-A's part of running threads.
//!
//! * The thread pointer is `TPIDRURO`, which user code can read with `mrc`
//!   but not write: the kernel sets it, through the ARM-private `set_tls`
//!   call.
//! * TLS is variant I: the thread pointer points at an 8-byte header, glibc's
//!   `tcbhead_t`, and the TLS blocks follow it. [`crate::thread`] places the
//!   control block below.
//! * `clone`'s `tls` argument is the thread pointer itself.
//! * The kernel's `clone` takes `flags, stack, parent_tid, tls, child_tid`,
//!   in r0 to r4.
//!
//! The assembly is adapted from musl's `src/thread/arm` (MIT).

use core::ffi::{c_int, c_void};

use crate::syscall::{self, nr};

/// The bytes at the thread pointer before the first TLS block: `tcbhead_t`,
/// whose `dtv` and private word the ABI reserves.
pub const TCB_SIZE: usize = 8;

/// A function `clone`'s child calls on its new stack.
pub type Entry = unsafe extern "C" fn(*mut c_void) -> c_int;

/// The calling thread's thread pointer. Zero before one is set.
pub fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reading `TPIDRURO` changes nothing.
    unsafe {
        core::arch::asm!(
            "mrc p15, 0, {}, c13, c0, 3",
            out(reg) tp,
            options(nomem, nostack, preserves_flags),
        );
    }
    tp
}

/// Makes `tp` the calling thread's thread pointer. Returns the kernel's
/// result: zero, or a negated error number.
///
/// # Safety
///
/// `tp` must be a thread pointer [`crate::thread`] laid out, which lives as
/// long as the thread.
pub unsafe fn set_thread_pointer(tp: usize) -> isize {
    // SAFETY: the caller vouches for the thread pointer.
    unsafe { syscall::syscall2(nr::ARM_SET_TLS, tp, 0) }
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
/// The child's stack pointer is `stack` rounded down to 16 bytes. If `entry`
/// returns, the thread exits with its result.
///
/// # Safety
///
/// `stack` must be the top of memory the child may use, `tls` a thread
/// pointer set up for it, `parent_tid` and `child_tid` what `flags` asks the
/// kernel to write, and `entry` sound to run on the new thread.
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
// In: r0 = entry, r1 = stack, r2 = flags, r3 = arg, and parent_tid, tls and
// child_tid on the stack. The system call wants r0 = flags, r1 = stack,
// r2 = parent_tid, r3 = tls, r4 = child_tid and the number in r7; r4 to r7
// are the caller's, so they are saved first, which moves the stack arguments
// 16 bytes up. The child keeps `entry` in r5 and `arg` in r6, which the
// system call preserves, and clears the frame pointer and link register.
core::arch::global_asm!(
    ".pushsection .text.ferrousli_clone,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".globl ferrousli_clone",
    ".hidden ferrousli_clone",
    ".type ferrousli_clone,%function",
    "ferrousli_clone:",
    "push {{r4, r5, r6, r7}}",
    "mov r7, #{clone}",
    "mov r6, r3",
    "mov r5, r0",
    "mov r0, r2",
    "and r1, r1, #-16",
    "ldr r2, [sp, #16]",
    "ldr r3, [sp, #20]",
    "ldr r4, [sp, #24]",
    "svc #0",
    "cmp r0, #0",
    "beq 1f",
    "pop {{r4, r5, r6, r7}}",
    "bx lr",
    "1:",
    "mov r11, #0",
    "mov lr, #0",
    "mov r0, r6",
    "blx r5",
    "mov r7, #{exit}",
    "svc #0",
    "udf #0",
    ".size ferrousli_clone,.-ferrousli_clone",
    ".popsection",
    clone = const nr::CLONE,
    exit = const nr::EXIT,
);

// ferrousli_unmap_self(base, len)
//
// `munmap` takes its arguments where the call left them. Neither system call
// uses the stack, which may be gone after the first.
core::arch::global_asm!(
    ".pushsection .text.ferrousli_unmap_self,\"ax\",%progbits",
    ".p2align 2",
    ".arm",
    ".globl ferrousli_unmap_self",
    ".hidden ferrousli_unmap_self",
    ".type ferrousli_unmap_self,%function",
    "ferrousli_unmap_self:",
    "mov r7, #{munmap}",
    "svc #0",
    "mov r0, #0",
    "mov r7, #{exit}",
    "svc #0",
    "udf #0",
    ".size ferrousli_unmap_self,.-ferrousli_unmap_self",
    ".popsection",
    munmap = const nr::MUNMAP,
    exit = const nr::EXIT,
);
