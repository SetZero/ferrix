//! The lock each stream carries, which `flockfile` exposes.
//!
//! C lets a thread take a stream's lock with `flockfile` and then call
//! functions that take it again, so the lock is recursive: it records its
//! owner and how many times the owner took it. The owner is named by its
//! thread pointer, which is unique to each thread and is read without a system
//! call.
//!
//! Today the program has one thread, and the lock is never contended. A thread
//! that finds it held spins and yields; when threads arrive, that waiting
//! becomes a futex.

use core::hint::spin_loop;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::syscall::{self, nr};

/// A lock that its owner may take again, and must release as often.
#[derive(Debug)]
pub struct RecursiveLock {
    /// The owning thread's pointer, or zero while unowned.
    owner: AtomicUsize,
    /// How many times the owner holds it.
    count: AtomicUsize,
}

/// A value that no thread pointer takes.
const UNOWNED: usize = 0;

/// The calling thread's identity: its thread pointer.
#[cfg(target_arch = "x86_64")]
fn thread_id() -> usize {
    let id: usize;
    // SAFETY: `%fs:0` holds the thread pointer itself, in this library's
    // control block and in the host C library's under unit tests.
    unsafe {
        core::arch::asm!(
            "mov {id}, qword ptr fs:[0]",
            id = out(reg) id,
            options(nostack, readonly, preserves_flags),
        );
    }
    id
}

/// The calling thread's identity: its thread pointer, read from the register
/// that holds it.
#[cfg(not(target_arch = "x86_64"))]
fn thread_id() -> usize {
    crate::arch::thread_pointer()
}

impl RecursiveLock {
    /// An unowned lock.
    pub const fn new() -> Self {
        Self {
            owner: AtomicUsize::new(UNOWNED),
            count: AtomicUsize::new(0),
        }
    }

    /// Takes the lock, waiting if another thread holds it.
    pub fn lock(&self) {
        let me = thread_id();
        if self.owner.load(Ordering::Relaxed) == me {
            let _ = self.count.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut spins = 0_u32;
        while self
            .owner
            .compare_exchange_weak(UNOWNED, me, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spins = spins.wrapping_add(1);
            if spins.is_multiple_of(64) {
                // SAFETY: `sched_yield` takes no arguments and cannot fail.
                let _ = unsafe { syscall::syscall0(nr::SCHED_YIELD) };
            } else {
                spin_loop();
            }
        }
        self.count.store(1, Ordering::Relaxed);
    }

    /// Takes the lock if no other thread holds it, and says whether it did.
    pub fn try_lock(&self) -> bool {
        let me = thread_id();
        if self.owner.load(Ordering::Relaxed) == me {
            let _ = self.count.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        self.try_lock_unowned()
    }

    /// Takes the lock only if nobody holds it, the calling thread included.
    pub fn try_lock_unowned(&self) -> bool {
        let me = thread_id();
        if self
            .owner
            .compare_exchange(UNOWNED, me, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        self.count.store(1, Ordering::Relaxed);
        true
    }

    /// Releases the lock once. A thread that does not hold it changes nothing.
    pub fn unlock(&self) {
        if self.owner.load(Ordering::Relaxed) != thread_id() {
            return;
        }
        let count = self.count.load(Ordering::Relaxed).saturating_sub(1);
        self.count.store(count, Ordering::Relaxed);
        if count == 0 {
            self.owner.store(UNOWNED, Ordering::Release);
        }
    }
}

impl Default for RecursiveLock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_may_take_it_again_and_releases_it_as_often() {
        let lock = RecursiveLock::new();
        lock.lock();
        assert!(lock.try_lock());
        assert!(!lock.try_lock_unowned());
        lock.unlock();
        assert_ne!(lock.owner.load(Ordering::Relaxed), UNOWNED);
        lock.unlock();
        assert_eq!(lock.owner.load(Ordering::Relaxed), UNOWNED);
        lock.unlock();
        assert_eq!(lock.count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn another_thread_cannot_take_a_held_lock() {
        let lock = RecursiveLock::new();
        lock.lock();
        std::thread::scope(|scope| {
            let other = scope.spawn(|| lock.try_lock());
            assert_eq!(other.join().ok(), Some(false));
        });
        lock.unlock();
    }
}
