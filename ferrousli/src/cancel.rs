//! Thread cancellation: the cancel state and type, and the cleanup handlers
//! `pthread_cleanup_push` registers.
//!
//! musl's header expands `pthread_cleanup_push` into a `struct __ptcb` on the
//! caller's stack and a call to [`_pthread_cleanup_push`], which links it into
//! the thread's list. `pthread_exit` runs the list, innermost first.
//!
//! # The masked state
//!
//! Besides `PTHREAD_CANCEL_ENABLE` and `PTHREAD_CANCEL_DISABLE`, a thread may
//! be in musl's `PTHREAD_CANCEL_MASKED` state, which only the library enters.
//! A cancellation point that finds a cancellation pending in that state
//! reports `ECANCELED` instead of acting, and disables cancellation. The
//! condition variable wait uses it to reacquire its mutex before acting on a
//! cancellation.

use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use crate::errno;
use crate::syscall;

/// `PTHREAD_CANCEL_ENABLE`.
pub const ENABLE: u8 = 0;
/// `PTHREAD_CANCEL_DISABLE`.
pub const DISABLE: u8 = 1;
/// `PTHREAD_CANCEL_MASKED`: musl's state for the library's own use.
pub const MASKED: u8 = 2;

/// `PTHREAD_CANCEL_DEFERRED`.
pub const DEFERRED: u8 = 0;
/// `PTHREAD_CANCEL_ASYNCHRONOUS`.
pub const ASYNCHRONOUS: u8 = 1;

/// `PTHREAD_CANCELED`: what a cancelled thread's join reports.
pub const CANCELED: *mut c_void = core::ptr::without_provenance_mut(usize::MAX);

/// A cleanup handler.
pub type Handler = unsafe extern "C" fn(*mut c_void);

/// `struct __ptcb`, from `pthread.h`: one registered cleanup handler, on the
/// stack of the code that pushed it.
#[repr(C)]
#[derive(Debug)]
pub struct Cleanup {
    /// `__f`: the handler.
    pub func: Option<Handler>,
    /// `__x`: its argument.
    pub arg: *mut c_void,
    /// `__next`: the handler pushed before this one.
    pub next: *mut Cleanup,
}

const _: () = assert!(size_of::<Cleanup>() == 24);
const _: () = assert!(offset_of!(Cleanup, next) == 16);

/// Sets the calling thread's cancel state to `new`, one of [`ENABLE`],
/// [`DISABLE`] and [`MASKED`], and returns the old one.
#[cfg(not(test))]
pub fn set_state(new: u8) -> u8 {
    crate::thread::me()
        .cancel_disable
        .swap(new, Ordering::Relaxed)
}

/// In unit tests there is no control block, and nothing is cancelled.
#[cfg(test)]
pub fn set_state(new: u8) -> u8 {
    let _ = new;
    ENABLE
}

/// Sets the calling thread's cancel state, storing the old one in `*old` if
/// `old` is not null. Fails with `EINVAL` for an unknown state.
///
/// # Safety
///
/// `old` must be null or valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setcancelstate(new: c_int, old: *mut c_int) -> c_int {
    let Ok(new) = u8::try_from(new) else {
        return errno::EINVAL;
    };
    if new > MASKED {
        return errno::EINVAL;
    }
    let previous = set_state(new);
    if !old.is_null() {
        // SAFETY: the caller vouches for a non-null `old`.
        unsafe { old.write(c_int::from(previous)) };
    }
    0
}

/// Sets the calling thread's cancel type, storing the old one in `*old` if
/// `old` is not null. Fails with `EINVAL` for an unknown type.
///
/// # Safety
///
/// `old` must be null or valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_setcanceltype(new: c_int, old: *mut c_int) -> c_int {
    let Ok(new) = u8::try_from(new) else {
        return errno::EINVAL;
    };
    if new > ASYNCHRONOUS {
        return errno::EINVAL;
    }
    let previous = crate::thread::me()
        .cancel_async
        .swap(new, Ordering::Relaxed);
    if !old.is_null() {
        // SAFETY: the caller vouches for a non-null `old`.
        unsafe { old.write(c_int::from(previous)) };
    }
    0
}

/// Registers `func(arg)` as the calling thread's innermost cleanup handler,
/// in the `struct __ptcb` at `cb`. `pthread_cleanup_push` expands to this.
///
/// # Safety
///
/// `cb` must be valid for writes of a `struct __ptcb`, and stay so until the
/// matching [`_pthread_cleanup_pop`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _pthread_cleanup_push(
    cb: *mut Cleanup,
    func: Option<Handler>,
    arg: *mut c_void,
) {
    let state = crate::thread::me();
    let next = state.cancel_buf.load(Ordering::Relaxed);
    // SAFETY: the caller vouches for `cb`.
    unsafe { cb.write(Cleanup { func, arg, next }) };
    state.cancel_buf.store(cb, Ordering::Relaxed);
}

/// Removes the innermost cleanup handler, the one at `cb`, and runs it if
/// `run` is nonzero. `pthread_cleanup_pop` expands to this.
///
/// # Safety
///
/// `cb` must be the handler the matching [`_pthread_cleanup_push`] filled in.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn _pthread_cleanup_pop(cb: *mut Cleanup, run: c_int) {
    // SAFETY: the caller vouches for `cb`, which push filled in.
    let Cleanup { func, arg, next } = unsafe { cb.read() };
    crate::thread::me()
        .cancel_buf
        .store(next, Ordering::Relaxed);
    if run != 0
        && let Some(func) = func
    {
        // SAFETY: the program registered the handler to be run with `arg`.
        unsafe { func(arg) };
    }
}

/// Runs and removes the calling thread's cleanup handlers, innermost first.
/// `pthread_exit` calls this.
pub fn run_cleanup_handlers() {
    let state = crate::thread::me();
    loop {
        let cb = state.cancel_buf.load(Ordering::Relaxed);
        if cb.is_null() {
            return;
        }
        // SAFETY: every entry on the list is a live `struct __ptcb` its pusher
        // has not yet popped, whose frame is still on this thread's stack.
        let Cleanup { func, arg, next } = unsafe { cb.read() };
        state.cancel_buf.store(next, Ordering::Relaxed);
        if let Some(func) = func {
            // SAFETY: the program registered the handler to be run with
            // `arg` if the thread exits.
            unsafe { func(arg) };
        }
    }
}

/// Acts on a pending cancellation, if the calling thread's cancel state is
/// enabled: the thread exits as if with `pthread_exit(PTHREAD_CANCELED)`.
pub fn testcancel() {
    let state = crate::thread::me();
    if state.cancel.load(Ordering::SeqCst) != 0
        && state.cancel_disable.load(Ordering::SeqCst) == ENABLE
    {
        crate::pthread::pthread_exit(CANCELED);
    }
}

/// Makes a system call that is a cancellation point.
///
/// Until cancellation requests exist, it is the plain system call.
///
/// # Safety
///
/// As the system call.
pub unsafe fn syscall_cp(
    number: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> isize {
    // SAFETY: the caller vouches for the call.
    unsafe { syscall::syscall6(number, a0, a1, a2, a3, a4, a5) }
}

/// Removes nothing: kept so that a null list head reads as empty.
#[allow(dead_code, reason = "documents the empty list")]
const EMPTY: *mut Cleanup = null_mut();
