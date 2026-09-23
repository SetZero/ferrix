//! AArch64's part of running threads.
//!
//! * The thread pointer is `TPIDR_EL0`, which user code reads and writes
//!   itself with `mrs` and `msr`.
//! * TLS is variant I: the thread pointer points at a 16-byte header, glibc's
//!   `tcbhead_t`, and the TLS blocks follow it. [`crate::thread`] places the
//!   control block below.
//! * `clone`'s `tls` argument is the thread pointer itself.
//! * The kernel's `clone` takes `flags, stack, parent_tid, tls, child_tid` in
//!   that order here, not x86-64's.
//!
//! The assembly is adapted from musl's `src/thread/aarch64` (MIT).

use core::ffi::{c_int, c_void};

use crate::syscall::nr;

/// The bytes at the thread pointer before the first TLS block: `tcbhead_t`,
/// whose `dtv` and private word the ABI reserves.
pub const TCB_SIZE: usize = 16;

/// A function `clone`'s child calls on its new stack.
pub type Entry = unsafe extern "C" fn(*mut c_void) -> c_int;

/// The calling thread's thread pointer. Zero before one is set.
pub fn thread_pointer() -> usize {
    let tp: usize;
    // SAFETY: reading `TPIDR_EL0` changes nothing.
    unsafe {
        core::arch::asm!(
            "mrs {}, tpidr_el0",
            out(reg) tp,
            options(nomem, nostack, preserves_flags),
        );
    }
    tp
}

/// Makes `tp` the calling thread's thread pointer. Returns zero, as the
/// system call other architectures make would.
///
/// # Safety
///
/// `tp` must be a thread pointer [`crate::thread`] laid out, which lives as
/// long as the thread.
pub unsafe fn set_thread_pointer(tp: usize) -> isize {
    // SAFETY: the caller vouches for the thread pointer. `TPIDR_EL0` is the
    // thread's own register.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el0, {}",
            in(reg) tp,
            options(nomem, nostack, preserves_flags),
        );
    }
    0
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
/// 16 the trampoline pops `entry` and `arg` from. If `entry` returns, the
/// thread exits with its result.
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
// In: x0 = entry, x1 = stack, x2 = flags, x3 = arg, x4 = parent_tid,
// x5 = tls, x6 = child_tid. The system call wants x0 = flags, x1 = stack,
// x2 = parent_tid, x3 = tls, x4 = child_tid. The child finds `entry` and
// `arg` on its new stack, where the parent stored them before the call.
//
// The child clears the frame pointer and the link register, ending the frame
// chain for debuggers.
core::arch::global_asm!(
    ".pushsection .text.ferrousli_clone,\"ax\",@progbits",
    ".p2align 2",
    ".globl ferrousli_clone",
    ".hidden ferrousli_clone",
    ".type ferrousli_clone,@function",
    "ferrousli_clone:",
    "and x1, x1, #-16",
    "stp x0, x3, [x1, #-16]!",
    "mov x0, x2",
    "mov x2, x4",
    "mov x3, x5",
    "mov x4, x6",
    "mov x8, #{clone}",
    "svc #0",
    "cbz x0, 1f",
    "ret",
    "1:",
    "mov x29, #0",
    "mov x30, #0",
    "ldp x1, x0, [sp], #16",
    "blr x1",
    "mov x8, #{exit}",
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
    ".pushsection .text.ferrousli_unmap_self,\"ax\",@progbits",
    ".p2align 2",
    ".globl ferrousli_unmap_self",
    ".hidden ferrousli_unmap_self",
    ".type ferrousli_unmap_self,@function",
    "ferrousli_unmap_self:",
    "mov x8, #{munmap}",
    "svc #0",
    "mov x0, #0",
    "mov x8, #{exit}",
    "svc #0",
    "udf #0",
    ".size ferrousli_unmap_self,.-ferrousli_unmap_self",
    ".popsection",
    munmap = const nr::MUNMAP,
    exit = const nr::EXIT,
);
