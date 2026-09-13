//! `pthread.h`'s read-write locks: `pthread_rwlock_t` and its attributes.
//!
//! Adapted from musl (MIT). The lock word counts readers, or holds
//! `0x7fffffff` for a writer, with the sign bit set while anyone sleeps on it.
//! A second word counts sleepers. Releasing the last hold wakes every sleeper
//! if there are any, and they race again. Like musl's, the lock prefers
//! neither readers nor writers.

use core::ffi::{c_int, c_uint};
use core::mem::size_of;
use core::sync::atomic::{AtomicI32, Ordering};

use crate::errno;
use crate::futex;
use crate::time::{CLOCK_REALTIME, Timespec};

/// The lock word of a lock a writer holds.
const WRITER: c_int = 0x7fff_ffff;
/// The most readers a lock counts.
const MAX_READERS: c_int = 0x7fff_fffe;
/// `_rw_shared` for a process-shared lock.
const SHARED: c_int = 128;

/// `pthread_rwlock_t`, in musl's layout: the lock word, the sleepers, and
/// whether it is process-shared, in 56 bytes.
#[repr(C)]
#[derive(Debug)]
pub struct Rwlock {
    lock: AtomicI32,
    waiters: AtomicI32,
    shared: AtomicI32,
    rest: [AtomicI32; 11],
    align: [usize; 0],
}

const _: () = assert!(size_of::<Rwlock>() == 56);

impl Rwlock {
    /// An unlocked, process-private lock: `PTHREAD_RWLOCK_INITIALIZER`.
    pub const fn new() -> Self {
        Self {
            lock: AtomicI32::new(0),
            waiters: AtomicI32::new(0),
            shared: AtomicI32::new(0),
            rest: [const { AtomicI32::new(0) }; 11],
            align: [],
        }
    }

    /// Whether its futex is private to this process.
    fn private(&self) -> bool {
        self.shared.load(Ordering::SeqCst) != SHARED
    }

    /// Takes a read hold, or fails with `EBUSY` if a writer holds it, or
    /// `EAGAIN` if it has too many readers.
    pub fn try_read(&self) -> c_int {
        loop {
            let value = self.lock.load(Ordering::SeqCst);
            let count = value & WRITER;
            if count == WRITER {
                return errno::EBUSY;
            }
            if count == MAX_READERS {
                return errno::EAGAIN;
            }
            if futex::cas(&self.lock, value, value + 1) == value {
                return 0;
            }
        }
    }

    /// Takes the write hold, or fails with `EBUSY` if anyone holds it.
    pub fn try_write(&self) -> c_int {
        if futex::cas(&self.lock, 0, WRITER) == 0 {
            0
        } else {
            errno::EBUSY
        }
    }

    /// Waits for a hold with `take`, until the absolute time `at` if not
    /// null. `blocks` says whether a lock word means waiting.
    ///
    /// # Safety
    ///
    /// `at` must be null or valid for a read of a `struct timespec`.
    unsafe fn wait(
        &self,
        take: fn(&Self) -> c_int,
        blocks: fn(c_int) -> bool,
        at: *const Timespec,
    ) -> c_int {
        let mut r = take(self);
        if r != errno::EBUSY {
            return r;
        }
        let mut spins = 100;
        while spins > 0
            && self.lock.load(Ordering::SeqCst) != 0
            && self.waiters.load(Ordering::SeqCst) == 0
        {
            core::hint::spin_loop();
            spins -= 1;
        }
        loop {
            r = take(self);
            if r != errno::EBUSY {
                return r;
            }
            let value = self.lock.load(Ordering::SeqCst);
            if !blocks(value) {
                continue;
            }
            let flagged = value | futex::WAITERS;
            let _ = self.waiters.fetch_add(1, Ordering::SeqCst);
            let _ = futex::cas(&self.lock, value, flagged);
            // SAFETY: the caller vouches for `at`.
            r = unsafe {
                futex::timedwait(&self.lock, flagged, CLOCK_REALTIME, at, self.private())
            };
            let _ = self.waiters.fetch_sub(1, Ordering::SeqCst);
            if r != 0 && r != errno::EINTR {
                return r;
            }
        }
    }

    /// Waits for a read hold, until the absolute time `at` if not null.
    ///
    /// # Safety
    ///
    /// `at` must be null or valid for a read of a `struct timespec`.
    pub unsafe fn read(&self, at: *const Timespec) -> c_int {
        // A reader waits only for a writer.
        // SAFETY: the caller vouches for `at`.
        unsafe { self.wait(Self::try_read, |v| v != 0 && v & WRITER == WRITER, at) }
    }

    /// Waits for the write hold, until the absolute time `at` if not null.
    ///
    /// # Safety
    ///
    /// `at` must be null or valid for a read of a `struct timespec`.
    pub unsafe fn write(&self, at: *const Timespec) -> c_int {
        // SAFETY: the caller vouches for `at`.
        unsafe { self.wait(Self::try_write, |v| v != 0, at) }
    }

    /// Releases one hold, waking every sleeper once the lock is free.
    pub fn unlock(&self) -> c_int {
        let (value, count, waiters, new) = loop {
            let value = self.lock.load(Ordering::SeqCst);
            let count = value & WRITER;
            let waiters = self.waiters.load(Ordering::SeqCst);
            let new = if count == WRITER || count == 1 {
                0
            } else {
                value - 1
            };
            if futex::cas(&self.lock, value, new) == value {
                break (value, count, waiters, new);
            }
        };
        if new == 0 && (waiters != 0 || value < 0) {
            futex::wake(&raw const self.lock, count, self.private());
        }
        0
    }
}

impl Default for Rwlock {
    fn default() -> Self {
        Self::new()
    }
}

/// `pthread_rwlockattr_t`: two words of `unsigned`, of which the first is the
/// process-shared flag.
#[repr(C)]
#[derive(Debug)]
pub struct RwlockAttr {
    shared: c_uint,
    reserved: c_uint,
}

/// Initialises `*rw`, process-shared if `*attr` says so.
///
/// # Safety
///
/// `rw` must be valid for a write of a `pthread_rwlock_t`, and `attr` null or
/// an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_init(rw: *mut Rwlock, attr: *const RwlockAttr) -> c_int {
    // SAFETY: the caller vouches for `rw`.
    unsafe { rw.write(Rwlock::new()) };
    // SAFETY: the caller passes null or an initialised attribute object.
    if let Some(attr) = unsafe { attr.as_ref() }
        && attr.shared != 0
    {
        // SAFETY: just written.
        unsafe { (*rw).shared.store(SHARED, Ordering::SeqCst) };
    }
    0
}

/// Destroys `*rw`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_rwlock_destroy(_rw: *mut Rwlock) -> c_int {
    0
}

/// Takes a read hold on `*rw`, waiting for any writer.
///
/// # Safety
///
/// `rw` must be an initialised lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_rdlock(rw: *mut Rwlock) -> c_int {
    // SAFETY: the caller vouches for the lock.
    let rw = unsafe { &*rw };
    // SAFETY: the caller vouches for the lock, and there is no time limit.
    unsafe { rw.read(core::ptr::null()) }
}

/// Takes a read hold on `*rw`, or fails with `EBUSY` if a writer holds it.
///
/// # Safety
///
/// `rw` must be an initialised lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_tryrdlock(rw: *mut Rwlock) -> c_int {
    // SAFETY: the caller vouches for the lock.
    unsafe { &*rw }.try_read()
}

/// Takes a read hold on `*rw`, waiting until the absolute time `*at` on
/// `CLOCK_REALTIME` at most, then failing with `ETIMEDOUT`.
///
/// # Safety
///
/// `rw` must be an initialised lock and `at` valid for a read of a
/// `struct timespec`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_timedrdlock(rw: *mut Rwlock, at: *const Timespec) -> c_int {
    // SAFETY: the caller vouches for the lock.
    let rw = unsafe { &*rw };
    // SAFETY: the caller vouches for both.
    unsafe { rw.read(at) }
}

/// Takes the write hold on `*rw`, waiting for every other holder.
///
/// # Safety
///
/// `rw` must be an initialised lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_wrlock(rw: *mut Rwlock) -> c_int {
    // SAFETY: the caller vouches for the lock.
    let rw = unsafe { &*rw };
    // SAFETY: the caller vouches for the lock, and there is no time limit.
    unsafe { rw.write(core::ptr::null()) }
}

/// Takes the write hold on `*rw`, or fails with `EBUSY` if anyone holds it.
///
/// # Safety
///
/// `rw` must be an initialised lock.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_trywrlock(rw: *mut Rwlock) -> c_int {
    // SAFETY: the caller vouches for the lock.
    unsafe { &*rw }.try_write()
}

/// Takes the write hold on `*rw`, waiting until the absolute time `*at` on
/// `CLOCK_REALTIME` at most, then failing with `ETIMEDOUT`.
///
/// # Safety
///
/// As [`pthread_rwlock_timedrdlock`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_timedwrlock(rw: *mut Rwlock, at: *const Timespec) -> c_int {
    // SAFETY: the caller vouches for the lock.
    let rw = unsafe { &*rw };
    // SAFETY: the caller vouches for both.
    unsafe { rw.write(at) }
}

/// Releases the calling thread's hold on `*rw`.
///
/// # Safety
///
/// `rw` must be an initialised lock the caller holds.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlock_unlock(rw: *mut Rwlock) -> c_int {
    // SAFETY: the caller vouches for the lock.
    unsafe { &*rw }.unlock()
}

/// Initialises `*attr`: process-private.
///
/// # Safety
///
/// `attr` must be valid for a write of a `pthread_rwlockattr_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlockattr_init(attr: *mut RwlockAttr) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    unsafe {
        attr.write(RwlockAttr {
            shared: 0,
            reserved: 0,
        });
    }
    0
}

/// Destroys `*attr`, which holds nothing to free.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pthread_rwlockattr_destroy(_attr: *mut RwlockAttr) -> c_int {
    0
}

/// Stores whether locks are process-shared in `*shared`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object and `shared` valid for a
/// write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlockattr_getpshared(
    attr: *const RwlockAttr,
    shared: *mut c_int,
) -> c_int {
    // SAFETY: the caller vouches for `attr`.
    let value = unsafe { (*attr).shared as c_int };
    // SAFETY: the caller vouches for `shared`.
    unsafe { shared.write(value) };
    0
}

/// Sets whether locks are process-shared: `PTHREAD_PROCESS_PRIVATE` or
/// `PTHREAD_PROCESS_SHARED`. Anything else fails with `EINVAL`.
///
/// # Safety
///
/// `attr` must be an initialised attribute object.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_rwlockattr_setpshared(
    attr: *mut RwlockAttr,
    shared: c_int,
) -> c_int {
    let Ok(shared @ 0..=1) = c_uint::try_from(shared) else {
        return errno::EINVAL;
    };
    // SAFETY: the caller vouches for `attr`.
    unsafe { (*attr).shared = shared };
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_share_and_a_writer_excludes() {
        let rw = Rwlock::new();
        assert_eq!(rw.try_read(), 0);
        assert_eq!(rw.try_read(), 0);
        assert_eq!(rw.try_write(), errno::EBUSY);
        assert_eq!(rw.unlock(), 0);
        assert_eq!(rw.unlock(), 0);
        assert_eq!(rw.try_write(), 0);
        assert_eq!(rw.try_read(), errno::EBUSY);
        assert_eq!(rw.unlock(), 0);
        assert_eq!(rw.lock.load(Ordering::SeqCst), 0);
    }
}
