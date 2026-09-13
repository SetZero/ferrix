//! `poll.h`: waiting for descriptors to become ready.
//!
//! Both calls are the kernel's `ppoll`, which every architecture has; AArch64
//! has no `poll`. The kernel writes the time left into the timeout it is
//! given, and C's timeout is `const`, so it always gets a copy.

use core::ffi::{c_int, c_short, c_ulong, c_void};
use core::mem::{offset_of, size_of};

use crate::errno;
use crate::syscall::{self, nr};
use crate::time::{self, KERNEL_SIGSET_SIZE, Timespec};

/// C's `struct pollfd`. The kernel's in `asm-generic/poll.h` is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Pollfd {
    /// The descriptor; a negative one is skipped.
    pub fd: c_int,
    /// The events to wait for.
    pub events: c_short,
    /// The events that happened.
    pub revents: c_short,
}

const _: () = assert!(size_of::<Pollfd>() == 8);
const _: () = assert!(offset_of!(Pollfd, revents) == 6);

/// Calls the kernel's `ppoll` with an optional timeout copy and signal mask.
///
/// # Safety
///
/// As [`ppoll`], for `fds` and `mask`.
unsafe fn raw_ppoll(
    fds: *mut Pollfd,
    count: c_ulong,
    mut timeout: Option<Timespec>,
    mask: *const c_void,
) -> c_int {
    // SAFETY: the caller vouches for `fds` and `mask`, and the timeout is a
    // live local copy or null.
    let ret = unsafe {
        syscall::syscall6(
            nr::PPOLL,
            fds.addr(),
            count as usize,
            time::timeout_address(&mut timeout),
            mask.addr(),
            KERNEL_SIGSET_SIZE,
            0,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Waits up to `timeout` milliseconds, or forever if it is negative, for an
/// event on one of the `count` descriptors at `fds`.
///
/// # Safety
///
/// `fds` must be valid for reads and writes of `count` `struct pollfd`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn poll(fds: *mut Pollfd, count: c_ulong, timeout: c_int) -> c_int {
    let timeout = (timeout >= 0).then(|| time::split(i64::from(timeout), 1000, 1_000_000));
    // SAFETY: the caller vouches for `fds`, and there is no mask.
    unsafe { raw_ppoll(fds, count, timeout, core::ptr::null()) }
}

/// [`poll`] with a `struct timespec` timeout, or none if it is null, and with
/// the signal mask replaced by `*mask` while waiting, if it is not null.
///
/// # Safety
///
/// `fds` must be valid for reads and writes of `count` `struct pollfd`,
/// `timeout` null or valid for a read of a `struct timespec`, and `mask` null
/// or valid for a read of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ppoll(
    fds: *mut Pollfd,
    count: c_ulong,
    timeout: *const Timespec,
    mask: *const c_void,
) -> c_int {
    // SAFETY: the caller vouches for `timeout`.
    let timeout = unsafe { time::copy_timeout(timeout) };
    // SAFETY: the caller vouches for `fds` and `mask`.
    unsafe { raw_ppoll(fds, count, timeout, mask) }
}
