//! `sys/timerfd.h`: a timer behind a descriptor, which becomes readable when
//! it expires and reads as the number of expiries since the last read.
//!
//! As musl 1.2.5's `linux/timerfd.c` (MIT; see [`crate::math`] for the
//! notice): each function is its system call. This library's `struct
//! timespec` is the kernel's `__kernel_timespec` on every architecture, so
//! ARMv7-A calls `timerfd_settime64` and `timerfd_gettime64` under the plain
//! names, as `nr` names them, and needs none of musl's 32-bit fallback.

use core::ffi::c_int;
use core::mem::{offset_of, size_of};

use crate::errno;
use crate::syscall::{self, nr};
use crate::time::Timespec;

/// C's `struct itimerspec`: a period, and the time to the next expiry. The
/// kernel's `__kernel_itimerspec`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Itimerspec {
    /// The period after the first expiry, or zero for a single expiry.
    pub it_interval: Timespec,
    /// The time to the next expiry, or zero for a timer that is off.
    pub it_value: Timespec,
}

const _: () = assert!(size_of::<Itimerspec>() == 32);
const _: () = assert!(offset_of!(Itimerspec, it_value) == 16);

/// Creates a timer on `clock`, returning its descriptor. `flags` may hold
/// `TFD_CLOEXEC` and `TFD_NONBLOCK`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn timerfd_create(clock: c_int, flags: c_int) -> c_int {
    // SAFETY: `timerfd_create` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::TIMERFD_CREATE, clock as usize, flags as usize) };
    errno::from_syscall(ret) as c_int
}

/// Arms timer `fd` with `*new`, or disarms it for a zero `it_value`, and
/// stores what it was in `*old` unless `old` is null. `flags` may hold
/// `TFD_TIMER_ABSTIME`, which makes `it_value` a time on the timer's clock
/// rather than one from now.
///
/// # Safety
///
/// `new` must be valid for a read of a `struct itimerspec`, and `old` for a
/// write of one or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn timerfd_settime(
    fd: c_int,
    flags: c_int,
    new: *const Itimerspec,
    old: *mut Itimerspec,
) -> c_int {
    // SAFETY: the caller vouches for both structures.
    let ret = unsafe {
        syscall::syscall4(
            nr::TIMERFD_SETTIME,
            fd as usize,
            flags as usize,
            new.addr(),
            old.addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Stores timer `fd`'s period and the time left to its next expiry in
/// `*current`.
///
/// # Safety
///
/// `current` must be valid for a write of a `struct itimerspec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn timerfd_gettime(fd: c_int, current: *mut Itimerspec) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let ret = unsafe { syscall::syscall2(nr::TIMERFD_GETTIME, fd as usize, current.addr()) };
    errno::from_syscall(ret) as c_int
}
