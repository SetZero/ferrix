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
    pub tv_usec: Suseconds,
}

/// C's `suseconds_t`: a `long`, but 64 bits on ARMv7-A, where `time_t` is
/// too, as musl and glibc's 64-bit-time ABI both make it.
#[cfg(not(target_arch = "arm"))]
pub type Suseconds = c_long;
/// C's `suseconds_t`: a `long`, but 64 bits on ARMv7-A, where `time_t` is
/// too, as musl and glibc's 64-bit-time ABI both make it.
#[cfg(target_arch = "arm")]
pub type Suseconds = i64;

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
        // Widening: `suseconds_t` is at least as wide as `long`.
        tv_usec: (ts.tv_nsec / 1000) as Suseconds,
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
        crate::cancel::syscall_cp(
            nr::CLOCK_NANOSLEEP,
            clock as usize,
            flags as usize,
            req.addr(),
            rem.addr(),
            0,
            0,
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
        // Below a second: it fits a `long`.
        tv_nsec: (useconds % 1_000_000 * 1000) as c_long,
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
    let ret = unsafe { crate::cancel::syscall_cp(nr::PPOLL, 0, 0, 0, 0, KERNEL_SIGSET_SIZE, 0) };
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
    // SAFETY: `new` and `old` are live locals.
    let _ = unsafe { kernel_setitimer(ITIMER_REAL as c_int, &raw const new, &raw mut old) };
    alarm_seconds(old.value)
}

/// The kernel's `struct __kernel_old_itimerval` on ARMv7-A: four 32-bit
/// `long`s, where C's `struct itimerval` has 64-bit seconds.
#[cfg(target_arch = "arm")]
type KernelItimerval = [c_long; 4];

/// `setitimer`'s system call, with C's structures: the kernel's value. On
/// ARMv7-A the kernel's structure is converted both ways, and a time whose
/// seconds do not fit 32 bits is `ENOTSUP`, as musl has it.
///
/// # Safety
///
/// `new` must be valid for a read of a `struct itimerval`, and `old` null or
/// valid for a write of one.
pub(crate) unsafe fn kernel_setitimer(
    which: c_int,
    new: *const Itimerval,
    old: *mut Itimerval,
) -> isize {
    #[cfg(target_arch = "arm")]
    {
        // SAFETY: the caller vouches for `new`.
        let value = unsafe { new.read() };
        let (Ok(is), Ok(vs)) = (
            c_long::try_from(value.interval.tv_sec),
            c_long::try_from(value.value.tv_sec),
        ) else {
            return -(errno::ENOTSUP as isize);
        };
        // Microseconds below a second fit a `long`.
        let narrow: KernelItimerval = [
            is,
            value.interval.tv_usec as c_long,
            vs,
            value.value.tv_usec as c_long,
        ];
        let mut before: KernelItimerval = [0; 4];
        // SAFETY: the kernel reads `narrow` and writes `before`, live locals.
        let ret = unsafe {
            syscall::syscall3(
                nr::SETITIMER,
                which as usize,
                (&raw const narrow).addr(),
                (&raw mut before).addr(),
            )
        };
        if ret == 0 && !old.is_null() {
            // SAFETY: the caller vouches for a non-null `old`.
            unsafe { old.write(itimerval_of(before)) };
        }
        ret
    }
    #[cfg(not(target_arch = "arm"))]
    // SAFETY: the kernel reads `new` and writes `old`, which the caller
    // vouches for.
    unsafe {
        syscall::syscall3(nr::SETITIMER, which as usize, new.addr(), old.addr())
    }
}

/// C's form of the kernel's 32-bit `itimerval`.
#[cfg(target_arch = "arm")]
fn itimerval_of(k: KernelItimerval) -> Itimerval {
    let [is, iu, vs, vu] = k.map(i64::from);
    Itimerval {
        interval: Timeval {
            tv_sec: is,
            tv_usec: iu,
        },
        value: Timeval {
            tv_sec: vs,
            tv_usec: vu,
        },
    }
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
        // Below a second: it fits a `long`.
        tv_nsec: ((value % per_second) * scale as i64) as c_long,
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
    // SAFETY: the caller's contract is `kernel_setitimer`'s.
    let ret = unsafe { kernel_setitimer(which, new, old) };
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
    #[cfg(not(target_arch = "arm"))]
    let ret = unsafe { syscall::syscall2(nr::GETITIMER, which as usize, value.addr()) };
    #[cfg(target_arch = "arm")]
    let ret = {
        let mut narrow: KernelItimerval = [0; 4];
        // SAFETY: the kernel writes `narrow`, a live local.
        let ret =
            unsafe { syscall::syscall2(nr::GETITIMER, which as usize, (&raw mut narrow).addr()) };
        if ret == 0 {
            // SAFETY: the caller vouches for `value`.
            unsafe { value.write(itimerval_of(narrow)) };
        }
        ret
    };
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
        // Below a second, checked above: it fits a `long`.
        tv_nsec: (tv.tv_usec * 1000) as c_long,
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
#[cfg(not(target_arch = "arm"))]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clock_adjtime(clock: c_int, tx: *mut c_void) -> c_int {
    // SAFETY: the kernel reads and writes `*tx`, which the caller vouches for.
    let ret = unsafe { syscall::syscall2(nr::CLOCK_ADJTIME, clock as usize, tx.addr()) };
    errno::from_syscall(ret) as c_int
}

/// C's `struct timex` on ARMv7-A, as musl's `sys/timex.h` declares it: `long`s
/// of 32 bits and a `struct timeval` of a 64-bit `time_t`.
#[cfg(target_arch = "arm")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Timex {
    modes: c_uint,
    offset: c_long,
    freq: c_long,
    maxerror: c_long,
    esterror: c_long,
    status: c_int,
    constant: c_long,
    precision: c_long,
    tolerance: c_long,
    time: Timeval,
    tick: c_long,
    ppsfreq: c_long,
    jitter: c_long,
    shift: c_int,
    stabil: c_long,
    jitcnt: c_long,
    calcnt: c_long,
    errcnt: c_long,
    stbcnt: c_long,
    tai: c_int,
    padding: [c_int; 11],
}

/// The kernel's `struct __kernel_timex`, which `clock_adjtime64` takes on
/// ARMv7-A: every field 64 bits, with padding after each `int`.
#[cfg(target_arch = "arm")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct KernelTimex {
    modes: u32,
    pad0: u32,
    offset: i64,
    freq: i64,
    maxerror: i64,
    esterror: i64,
    status: i32,
    pad1: u32,
    constant: i64,
    precision: i64,
    tolerance: i64,
    time_sec: i64,
    time_usec: i64,
    tick: i64,
    ppsfreq: i64,
    jitter: i64,
    shift: i32,
    pad2: u32,
    stabil: i64,
    jitcnt: i64,
    calcnt: i64,
    errcnt: i64,
    stbcnt: i64,
    tai: i32,
    pad3: [u32; 11],
}

#[cfg(target_arch = "arm")]
const _: () = assert!(size_of::<Timex>() == 144);
#[cfg(target_arch = "arm")]
const _: () = assert!(size_of::<KernelTimex>() == 208);

/// Reads clock `clock`'s discipline into `*tx`, adjusting it first as the
/// modes in `*tx` ask, and returns the clock's state. On ARMv7-A the kernel's
/// structure is wider than C's, and every field is copied across and back,
/// as musl does.
///
/// # Safety
///
/// `tx` must be valid for a read and a write of a `struct timex`.
#[cfg(target_arch = "arm")]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clock_adjtime(clock: c_int, tx: *mut c_void) -> c_int {
    let tx = tx.cast::<Timex>();
    // SAFETY: the caller vouches for `*tx`.
    let c = unsafe { tx.read() };
    let mut k = KernelTimex {
        modes: c.modes,
        offset: i64::from(c.offset),
        freq: i64::from(c.freq),
        maxerror: i64::from(c.maxerror),
        esterror: i64::from(c.esterror),
        status: c.status,
        constant: i64::from(c.constant),
        precision: i64::from(c.precision),
        tolerance: i64::from(c.tolerance),
        time_sec: c.time.tv_sec,
        time_usec: c.time.tv_usec,
        tick: i64::from(c.tick),
        ppsfreq: i64::from(c.ppsfreq),
        jitter: i64::from(c.jitter),
        shift: c.shift,
        stabil: i64::from(c.stabil),
        jitcnt: i64::from(c.jitcnt),
        calcnt: i64::from(c.calcnt),
        errcnt: i64::from(c.errcnt),
        stbcnt: i64::from(c.stbcnt),
        tai: c.tai,
        ..KernelTimex::default()
    };
    // SAFETY: the kernel reads and writes `k`, a live local.
    let ret =
        unsafe { syscall::syscall2(nr::CLOCK_ADJTIME64, clock as usize, (&raw mut k).addr()) };
    if ret < 0 {
        return errno::from_syscall(ret) as c_int;
    }
    // The kernel's values are its own clock's, which fit C's `long`s.
    let back = Timex {
        modes: k.modes,
        offset: k.offset as c_long,
        freq: k.freq as c_long,
        maxerror: k.maxerror as c_long,
        esterror: k.esterror as c_long,
        status: k.status,
        constant: k.constant as c_long,
        precision: k.precision as c_long,
        tolerance: k.tolerance as c_long,
        time: Timeval {
            tv_sec: k.time_sec,
            tv_usec: k.time_usec,
        },
        tick: k.tick as c_long,
        ppsfreq: k.ppsfreq as c_long,
        jitter: k.jitter as c_long,
        shift: k.shift,
        stabil: k.stabil as c_long,
        jitcnt: k.jitcnt as c_long,
        calcnt: k.calcnt as c_long,
        errcnt: k.errcnt as c_long,
        stbcnt: k.stbcnt as c_long,
        tai: k.tai,
        padding: c.padding,
    };
    // SAFETY: as above.
    unsafe { tx.write(back) };
    ret as c_int
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
