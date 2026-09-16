//! `pthread.h`'s barriers and spin locks.
//!
//! Adapted from musl (MIT).
//!
//! # Barriers
//!
//! The first thread to arrive at a private barrier becomes the owner of an
//! instance on its own stack, and returns `PTHREAD_BARRIER_SERIAL_THREAD`
//! once every other thread of the round has left. Later arrivals count
//! themselves into the instance and sleep until the last one arrives, which
//! detaches the instance from the barrier so a new round can begin.
//!
//! A process-shared barrier cannot point at a stack another process cannot
//! see, so it counts arrivals in the barrier itself, under a lock that holds
//! the round's limit. musl makes `munmap` wait for such a barrier's threads to
//! leave; ferrousli's `munmap` does not, so that wait is left out.

use core::ffi::{c_int, c_uint};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

use crate::{errno, futex};

/// `PTHREAD_BARRIER_SERIAL_THREAD`.
const SERIAL_THREAD: c_int = -1;

/// `pthread_barrier_t`, in musl's layout, 32 bytes.
#[repr(C)]
#[derive(Debug)]
pub struct Barrier {
    /// `_b_lock`.
    lock: AtomicI32,
    /// `_b_waiters`: threads sleeping on the lock.
    waiters: AtomicI32,
    /// `_b_limit`: the thread count less one, with the sign bit set for a
    /// process-shared barrier.
    limit: AtomicI32,
    /// `_b_count`: arrivals of a process-shared barrier.
    count: AtomicI32,
    /// `_b_waiters2`: threads sleeping on the count.
    waiters2: AtomicI32,
    /// Unused by this layout.
    spare: AtomicI32,
    /// `_b_inst`: the current round of a private barrier.
    inst: AtomicPtr<Instance>,
}

const _: () = assert!(size_of::<Barrier>() == 32);
const _: () = assert!(offset_of!(Barrier, inst) == 24);

/// One round of a private barrier, on its owner's stack.
#[derive(Debug)]
struct Instance {
    /// Arrivals other than the owner.
    count: AtomicI32,
    /// Set when the last arrival comes.
    last: AtomicI32,
    /// Threads sleeping on `last`.
    waiters: AtomicI32,
    /// Raised by the owner and by the last thread to leave.
    finished: AtomicI32,
}

/// Waits at a process-shared barrier with room for `limit` threads.
fn shared_wait(b: &Barrier) -> c_int {
    let limit = (b.limit.load(Ordering::SeqCst) & c_int::MAX) + 1;
    if limit == 1 {
        return SERIAL_THREAD;
    }
    loop {
        let held = futex::cas(&b.lock, 0, limit);
        if held == 0 {
            break;
        }
        futex::wait_counted(&b.lock, Some(&b.waiters), held, false);
    }

    let mut ret = 0;
    // Under the lock: count this thread in.
    let arrived = b.count.load(Ordering::SeqCst) + 1;
    b.count.store(arrived, Ordering::SeqCst);
    if arrived == limit {
        b.count.store(0, Ordering::SeqCst);
        ret = SERIAL_THREAD;
        if b.waiters2.load(Ordering::SeqCst) != 0 {
            futex::wake(&raw const b.count, -1, false);
        }
    } else {
        b.lock.store(0, Ordering::SeqCst);
        if b.waiters.load(Ordering::SeqCst) != 0 {
            futex::wake(&raw const b.lock, 1, false);
        }
        loop {
            let count = b.count.load(Ordering::SeqCst);
            if count <= 0 {
                break;
            }
            futex::wait_counted(&b.count, Some(&b.waiters2), count, false);
        }
    }

    // Every thread of the round counts itself out before any leaves.
    if b.count.fetch_sub(1, Ordering::SeqCst) == 1 - limit {
        b.count.store(0, Ordering::SeqCst);
        if b.waiters2.load(Ordering::SeqCst) != 0 {
            futex::wake(&raw const b.count, -1, false);
        }
    } else {
        loop {
            let count = b.count.load(Ordering::SeqCst);
            if count == 0 {
                break;
            }
            futex::wait_counted(&b.count, Some(&b.waiters2), count, false);
        }
    }

    // Release the lock's hold for this thread, waking a destroyer or the
    // next round once the last one is out.
    let (held, waiting) = loop {
        let held = b.lock.load(Ordering::SeqCst);
        let waiting = b.waiters.load(Ordering::SeqCst);
        let next = if held == c_int::MIN + 1 { 0 } else { held - 1 };
        if futex::cas(&b.lock, held, next) == held {
            break (held, waiting);
        }
    };
    if held == c_int::MIN + 1 || (held == 1 && waiting != 0) {
        futex::wake(&raw const b.lock, 1, false);
    }
    ret
}

/// Waits at a private barrier with room for `limit + 1` threads.
fn private_wait(b: &Barrier, limit: c_int) -> c_int {
    while b.lock.swap(1, Ordering::SeqCst) != 0 {
        futex::wait_counted(&b.lock, Some(&b.waiters), 1, true);
    }
    let inst = b.inst.load(Ordering::SeqCst);

    if inst.is_null() {
        // The first to arrive owns the round.
        let owned = Instance {
            count: AtomicI32::new(0),
            last: AtomicI32::new(0),
            waiters: AtomicI32::new(0),
            finished: AtomicI32::new(0),
        };
        b.inst
            .store((&raw const owned).cast_mut(), Ordering::SeqCst);
        b.lock.store(0, Ordering::SeqCst);
        if b.waiters.load(Ordering::SeqCst) != 0 {
            futex::wake(&raw const b.lock, 1, true);
        }
        let mut spins = 200;
        while spins > 0 && owned.finished.load(Ordering::SeqCst) == 0 {
            core::hint::spin_loop();
            spins -= 1;
        }
        let _ = owned.finished.fetch_add(1, Ordering::SeqCst);
        while owned.finished.load(Ordering::SeqCst) == 1 {
            futex::wait(&owned.finished, 1, true);
        }
        return SERIAL_THREAD;
    }

    // SAFETY: the owner's instance stays on its stack until the last thread
    // of the round has raised `finished`, which this thread does last.
    let inst = unsafe { &*inst };
    if inst.count.fetch_add(1, Ordering::SeqCst) + 1 == limit {
        // The last arrival: start a new round, and release this one.
        b.inst.store(null_mut(), Ordering::SeqCst);
        b.lock.store(0, Ordering::SeqCst);
        if b.waiters.load(Ordering::SeqCst) != 0 {
            futex::wake(&raw const b.lock, 1, true);
        }
        inst.last.store(1, Ordering::SeqCst);
        if inst.waiters.load(Ordering::SeqCst) != 0 {
            futex::wake(&raw const inst.last, -1, true);
        }
    } else {
        b.lock.store(0, Ordering::SeqCst);
        if b.waiters.load(Ordering::SeqCst) != 0 {
            futex::wake(&raw const b.lock, 1, true);
        }
        futex::wait_counted(&inst.last, Some(&inst.waiters), 0, true);
    }

    // The last thread to leave lets the owner go.
    if inst.count.fetch_sub(1, Ordering::SeqCst) == 1
        && inst.finished.fetch_add(1, Ordering::SeqCst) != 0
    {
        futex::wake(&raw const inst.finished, 1, true);
    }
    0
}

/// `pthread_barrierattr_t`: `INT_MIN` for a process-shared barrier, else 0.
#[repr(C)]
#[derive(Debug)]
pub struct BarrierAttr {
    bits: c_uint,
}

/// Initialises `*b` for rounds of `count` threads, process-shared if `*attr`
/// says so. A count of 0 fails with `EINVAL`.
///
/// # Safety
///
/// `b` must be valid for a write of a `pthread_barrier_t`, and `attr` null or
/// an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_barrier_init(
    b: *mut Barrier,
    attr: *const BarrierAttr,
    count: c_uint,
) -> c_int {
    if count.wrapping_sub(1) > c_int::MAX as c_uint - 1 {
        return errno::EINVAL;
    }
    // SAFETY: the caller passes null or an initialised attribute object.
    let bits = unsafe { attr.as_ref() }.map_or(0, |attr| attr.bits);
    let limit = ((count - 1) | bits) as c_int;
    // SAFETY: the caller vouches for `b`.
    unsafe {
        b.write(Barrier {
            lock: AtomicI32::new(0),
            waiters: AtomicI32::new(0),
            limit: AtomicI32::new(limit),
            count: AtomicI32::new(0),
            waiters2: AtomicI32::new(0),
            spare: AtomicI32::new(0),
            inst: AtomicPtr::new(null_mut()),
        });
    }
    0
}

/// Destroys `*b`. For a process-shared barrier, waits for threads still
/// leaving it.
///
/// # Safety
///
/// `b` must be an initialised barrier.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_barrier_destroy(b: *mut Barrier) -> c_int {
    // SAFETY: the caller vouches for the barrier, which is all atomics.
    let b = unsafe { &*b };
    if b.limit.load(Ordering::SeqCst) < 0 && b.lock.load(Ordering::SeqCst) != 0 {
        let _ = b.lock.fetch_or(c_int::MIN, Ordering::SeqCst);
        loop {
            let held = b.lock.load(Ordering::SeqCst);
            if held & c_int::MAX == 0 {
                break;
            }
            futex::wait_counted(&b.lock, None, held, false);
        }
    }
    0
}

/// Waits at `*b` until its round is full. One thread of each round returns
/// `PTHREAD_BARRIER_SERIAL_THREAD`, and the rest 0.
///
/// # Safety
///
/// `b` must be an initialised barrier.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_barrier_wait(b: *mut Barrier) -> c_int {
    // SAFETY: the caller vouches for the barrier, which is all atomics.
    let b = unsafe { &*b };
    let limit = b.limit.load(Ordering::SeqCst);
    if limit == 0 {
        SERIAL_THREAD
    } else if limit < 0 {
        shared_wait(b)
    } else {
        private_wait(b, limit)
    }
}

/// Initialises `*attr`: process-private.
///
/// # Safety
///
/// `attr` must be valid for a write of a `pthread_barrierattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_barrierattr_init(attr: *mut BarrierAttr) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    unsafe { attr.write(BarrierAttr { bits: 0 }) };
    0
}

/// Destroys `*attr`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_barrierattr_destroy(_attr: *mut BarrierAttr) -> c_int {
    0
}

/// Stores whether barriers are process-shared in `*shared`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `shared` valid for a
/// write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_barrierattr_getpshared(
    attr: *const BarrierAttr,
    shared: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let bits = unsafe { (*attr).bits };
    // SAFETY: the caller vouches for `shared`.
    unsafe { shared.write(c_int::from(bits != 0)) };
    0
}

/// Sets whether barriers are process-shared. Anything but 0 or 1 fails with
/// `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_barrierattr_setpshared(
    attr: *mut BarrierAttr,
    shared: c_int,
) -> c_int {
    let bits = match shared {
        0 => 0,
        1 => c_int::MIN as c_uint,
        _ => return errno::EINVAL,
    };
    // SAFETY: the caller vouches for `attr`.
    unsafe { (*attr).bits = bits };
    0
}

/// The spin lock at `s`.
///
/// # Safety
///
/// `s` must be a `pthread_spinlock_t` that outlives `'a`.
unsafe fn spin<'a>(s: *mut c_int) -> &'a AtomicI32 {
    // SAFETY: the caller vouches for the `int`, and every access is atomic.
    unsafe { AtomicI32::from_ptr(s) }
}

/// Initialises the spin lock `*s`, unlocked. Sharing needs nothing more.
///
/// # Safety
///
/// `s` must be valid for a write of a `pthread_spinlock_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_spin_init(s: *mut c_int, _shared: c_int) -> c_int {
    // SAFETY: the caller vouches for `s`.
    unsafe { s.write(0) };
    0
}

/// Destroys a spin lock, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_spin_destroy(_s: *mut c_int) -> c_int {
    0
}

/// Takes the spin lock `*s`, spinning until it is free.
///
/// # Safety
///
/// `s` must be an initialised spin lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_spin_lock(s: *mut c_int) -> c_int {
    // SAFETY: the caller vouches for the lock.
    let s = unsafe { spin(s) };
    while s.load(Ordering::SeqCst) != 0 || futex::cas(s, 0, errno::EBUSY) != 0 {
        core::hint::spin_loop();
    }
    0
}

/// Takes the spin lock `*s` if it is free, or fails with `EBUSY`.
///
/// # Safety
///
/// `s` must be an initialised spin lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_spin_trylock(s: *mut c_int) -> c_int {
    // SAFETY: the caller vouches for the lock.
    futex::cas(unsafe { spin(s) }, 0, errno::EBUSY)
}

/// Releases the spin lock `*s`.
///
/// # Safety
///
/// `s` must be an initialised spin lock the caller holds.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_spin_unlock(s: *mut c_int) -> c_int {
    // SAFETY: the caller vouches for the lock.
    unsafe { spin(s) }.store(0, Ordering::SeqCst);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_barrier_of_one_returns_serial_at_once_and_zero_is_refused() {
        let mut b = core::mem::MaybeUninit::<Barrier>::uninit();
        // SAFETY: `b` is a live local.
        let ret = unsafe { pthread_barrier_init(b.as_mut_ptr(), core::ptr::null(), 0) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: as above.
        let ret = unsafe { pthread_barrier_init(b.as_mut_ptr(), core::ptr::null(), 1) };
        assert_eq!(ret, 0);
        // SAFETY: initialised just above.
        let ret = unsafe { pthread_barrier_wait(b.as_mut_ptr()) };
        assert_eq!(ret, SERIAL_THREAD);
    }

    #[test]
    fn a_spin_lock_is_busy_while_held() {
        let mut s = 7;
        // SAFETY: `s` is a live local, here and below.
        let ret = unsafe { pthread_spin_init(&raw mut s, 0) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { pthread_spin_trylock(&raw mut s) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { pthread_spin_trylock(&raw mut s) };
        assert_eq!(ret, errno::EBUSY);
        // SAFETY: as above.
        let ret = unsafe { pthread_spin_unlock(&raw mut s) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { pthread_spin_lock(&raw mut s) };
        assert_eq!(ret, 0);
    }
}
