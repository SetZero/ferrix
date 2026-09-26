//! `sys/epoll.h`: sets of descriptors a thread waits on together, which
//! Linux adds beside `poll` and event loops such as calloop are built on.
//!
//! Each call is its system call, as in musl 1.2.5's `linux/epoll.c` (MIT; see
//! [`crate::math`] for the notice), using only the calls every architecture
//! has: `epoll_create` is `epoll_create1` after checking its size, and
//! `epoll_wait` is `epoll_pwait` with no mask. The waits are cancellation
//! points. `epoll_pwait2`, whose timeout is a `struct timespec`, is glibc's and
//! not in musl 1.2.5; the kernel only reads its timeout, so it is passed on.
//!
//! `struct epoll_event` is packed on x86-64, as the kernel's is: 12 bytes, the
//! data straight after the events.

use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};

use crate::errno;
use crate::syscall::{self, nr};
use crate::time::{KERNEL_SIGSET_SIZE, Timespec};

/// C's `struct epoll_event`: packed on x86-64, as the kernel's is there and
/// nowhere else.
#[cfg_attr(target_arch = "x86_64", repr(C, packed))]
#[cfg_attr(not(target_arch = "x86_64"), repr(C))]
#[derive(Debug, Clone, Copy, Default)]
pub struct EpollEvent {
    /// The events asked for, or those that happened.
    pub events: u32,
    /// What the caller registered with the descriptor, handed back with it.
    pub data: u64,
}

#[cfg(target_arch = "x86_64")]
const _: () = assert!(size_of::<EpollEvent>() == 12);
#[cfg(target_arch = "x86_64")]
const _: () = assert!(offset_of!(EpollEvent, data) == 4);
#[cfg(not(target_arch = "x86_64"))]
const _: () = assert!(size_of::<EpollEvent>() == 16);
#[cfg(not(target_arch = "x86_64"))]
const _: () = assert!(offset_of!(EpollEvent, data) == 8);

/// Creates a set, returning its descriptor. `flags` may hold
/// `EPOLL_CLOEXEC`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn epoll_create1(flags: c_int) -> c_int {
    // SAFETY: `epoll_create1` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::EPOLL_CREATE1, flags as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Creates a set. The size is a hint the kernel no longer uses, but it must
/// be positive, or this fails with `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn epoll_create(size: c_int) -> c_int {
    if size <= 0 {
        errno::set(errno::EINVAL);
        return -1;
    }
    epoll_create1(0)
}

/// Adds `fd` to the set `epfd`, changes what it waits for, or removes it,
/// as `op` says.
///
/// # Safety
///
/// `event` must be valid for a read of a `struct epoll_event`, or null for
/// `EPOLL_CTL_DEL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn epoll_ctl(
    epfd: c_int,
    op: c_int,
    fd: c_int,
    event: *mut EpollEvent,
) -> c_int {
    // SAFETY: the kernel reads at most one event, which the caller vouches
    // for.
    let ret = unsafe {
        syscall::syscall4(
            nr::EPOLL_CTL,
            epfd as usize,
            op as usize,
            fd as usize,
            event.addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Waits up to `timeout` milliseconds, or forever if it is negative, for
/// events in the set `epfd`, storing up to `count` of them at `events`, with
/// the signal mask replaced by `*mask` meanwhile if it is not null. Returns
/// how many were stored.
///
/// # Safety
///
/// `events` must be valid for writes of `count` `struct epoll_event`, and
/// `mask` null or valid for a read of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn epoll_pwait(
    epfd: c_int,
    events: *mut EpollEvent,
    count: c_int,
    timeout: c_int,
    mask: *const c_void,
) -> c_int {
    // SAFETY: the kernel writes at most `count` events and reads the mask,
    // which the caller vouches for.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::EPOLL_PWAIT,
            epfd as usize,
            events.addr(),
            count as usize,
            timeout as usize,
            mask.addr(),
            KERNEL_SIGSET_SIZE,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// [`epoll_pwait`] with no signal mask.
///
/// # Safety
///
/// `events` must be valid for writes of `count` `struct epoll_event`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn epoll_wait(
    epfd: c_int,
    events: *mut EpollEvent,
    count: c_int,
    timeout: c_int,
) -> c_int {
    // SAFETY: the caller vouches for `events`, and there is no mask.
    unsafe { epoll_pwait(epfd, events, count, timeout, core::ptr::null()) }
}

/// [`epoll_pwait`] with a `struct timespec` timeout, or none if it is null.
///
/// Linux has the call since 5.11. On a kernel without it, and under qemu-user,
/// which lacks it too, the wait is made with `epoll_pwait` and the timeout
/// rounded up to whole milliseconds, so that it is never cut short.
///
/// # Safety
///
/// As [`epoll_pwait`], and `timeout` must be null or valid for a read of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn epoll_pwait2(
    epfd: c_int,
    events: *mut EpollEvent,
    count: c_int,
    timeout: *const Timespec,
    mask: *const c_void,
) -> c_int {
    // SAFETY: the kernel writes at most `count` events and reads the timeout
    // and the mask, which the caller vouches for.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::EPOLL_PWAIT2,
            epfd as usize,
            events.addr(),
            count as usize,
            timeout.addr(),
            mask.addr(),
            KERNEL_SIGSET_SIZE,
        )
    };
    if ret != -(errno::ENOSYS as isize) {
        return errno::from_syscall(ret) as c_int;
    }
    let millis = if timeout.is_null() {
        -1
    } else {
        // SAFETY: the caller vouches for a non-null timeout.
        let timeout = unsafe { timeout.read() };
        if timeout.tv_sec < 0 || !(0..1_000_000_000).contains(&i64::from(timeout.tv_nsec)) {
            errno::set(errno::EINVAL);
            return -1;
        }
        let millis = timeout
            .tv_sec
            .saturating_mul(1000)
            .saturating_add((i64::from(timeout.tv_nsec) + 999_999) / 1_000_000);
        c_int::try_from(millis).unwrap_or(c_int::MAX)
    };
    // SAFETY: as above.
    unsafe { epoll_pwait(epfd, events, count, millis, mask) }
}
