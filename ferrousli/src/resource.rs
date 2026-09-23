//! `sys/resource.h`: resource limits, usage and scheduling priority.

use core::ffi::{c_int, c_long, c_uint};
use core::mem::{offset_of, size_of};
use core::ptr::null;

use crate::errno;
use crate::syscall::{self, nr};
use crate::time::Timeval;

/// C's `struct rlimit`. `rlim_t` is 64 bits, as in the kernel's `struct
/// rlimit64` from `linux/resource.h`, which `prlimit64` reads and writes, and
/// `RLIM_INFINITY` is all ones in both.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rlimit {
    /// The soft limit.
    pub rlim_cur: u64,
    /// The hard limit.
    pub rlim_max: u64,
}

const _: () = assert!(size_of::<Rlimit>() == 16);

/// C's `struct rusage`.
///
/// Its first 144 bytes are the kernel's `struct rusage` from
/// `linux/resource.h`: two `struct __kernel_old_timeval` and fourteen longs.
/// musl adds sixteen reserved longs, which the kernel never writes. glibc's
/// has no reserved tail, and is the kernel's prefix.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Rusage {
    /// User CPU time.
    pub ru_utime: Timeval,
    /// System CPU time.
    pub ru_stime: Timeval,
    /// Linux's counters: `ru_maxrss` through `ru_nivcsw`.
    pub counters: [c_long; 14],
    /// Room for more, which musl reserves.
    pub __reserved: [c_long; 16],
}

const _: () = assert!(offset_of!(Rusage, counters) == 32);
// Where the kernel's structure ends.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Rusage, __reserved) == 144);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Rusage>() == 272);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Rusage>() == 152);

/// `RLIM_INFINITY`: no limit.
pub const RLIM_INFINITY: u64 = u64::MAX;
/// `RLIMIT_NPROC`, from `asm-generic/resource.h`.
pub const RLIMIT_NPROC: c_int = 6;
/// `RLIMIT_NOFILE`, from `asm-generic/resource.h`.
pub const RLIMIT_NOFILE: c_int = 7;

/// The kernel's nice value from `getpriority`'s result, which is `20 - nice`
/// so that it is never negative.
const NICE_BIAS: c_int = 20;

/// Replaces and reads the limit `resource` of process `pid`, or of the
/// calling process if it is zero. Either pointer may be null.
///
/// # Safety
///
/// `new` must be null or valid for a read, and `old` null or valid for a
/// write, of a `struct rlimit`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn prlimit(
    pid: c_int,
    resource: c_int,
    new: *const Rlimit,
    old: *mut Rlimit,
) -> c_int {
    // SAFETY: the kernel reads `new` and writes `old`, as the caller vouches.
    let ret = unsafe {
        syscall::syscall4(
            nr::PRLIMIT64,
            pid as usize,
            resource as usize,
            new.addr(),
            old.addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Reads the calling process's limit `resource` into `*limit`.
///
/// # Safety
///
/// `limit` must be valid for a write of a `struct rlimit`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getrlimit(resource: c_int, limit: *mut Rlimit) -> c_int {
    // SAFETY: the caller's contract is `prlimit`'s, with nothing to read.
    unsafe { prlimit(0, resource, null(), limit) }
}

/// Sets the calling process's limit `resource` to `*limit`.
///
/// # Safety
///
/// `limit` must be valid for a read of a `struct rlimit`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setrlimit(resource: c_int, limit: *const Rlimit) -> c_int {
    // SAFETY: the caller's contract is `prlimit`'s, with nothing to write.
    unsafe { prlimit(0, resource, limit, core::ptr::null_mut()) }
}

/// glibc's large-file name for [`getrlimit`]; `struct rlimit` is already
/// 64-bit here.
///
/// # Safety
///
/// As [`getrlimit`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getrlimit64(resource: c_int, limit: *mut Rlimit) -> c_int {
    // SAFETY: the caller's contract is `getrlimit`'s.
    unsafe { getrlimit(resource, limit) }
}

/// glibc's large-file name for [`setrlimit`].
///
/// # Safety
///
/// As [`setrlimit`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setrlimit64(resource: c_int, limit: *const Rlimit) -> c_int {
    // SAFETY: the caller's contract is `setrlimit`'s.
    unsafe { setrlimit(resource, limit) }
}

/// Where the kernel is to write a `struct rusage` meant for `usage`.
///
/// ARMv7-A's kernel writes its times as 32-bit `timeval`s, 16 bytes where
/// C's are 32, before the counters. So it is pointed 16 bytes in, where its
/// counters land on C's, and [`widen`] then moves the times, as musl does.
pub(crate) fn kernel_usage(usage: *mut Rusage) -> usize {
    #[cfg(target_arch = "arm")]
    if !usage.is_null() {
        return usage.addr() + 16;
    }
    usage.addr()
}

/// Turns the 32-bit times the kernel wrote at [`kernel_usage`]'s address into
/// C's `struct timeval`s, on ARMv7-A; nothing elsewhere.
///
/// # Safety
///
/// `usage` must be null or a `struct rusage` the kernel just wrote through
/// [`kernel_usage`].
pub(crate) unsafe fn widen(usage: *mut Rusage) {
    #[cfg(target_arch = "arm")]
    if !usage.is_null() {
        // SAFETY: the kernel wrote four `long`s at 16, inside the structure.
        let times = unsafe {
            usage
                .wrapping_byte_add(16)
                .cast::<[c_long; 4]>()
                .read_unaligned()
        };
        let [us, uu, ss, su] = times.map(i64::from);
        // SAFETY: the caller vouches for the structure.
        let usage = unsafe { &mut *usage };
        usage.ru_utime = Timeval {
            tv_sec: us,
            tv_usec: uu,
        };
        usage.ru_stime = Timeval {
            tv_sec: ss,
            tv_usec: su,
        };
    }
    let _ = usage;
}

/// Reads the resource usage of `who` into `*usage`.
///
/// # Safety
///
/// `usage` must be valid for a write of a `struct rusage`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getrusage(who: c_int, usage: *mut Rusage) -> c_int {
    // SAFETY: the kernel writes its prefix of `usage`, as the caller vouches.
    let ret = unsafe { syscall::syscall2(nr::GETRUSAGE, who as usize, kernel_usage(usage)) };
    if ret == 0 {
        // SAFETY: the kernel just wrote it.
        unsafe { widen(usage) };
    }
    errno::from_syscall(ret) as c_int
}

/// The nice value of `who`, of kind `which`.
///
/// A nice value can be -1, so a caller that must tell it from failure clears
/// `errno` first.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getpriority(which: c_int, who: c_uint) -> c_int {
    // SAFETY: `getpriority` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::GETPRIORITY, which as usize, who as usize) };
    match errno::decode(ret) {
        Ok(biased) => NICE_BIAS - biased as c_int,
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// Sets the nice value of `who`, of kind `which`, to `priority`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setpriority(which: c_int, who: c_uint, priority: c_int) -> c_int {
    // SAFETY: `setpriority` reads no memory.
    let ret = unsafe {
        syscall::syscall3(
            nr::SETPRIORITY,
            which as usize,
            who as usize,
            priority as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_file_limit_reads_back() {
        let mut limit = Rlimit::default();
        // SAFETY: `limit` is a live local.
        assert_eq!(unsafe { getrlimit(RLIMIT_NOFILE, &raw mut limit) }, 0);
        assert!(limit.rlim_cur <= limit.rlim_max);
        // SAFETY: as above.
        assert_eq!(unsafe { getrlimit(-1, &raw mut limit) }, -1);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::EINVAL);
    }

    #[test]
    fn the_nice_value_is_unbiased() {
        let nice = getpriority(0, 0);
        assert!((-20..=19).contains(&nice));
    }
}
