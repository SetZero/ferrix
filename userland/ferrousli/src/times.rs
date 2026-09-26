//! The process's CPU time: `sys/times.h`'s `times`, and `time.h`'s `clock`.
//!
//! The wall clocks, `time`, `clock_gettime` and the rest, are in
//! [`crate::time`]. Both functions here are one system call each, as in musl.

use core::ffi::c_long;
use core::mem::size_of;

use crate::errno;
use crate::syscall::{self, nr};
use crate::time::Timespec;

/// `CLOCK_PROCESS_CPUTIME_ID`, from `linux/time.h`.
const CLOCK_PROCESS_CPUTIME_ID: usize = 2;
/// `CLOCKS_PER_SEC`, from `time.h`: `clock` counts microseconds.
const CLOCKS_PER_SEC: i64 = 1_000_000;

/// C's `struct tms`, from `sys/times.h`, which is the kernel's from
/// `linux/times.h`: four `clock_t`s, each a `long`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Tms {
    /// User CPU time of the process.
    pub tms_utime: c_long,
    /// System CPU time of the process.
    pub tms_stime: c_long,
    /// User CPU time of its waited-for children.
    pub tms_cutime: c_long,
    /// System CPU time of its waited-for children.
    pub tms_cstime: c_long,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Tms>() == 32);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Tms>() == 16);

/// Fills `*buf` with the CPU time of the process and its children, in clock
/// ticks, and returns the ticks since an arbitrary point in the past. Returns
/// -1 with `errno` set on failure.
///
/// # Safety
///
/// `buf` must be null or valid for writing a `struct tms`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn times(buf: *mut Tms) -> c_long {
    // SAFETY: the caller passes null, which Linux accepts, or a writable
    // `struct tms`.
    let ret = unsafe { syscall::syscall2(nr::TIMES, buf.addr(), 0) };
    errno::from_syscall(ret) as c_long
}

/// The CPU time the process has used, in `CLOCKS_PER_SEC`ths of a second, or
/// -1 if it is not available or does not fit a `clock_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn clock() -> c_long {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live `struct timespec` the kernel writes.
    let ret = unsafe {
        syscall::syscall2(
            nr::CLOCK_GETTIME,
            CLOCK_PROCESS_CPUTIME_ID,
            (&raw mut ts).addr(),
        )
    };
    if errno::from_syscall(ret) < 0 {
        return -1;
    }
    ts.tv_sec
        .checked_mul(i64::from(CLOCKS_PER_SEC))
        .and_then(|micros| micros.checked_add(i64::from(ts.tv_nsec / 1000)))
        // A `clock_t` is a 32-bit `long` on ARMv7-A, which a process
        // outgrows after 35 minutes of CPU time, as POSIX allows.
        .and_then(|micros| c_long::try_from(micros).ok())
        .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_moves_forward() {
        let before = clock();
        let mut spin = 0u64;
        for i in 0..20_000_000u64 {
            spin = spin.wrapping_add(i).rotate_left(1);
        }
        assert_ne!(spin, 0);
        assert!(before >= 0);
        assert!(clock() > before);
        let mut buf = Tms {
            tms_utime: -1,
            tms_stime: -1,
            tms_cutime: -1,
            tms_cstime: -1,
        };
        // SAFETY: `buf` is a live `struct tms`.
        assert!(unsafe { times(&raw mut buf) } > 0);
        assert!(buf.tms_utime >= 0 && buf.tms_cutime >= 0);
    }
}
