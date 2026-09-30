//! `sys/signalfd.h`: the caller's pending signals, read through a descriptor
//! as `struct signalfd_siginfo`s.
//!
//! As musl 1.2.5's `linux/signalfd.c` (MIT; see [`crate::math`] for the
//! notice), without its fallback: every architecture this library builds for
//! has `signalfd4`, so musl's retry with the older `signalfd` and two `fcntl`
//! calls for the flags is never taken. The kernel reads one word of the set,
//! [`KERNEL_SIGSET_SIZE`] bytes.

use core::ffi::c_int;

use crate::errno;
use crate::sigset::{KERNEL_SIGSET_SIZE, SigSet};
use crate::syscall::{self, nr};

/// Makes a descriptor that reads the signals in `mask`, for an `fd` of -1, or
/// makes the signalfd `fd` read them instead, returning the descriptor.
/// `flags` may hold `SFD_CLOEXEC` and `SFD_NONBLOCK`, and only a new
/// descriptor takes them. The signals should be blocked, or they are
/// delivered before they can be read.
///
/// # Safety
///
/// `mask` must be valid for reads of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn signalfd(fd: c_int, mask: *const SigSet, flags: c_int) -> c_int {
    // SAFETY: the caller vouches for the set, of which the kernel reads one
    // word; it writes nothing.
    let ret = unsafe {
        syscall::syscall4(
            nr::SIGNALFD4,
            fd as usize,
            mask.addr(),
            KERNEL_SIGSET_SIZE,
            flags as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}
