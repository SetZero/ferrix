//! `time.h`'s POSIX timers: `timer_create`, `timer_settime`, `timer_gettime`,
//! `timer_getoverrun` and `timer_delete`.
//!
//! As musl 1.2.5's `time/timer_*.c` (MIT; see [`crate::math`] for the
//! notice), each is its system call, and a `timer_t` is the kernel's timer id
//! widened to a pointer. This library's `struct itimerspec` is the kernel's
//! `__kernel_itimerspec` on every architecture ([`Itimerspec`]), so ARMv7-A
//! calls `timer_settime64` and `timer_gettime64` under the plain names, as
//! `nr` names them.
//!
//! `SIGEV_THREAD`, a timer that runs a function on a thread of its own, is
//! not here: musl starts a thread per timer and has the kernel signal it
//! (`SIGEV_THREAD_ID`), and this library does not yet. `timer_create` refuses
//! it with `ENOTSUP` rather than making a timer that would never call the
//! function. The other three kinds -- a signal to the process, to one thread,
//! or nothing -- are the kernel's.
//!
//! Claude Code's native build imports `timer_create`, `timer_settime` and
//! `timer_delete` by name, so without them the loader refuses to start it,
//! though it never calls them in a session.

use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::syscall::{self, nr};
use crate::timerfd::Itimerspec;

/// C's `timer_t`: the kernel's timer id, an `int`, widened to a pointer.
pub type TimerT = *mut c_void;

/// `SIGEV_THREAD`: call `sigev_notify_function` on a new thread at expiry.
const SIGEV_THREAD: c_int = 2;

/// C's `struct sigevent`, as `include/signal.h` and the kernel's
/// `sigevent_t` lay it out: a `union sigval`, the signal, how to notify, and
/// the rest of 64 bytes, which holds a thread id for `SIGEV_THREAD_ID` or a
/// function and its attributes for `SIGEV_THREAD`. Only `sigev_notify` is
/// read here; the kernel reads the rest.
#[repr(C)]
#[derive(Debug)]
pub struct Sigevent {
    /// `sigev_value`: the `union sigval` a signal carries, pointer-sized.
    pub sigev_value: *mut c_void,
    /// `sigev_signo`: the signal to send.
    pub sigev_signo: c_int,
    /// `sigev_notify`: `SIGEV_SIGNAL`, `SIGEV_NONE`, `SIGEV_THREAD` or
    /// `SIGEV_THREAD_ID`.
    pub sigev_notify: c_int,
    /// The union after it.
    pub fields: [u8; 64 - 2 * size_of::<c_int>() - size_of::<*mut c_void>()],
}

const _: () = assert!(size_of::<Sigevent>() == 64);
const _: () = assert!(offset_of!(Sigevent, sigev_notify) == size_of::<*mut c_void>() + 4);

/// The kernel's id for `timer`, which [`timer_create`] made from it.
fn kernel_id(timer: TimerT) -> usize {
    // Truncating to the `int` the kernel gave and sign-extending it back is
    // the round trip `timer_create`'s widening makes.
    timer.addr() as c_int as usize
}

/// Makes a timer on `clock` that notifies as `*event` says, or with
/// `SIGALRM` to the process for a null `event`, and stores it in `*timer`.
/// Fails with `ENOTSUP` for `SIGEV_THREAD`, and otherwise as the kernel does.
///
/// # Safety
///
/// `event` must be null or valid for a read of a `struct sigevent`, and
/// `timer` valid for a write of a `timer_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn timer_create(
    clock: c_int,
    event: *const Sigevent,
    timer: *mut TimerT,
) -> c_int {
    if !event.is_null() {
        // SAFETY: the caller vouches for a non-null `event`.
        let notify = unsafe { (*event).sigev_notify };
        if notify == SIGEV_THREAD {
            errno::set(errno::ENOTSUP);
            return -1;
        }
    }
    let mut id: c_int = 0;
    // SAFETY: the caller vouches for `event`, and `id` is a live `int`.
    let ret = unsafe {
        syscall::syscall3(
            nr::TIMER_CREATE,
            clock as usize,
            event.addr(),
            (&raw mut id).addr(),
        )
    };
    if errno::from_syscall(ret) < 0 {
        return -1;
    }
    // SAFETY: the caller vouches for `timer`.
    unsafe { timer.write(null_mut::<c_void>().with_addr(id as isize as usize)) };
    0
}

/// Arms `timer` with `*new`, or disarms it for a zero `it_value`, and stores
/// what it was in `*old` unless `old` is null. `flags` may hold
/// `TIMER_ABSTIME`, which makes `it_value` a time on the timer's clock rather
/// than one from now.
///
/// # Safety
///
/// `new` must be valid for a read of a `struct itimerspec`, and `old` for a
/// write of one or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn timer_settime(
    timer: TimerT,
    flags: c_int,
    new: *const Itimerspec,
    old: *mut Itimerspec,
) -> c_int {
    // SAFETY: the caller vouches for both structures.
    let ret = unsafe {
        syscall::syscall4(
            nr::TIMER_SETTIME,
            kernel_id(timer),
            flags as usize,
            new.addr(),
            old.addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Stores `timer`'s period and the time left to its next expiry in
/// `*current`.
///
/// # Safety
///
/// `current` must be valid for a write of a `struct itimerspec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn timer_gettime(timer: TimerT, current: *mut Itimerspec) -> c_int {
    // SAFETY: the caller vouches for the structure.
    let ret = unsafe { syscall::syscall2(nr::TIMER_GETTIME, kernel_id(timer), current.addr()) };
    errno::from_syscall(ret) as c_int
}

/// How many expiries of `timer` went unsignalled before the last signal was
/// taken.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn timer_getoverrun(timer: TimerT) -> c_int {
    // SAFETY: `timer_getoverrun` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::TIMER_GETOVERRUN, kernel_id(timer), 0) };
    errno::from_syscall(ret) as c_int
}

/// Disarms and frees `timer`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn timer_delete(timer: TimerT) -> c_int {
    // SAFETY: `timer_delete` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::TIMER_DELETE, kernel_id(timer), 0) };
    errno::from_syscall(ret) as c_int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timer_id_survives_the_trip_through_a_pointer() {
        for id in [0, 1, 7, c_int::MAX] {
            let timer = null_mut::<c_void>().with_addr(id as isize as usize);
            assert_eq!(kernel_id(timer), id as usize);
        }
    }
}
