//! The lock for the process-wide state the library keeps: the allocator, the
//! exit handlers, the thread-specific keys.
//!
//! It is musl's `__lock` (MIT): one `int` holding a flag and a count. The sign
//! bit says the lock is held, and the rest counts the threads inside the
//! critical section or waiting for it, the holder included:
//!
//! * `0`: free, and nobody is waiting.
//! * `x < 0`: held, with `x - INT_MIN` threads counted.
//! * `x > 0`: free, with `x` threads about to take it.
//!
//! A thread that finds the lock held spins briefly, then counts itself in and
//! sleeps on a futex. The holder wakes one sleeper on release if the count
//! says anyone else is there. Uncontended, taking and releasing it is one
//! compare-and-swap and one addition, and no system call.
//!
//! The type keeps its old name, [`SpinLock`], so that nothing using it had to
//! change when it stopped spinning.

use core::sync::atomic::{AtomicI32, Ordering};

use crate::futex;

/// The held flag with a count of one: the state a lone holder leaves.
const HELD_ALONE: i32 = i32::MIN + 1;

/// A lock with no data of its own. It guards statics that are atomics, so
/// that no `unsafe impl Sync` is needed. It is `#[repr(C)]` and an `int` wide,
/// so a control block or a C structure can hold one.
#[repr(C)]
#[derive(Debug)]
pub struct SpinLock {
    state: AtomicI32,
}

impl SpinLock {
    /// An unheld lock.
    pub const fn new() -> Self {
        Self {
            state: AtomicI32::new(0),
        }
    }

    /// Waits for the lock, and holds it until the guard is dropped.
    pub fn lock(&self) -> Guard<'_> {
        self.acquire();
        Guard { lock: self }
    }

    /// Takes the lock without a guard. [`SpinLock::release`] gives it back.
    pub fn acquire(&self) {
        let l = &self.state;
        let mut current = futex::cas(l, 0, HELD_ALONE);
        if current == 0 {
            return;
        }
        // Medium congestion: try a few times to take it, counting in.
        let mut spins = 0;
        while spins < 10 {
            if current < 0 {
                current = current.wrapping_sub(HELD_ALONE);
            }
            let wanted = i32::MIN.wrapping_add(current).wrapping_add(1);
            let seen = futex::cas(l, current, wanted);
            if seen == current {
                return;
            }
            current = seen;
            spins += 1;
        }
        // Count this thread in, then sleep whenever someone holds the lock.
        // The only change the loop makes is taking it.
        current = l.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        loop {
            if current < 0 {
                futex::wait(l, current, true);
                current = current.wrapping_sub(HELD_ALONE);
            }
            // The count includes this thread, so it is above zero here.
            let wanted = i32::MIN.wrapping_add(current);
            let seen = futex::cas(l, current, wanted);
            if seen == current {
                return;
            }
            current = seen;
        }
    }

    /// Releases a lock taken with [`SpinLock::acquire`].
    pub fn release(&self) {
        let l = &self.state;
        if l.load(Ordering::SeqCst) < 0 && l.fetch_add(i32::MAX, Ordering::SeqCst) != HELD_ALONE {
            futex::wake(l.as_ptr().cast_const().cast(), 1, true);
        }
    }

    /// Forgets that anyone held the lock or waited for it.
    ///
    /// Only the child of `fork` calls this, for locks `fork` took before
    /// copying the process: the threads that were counted do not exist there.
    pub fn reset(&self) {
        self.state.store(0, Ordering::SeqCst);
    }
}

impl Default for SpinLock {
    fn default() -> Self {
        Self::new()
    }
}

/// Holds a [`SpinLock`] while it lives.
#[derive(Debug)]
pub struct Guard<'a> {
    lock: &'a SpinLock,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.lock.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn a_dropped_guard_releases_the_lock() {
        let lock = SpinLock::new();
        drop(lock.lock());
        let _held = lock.lock();
        assert_eq!(lock.state.load(Ordering::Relaxed), HELD_ALONE);
    }

    #[test]
    fn contended_threads_each_get_the_lock_and_leave_it_free() {
        struct Shared {
            lock: SpinLock,
            // Read and written in two steps under the lock, so a broken lock
            // loses increments.
            counter: AtomicUsize,
        }
        let shared = Arc::new(Shared {
            lock: SpinLock::new(),
            counter: AtomicUsize::new(0),
        });
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        let _guard = shared.lock.lock();
                        let value = shared.counter.load(Ordering::Relaxed);
                        std::hint::black_box(());
                        shared.counter.store(value + 1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for worker in workers {
            assert!(worker.join().is_ok());
        }
        assert_eq!(shared.counter.load(Ordering::Relaxed), 160_000);
        assert_eq!(shared.lock.state.load(Ordering::Relaxed), 0);
    }
}
