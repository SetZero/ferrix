//! `sched.h`: yielding the processor, the affinity calls, and the namespace
//! calls `setns` and `unshare`.
//!
//! Each is its system call, as in musl. The kernel writes only as much of an
//! affinity mask as it has processors for, so `sched_getaffinity` zeroes the
//! rest of the caller's set, as musl's does. `__sched_cpucount` is what C's
//! `CPU_COUNT` macro calls.

use core::ffi::{c_int, c_void};

use crate::errno;
use crate::syscall::{self, nr};

/// Gives up the processor to another runnable thread, if there is one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sched_yield() -> c_int {
    // SAFETY: `sched_yield` reads no memory.
    let ret = unsafe { syscall::syscall0(nr::SCHED_YIELD) };
    errno::from_syscall(ret) as c_int
}

/// Stores the set of processors thread `pid`, or the caller if it is zero, may
/// run on in the `size` bytes at `set`, with the bytes past the kernel's mask
/// zeroed.
///
/// # Safety
///
/// `set` must be valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_getaffinity(pid: c_int, size: usize, set: *mut c_void) -> c_int {
    // SAFETY: the kernel writes at most `size` bytes to `set`, which the
    // caller vouches for.
    let ret = unsafe { syscall::syscall3(nr::SCHED_GETAFFINITY, pid as usize, size, set.addr()) };
    let written = match errno::decode(ret) {
        Ok(written) => written,
        Err(error) => {
            errno::set(error);
            return -1;
        }
    };
    for offset in written..size {
        // SAFETY: the offset is below `size`, which the caller vouches for.
        unsafe { set.cast::<u8>().wrapping_add(offset).write(0) };
    }
    0
}

/// Lets thread `pid`, or the caller if it is zero, run only on the processors
/// in the `size` bytes at `set`.
///
/// # Safety
///
/// `set` must be valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sched_setaffinity(pid: c_int, size: usize, set: *const c_void) -> c_int {
    // SAFETY: the kernel reads at most `size` bytes from `set`.
    let ret = unsafe { syscall::syscall3(nr::SCHED_SETAFFINITY, pid as usize, size, set.addr()) };
    errno::from_syscall(ret) as c_int
}

/// The number of processors in the `size` bytes of the set at `set`.
///
/// # Safety
///
/// `set` must be valid for reads of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __sched_cpucount(size: usize, set: *const c_void) -> c_int {
    let mut count: u32 = 0;
    for offset in 0..size {
        // SAFETY: the offset is below `size`, which the caller vouches for.
        let byte = unsafe { set.cast::<u8>().wrapping_add(offset).read() };
        count += byte.count_ones();
    }
    c_int::try_from(count).unwrap_or(c_int::MAX)
}

/// Moves the calling thread into the namespace open as `fd`, which must be of
/// type `nstype` unless that is zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setns(fd: c_int, nstype: c_int) -> c_int {
    // SAFETY: `setns` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::SETNS, fd as usize, nstype as usize) };
    errno::from_syscall(ret) as c_int
}

/// Gives the calling process its own copy of the namespaces and other state
/// the `CLONE_*` bits in `flags` name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn unshare(flags: c_int) -> c_int {
    // SAFETY: `unshare` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::UNSHARE, flags as usize, 0) };
    errno::from_syscall(ret) as c_int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_affinity_mask_is_read_zero_filled_and_counted() {
        let mut set = [0xffu8; 128];
        // SAFETY: the set is a live local of the size given.
        let ret = unsafe { sched_getaffinity(0, set.len(), set.as_mut_ptr().cast()) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let count = unsafe { __sched_cpucount(set.len(), set.as_ptr().cast()) };
        assert!(count > 0);
        // SAFETY: as above.
        let ret = unsafe { sched_setaffinity(0, set.len(), set.as_ptr().cast()) };
        assert_eq!(ret, 0);
        let bits = [0b1011_0000u8, 0, 1];
        // SAFETY: as above.
        let count = unsafe { __sched_cpucount(bits.len(), bits.as_ptr().cast()) };
        assert_eq!(count, 4);
        assert_eq!(sched_yield(), 0);
        assert_eq!(unshare(0), 0);
        assert_eq!(setns(-1, 0), -1);
    }
}
