//! `time.h` and `sys/time.h`: reading the clocks and sleeping, and the
//! `unistd.h` calls built on them, `sleep`, `usleep`, `pause` and `alarm`.
//!
//! Every clock is read with a system call. There is no vDSO use yet, so
//! `clock_gettime` costs a trip into the kernel. `strftime`, `localtime`,
//! `mktime` and time zones are not here.

use core::ffi::{c_int, c_long, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null;

use crate::errno;
use crate::syscall::{self, nr};

/// C's `struct timespec`: seconds and nanoseconds.
///
/// musl's header pads it with zero-width bit-fields, which on a 64-bit
/// little-endian target are empty. It is then the kernel's `struct
/// __kernel_timespec` from `linux/time_types.h`, two 64-bit integers.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timespec {
    /// Whole seconds, C's `time_t`.
    pub tv_sec: i64,
    /// Nanoseconds, `0..1_000_000_000`.
    pub tv_nsec: c_long,
}

const _: () = assert!(size_of::<Timespec>() == 16);
const _: () = assert!(offset_of!(Timespec, tv_nsec) == 8);

/// C's `struct timeval`: seconds and microseconds. The kernel's `struct
/// __kernel_old_timeval` has the same two longs.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeval {
    /// Whole seconds, C's `time_t`.
    pub tv_sec: i64,
    /// Microseconds, C's `suseconds_t`.
    pub tv_usec: c_long,
}

const _: () = assert!(size_of::<Timeval>() == 16);
const _: () = assert!(offset_of!(Timeval, tv_usec) == 8);

/// C's `struct itimerval`, which is the kernel's `struct
/// __kernel_old_itimerval`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Itimerval {
    /// The period after the first expiry, or zero for a single expiry.
    pub interval: Timeval,
    /// The time left to the next expiry, or zero if the timer is off.
    pub value: Timeval,
}

const _: () = assert!(size_of::<Itimerval>() == 32);

/// `CLOCK_REALTIME`, from `linux/time.h`.
pub const CLOCK_REALTIME: c_int = 0;
/// `CLOCK_MONOTONIC`, from `linux/time.h`.
pub const CLOCK_MONOTONIC: c_int = 1;
/// `CLOCK_THREAD_CPUTIME_ID`, from `linux/time.h`.
pub const CLOCK_THREAD_CPUTIME_ID: c_int = 3;
/// `ITIMER_REAL`, from `linux/time.h`.
const ITIMER_REAL: usize = 0;
/// The size of the kernel's signal set, `_NSIG / 8` with `_NSIG` from
/// `asm-generic/signal.h`.
pub const KERNEL_SIGSET_SIZE: usize = 8;

/// Nanoseconds in a second.
const NANOS: c_long = 1_000_000_000;

/// Reads clock `clock` into `*ts`.
///
/// # Safety
///
/// `ts` must be valid for a write of a `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clock_gettime(clock: c_int, ts: *mut Timespec) -> c_int {
    // SAFETY: the caller vouches for `ts`, the one thing the kernel writes.
    let ret = unsafe { syscall::syscall2(nr::CLOCK_GETTIME, clock as usize, ts.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Reads the resolution of clock `clock` into `*ts`, if `ts` is not null.
///
/// # Safety
///
/// `ts` must be null or valid for a write of a `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clock_getres(clock: c_int, ts: *mut Timespec) -> c_int {
    // SAFETY: the caller vouches for `ts`, and the kernel skips a null one.
    let ret = unsafe { syscall::syscall2(nr::CLOCK_GETRES, clock as usize, ts.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Sets clock `clock` to `*ts`.
///
/// # Safety
///
/// `ts` must be valid for a read of a `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clock_settime(clock: c_int, ts: *const Timespec) -> c_int {
    // SAFETY: the kernel only reads `ts`, which the caller vouches for.
    let ret = unsafe { syscall::syscall2(nr::CLOCK_SETTIME, clock as usize, ts.addr()) };
    errno::from_syscall(ret) as c_int
}

/// The seconds since the Epoch, also stored in `*t` if `t` is not null.
///
/// # Safety
///
/// `t` must be null or valid for a write of a `time_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn time(t: *mut i64) -> i64 {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live local. Reading `CLOCK_REALTIME` cannot fail with
    // a valid pointer, so the result is not checked, as in musl.
    let _ = unsafe { clock_gettime(CLOCK_REALTIME, &raw mut ts) };
    if !t.is_null() {
        // SAFETY: the caller vouches for a non-null `t`.
        unsafe { t.write(ts.tv_sec) };
    }
    ts.tv_sec
}

/// The microsecond form of `ts`, truncated.
const fn timeval_of(ts: Timespec) -> Timeval {
    Timeval {
        tv_sec: ts.tv_sec,
        tv_usec: ts.tv_nsec / 1000,
    }
}

/// Reads the real-time clock into `*tv`, if `tv` is not null.
///
/// The time zone argument is obsolete and ignored, as in musl: it is not
/// written.
///
/// # Safety
///
/// `tv` must be null or valid for a write of a `struct timeval`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gettimeofday(tv: *mut Timeval, _tz: *mut c_void) -> c_int {
    if tv.is_null() {
        return 0;
    }
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live local.
    let _ = unsafe { clock_gettime(CLOCK_REALTIME, &raw mut ts) };
    // SAFETY: the caller vouches for a non-null `tv`.
    unsafe { tv.write(timeval_of(ts)) };
    0
}

/// Sleeps on clock `clock` for `*req`, or until `*req` with `TIMER_ABSTIME`
/// in `flags`. If a signal interrupts a relative sleep and `rem` is not null,
/// the time left is stored there.
///
/// Unlike most calls, this returns the error number, and leaves `errno`
/// alone.
///
/// # Safety
///
/// `req` must be valid for a read, and `rem` null or valid for a write, of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clock_nanosleep(
    clock: c_int,
    flags: c_int,
    req: *const Timespec,
    rem: *mut Timespec,
) -> c_int {
    // The kernel accepts a per-thread CPU clock here and would sleep for as
    // long as the thread's CPU time does not advance, which is forever.
    // POSIX makes it an error, and musl refuses it the same way.
    if clock == CLOCK_THREAD_CPUTIME_ID {
        return errno::EINVAL;
    }
    // SAFETY: the kernel reads `req` and writes `rem` only when it is not
    // null; the caller vouches for both.
    let ret = unsafe {
        syscall::syscall4(
            nr::CLOCK_NANOSLEEP,
            clock as usize,
            flags as usize,
            req.addr(),
            rem.addr(),
        )
    };
    match errno::decode(ret) {
        Ok(_) => 0,
        Err(error) => error,
    }
}

/// Sleeps for `*req` on the real-time clock. If a signal interrupts it and
/// `rem` is not null, the time left is stored there.
///
/// # Safety
///
/// As [`clock_nanosleep`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> c_int {
    // SAFETY: the caller's contract is `clock_nanosleep`'s.
    match unsafe { clock_nanosleep(CLOCK_REALTIME, 0, req, rem) } {
        0 => 0,
        error => {
            errno::set(error);
            -1
        }
    }
}

/// Sleeps for `seconds`, and returns the whole seconds left if a signal cut
/// the sleep short.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sleep(seconds: c_uint) -> c_uint {
    let mut ts = Timespec {
        tv_sec: i64::from(seconds),
        tv_nsec: 0,
    };
    // SAFETY: the request and the remainder are the same live local, which
    // the kernel reads before it writes.
    if unsafe { nanosleep(&raw const ts, &raw mut ts) } == 0 {
        return 0;
    }
    c_uint::try_from(ts.tv_sec).unwrap_or(seconds)
}

/// Sleeps for `useconds` microseconds.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn usleep(useconds: c_uint) -> c_int {
    let ts = Timespec {
        tv_sec: i64::from(useconds / 1_000_000),
        tv_nsec: c_long::from(useconds % 1_000_000) * 1000,
    };
    // SAFETY: `ts` is a live local, and no remainder is asked for.
    unsafe { nanosleep(&raw const ts, core::ptr::null_mut()) }
}

/// Waits until a signal handler runs, and fails with `EINTR`.
///
/// It is `ppoll` with nothing to wait for and no timeout, which is how musl
/// writes it where the kernel has no `pause`, as on AArch64.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pause() -> c_int {
    // SAFETY: with no descriptors, no timeout and no mask, the kernel reads
    // and writes nothing.
    let ret = unsafe { syscall::syscall6(nr::PPOLL, 0, 0, 0, 0, KERNEL_SIGSET_SIZE, 0) };
    errno::from_syscall(ret) as c_int
}

/// The whole seconds an alarm had left, counting a part second as one, so
/// that an alarm about to fire does not read as none.
const fn alarm_seconds(old: Timeval) -> c_uint {
    let seconds = old.tv_sec as c_uint;
    if old.tv_usec != 0 {
        seconds.wrapping_add(1)
    } else {
        seconds
    }
}

/// Delivers `SIGALRM` in `seconds`, or cancels the alarm if it is zero, and
/// returns the seconds left on the previous alarm.
///
/// It is `setitimer`, as in musl, which every architecture has.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn alarm(seconds: c_uint) -> c_uint {
    let new = Itimerval {
        interval: Timeval::default(),
        value: Timeval {
            tv_sec: i64::from(seconds),
            tv_usec: 0,
        },
    };
    let mut old = Itimerval::default();
    // SAFETY: the kernel reads `new` and writes `old`, both live locals.
    let _ = unsafe {
        syscall::syscall3(
            nr::SETITIMER,
            ITIMER_REAL,
            (&raw const new).addr(),
            (&raw mut old).addr(),
        )
    };
    alarm_seconds(old.value)
}

/// A timeout as the kernel's `ppoll` and `pselect6` take it, which they may
/// write the time left back into: a copy of `*ts`, or `None` for null.
///
/// # Safety
///
/// `ts` must be null or valid for a read of a `struct timespec`.
pub unsafe fn copy_timeout(ts: *const Timespec) -> Option<Timespec> {
    if ts.is_null() {
        None
    } else {
        // SAFETY: the caller vouches for a non-null `ts`.
        Some(unsafe { ts.read() })
    }
}

/// The address to pass for an optional timeout: the copy's, or null.
pub fn timeout_address(copy: &mut Option<Timespec>) -> usize {
    match copy {
        Some(ts) => (&raw mut *ts).addr(),
        None => null::<Timespec>().addr(),
    }
}

/// Splits `value` whole units of `per_second` into seconds and the rest,
/// scaling the rest by `scale`. For turning milliseconds or microseconds into
/// a `struct timespec`.
pub const fn split(value: i64, per_second: i64, scale: c_long) -> Timespec {
    Timespec {
        tv_sec: value / per_second,
        tv_nsec: (value % per_second) * scale,
    }
}

/// Whether `ts` is a valid relative or absolute time.
pub const fn is_valid(ts: &Timespec) -> bool {
    ts.tv_sec >= 0 && ts.tv_nsec >= 0 && ts.tv_nsec < NANOS
}

/// Sets interval timer `which` to `*new`, storing its old setting in `*old`
/// if that is not null.
///
/// # Safety
///
/// `new` must be valid for a read of a `struct itimerval`, and `old` null or
/// valid for a write of one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setitimer(
    which: c_int,
    new: *const Itimerval,
    old: *mut Itimerval,
) -> c_int {
    // SAFETY: the kernel reads `new` and writes `old`, which the caller vouches
    // for.
    let ret = unsafe { syscall::syscall3(nr::SETITIMER, which as usize, new.addr(), old.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Stores interval timer `which`'s setting in `*value`.
///
/// # Safety
///
/// `value` must be valid for a write of a `struct itimerval`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getitimer(which: c_int, value: *mut Itimerval) -> c_int {
    // SAFETY: the kernel writes `value`, which the caller vouches for.
    let ret = unsafe { syscall::syscall2(nr::GETITIMER, which as usize, value.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Sets the real-time clock to `*tv`. As in musl, the time zone is ignored, a
/// null `tv` sets nothing and succeeds, and microseconds outside `0..1000000`
/// fail with `EINVAL` before the kernel is asked.
///
/// # Safety
///
/// `tv` must be null or valid for a read of a `struct timeval`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn settimeofday(tv: *const Timeval, _tz: *const c_void) -> c_int {
    if tv.is_null() {
        return 0;
    }
    // SAFETY: the caller passes a valid `struct timeval`.
    let tv = unsafe { tv.read() };
    if !(0..1_000_000).contains(&tv.tv_usec) {
        errno::set(errno::EINVAL);
        return -1;
    }
    let ts = Timespec {
        tv_sec: tv.tv_sec,
        tv_nsec: tv.tv_usec * 1000,
    };
    // SAFETY: `ts` is a live local.
    unsafe { clock_settime(CLOCK_REALTIME, &raw const ts) }
}

/// Reads clock `clock`'s discipline into `*tx`, adjusting it first as the
/// modes in `*tx` ask, and returns the clock's state.
///
/// # Safety
///
/// `tx` must be valid for a read and a write of a `struct timex`, whose layout
/// on x86-64 is the kernel's.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clock_adjtime(clock: c_int, tx: *mut c_void) -> c_int {
    // SAFETY: the kernel reads and writes `*tx`, which the caller vouches for.
    let ret = unsafe { syscall::syscall2(nr::CLOCK_ADJTIME, clock as usize, tx.addr()) };
    errno::from_syscall(ret) as c_int
}

/// `clock_adjtime` on the real-time clock.
///
/// # Safety
///
/// As `clock_adjtime`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn adjtimex(tx: *mut c_void) -> c_int {
    // SAFETY: the caller's promises are `clock_adjtime`'s.
    unsafe { clock_adjtime(CLOCK_REALTIME, tx) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeval_truncates_nanoseconds_to_microseconds() {
        let tv = timeval_of(Timespec {
            tv_sec: 7,
            tv_nsec: 999_999_999,
        });
        assert_eq!(
            tv,
            Timeval {
                tv_sec: 7,
                tv_usec: 999_999
            }
        );
    }

    #[test]
    fn an_alarm_with_a_part_second_left_counts_it() {
        assert_eq!(
            alarm_seconds(Timeval {
                tv_sec: 0,
                tv_usec: 0
            }),
            0
        );
        assert_eq!(
            alarm_seconds(Timeval {
                tv_sec: 0,
                tv_usec: 1
            }),
            1
        );
        assert_eq!(
            alarm_seconds(Timeval {
                tv_sec: 99,
                tv_usec: 500_000
            }),
            100
        );
    }

    #[test]
    fn milliseconds_split_into_a_timespec() {
        assert_eq!(
            split(2500, 1000, 1_000_000),
            Timespec {
                tv_sec: 2,
                tv_nsec: 500_000_000
            }
        );
        assert!(is_valid(&split(2500, 1000, 1_000_000)));
        assert!(!is_valid(&Timespec {
            tv_sec: 0,
            tv_nsec: NANOS
        }));
        assert!(!is_valid(&Timespec {
            tv_sec: -1,
            tv_nsec: 0
        }));
    }

    #[test]
    fn the_clocks_read_and_a_thread_cpu_clock_sleep_is_refused() {
        let mut ts = Timespec::default();
        // SAFETY: `ts` is a live local.
        assert_eq!(unsafe { clock_gettime(CLOCK_MONOTONIC, &raw mut ts) }, 0);
        assert!(is_valid(&ts));
        // SAFETY: the pointer is null, which `time` allows.
        assert!(unsafe { time(core::ptr::null_mut()) } > 1_600_000_000);
        // SAFETY: `ts` is a live local.
        let refused = unsafe {
            clock_nanosleep(
                CLOCK_THREAD_CPUTIME_ID,
                0,
                &raw const ts,
                core::ptr::null_mut(),
            )
        };
        assert_eq!(refused, errno::EINVAL);
    }
}
