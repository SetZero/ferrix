//! `threads.h`: C11's threads, mutexes, condition variables, thread-specific
//! storage and `call_once`, built on the `pthread.h` ones.
//!
//! The types are the same objects: `thrd_t` is a `pthread_t`, `mtx_t` a
//! `pthread_mutex_t`, `cnd_t` a `pthread_cond_t`, `tss_t` a `pthread_key_t`
//! and `once_flag` a `pthread_once_t`. What differs is the results, which C11
//! reduces to a handful of `thrd_` codes.

use core::ffi::{c_int, c_uint, c_void};
use core::ptr::{null, null_mut, without_provenance_mut};

use crate::cond::{self, Cond};
use crate::mutex::{self, Mutex};
use crate::pthread::{self, Routine};
use crate::thread::Thread;
use crate::time::{CLOCK_REALTIME, Timespec};
use crate::{errno, key};

/// `thrd_success`.
const SUCCESS: c_int = 0;
/// `thrd_busy`.
const BUSY: c_int = 1;
/// `thrd_error`.
const ERROR: c_int = 2;
/// `thrd_nomem`.
const NOMEM: c_int = 3;
/// `thrd_timedout`.
const TIMEDOUT: c_int = 4;

/// `mtx_recursive`.
const MTX_RECURSIVE: c_int = 1;
/// Every bit `mtx_init` accepts: `mtx_recursive | mtx_timed`.
const MTX_KNOWN: c_int = 3;

/// `thrd_success` for 0, `thrd_timedout` for `ETIMEDOUT`, and `thrd_error`
/// for anything else.
const fn timed(ret: c_int) -> c_int {
    match ret {
        0 => SUCCESS,
        errno::ETIMEDOUT => TIMEDOUT,
        _ => ERROR,
    }
}

/// Creates a thread running `func(arg)` with the default attributes, and
/// stores it in `*thr`. `thrd_nomem` if there is no memory or thread for it.
///
/// # Safety
///
/// `thr` must be valid for a write of a `thrd_t`, and `func` sound to run with
/// `arg` on another thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn thrd_create(
    thr: *mut *mut Thread,
    func: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    arg: *mut c_void,
) -> c_int {
    let Some(func) = func else {
        return ERROR;
    };
    // SAFETY: the caller vouches for the routine.
    match unsafe { pthread::create(None, Routine::C11(func), arg) } {
        Ok(new) => {
            // SAFETY: the caller vouches for `thr`.
            unsafe { thr.write(new) };
            SUCCESS
        }
        Err(errno::EAGAIN) => NOMEM,
        Err(_) => ERROR,
    }
}

/// Ends the calling thread with `result`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn thrd_exit(result: c_int) -> ! {
    pthread::pthread_exit(without_provenance_mut(result as isize as usize))
}

/// Waits for thread `t` to end, and stores its result in `*result` if
/// `result` is not null.
///
/// # Safety
///
/// As `pthread_join`, and `result` must be null or valid for a write of an
/// `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn thrd_join(t: *mut Thread, result: *mut c_int) -> c_int {
    let mut value: *mut c_void = null_mut();
    // SAFETY: the caller vouches for `t`; `value` is a live local.
    if unsafe { pthread::pthread_join(t, &raw mut value) } != 0 {
        return ERROR;
    }
    if !result.is_null() {
        // SAFETY: the caller vouches for a non-null `result`.
        unsafe { result.write(value.addr() as c_int) };
    }
    SUCCESS
}

/// Detaches thread `t`.
///
/// # Safety
///
/// As `pthread_detach`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn thrd_detach(t: *mut Thread) -> c_int {
    // SAFETY: the caller's contract.
    if unsafe { pthread::pthread_detach(t) } == 0 {
        SUCCESS
    } else {
        ERROR
    }
}

/// The calling thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn thrd_current() -> *mut Thread {
    pthread::pthread_self()
}

/// Nonzero if `a` and `b` are the same thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn thrd_equal(a: *mut Thread, b: *mut Thread) -> c_int {
    c_int::from(a == b)
}

/// Sleeps for `*req` on the real-time clock. Returns 0, -1 if a signal
/// interrupted it, storing the time left in `*rem` if not null, or -2 for
/// another error, as C11 says.
///
/// # Safety
///
/// `req` must be valid for a read of a `struct timespec`, and `rem` null or
/// valid for a write of one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn thrd_sleep(req: *const Timespec, rem: *mut Timespec) -> c_int {
    // SAFETY: the caller's contract is `clock_nanosleep`'s.
    match unsafe { crate::time::clock_nanosleep(CLOCK_REALTIME, 0, req, rem) } {
        0 => 0,
        errno::EINTR => -1,
        _ => -2,
    }
}

/// Gives the processor to another runnable thread.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn thrd_yield() {
    let _ = crate::sched::sched_yield();
}

/// Runs `func` once for `*flag`, however many threads call this.
///
/// # Safety
///
/// As `pthread_once`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn call_once(flag: *mut c_int, func: Option<unsafe extern "C" fn()>) {
    // SAFETY: the caller's contract.
    let _ = unsafe { key::pthread_once(flag, func) };
}

/// Initialises `*m`, recursive if `kind` holds `mtx_recursive`. Every mutex
/// may be timed. An unknown kind is `thrd_error`.
///
/// # Safety
///
/// `m` must be valid for a write of an `mtx_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mtx_init(m: *mut Mutex, kind: c_int) -> c_int {
    if kind & !MTX_KNOWN != 0 {
        return ERROR;
    }
    let pthread_kind = if kind & MTX_RECURSIVE != 0 {
        mutex::RECURSIVE
    } else {
        mutex::NORMAL
    };
    // SAFETY: the caller vouches for `m`.
    unsafe { m.write(Mutex::new(pthread_kind)) };
    SUCCESS
}

/// Destroys `*m`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn mtx_destroy(_m: *mut Mutex) {}

/// Takes `*m`, waiting for it.
///
/// # Safety
///
/// `m` must be an initialised mutex.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mtx_lock(m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    if mutex::lock(unsafe { &*m }) == 0 {
        SUCCESS
    } else {
        ERROR
    }
}

/// Takes `*m`, waiting until the absolute time `*ts` at most, or for ever if
/// `ts` is null, which musl also allows.
///
/// # Safety
///
/// `m` must be an initialised mutex, and `ts` null or valid for a read of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mtx_timedlock(m: *mut Mutex, ts: *const Timespec) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    let m = unsafe { &*m };
    // SAFETY: the caller vouches for `ts`.
    timed(unsafe { mutex::timedlock(m, ts) })
}

/// Takes `*m` if it is free: `thrd_busy` if not.
///
/// # Safety
///
/// `m` must be an initialised mutex.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mtx_trylock(m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    match mutex::trylock(unsafe { &*m }) {
        0 => SUCCESS,
        errno::EBUSY => BUSY,
        _ => ERROR,
    }
}

/// Releases `*m`.
///
/// # Safety
///
/// `m` must be an initialised mutex the caller holds.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mtx_unlock(m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    if mutex::unlock(unsafe { &*m }) == 0 {
        SUCCESS
    } else {
        ERROR
    }
}

/// Initialises `*c`.
///
/// # Safety
///
/// `c` must be valid for a write of a `cnd_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cnd_init(c: *mut Cond) -> c_int {
    // SAFETY: the caller vouches for `c`, and there are no attributes.
    let _ = unsafe { cond::pthread_cond_init(c, null()) };
    SUCCESS
}

/// Destroys `*c`.
///
/// # Safety
///
/// `c` must be an initialised condition variable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cnd_destroy(c: *mut Cond) {
    // SAFETY: the caller's contract.
    let _ = unsafe { cond::pthread_cond_destroy(c) };
}

/// Wakes one thread waiting on `*c`.
///
/// # Safety
///
/// `c` must be an initialised condition variable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cnd_signal(c: *mut Cond) -> c_int {
    // SAFETY: the caller's contract.
    let _ = unsafe { cond::pthread_cond_signal(c) };
    SUCCESS
}

/// Wakes every thread waiting on `*c`.
///
/// # Safety
///
/// `c` must be an initialised condition variable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cnd_broadcast(c: *mut Cond) -> c_int {
    // SAFETY: the caller's contract.
    let _ = unsafe { cond::pthread_cond_broadcast(c) };
    SUCCESS
}

/// Releases `*m`, waits on `*c`, and takes `*m` again.
///
/// # Safety
///
/// `c` must be an initialised condition variable and `m` an initialised mutex
/// the caller holds.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cnd_wait(c: *mut Cond, m: *mut Mutex) -> c_int {
    // SAFETY: the caller's contract, with no time limit.
    timed(unsafe { cond::timedwait(c, m, null()) })
}

/// As [`cnd_wait`], giving up at the absolute time `*ts` with
/// `thrd_timedout`.
///
/// # Safety
///
/// As [`cnd_wait`], and `ts` must be valid for a read of a `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn cnd_timedwait(c: *mut Cond, m: *mut Mutex, ts: *const Timespec) -> c_int {
    // SAFETY: the caller's contract.
    timed(unsafe { cond::timedwait(c, m, ts) })
}

/// Creates thread-specific storage with destructor `dtor`, and stores its key
/// in `*key`.
///
/// # Safety
///
/// As `pthread_key_create`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn tss_create(
    tss: *mut c_uint,
    dtor: Option<unsafe extern "C" fn(*mut c_void)>,
) -> c_int {
    // SAFETY: the caller's contract.
    if unsafe { key::pthread_key_create(tss, dtor) } == 0 {
        SUCCESS
    } else {
        ERROR
    }
}

/// Deletes thread-specific storage `tss`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tss_delete(tss: c_uint) {
    let _ = key::pthread_key_delete(tss);
}

/// Sets the calling thread's value for `tss`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tss_set(tss: c_uint, value: *mut c_void) -> c_int {
    if key::pthread_setspecific(tss, value) == 0 {
        SUCCESS
    } else {
        ERROR
    }
}

/// The calling thread's value for `tss`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tss_get(tss: c_uint) -> *mut c_void {
    key::pthread_getspecific(tss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_reduce_to_c11_codes() {
        assert_eq!(timed(0), SUCCESS);
        assert_eq!(timed(errno::ETIMEDOUT), TIMEDOUT);
        assert_eq!(timed(errno::EINVAL), ERROR);
        let mut m = core::mem::MaybeUninit::<Mutex>::uninit();
        // SAFETY: `m` is a live local.
        assert_eq!(unsafe { mtx_init(m.as_mut_ptr(), 8) }, ERROR);
        // SAFETY: as above.
        assert_eq!(unsafe { mtx_init(m.as_mut_ptr(), MTX_RECURSIVE) }, SUCCESS);
    }
}
