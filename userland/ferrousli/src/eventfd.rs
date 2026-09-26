//! `sys/eventfd.h`: a descriptor holding a 64-bit counter, which one thread
//! adds to by writing and another waits on by reading or polling.
//!
//! As musl 1.2.5's `linux/eventfd.c` (MIT; see [`crate::math`] for the
//! notice): `eventfd` is the kernel's `eventfd2`, which every architecture has
//! and which takes the flags; `eventfd_read` and `eventfd_write` are a read and
//! a write of the eight bytes, and cancellation points as those are.

use core::ffi::{c_int, c_uint};
use core::mem::size_of;

use crate::errno;
use crate::syscall::{self, nr};
use crate::unistd::{read, write};

/// C's `eventfd_t`.
pub type EventfdT = u64;

/// Creates a counter starting at `initial`, returning its descriptor. `flags`
/// may hold `EFD_CLOEXEC`, `EFD_NONBLOCK` and `EFD_SEMAPHORE`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn eventfd(initial: c_uint, flags: c_int) -> c_int {
    // SAFETY: `eventfd2` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::EVENTFD2, initial as usize, flags as usize) };
    errno::from_syscall(ret) as c_int
}

/// Reads the counter into `*value`, which resets it, or takes one from it for
/// `EFD_SEMAPHORE`. Returns 0, or -1 with `errno` set.
///
/// # Safety
///
/// `value` must be valid for a write of an `eventfd_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn eventfd_read(fd: c_int, value: *mut EventfdT) -> c_int {
    // SAFETY: the caller vouches for eight writable bytes at `value`.
    let got = unsafe { read(fd, value.cast(), size_of::<EventfdT>()) };
    if got == size_of::<EventfdT>() as isize {
        0
    } else {
        -1
    }
}

/// Adds `value` to the counter. Returns 0, or -1 with `errno` set.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn eventfd_write(fd: c_int, value: EventfdT) -> c_int {
    // SAFETY: the eight bytes are the live parameter.
    let put = unsafe { write(fd, (&raw const value).cast(), size_of::<EventfdT>()) };
    if put == size_of::<EventfdT>() as isize {
        0
    } else {
        -1
    }
}
