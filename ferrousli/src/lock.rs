//! A lock for the little process-wide state the library keeps.
//!
//! It spins rather than sleeping, which is only acceptable while nothing holds
//! it across a system call or a call into the program. When threads arrive it
//! becomes a futex.

use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};

/// A spin lock with no data of its own. It guards statics that are atomics, so
/// that no `unsafe impl Sync` is needed.
#[derive(Debug)]
pub struct SpinLock {
    held: AtomicBool,
}

impl SpinLock {
    /// An unheld lock.
    pub const fn new() -> Self {
        Self {
            held: AtomicBool::new(false),
        }
    }

    /// Waits for the lock, and holds it until the guard is dropped.
    pub fn lock(&self) -> Guard<'_> {
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        Guard { lock: self }
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
        self.lock.held.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_guard_releases_the_lock() {
        let lock = SpinLock::new();
        drop(lock.lock());
        let _held = lock.lock();
        assert!(lock.held.load(Ordering::Relaxed));
    }
}
