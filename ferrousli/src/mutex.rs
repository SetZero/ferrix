//! `pthread.h`'s mutexes: normal, recursive and error-checking; robust;
//! priority-inheriting; process-shared; and their attributes.
//!
//! Adapted from musl (MIT), `src/thread/pthread_mutex_*.c`.
//!
//! # The lock word
//!
//! A normal mutex holds `EBUSY` while locked, with the sign bit set once a
//! waiter has slept. Every other kind holds its owner's thread id, and so can
//! refuse a relock or an unlock by another thread:
//!
//! * bits 0 to 29: the owner's id, `FUTEX_TID_MASK`;
//! * bit 30: `FUTEX_OWNER_DIED`, the owner of a robust mutex ended holding it;
//! * bit 31: `FUTEX_WAITERS`, a thread sleeps on it.
//!
//! An owner id of `0x3fffffff` marks a robust mutex made unrecoverable.
//!
//! # The kind word
//!
//! The mutex attribute is copied into the mutex: bits 0 and 1 are the type,
//! 4 robust, 8 priority-inheriting, and 128 process-shared.
//!
//! # Robust mutexes
//!
//! Every mutex that records an owner is linked into the owner's list, the
//! kernel's `struct robust_list` through the mutex's `next` field, with `prev`
//! beside it so that unlocking can unlink from the middle. When a thread calls
//! `pthread_exit`, [`abandon_robust_list`] marks each mutex still on its list
//! as its owner having died. For a process-shared mutex, whose owner might be
//! killed outright, the list is also registered with the kernel through
//! `set_robust_list`, and the kernel does the same when the thread dies.
//!
//! # What is left out
//!
//! musl takes a lock around unlocking a process-shared robust mutex, which
//! `munmap` waits for, so that the kernel never follows the list into memory
//! unmapped while the mutex was being unlinked. Ferrousli's `munmap` does not
//! wait, so the lock is left out with it. Priority protection
//! (`PTHREAD_PRIO_PROTECT`) is refused with `ENOTSUP`, as in musl.

use core::ffi::{c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

use crate::errno;
use crate::futex::{self, OWNER_DIED, TID_MASK, WAITERS};
use crate::syscall::{self, nr};
use crate::thread::{self, State};
use crate::time::{CLOCK_REALTIME, Timespec};

/// The mask of the kind word's type bits.
const TYPE_MASK: c_int = 3;
/// `PTHREAD_MUTEX_NORMAL`.
pub const NORMAL: c_int = 0;
/// `PTHREAD_MUTEX_RECURSIVE`.
pub const RECURSIVE: c_int = 1;
/// `PTHREAD_MUTEX_ERRORCHECK`.
pub const ERRORCHECK: c_int = 2;
/// The kind bit of a robust mutex.
const ROBUST: c_int = 4;
/// The kind bit of a priority-inheriting mutex.
const PRIO_INHERIT: c_int = 8;
/// The kind bit of a process-shared mutex.
const SHARED: c_int = 128;
/// The owner id of a robust mutex made unrecoverable.
const UNRECOVERABLE: c_int = TID_MASK;

/// `PTHREAD_PRIO_NONE`.
const PRIO_NONE: c_int = 0;
/// `PTHREAD_PRIO_INHERIT`.
const PRIO_INHERIT_PROTOCOL: c_int = 1;
/// `PTHREAD_PRIO_PROTECT`.
const PRIO_PROTECT: c_int = 2;

/// `pthread_mutex_t`, in musl's layout, 40 bytes.
#[repr(C)]
#[derive(Debug)]
pub struct Mutex {
    /// `_m_type`: the kind word.
    pub kind: AtomicI32,
    /// `_m_lock`: the lock word.
    pub lock: AtomicI32,
    /// `_m_waiters`: threads sleeping, or about to.
    pub waiters: AtomicI32,
    /// Unused by this layout.
    spare: [AtomicI32; 2],
    /// `_m_count`: extra holds of a recursive mutex; -1 briefly after the
    /// kernel handed over a priority-inheriting one.
    pub count: AtomicI32,
    /// `_m_prev`: the previous entry's `next` field, or the list head.
    pub prev: AtomicPtr<c_void>,
    /// `_m_next`: the next entry's `next` field, or the list head. This is
    /// the kernel's `struct robust_list`.
    pub next: AtomicPtr<c_void>,
}

const _: () = assert!(size_of::<Mutex>() == 40);
const _: () = assert!(offset_of!(Mutex, count) == 20);
const _: () = assert!(offset_of!(Mutex, prev) == 24);
const _: () = assert!(offset_of!(Mutex, next) == 32);

/// The distance from a mutex's `next` field to its lock word, which the
/// kernel's robust list handling is told.
const ROBUST_OFFSET: isize = offset_of!(Mutex, lock) as isize - offset_of!(Mutex, next) as isize;

impl Mutex {
    /// An unlocked mutex of `kind`.
    pub const fn new(kind: c_int) -> Self {
        Self {
            kind: AtomicI32::new(kind),
            lock: AtomicI32::new(0),
            waiters: AtomicI32::new(0),
            spare: [const { AtomicI32::new(0) }; 2],
            count: AtomicI32::new(0),
            prev: AtomicPtr::new(null_mut()),
            next: AtomicPtr::new(null_mut()),
        }
    }

    /// The address of this mutex's `next` field, which is what robust lists
    /// link.
    fn node(&self) -> *mut c_void {
        (&raw const self.next).cast_mut().cast()
    }
}

/// Whether a mutex of `kind` uses a private futex.
const fn private(kind: c_int) -> bool {
    kind & SHARED == 0
}

/// The private flag for a raw `futex` call on a mutex of `kind`.
const fn private_flag(kind: c_int) -> usize {
    if private(kind) {
        futex::FUTEX_PRIVATE
    } else {
        0
    }
}

/// The list link at `node`: the head or some mutex's `next` field.
///
/// # Safety
///
/// `node` must be a live list link.
unsafe fn link<'a>(node: *mut c_void) -> &'a AtomicPtr<c_void> {
    // SAFETY: the caller vouches for the link, which is an atomic pointer.
    unsafe { &*node.cast::<AtomicPtr<c_void>>() }
}

/// The `prev` field of the mutex whose `next` field is at `node`.
///
/// # Safety
///
/// `node` must be a mutex's `next` field.
unsafe fn prev_of<'a>(node: *mut c_void) -> &'a AtomicPtr<c_void> {
    // SAFETY: `prev` is the word before `next`, and atomic.
    unsafe { &*node.cast::<AtomicPtr<c_void>>().wrapping_sub(1) }
}

/// Makes a `futex` call on `m`'s lock word with `op` and a timeout.
fn futex_call(m: &Mutex, op: usize, at: *const Timespec) -> Result<(), c_int> {
    // SAFETY: the kernel reads and may write the live lock word, and reads
    // the timeout, which the caller made null or valid.
    let ret = unsafe {
        syscall::syscall4(
            nr::FUTEX,
            m.lock.as_ptr().addr(),
            op | private_flag(m.kind.load(Ordering::SeqCst)),
            0,
            at.addr(),
        )
    };
    errno::decode(ret).map(|_| ())
}

/// Tries to take a mutex that records its owner. musl's
/// `__pthread_mutex_trylock_owner`.
fn trylock_owner(m: &Mutex) -> c_int {
    let kind = m.kind.load(Ordering::SeqCst);
    let me = thread::me();
    let mut tid = thread::my_tid();
    let mut old = m.lock.load(Ordering::SeqCst);
    let own = old & TID_MASK;

    let handed_over = own == tid && kind & PRIO_INHERIT != 0 && m.count.load(Ordering::SeqCst) < 0;
    if handed_over {
        // The kernel gave this thread the lock in `timedlock_pi`.
        old &= OWNER_DIED;
        m.count.store(0, Ordering::SeqCst);
    } else {
        if own == tid && kind & TYPE_MASK == RECURSIVE {
            let count = m.count.load(Ordering::SeqCst);
            if count as c_uint >= c_int::MAX as c_uint {
                return errno::EAGAIN;
            }
            m.count.store(count + 1, Ordering::SeqCst);
            return 0;
        }
        if own == UNRECOVERABLE {
            return errno::ENOTRECOVERABLE;
        }
        if own != 0 || (old != 0 && kind & ROBUST == 0) {
            return errno::EBUSY;
        }
        if kind & SHARED != 0 {
            if me.robust_off.load(Ordering::SeqCst) == 0 {
                me.robust_off.store(ROBUST_OFFSET, Ordering::SeqCst);
                // SAFETY: the head is in this thread's control block, which
                // lives as long as the thread the kernel attaches it to.
                let _ = unsafe {
                    syscall::syscall2(
                        nr::SET_ROBUST_LIST,
                        me.robust_head_address().addr(),
                        3 * size_of::<usize>(),
                    )
                };
            }
            if m.waiters.load(Ordering::SeqCst) != 0 {
                tid |= WAITERS;
            }
            me.robust_pending.store(m.node(), Ordering::SeqCst);
        }
        tid |= old & OWNER_DIED;
        if futex::cas(&m.lock, old, tid) != old {
            me.robust_pending.store(null_mut(), Ordering::SeqCst);
            if kind & (ROBUST | PRIO_INHERIT) == ROBUST | PRIO_INHERIT
                && m.waiters.load(Ordering::SeqCst) != 0
            {
                return errno::ENOTRECOVERABLE;
            }
            return errno::EBUSY;
        }
    }

    if kind & PRIO_INHERIT != 0 && m.waiters.load(Ordering::SeqCst) != 0 {
        // An unlock marked it unrecoverable while the kernel handed it over.
        let _ = futex_call(m, futex::FUTEX_UNLOCK_PI, core::ptr::null());
        me.robust_pending.store(null_mut(), Ordering::SeqCst);
        return if kind & ROBUST != 0 {
            errno::ENOTRECOVERABLE
        } else {
            errno::EBUSY
        };
    }

    let head = me.robust_head_address();
    let next = me.robust_head.load(Ordering::SeqCst);
    m.next.store(next, Ordering::SeqCst);
    m.prev.store(head, Ordering::SeqCst);
    if next != head {
        // SAFETY: a list entry other than the head is a held mutex's `next`.
        unsafe { prev_of(next) }.store(m.node(), Ordering::SeqCst);
    }
    me.robust_head.store(m.node(), Ordering::SeqCst);
    me.robust_pending.store(null_mut(), Ordering::SeqCst);

    if old != 0 {
        m.count.store(0, Ordering::SeqCst);
        return errno::EOWNERDEAD;
    }
    0
}

/// Tries to take `m` without waiting.
pub fn trylock(m: &Mutex) -> c_int {
    if m.kind.load(Ordering::SeqCst) & 15 == NORMAL {
        return futex::cas(&m.lock, 0, errno::EBUSY) & errno::EBUSY;
    }
    trylock_owner(m)
}

/// Waits for a priority-inheriting mutex through the kernel.
///
/// # Safety
///
/// `at` must be null or valid for a read of a `struct timespec`.
unsafe fn timedlock_pi(m: &Mutex, at: *const Timespec) -> c_int {
    let kind = m.kind.load(Ordering::SeqCst);
    let me = thread::me();
    if !private(kind) {
        me.robust_pending.store(m.node(), Ordering::SeqCst);
    }
    let mut e = loop {
        match futex_call(m, futex::FUTEX_LOCK_PI, at) {
            Err(errno::EINTR) => {}
            Err(error) => break error,
            Ok(()) => break 0,
        }
    };
    if e != 0 {
        me.robust_pending.store(null_mut(), Ordering::SeqCst);
    }
    match e {
        0 => {
            // A spurious success on a mutex that is not robust but whose
            // owner died, or was marked unrecoverable: give it back and wait
            // for ever, as a deadlock would.
            if kind & ROBUST == 0
                && (m.lock.load(Ordering::SeqCst) & OWNER_DIED != 0
                    || m.waiters.load(Ordering::SeqCst) != 0)
            {
                m.waiters.store(-1, Ordering::SeqCst);
                let _ = futex_call(m, futex::FUTEX_UNLOCK_PI, core::ptr::null());
                me.robust_pending.store(null_mut(), Ordering::SeqCst);
            } else {
                // Tell `trylock_owner` the kernel handed it over.
                m.count.store(-1, Ordering::SeqCst);
                return trylock(m);
            }
        }
        errno::ETIMEDOUT => return e,
        errno::EDEADLK if kind & TYPE_MASK == ERRORCHECK => return e,
        _ => {}
    }
    // Deadlocked: wait until the time runs out, or for ever.
    let never = AtomicI32::new(0);
    loop {
        // SAFETY: the caller vouches for `at`.
        e = unsafe { futex::timedwait(&never, 0, CLOCK_REALTIME, at, true) };
        if e == errno::ETIMEDOUT {
            return e;
        }
    }
}

/// Takes `m`, waiting until the absolute time `at` on `CLOCK_REALTIME` if it
/// is not null.
///
/// # Safety
///
/// `at` must be null or valid for a read of a `struct timespec`.
pub unsafe fn timedlock(m: &Mutex, at: *const Timespec) -> c_int {
    let kind = m.kind.load(Ordering::SeqCst);
    if kind & 15 == NORMAL && futex::cas(&m.lock, 0, errno::EBUSY) == 0 {
        return 0;
    }
    let mut r = trylock(m);
    if r != errno::EBUSY {
        return r;
    }
    if kind & PRIO_INHERIT != 0 {
        // SAFETY: the caller vouches for `at`.
        return unsafe { timedlock_pi(m, at) };
    }
    let mut spins = 100;
    while spins > 0 && m.lock.load(Ordering::SeqCst) != 0 && m.waiters.load(Ordering::SeqCst) == 0 {
        core::hint::spin_loop();
        spins -= 1;
    }
    loop {
        r = trylock(m);
        if r != errno::EBUSY {
            return r;
        }
        let value = m.lock.load(Ordering::SeqCst);
        let own = value & TID_MASK;
        if own == 0 && (value == 0 || kind & ROBUST != 0) {
            continue;
        }
        if kind & TYPE_MASK == ERRORCHECK && own == thread::my_tid() {
            return errno::EDEADLK;
        }
        let _ = m.waiters.fetch_add(1, Ordering::SeqCst);
        let flagged = value | WAITERS;
        let _ = futex::cas(&m.lock, value, flagged);
        // SAFETY: the caller vouches for `at`.
        r = unsafe { futex::timedwait(&m.lock, flagged, CLOCK_REALTIME, at, private(kind)) };
        let _ = m.waiters.fetch_sub(1, Ordering::SeqCst);
        if r != 0 && r != errno::EINTR {
            return r;
        }
    }
}

/// Takes `m`, waiting as long as it takes.
pub fn lock(m: &Mutex) -> c_int {
    // SAFETY: there is no time limit.
    unsafe { timedlock(m, core::ptr::null()) }
}

/// Releases `m`.
pub fn unlock(m: &Mutex) -> c_int {
    let waiters = m.waiters.load(Ordering::SeqCst);
    let kind_word = m.kind.load(Ordering::SeqCst);
    let kind = kind_word & 15;
    let private = private(kind_word);
    let mut new = 0;
    let mut old = 0;
    if kind != NORMAL {
        let me = thread::me();
        old = m.lock.load(Ordering::SeqCst);
        if old & TID_MASK != thread::my_tid() {
            return errno::EPERM;
        }
        if kind & TYPE_MASK == RECURSIVE && m.count.load(Ordering::SeqCst) != 0 {
            let _ = m.count.fetch_sub(1, Ordering::SeqCst);
            return 0;
        }
        if kind & ROBUST != 0 && old & OWNER_DIED != 0 {
            // Unlocked without `pthread_mutex_consistent`: unrecoverable.
            new = 0x7fff_ffff;
        }
        if !private {
            me.robust_pending.store(m.node(), Ordering::SeqCst);
        }
        let prev = m.prev.load(Ordering::SeqCst);
        let next = m.next.load(Ordering::SeqCst);
        // SAFETY: a held mutex's neighbours are live links in the owner's list.
        unsafe { link(prev) }.store(next, Ordering::SeqCst);
        if next != me.robust_head_address() {
            // SAFETY: as above; `next` is another held mutex's `next` field.
            unsafe { prev_of(next) }.store(prev, Ordering::SeqCst);
        }
    }
    let (cont, waiters) = if kind & PRIO_INHERIT != 0 {
        if old < 0 || futex::cas(&m.lock, old, new) != old {
            if new != 0 {
                m.waiters.store(-1, Ordering::SeqCst);
            }
            let _ = futex_call(m, futex::FUTEX_UNLOCK_PI, core::ptr::null());
        }
        (0, 0)
    } else {
        (m.lock.swap(new, Ordering::SeqCst), waiters)
    };
    if kind != NORMAL && !private {
        thread::me()
            .robust_pending
            .store(null_mut(), Ordering::SeqCst);
    }
    if waiters != 0 || cont < 0 {
        futex::wake(&raw const m.lock, 1, private);
    }
    0
}

/// Marks every mutex on an exiting thread's robust list as its owner having
/// died, and wakes one waiter on each. `pthread_exit` calls this.
pub fn abandon_robust_list(state: &State) {
    let head = state.robust_head_address();
    loop {
        let node = state.robust_head.load(Ordering::SeqCst);
        if node.is_null() || node == head {
            return;
        }
        // SAFETY: an entry on the list is a held mutex's `next` field.
        let m = unsafe {
            &*node
                .wrapping_byte_sub(offset_of!(Mutex, next))
                .cast::<Mutex>()
        };
        let waiters = m.waiters.load(Ordering::SeqCst);
        let private = private(m.kind.load(Ordering::SeqCst));
        state.robust_pending.store(node, Ordering::SeqCst);
        state
            .robust_head
            .store(m.next.load(Ordering::SeqCst), Ordering::SeqCst);
        let cont = m.lock.swap(OWNER_DIED, Ordering::SeqCst);
        state.robust_pending.store(null_mut(), Ordering::SeqCst);
        if cont < 0 || waiters != 0 {
            futex::wake(&raw const m.lock, 1, private);
        }
    }
}

/// `pthread_mutexattr_t`: the kind word a mutex copies.
#[repr(C)]
#[derive(Debug)]
pub struct MutexAttr {
    kind: c_uint,
}

/// Initialises `*m` with the kind `*attr` describes, or a normal mutex if
/// `attr` is null.
///
/// # Safety
///
/// `m` must be valid for a write of a `pthread_mutex_t`, and `attr` null or
/// an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutex_init(m: *mut Mutex, attr: *const MutexAttr) -> c_int {
    // SAFETY: the caller passes null or an initialised attribute object.
    let kind = unsafe { attr.as_ref() }.map_or(0, |attr| attr.kind as c_int);
    // SAFETY: the caller vouches for `m`.
    unsafe { m.write(Mutex::new(kind)) };
    0
}

/// Destroys `*m`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_mutex_destroy(_m: *mut Mutex) -> c_int {
    0
}

/// Takes `*m`, waiting for it.
///
/// A normal mutex locked again by its owner deadlocks, as POSIX says.
/// An error-checking one fails with `EDEADLK`; a recursive one counts. A
/// robust mutex whose owner died is taken with `EOWNERDEAD`, and one left
/// inconsistent fails with `ENOTRECOVERABLE`.
///
/// # Safety
///
/// `m` must be an initialised mutex.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutex_lock(m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for the mutex, which is all atomics.
    lock(unsafe { &*m })
}

/// Takes `*m` if nobody holds it, or fails with `EBUSY`.
///
/// # Safety
///
/// `m` must be an initialised mutex.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutex_trylock(m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    trylock(unsafe { &*m })
}

/// Takes `*m`, waiting until the absolute time `*at` on `CLOCK_REALTIME` at
/// most, then failing with `ETIMEDOUT`.
///
/// # Safety
///
/// `m` must be an initialised mutex, and `at` valid for a read of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutex_timedlock(m: *mut Mutex, at: *const Timespec) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    let m = unsafe { &*m };
    // SAFETY: the caller vouches for `at`.
    unsafe { timedlock(m, at) }
}

/// Releases `*m`. A mutex that records its owner fails with `EPERM` when the
/// caller does not hold it.
///
/// # Safety
///
/// `m` must be an initialised mutex.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutex_unlock(m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    unlock(unsafe { &*m })
}

/// Marks a robust mutex the caller took with `EOWNERDEAD` as consistent
/// again. Fails with `EINVAL` if it is not robust or not in that state, and
/// `EPERM` if the caller does not hold it.
///
/// # Safety
///
/// `m` must be an initialised mutex.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutex_consistent(m: *mut Mutex) -> c_int {
    // SAFETY: the caller vouches for the mutex.
    let m = unsafe { &*m };
    let old = m.lock.load(Ordering::SeqCst);
    let own = old & TID_MASK;
    if m.kind.load(Ordering::SeqCst) & ROBUST == 0 || own == 0 || old & OWNER_DIED == 0 {
        return errno::EINVAL;
    }
    if own != thread::my_tid() {
        return errno::EPERM;
    }
    let _ = m.lock.fetch_and(!OWNER_DIED, Ordering::SeqCst);
    0
}

/// Priority ceilings need `PTHREAD_PRIO_PROTECT`, which is not supported:
/// fails with `EINVAL`, as in musl.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_mutex_getprioceiling(_m: *const Mutex, _ceiling: *mut c_int) -> c_int {
    errno::EINVAL
}

/// As [`pthread_mutex_getprioceiling`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_mutex_setprioceiling(
    _m: *mut Mutex,
    _ceiling: c_int,
    _old: *mut c_int,
) -> c_int {
    errno::EINVAL
}

/// Initialises `*attr`: a normal, stalled, private mutex with no priority
/// protocol.
///
/// # Safety
///
/// `attr` must be valid for a write of a `pthread_mutexattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_init(attr: *mut MutexAttr) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    unsafe { attr.write(MutexAttr { kind: 0 }) };
    0
}

/// Destroys `*attr`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_mutexattr_destroy(_attr: *mut MutexAttr) -> c_int {
    0
}

/// Reads the bits of `*attr` under `mask` shifted down by `shift` into
/// `*out`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `out` valid for a write
/// of an `int`.
unsafe fn get_bits(attr: *const MutexAttr, mask: c_uint, shift: u32, out: *mut c_int) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let kind = unsafe { (*attr).kind };
    // SAFETY: the caller vouches for `out`.
    unsafe { out.write(((kind & mask) >> shift) as c_int) };
    0
}

/// Replaces the bits of `*attr` under `mask` with `value` shifted up by
/// `shift`, if `value` is at most `max`, or fails with `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
unsafe fn set_bits(
    attr: *mut MutexAttr,
    mask: c_uint,
    shift: u32,
    value: c_int,
    max: c_int,
) -> c_int {
    if !(0..=max).contains(&value) {
        return errno::EINVAL;
    }
    // SAFETY: the caller vouches for `attr`.
    let attr = unsafe { &mut *attr };
    attr.kind = (attr.kind & !mask) | ((value as c_uint) << shift);
    0
}

/// Stores the mutex type in `*kind`.
///
/// # Safety
///
/// As [`get_bits`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_gettype(
    attr: *const MutexAttr,
    kind: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { get_bits(attr, 3, 0, kind) }
}

/// Sets the mutex type: normal, recursive or error-checking. Anything else
/// fails with `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_settype(attr: *mut MutexAttr, kind: c_int) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { set_bits(attr, 3, 0, kind, ERRORCHECK) }
}

/// Stores whether mutexes are robust in `*robust`.
///
/// # Safety
///
/// As [`get_bits`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_getrobust(
    attr: *const MutexAttr,
    robust: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { get_bits(attr, ROBUST as c_uint, 2, robust) }
}

/// Sets whether mutexes are robust: `PTHREAD_MUTEX_STALLED` or
/// `PTHREAD_MUTEX_ROBUST`. Anything else fails with `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_setrobust(attr: *mut MutexAttr, robust: c_int) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { set_bits(attr, ROBUST as c_uint, 2, robust, 1) }
}

/// Stores whether mutexes are process-shared in `*shared`.
///
/// # Safety
///
/// As [`get_bits`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_getpshared(
    attr: *const MutexAttr,
    shared: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { get_bits(attr, SHARED as c_uint, 7, shared) }
}

/// Sets whether mutexes are process-shared. Anything but 0 or 1 fails with
/// `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_setpshared(
    attr: *mut MutexAttr,
    shared: c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { set_bits(attr, SHARED as c_uint, 7, shared, 1) }
}

/// Stores the priority protocol in `*protocol`.
///
/// # Safety
///
/// As [`get_bits`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_getprotocol(
    attr: *const MutexAttr,
    protocol: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { get_bits(attr, PRIO_INHERIT as c_uint, 3, protocol) }
}

/// Sets the priority protocol: `PTHREAD_PRIO_NONE` or `PTHREAD_PRIO_INHERIT`.
/// `PTHREAD_PRIO_PROTECT` fails with `ENOTSUP`, and anything else with
/// `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_setprotocol(
    attr: *mut MutexAttr,
    protocol: c_int,
) -> c_int {
    match protocol {
        PRIO_NONE | PRIO_INHERIT_PROTOCOL => {
            // SAFETY: the caller's contract.
            unsafe { set_bits(attr, PRIO_INHERIT as c_uint, 3, protocol, 1) }
        }
        PRIO_PROTECT => errno::ENOTSUP,
        _ => errno::EINVAL,
    }
}

/// Stores the priority ceiling, which is always 0 since
/// `PTHREAD_PRIO_PROTECT` is not supported.
///
/// # Safety
///
/// `ceiling` must be valid for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_mutexattr_getprioceiling(
    _attr: *const MutexAttr,
    ceiling: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `ceiling`.
    unsafe { ceiling.write(0) };
    0
}

/// Accepts a priority ceiling in `SCHED_FIFO`'s range, 1 to 99, and ignores
/// it, since `PTHREAD_PRIO_PROTECT` is not supported. Anything else fails
/// with `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_mutexattr_setprioceiling(_attr: *mut MutexAttr, ceiling: c_int) -> c_int {
    if (1..=99).contains(&ceiling) {
        0
    } else {
        errno::EINVAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_mutex_is_busy_while_held() {
        let m = Mutex::new(NORMAL);
        assert_eq!(trylock(&m), 0);
        assert_eq!(trylock(&m), errno::EBUSY);
        assert_eq!(unlock(&m), 0);
        assert_eq!(trylock(&m), 0);
    }

    #[test]
    fn attribute_bits_round_trip_and_bad_values_are_refused() {
        let mut attr = MutexAttr { kind: 0 };
        let mut out = -1;
        // SAFETY: `attr` is a live local, here and below.
        let ret = unsafe { pthread_mutexattr_settype(&raw mut attr, RECURSIVE) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { pthread_mutexattr_setrobust(&raw mut attr, 1) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { pthread_mutexattr_setpshared(&raw mut attr, 1) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { pthread_mutexattr_setprotocol(&raw mut attr, PRIO_INHERIT_PROTOCOL) };
        assert_eq!(ret, 0);
        assert_eq!(attr.kind, 1 | 4 | 8 | 128);
        // SAFETY: as above, and `out` is a live local.
        let ret = unsafe { pthread_mutexattr_gettype(&raw const attr, &raw mut out) };
        assert_eq!(ret, 0);
        assert_eq!(out, RECURSIVE);
        // SAFETY: as above.
        let ret = unsafe { pthread_mutexattr_settype(&raw mut attr, 3) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: as above.
        let ret = unsafe { pthread_mutexattr_setrobust(&raw mut attr, 2) };
        assert_eq!(ret, errno::EINVAL);
        // SAFETY: as above.
        let ret = unsafe { pthread_mutexattr_setprotocol(&raw mut attr, PRIO_PROTECT) };
        assert_eq!(ret, errno::ENOTSUP);
        assert_eq!(ROBUST_OFFSET, -28);
    }
}
