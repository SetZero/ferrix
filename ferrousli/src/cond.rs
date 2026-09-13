//! `pthread.h`'s condition variables and their attributes.
//!
//! Adapted from musl's `pthread_cond_timedwait.c` (MIT).
//!
//! # Private condition variables
//!
//! Each waiter puts a node on its own stack at the head of the variable's
//! list, under a small lock in the variable. A node holds a barrier, a lock
//! the waiter sleeps on, which starts out held.
//!
//! `signal` and `broadcast` take nodes from the tail, marking each signalled,
//! and cut them off the list, then release the barrier of the first one. That
//! waiter wakes, reacquires the mutex, and releases the next node's barrier,
//! requeueing its sleeper onto the mutex rather than waking it, so a
//! broadcast wakes one thread at a time as the mutex comes free instead of
//! all of them into a crowd on it. A waiter that times out or is cancelled
//! first marks its node leaving and removes it; a signaller that finds such a
//! node waits for it to be gone. No wakeup is lost: a waiter is on the list
//! before it releases the mutex, and a signal reaches every node on the list.
//!
//! # Process-shared condition variables
//!
//! Another process cannot see a waiter's stack, so a shared variable is a
//! sequence number: waiters sleep on it, and `signal` increments it and wakes
//! one.
//!
//! # Cancellation
//!
//! The wait is a cancellation point, entered in the masked cancel state. A
//! cancellation reported there is acted on only after the mutex is held again,
//! as POSIX requires, and only if the wait did not also consume a signal.

use core::ffi::{c_int, c_uint, c_void};
use core::mem::size_of;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicUsize, Ordering};

use crate::mutex::{self, Mutex};
use crate::syscall::{self, nr};
use crate::time::Timespec;
use crate::{cancel, errno, futex, thread};

/// `pthread_cond_t`: 48 bytes that musl reads as `int`s and pointers
/// overlapping. See the accessors.
#[repr(C)]
#[derive(Debug)]
pub struct Cond {
    words: [AtomicUsize; 6],
}

const _: () = assert!(size_of::<Cond>() == 48);

/// A waiter's node, on its stack.
#[repr(C)]
#[derive(Debug)]
struct Waiter {
    prev: AtomicPtr<Waiter>,
    next: AtomicPtr<Waiter>,
    state: AtomicI32,
    barrier: AtomicI32,
    notify: AtomicPtr<AtomicI32>,
}

/// Waiting to be signalled.
const WAITING: c_int = 0;
/// Signalled.
const SIGNALED: c_int = 1;
/// Leaving without a signal: timed out or cancelled.
const LEAVING: c_int = 2;

/// The `int` at index `i` of `c`, as musl's `__u.__vi[i]`.
///
/// # Safety
///
/// `c` must be a condition variable that outlives `'a`.
unsafe fn int<'a>(c: *mut Cond, i: usize) -> &'a AtomicI32 {
    // SAFETY: the caller vouches for the 48 bytes, and `i` is below 12.
    unsafe { AtomicI32::from_ptr(c.cast::<c_int>().wrapping_add(i)) }
}

/// The pointer at index `i` of `c`, as musl's `__u.__p[i]`.
///
/// # Safety
///
/// As [`int`].
unsafe fn word<'a>(c: *mut Cond, i: usize) -> &'a AtomicPtr<Waiter> {
    // SAFETY: the caller vouches for the 48 bytes, and `i` is below 6.
    unsafe { AtomicPtr::from_ptr(c.cast::<*mut Waiter>().wrapping_add(i)) }
}

/// `_c_shared`: nonzero for a process-shared variable.
const SHARED_WORD: usize = 0;
/// `_c_head`, of a private variable.
const HEAD_WORD: usize = 1;
/// `_c_tail`, of a private variable.
const TAIL_WORD: usize = 5;
/// `_c_seq`, of a shared variable.
const SEQ_INT: usize = 2;
/// `_c_waiters`, of a shared variable.
const WAITERS_INT: usize = 3;
/// `_c_clock`.
const CLOCK_INT: usize = 4;
/// `_c_lock`, of a private variable.
const LOCK_INT: usize = 8;

/// The lock, head and tail of a private variable.
///
/// # Safety
///
/// As [`int`].
unsafe fn list<'a>(c: *mut Cond) -> (&'a AtomicI32, &'a AtomicPtr<Waiter>, &'a AtomicPtr<Waiter>) {
    // SAFETY: the caller vouches for `c`.
    let lock = unsafe { int(c, LOCK_INT) };
    // SAFETY: as above.
    let head = unsafe { word(c, HEAD_WORD) };
    // SAFETY: as above.
    let tail = unsafe { word(c, TAIL_WORD) };
    (lock, head, tail)
}

/// Takes a lock that may be freed by whoever the release wakes: 0 free, 1
/// held, 2 held with sleepers.
fn lock(l: &AtomicI32) {
    if futex::cas(l, 0, 1) != 0 {
        let _ = futex::cas(l, 1, 2);
        loop {
            futex::wait_counted(l, None, 2, true);
            if futex::cas(l, 0, 2) == 0 {
                return;
            }
        }
    }
}

/// Releases a lock taken with [`lock`].
fn unlock(l: &AtomicI32) {
    if l.swap(0, Ordering::SeqCst) == 2 {
        futex::wake(l, 1, true);
    }
}

/// Releases the barrier `l`, waking its sleeper if `wake`, or else moving it
/// to sleep on the mutex word `onto` instead.
fn unlock_requeue(l: &AtomicI32, onto: &AtomicI32, wake: bool) {
    l.store(0, Ordering::SeqCst);
    if wake {
        futex::wake(l, 1, true);
    } else {
        // SAFETY: requeueing reads no memory beyond the two futex words'
        // addresses, which are keys.
        let _ = unsafe {
            syscall::syscall6(
                nr::FUTEX,
                l.as_ptr().addr(),
                futex::FUTEX_REQUEUE | futex::FUTEX_PRIVATE,
                0,
                1,
                onto.as_ptr().addr(),
                0,
            )
        };
    }
}

/// Releases `m`, waits on `c` until signalled or until the absolute time `ts`
/// if it is not null, and takes `m` again.
///
/// # Safety
///
/// `c` must be an initialised condition variable and `m` an initialised mutex
/// the caller holds, both outliving the call, and `ts` null or valid for a
/// read of a `struct timespec`.
pub unsafe fn timedwait(c: *mut Cond, m: *mut Mutex, ts: *const Timespec) -> c_int {
    // SAFETY: the caller vouches for the mutex, which is all atomics.
    let m = unsafe { &*m };
    let kind = m.kind.load(Ordering::SeqCst);
    if kind & 15 != mutex::NORMAL && m.lock.load(Ordering::SeqCst) & c_int::MAX != thread::my_tid()
    {
        return errno::EPERM;
    }
    if !ts.is_null() {
        // SAFETY: the caller vouches for a non-null `ts`.
        let nsec = unsafe { (*ts).tv_nsec };
        if !(0..1_000_000_000).contains(&nsec) {
            return errno::EINVAL;
        }
    }
    cancel::testcancel();

    // SAFETY: the caller vouches for `c` throughout.
    let clock = unsafe { int(c, CLOCK_INT) }.load(Ordering::SeqCst);
    // SAFETY: as above.
    let shared = !unsafe { word(c, SHARED_WORD) }
        .load(Ordering::SeqCst)
        .is_null();
    let node = Waiter {
        prev: AtomicPtr::new(null_mut()),
        next: AtomicPtr::new(null_mut()),
        state: AtomicI32::new(WAITING),
        barrier: AtomicI32::new(2),
        notify: AtomicPtr::new(null_mut()),
    };
    let me = (&raw const node).cast_mut();

    let (fut, seq) = if shared {
        // SAFETY: as above.
        let seq_word = unsafe { int(c, SEQ_INT) };
        let seq = seq_word.load(Ordering::SeqCst);
        // SAFETY: as above.
        let _ = unsafe { int(c, WAITERS_INT) }.fetch_add(1, Ordering::SeqCst);
        (seq_word, seq)
    } else {
        // SAFETY: as above.
        let (cv_lock, head, tail) = unsafe { list(c) };
        lock(cv_lock);
        let first = head.load(Ordering::SeqCst);
        node.next.store(first, Ordering::SeqCst);
        head.store(me, Ordering::SeqCst);
        if tail.load(Ordering::SeqCst).is_null() {
            tail.store(me, Ordering::SeqCst);
        } else {
            // SAFETY: the old head is a waiter still on the list, whose node
            // stays on its stack until it removes itself under this lock.
            unsafe { &*first }.prev.store(me, Ordering::SeqCst);
        }
        unlock(cv_lock);
        (&node.barrier, 2)
    };

    let _ = mutex::unlock(m);

    let cs = cancel::set_state(cancel::MASKED);
    if cs == cancel::DISABLE {
        let _ = cancel::set_state(cs);
    }

    let mut e = loop {
        // SAFETY: the caller vouches for `ts`.
        let e = unsafe { futex::timedwait_cp(fut, seq, clock, ts, !shared) };
        if fut.load(Ordering::SeqCst) != seq || (e != 0 && e != errno::EINTR) {
            break e;
        }
    };
    if e == errno::EINTR {
        e = 0;
    }

    let oldstate = if shared {
        // A signal may have been consumed: that is a legitimate spurious
        // wake, and cancellation is suppressed.
        // SAFETY: as above.
        if e == errno::ECANCELED && unsafe { int(c, SEQ_INT) }.load(Ordering::SeqCst) != seq {
            e = 0;
        }
        // SAFETY: as above.
        let waiters = unsafe { int(c, WAITERS_INT) };
        if waiters.fetch_sub(1, Ordering::SeqCst) == -0x7fff_ffff {
            futex::wake(waiters, 1, false);
        }
        WAITING
    } else {
        let oldstate = futex::cas(&node.state, WAITING, LEAVING);
        if oldstate == WAITING {
            // Not signalled, so the variable is still valid, and a signaller
            // that meets this node waits for it to leave.
            // SAFETY: as above.
            let (cv_lock, head, tail) = unsafe { list(c) };
            lock(cv_lock);
            let prev = node.prev.load(Ordering::SeqCst);
            let next = node.next.load(Ordering::SeqCst);
            if head.load(Ordering::SeqCst) == me {
                head.store(next, Ordering::SeqCst);
            } else if !prev.is_null() {
                // SAFETY: a neighbour on the list is live under the lock.
                unsafe { &*prev }.next.store(next, Ordering::SeqCst);
            }
            if tail.load(Ordering::SeqCst) == me {
                tail.store(prev, Ordering::SeqCst);
            } else if !next.is_null() {
                // SAFETY: as above.
                unsafe { &*next }.prev.store(prev, Ordering::SeqCst);
            }
            unlock(cv_lock);
            let notify = node.notify.load(Ordering::SeqCst);
            if !notify.is_null() {
                // SAFETY: the signaller that set it waits for this count.
                let notify = unsafe { &*notify };
                if notify.fetch_sub(1, Ordering::SeqCst) == 1 {
                    futex::wake(notify, 1, true);
                }
            }
        } else {
            // Signalled: wait for this node's turn.
            lock(&node.barrier);
        }
        oldstate
    };

    // An error locking overrides anything else: the caller must know the
    // state of the mutex.
    let relocked = mutex::lock(m);
    if relocked != 0 {
        e = relocked;
    }

    if oldstate != WAITING {
        let prev = node.prev.load(Ordering::SeqCst);
        if node.next.load(Ordering::SeqCst).is_null() && kind & 8 == 0 {
            let _ = m.waiters.fetch_add(1, Ordering::SeqCst);
        }
        if prev.is_null() {
            if kind & 8 == 0 {
                let _ = m.waiters.fetch_sub(1, Ordering::SeqCst);
            }
        } else {
            let value = m.lock.load(Ordering::SeqCst);
            if value > 0 {
                let _ = futex::cas(&m.lock, value, value | futex::WAITERS);
            }
            // SAFETY: the previous node's waiter sleeps on its barrier until
            // released here, so its node is live.
            let barrier = unsafe { &(*prev).barrier };
            unlock_requeue(barrier, &m.lock, kind & (8 | 128) != 0);
        }
        // A signal was consumed, so cancellation may not act.
        if e == errno::ECANCELED {
            e = 0;
        }
    }

    let _ = cancel::set_state(cs);
    if e == errno::ECANCELED {
        cancel::testcancel();
        let _ = cancel::set_state(cancel::DISABLE);
    }
    e
}

/// Signals up to `n` waiters of a private variable, or all if `n` is
/// negative.
///
/// # Safety
///
/// `c` must be an initialised private condition variable.
unsafe fn private_signal(c: *mut Cond, mut n: i64) -> c_int {
    let notified = AtomicI32::new(0);
    let mut first: *mut Waiter = null_mut();
    // SAFETY: the caller vouches for `c`.
    let (cv_lock, head, tail) = unsafe { list(c) };
    lock(cv_lock);
    let mut p = tail.load(Ordering::SeqCst);
    while n != 0 && !p.is_null() {
        // SAFETY: nodes on the list are live under the lock.
        let waiter = unsafe { &*p };
        if futex::cas(&waiter.state, WAITING, SIGNALED) == WAITING {
            n -= 1;
            if first.is_null() {
                first = p;
            }
        } else {
            let _ = notified.fetch_add(1, Ordering::SeqCst);
            waiter
                .notify
                .store((&raw const notified).cast_mut(), Ordering::SeqCst);
        }
        p = waiter.prev.load(Ordering::SeqCst);
    }
    // Split the list, leaving the rest on the variable.
    if p.is_null() {
        head.store(null_mut(), Ordering::SeqCst);
    } else {
        // SAFETY: as above.
        let rest = unsafe { &*p };
        let next = rest.next.load(Ordering::SeqCst);
        if !next.is_null() {
            // SAFETY: as above.
            unsafe { &*next }.prev.store(null_mut(), Ordering::SeqCst);
        }
        rest.next.store(null_mut(), Ordering::SeqCst);
    }
    tail.store(p, Ordering::SeqCst);
    unlock(cv_lock);

    // Wait for leaving waiters to take themselves off before letting the
    // signalled ones go.
    loop {
        let count = notified.load(Ordering::SeqCst);
        if count == 0 {
            break;
        }
        futex::wait_counted(&notified, None, count, true);
    }
    if !first.is_null() {
        // SAFETY: the first signalled waiter sleeps on its barrier until this
        // releases it.
        unlock(unsafe { &(*first).barrier });
    }
    0
}

/// Wakes up to `n` waiters, or all if `n` is negative.
///
/// # Safety
///
/// `c` must be an initialised condition variable.
unsafe fn signal(c: *mut Cond, n: i64) -> c_int {
    // SAFETY: the caller vouches for `c`.
    if unsafe { word(c, SHARED_WORD) }
        .load(Ordering::SeqCst)
        .is_null()
    {
        // SAFETY: as above.
        return unsafe { private_signal(c, n) };
    }
    // SAFETY: as above.
    if unsafe { int(c, WAITERS_INT) }.load(Ordering::SeqCst) == 0 {
        return 0;
    }
    // SAFETY: as above.
    let seq = unsafe { int(c, SEQ_INT) };
    let _ = seq.fetch_add(1, Ordering::SeqCst);
    futex::wake(seq, if n < 0 { -1 } else { 1 }, false);
    0
}

/// `pthread_condattr_t`: the clock in the low 31 bits, and the
/// process-shared flag in the top one.
#[repr(C)]
#[derive(Debug)]
pub struct CondAttr {
    bits: c_uint,
}

/// Initialises `*c`, with the clock and sharing `*attr` describes if `attr` is
/// not null.
///
/// # Safety
///
/// `c` must be valid for a write of a `pthread_cond_t`, and `attr` null or an
/// initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_cond_init(c: *mut Cond, attr: *const CondAttr) -> c_int {
    // SAFETY: the caller vouches for `c`.
    unsafe {
        c.write(Cond {
            words: [const { AtomicUsize::new(0) }; 6],
        });
    }
    // SAFETY: the caller passes null or an initialised attribute object.
    if let Some(attr) = unsafe { attr.as_ref() } {
        // SAFETY: just initialised.
        unsafe { int(c, CLOCK_INT) }.store((attr.bits & 0x7fff_ffff) as c_int, Ordering::SeqCst);
        if attr.bits >> 31 != 0 {
            // SAFETY: as above.
            unsafe { word(c, SHARED_WORD) }.store(
                core::ptr::without_provenance_mut(usize::MAX),
                Ordering::SeqCst,
            );
        }
    }
    0
}

/// Destroys `*c`. For a process-shared variable, wakes every waiter and waits
/// for them to leave, as musl does.
///
/// # Safety
///
/// `c` must be an initialised condition variable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_cond_destroy(c: *mut Cond) -> c_int {
    // SAFETY: the caller vouches for `c` throughout.
    let shared = !unsafe { word(c, SHARED_WORD) }
        .load(Ordering::SeqCst)
        .is_null();
    // SAFETY: as above.
    let waiters = unsafe { int(c, WAITERS_INT) };
    if shared && waiters.load(Ordering::SeqCst) != 0 {
        let _ = waiters.fetch_or(c_int::MIN, Ordering::SeqCst);
        // SAFETY: as above.
        let seq = unsafe { int(c, SEQ_INT) };
        let _ = seq.fetch_add(1, Ordering::SeqCst);
        futex::wake(seq, -1, false);
        loop {
            let count = waiters.load(Ordering::SeqCst);
            if count & c_int::MAX == 0 {
                break;
            }
            futex::wait_counted(waiters, None, count, false);
        }
    }
    0
}

/// Releases `*m`, waits on `*c` until signalled, and takes `*m` again. A
/// cancellation point; a cancelled thread holds `*m` again when its cleanup
/// handlers run. Fails with `EPERM` if the caller does not hold a mutex that
/// records its owner.
///
/// # Safety
///
/// `c` must be an initialised condition variable and `m` an initialised mutex.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_cond_wait(c: *mut Cond, m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for both, and there is no time limit.
    unsafe { timedwait(c, m, core::ptr::null()) }
}

/// As [`pthread_cond_wait`], but gives up at the absolute time `*ts` on the
/// variable's clock with `ETIMEDOUT`, still holding `*m` again. A time with
/// nanoseconds out of range fails with `EINVAL`.
///
/// # Safety
///
/// As [`pthread_cond_wait`], and `ts` must be valid for a read of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_cond_timedwait(
    c: *mut Cond,
    m: *mut Mutex,
    ts: *const Timespec,
) -> c_int {
    // SAFETY: the caller vouches for all three.
    unsafe { timedwait(c, m, ts) }
}

/// Wakes one thread waiting on `*c`, if any.
///
/// # Safety
///
/// `c` must be an initialised condition variable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_cond_signal(c: *mut Cond) -> c_int {
    // SAFETY: the caller vouches for `c`.
    unsafe { signal(c, 1) }
}

/// Wakes every thread waiting on `*c`.
///
/// # Safety
///
/// `c` must be an initialised condition variable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_cond_broadcast(c: *mut Cond) -> c_int {
    // SAFETY: the caller vouches for `c`.
    unsafe { signal(c, -1) }
}

/// Initialises `*attr`: `CLOCK_REALTIME`, process-private.
///
/// # Safety
///
/// `attr` must be valid for a write of a `pthread_condattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_condattr_init(attr: *mut CondAttr) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    unsafe { attr.write(CondAttr { bits: 0 }) };
    0
}

/// Destroys `*attr`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_condattr_destroy(_attr: *mut CondAttr) -> c_int {
    0
}

/// Stores the clock timed waits measure against in `*clock`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `clock` valid for a
/// write of a `clockid_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_condattr_getclock(
    attr: *const CondAttr,
    clock: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { ((*attr).bits & 0x7fff_ffff) as c_int };
    // SAFETY: the caller vouches for `clock`.
    unsafe { clock.write(value) };
    0
}

/// Sets the clock timed waits measure against. The CPU-time clocks, which a
/// sleeping thread does not advance, and negative clocks fail with `EINVAL`,
/// as in musl.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_condattr_setclock(attr: *mut CondAttr, clock: c_int) -> c_int {
    if clock < 0 || (clock as c_uint).wrapping_sub(2) < 2 {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    let attr = unsafe { &mut *attr };
    attr.bits = (attr.bits & 0x8000_0000) | clock as c_uint;
    0
}

/// Stores whether variables are process-shared in `*shared`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `shared` valid for a
/// write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_condattr_getpshared(
    attr: *const CondAttr,
    shared: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { ((*attr).bits >> 31) as c_int };
    // SAFETY: the caller vouches for `shared`.
    unsafe { shared.write(value) };
    0
}

/// Sets whether variables are process-shared. Anything but 0 or 1 fails with
/// `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_condattr_setpshared(attr: *mut CondAttr, shared: c_int) -> c_int {
    if !(0..=1).contains(&shared) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    let attr = unsafe { &mut *attr };
    attr.bits = (attr.bits & 0x7fff_ffff) | ((shared as c_uint) << 31);
    0
}

/// Unused: keeps the pointer type named in one place.
#[allow(dead_code, reason = "documents what the shared word holds")]
type SharedMarker = *mut c_void;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_attribute_refuses_cpu_clocks() {
        let mut attr = CondAttr { bits: 0 };
        // SAFETY: `attr` is a live local throughout.
        let ret = unsafe { pthread_condattr_setclock(&raw mut attr, 2) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: `attr` is a live local throughout.
        let ret = unsafe { pthread_condattr_setclock(&raw mut attr, 3) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: `attr` is a live local throughout.
        let ret = unsafe { pthread_condattr_setclock(&raw mut attr, -1) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: `attr` is a live local throughout.
        let ret = unsafe { pthread_condattr_setpshared(&raw mut attr, 1) };
        assert_eq!(ret, 0);
        // SAFETY: `attr` is a live local throughout.
        let ret = unsafe { pthread_condattr_setclock(&raw mut attr, 1) };
        assert_eq!(ret, 0);
        assert_eq!(attr.bits, 0x8000_0001);
    }
}
