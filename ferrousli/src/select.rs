//! `sys/select.h`: waiting for descriptors in bit sets to become ready.
//!
//! Both calls are the kernel's `pselect6`, which every architecture has;
//! AArch64 has no `select`. Linux's `select` writes the time left back into
//! the caller's `struct timeval`, and glibc's does too, so this `select`
//! converts it back. musl's does not.

use core::ffi::{c_int, c_long, c_ulong, c_void};
use core::mem::size_of;

use crate::errno;
use crate::syscall::nr;
use crate::time::{self, KERNEL_SIGSET_SIZE, Timespec, Timeval};

/// C's `fd_set`: `FD_SETSIZE`, 1024, bits in unsigned longs. The kernel
/// reads as many bits as the first argument says.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FdSet {
    /// The bits, lowest descriptor first.
    pub fds_bits: [c_ulong; 16],
}

const _: () = assert!(size_of::<FdSet>() == 128);

/// The signal mask argument the kernel's `pselect6` takes: a pointer to the
/// mask and the mask's size. From `fs/select.c`.
#[repr(C)]
#[derive(Debug)]
struct SigsetArgument {
    mask: *const c_void,
    size: usize,
}

const _: () = assert!(size_of::<SigsetArgument>() == 16);

/// Microseconds in a second.
const MICROS: c_long = 1_000_000;

/// A `struct timeval` timeout as a `struct timespec`, with microseconds of a
/// second or more carried into seconds, saturating at the largest time.
/// `None` if either part is negative.
///
/// Adapted from musl's `select` (MIT).
fn timespec_of(tv: Timeval) -> Option<Timespec> {
    if tv.tv_sec < 0 || tv.tv_usec < 0 {
        return None;
    }
    let carry = tv.tv_usec / MICROS;
    Some(match tv.tv_sec.checked_add(carry) {
        Some(tv_sec) => Timespec {
            tv_sec,
            tv_nsec: (tv.tv_usec % MICROS) * 1000,
        },
        None => Timespec {
            tv_sec: i64::MAX,
            tv_nsec: 999_999_999,
        },
    })
}

/// Calls the kernel's `pselect6`.
///
/// # Safety
///
/// As [`pselect`], for the sets and `mask`.
unsafe fn raw_pselect(
    count: c_int,
    read: *mut FdSet,
    write: *mut FdSet,
    except: *mut FdSet,
    timeout: &mut Option<Timespec>,
    mask: *const c_void,
) -> c_int {
    let sigset = SigsetArgument {
        mask,
        size: KERNEL_SIGSET_SIZE,
    };
    // SAFETY: the caller vouches for the sets and the mask. The timeout is a
    // live copy or null, and `sigset` a live local the kernel only reads.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::PSELECT6,
            count as usize,
            read.addr(),
            write.addr(),
            except.addr(),
            time::timeout_address(timeout),
            (&raw const sigset).addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Waits up to `*timeout`, or forever if it is null, until a descriptor below
/// `count` in one of the sets is ready, and leaves only the ready ones set.
/// The time left is written back into `*timeout`.
///
/// # Safety
///
/// Each set must be null or valid for reads and writes of an `fd_set`, and
/// `timeout` null or valid for reads and writes of a `struct timeval`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn select(
    count: c_int,
    read: *mut FdSet,
    write: *mut FdSet,
    except: *mut FdSet,
    timeout: *mut Timeval,
) -> c_int {
    let mut copy = None;
    if !timeout.is_null() {
        // SAFETY: the caller vouches for a non-null `timeout`.
        let tv = unsafe { timeout.read() };
        let Some(ts) = timespec_of(tv) else {
            errno::set(errno::EINVAL);
            return -1;
        };
        copy = Some(ts);
    }
    // SAFETY: the caller vouches for the sets, and there is no mask.
    let ret = unsafe { raw_pselect(count, read, write, except, &mut copy, core::ptr::null()) };
    if let Some(left) = copy {
        let tv = Timeval {
            tv_sec: left.tv_sec,
            tv_usec: left.tv_nsec / 1000,
        };
        // SAFETY: `copy` is only `Some` when `timeout` was not null.
        unsafe { timeout.write(tv) };
    }
    ret
}

/// [`select`] with a `struct timespec` timeout that is not written back, and
/// with the signal mask replaced by `*mask` while waiting, if it is not null.
///
/// # Safety
///
/// Each set must be null or valid for reads and writes of an `fd_set`,
/// `timeout` null or valid for a read of a `struct timespec`, and `mask` null
/// or valid for a read of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pselect(
    count: c_int,
    read: *mut FdSet,
    write: *mut FdSet,
    except: *mut FdSet,
    timeout: *const Timespec,
    mask: *const c_void,
) -> c_int {
    // SAFETY: the caller vouches for `timeout`.
    let mut copy = unsafe { time::copy_timeout(timeout) };
    // SAFETY: the caller vouches for the sets and the mask.
    unsafe { raw_pselect(count, read, write, except, &mut copy, mask) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microseconds_carry_into_seconds() {
        assert_eq!(
            timespec_of(Timeval {
                tv_sec: 1,
                tv_usec: 2_500_000
            }),
            Some(Timespec {
                tv_sec: 3,
                tv_nsec: 500_000_000
            })
        );
    }

    #[test]
    fn a_negative_timeout_is_refused_and_a_huge_one_saturates() {
        assert_eq!(
            timespec_of(Timeval {
                tv_sec: -1,
                tv_usec: 0
            }),
            None
        );
        assert_eq!(
            timespec_of(Timeval {
                tv_sec: 0,
                tv_usec: -1
            }),
            None
        );
        assert_eq!(
            timespec_of(Timeval {
                tv_sec: i64::MAX,
                tv_usec: 1_000_000
            }),
            Some(Timespec {
                tv_sec: i64::MAX,
                tv_nsec: 999_999_999
            })
        );
    }
}
