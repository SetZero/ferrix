//! Futexes: the kernel's wait queues keyed by an address, and the waits every
//! lock, condition variable and join is built on.
//!
//! A futex is an `int` the program owns. `FUTEX_WAIT` sleeps only if the `int`
//! still holds the value the caller expects, checked by the kernel under its
//! own lock, so a wake that lands between the caller reading the value and
//! sleeping is never lost. `FUTEX_WAKE` wakes sleepers on the same address.
//!
//! The waits here follow musl's `__wait`, `__wake` and `__timedwait` (MIT).
//! Every constant is from `linux/futex.h`.

use core::ffi::c_int;
use core::sync::atomic::{AtomicI32, Ordering};

use crate::syscall::{self, nr};

/// `FUTEX_WAIT`.
pub const FUTEX_WAIT: usize = 0;
/// `FUTEX_WAKE`.
pub const FUTEX_WAKE: usize = 1;
/// `FUTEX_REQUEUE`.
pub const FUTEX_REQUEUE: usize = 3;
/// `FUTEX_LOCK_PI`.
pub const FUTEX_LOCK_PI: usize = 6;
/// `FUTEX_UNLOCK_PI`.
pub const FUTEX_UNLOCK_PI: usize = 7;
/// `FUTEX_PRIVATE_FLAG`: the futex is not shared with another process, which
/// lets the kernel key it by address alone.
pub const FUTEX_PRIVATE: usize = 128;

/// `FUTEX_WAITERS`: a robust or priority-inheriting lock has sleepers.
pub const WAITERS: c_int = 0x8000_0000_u32.cast_signed();
/// `FUTEX_OWNER_DIED`: the owner of a robust lock ended while holding it.
pub const OWNER_DIED: c_int = 0x4000_0000;
/// `FUTEX_TID_MASK`: the owner's thread id in a robust lock.
pub const TID_MASK: c_int = 0x3fff_ffff;

/// The private flag, if `private`.
const fn flag(private: bool) -> usize {
    if private { FUTEX_PRIVATE } else { 0 }
}

/// Compares `atom` with `old` and stores `new` if they are equal, returning the
/// value it held either way. musl's `a_cas`.
pub fn cas(atom: &AtomicI32, old: c_int, new: c_int) -> c_int {
    match atom.compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(value) | Err(value) => value,
    }
}

/// Wakes up to `count` threads sleeping on the futex at `addr`, or every one
/// if `count` is negative.
///
/// It takes an address rather than a reference, because the object may be
/// freed by a woken thread before the call returns; the kernel only uses the
/// address as a key.
pub fn wake(addr: *const AtomicI32, count: c_int, private: bool) {
    let count = if count < 0 { c_int::MAX } else { count };
    // SAFETY: `FUTEX_WAKE` reads no memory; the address is only a key.
    let _ = unsafe {
        syscall::syscall3(
            nr::FUTEX,
            addr.addr(),
            FUTEX_WAKE | flag(private),
            count as usize,
        )
    };
}

/// Sleeps on `atom` while it holds `value`, until woken. It may return early,
/// for a signal or a spurious wake, so callers check their condition again.
pub fn wait(atom: &AtomicI32, value: c_int, private: bool) {
    // SAFETY: the kernel reads the `int` at a live atomic, and with a null
    // timeout writes nothing.
    let _ = unsafe {
        syscall::syscall4(
            nr::FUTEX,
            atom.as_ptr().addr(),
            FUTEX_WAIT | flag(private),
            value as usize,
            0,
        )
    };
}

/// Waits for `atom` to stop holding `value`: spins briefly while nobody else
/// is waiting, then counts itself in `waiters`, if given, and sleeps. musl's
/// `__wait`.
pub fn wait_counted(atom: &AtomicI32, waiters: Option<&AtomicI32>, value: c_int, private: bool) {
    let mut spins = 100;
    while spins > 0 && waiters.is_none_or(|w| w.load(Ordering::SeqCst) == 0) {
        if atom.load(Ordering::SeqCst) != value {
            return;
        }
        core::hint::spin_loop();
        spins -= 1;
    }
    if let Some(w) = waiters {
        let _ = w.fetch_add(1, Ordering::SeqCst);
    }
    while atom.load(Ordering::SeqCst) == value {
        wait(atom, value, private);
    }
    if let Some(w) = waiters {
        let _ = w.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cas_returns_the_old_value_whether_or_not_it_stores() {
        let atom = AtomicI32::new(5);
        assert_eq!(cas(&atom, 4, 9), 5);
        assert_eq!(atom.load(Ordering::SeqCst), 5);
        assert_eq!(cas(&atom, 5, 9), 5);
        assert_eq!(atom.load(Ordering::SeqCst), 9);
    }

    #[test]
    fn a_wait_whose_value_has_changed_returns_at_once() {
        let atom = AtomicI32::new(1);
        wait(&atom, 0, true);
        wait_counted(&atom, None, 0, true);
    }
}
