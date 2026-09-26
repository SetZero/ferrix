//! Signal sets and the signal mask: `sigemptyset` and its relatives,
//! `sigprocmask`, `pthread_sigmask`, `sigpending`, `sigsuspend`, and the System
//! V calls built on them.
//!
//! # Two sizes of set
//!
//! C's `sigset_t` is 128 bytes, room for 1024 signals. Linux has 64, and its
//! calls take an 8-byte set. So the kernel reads and writes only the first
//! word of a C set. The functions here clear the other fifteen whenever they
//! fill a set in, so that two sets holding the same signals compare equal with
//! `memcmp`.
//!
//! # Reserved signals
//!
//! musl keeps signals 32, 33 and 34 for itself: thread cancellation, set-id
//! synchronisation across threads, and one spare. `SIGRTMIN` is therefore 35.
//! (glibc keeps only 32 and 33, and its `SIGRTMIN` is 34.) Ferrousli follows
//! musl exactly:
//!
//! * `sigfillset` leaves them out.
//! * `sigaddset` and `sigdelset` refuse them with `EINVAL`.
//! * `sigismember` reports them, since a set the kernel wrote may hold them.
//! * `sigprocmask` passes a set to the kernel as it is, so a set built by hand
//!   can block them, but never reports them in the old mask. A program cannot
//!   see them, and cannot put them back with a mask it saved.
//! * `sigaction` refuses them; see [`crate::sigaction`].
//!
//! Nothing uses them until there are threads. They are reserved now so that
//! programs cannot come to depend on them.

use core::ffi::{c_int, c_ulong};
use core::mem::{align_of, size_of};

use crate::errno;
use crate::syscall::{self, nr};

/// `SIG_BLOCK`: add a set to the mask.
pub const SIG_BLOCK: c_int = 0;
/// `SIG_UNBLOCK`: remove a set from the mask.
pub const SIG_UNBLOCK: c_int = 1;
/// `SIG_SETMASK`: replace the mask.
pub const SIG_SETMASK: c_int = 2;

/// `_NSIG`: one more than the highest signal number.
pub const NSIG: c_int = 65;

/// The size of the kernel's signal set, `_NSIG / 8`.
pub const KERNEL_SIGSET_SIZE: usize = 8;

/// The first signal the library reserves.
const RESERVED_FIRST: c_int = 32;
/// The last signal the library reserves.
const RESERVED_LAST: c_int = 34;

/// `SIGRTMIN`, the first real-time signal a program may use.
pub const SIGRTMIN: c_int = RESERVED_LAST + 1;
/// `SIGRTMAX`, the last real-time signal.
pub const SIGRTMAX: c_int = NSIG - 1;

/// The reserved signals, as bits of the kernel's set.
const RESERVED_BITS: u64 = bit_of(32) | bit_of(33) | bit_of(34);
/// Every signal a program may use, as bits of the kernel's set.
const FILLED: u64 = !RESERVED_BITS;

const _: () = assert!(RESERVED_FIRST == 32 && RESERVED_LAST == 34);
// musl's `sigfillset` writes this constant; the two must agree.
const _: () = assert!(FILLED == 0xffff_fffc_7fff_ffff);

/// Signal `sig`'s bit in the kernel's set, for a `sig` known to be in range.
const fn bit_of(sig: c_int) -> u64 {
    1 << (sig - 1)
}

/// The `unsigned long`s the kernel's 64 signals take: one, or two on a
/// 32-bit target, low half first.
const KERNEL_WORDS: usize = 8 / size_of::<c_ulong>();
/// The `unsigned long`s of C's 1024-bit `sigset_t`.
const WORDS: usize = 128 / size_of::<c_ulong>();

/// C's `sigset_t`: 1024 bits in `unsigned long`s, of which the kernel uses
/// the first 64.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigSet {
    /// The kernel's set. Bit `n - 1` is signal `n`.
    word: [c_ulong; KERNEL_WORDS],
    /// Room C reserves for more signals than Linux has.
    upper: [c_ulong; WORDS - KERNEL_WORDS],
}

const _: () = assert!(size_of::<SigSet>() == 128);
const _: () = assert!(align_of::<SigSet>() == align_of::<c_ulong>());

impl SigSet {
    /// The empty set.
    pub const EMPTY: Self = Self::from_word(0);

    /// The set whose kernel word is `word`.
    #[cfg(target_pointer_width = "64")]
    pub const fn from_word(word: u64) -> Self {
        Self {
            word: [word],
            upper: [0; WORDS - KERNEL_WORDS],
        }
    }

    /// The set whose kernel word is `word`, low half first.
    #[cfg(target_pointer_width = "32")]
    pub const fn from_word(word: u64) -> Self {
        Self {
            word: [word as c_ulong, (word >> 32) as c_ulong],
            upper: [0; WORDS - KERNEL_WORDS],
        }
    }

    /// The kernel's word of the set.
    #[cfg(target_pointer_width = "64")]
    pub const fn word(&self) -> u64 {
        let [word] = self.word;
        word
    }

    /// The kernel's word of the set, from its two halves.
    #[cfg(target_pointer_width = "32")]
    pub const fn word(&self) -> u64 {
        let [low, high] = self.word;
        low as u64 | (high as u64) << 32
    }

    /// Replaces the kernel's word of the set, leaving the rest alone.
    fn set_word(&mut self, word: u64) {
        self.word = Self::from_word(word).word;
    }
}

/// The kernel's 64-bit signal set as it sits inside one of the kernel's own
/// structures, such as its `struct sigaction`: one `unsigned long`, or two on
/// a 32-bit target, where the kernel aligns it to 4 bytes, not 8.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelMask([c_ulong; KERNEL_WORDS]);

impl KernelMask {
    /// The set whose bits are `word`'s.
    pub const fn new(word: u64) -> Self {
        Self(SigSet::from_word(word).word)
    }

    /// The set's bits.
    pub const fn get(self) -> u64 {
        SigSet {
            word: self.0,
            upper: [0; WORDS - KERNEL_WORDS],
        }
        .word()
    }
}

/// Signal `sig`'s bit, or `None` if there is no such signal.
pub fn bit(sig: c_int) -> Option<u64> {
    (1..NSIG).contains(&sig).then(|| bit_of(sig))
}

/// Whether the library reserves `sig`.
pub fn is_reserved(sig: c_int) -> bool {
    (RESERVED_FIRST..=RESERVED_LAST).contains(&sig)
}

/// Signal `sig`'s bit, or `None` if a program may not name it: there is no
/// such signal, or the library reserves it.
pub fn usable_bit(sig: c_int) -> Option<u64> {
    if is_reserved(sig) { None } else { bit(sig) }
}

/// Empties `set`. Always returns 0.
///
/// # Safety
///
/// `set` must be valid for writes of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigemptyset(set: *mut SigSet) -> c_int {
    // SAFETY: the caller vouches for the set.
    unsafe { set.write(SigSet::EMPTY) };
    0
}

/// Fills `set` with every signal a program may use, which leaves out the
/// reserved ones. Always returns 0.
///
/// # Safety
///
/// `set` must be valid for writes of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigfillset(set: *mut SigSet) -> c_int {
    // SAFETY: the caller vouches for the set.
    unsafe { set.write(SigSet::from_word(FILLED)) };
    0
}

/// Adds `sig` to `set`. Fails with `EINVAL` for a signal that does not exist
/// or is reserved.
///
/// # Safety
///
/// `set` must be valid for reads and writes of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigaddset(set: *mut SigSet, sig: c_int) -> c_int {
    let Some(bit) = usable_bit(sig) else {
        errno::set(errno::EINVAL);
        return -1;
    };
    // SAFETY: the caller vouches for the set, and nothing else refers to it
    // while this runs.
    let set = unsafe { &mut *set };
    set.set_word(set.word() | bit);
    0
}

/// Removes `sig` from `set`. Fails with `EINVAL` for a signal that does not
/// exist or is reserved.
///
/// # Safety
///
/// `set` must be valid for reads and writes of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigdelset(set: *mut SigSet, sig: c_int) -> c_int {
    let Some(bit) = usable_bit(sig) else {
        errno::set(errno::EINVAL);
        return -1;
    };
    // SAFETY: as in `sigaddset`.
    let set = unsafe { &mut *set };
    set.set_word(set.word() & !bit);
    0
}

/// 1 if `sig` is in `set`, 0 if not.
///
/// A signal that does not exist fails with `EINVAL`, as POSIX allows and glibc
/// does. musl returns 0 instead. A reserved signal is reported like any other,
/// as musl does.
///
/// # Safety
///
/// `set` must be valid for reads of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigismember(set: *const SigSet, sig: c_int) -> c_int {
    let Some(bit) = bit(sig) else {
        errno::set(errno::EINVAL);
        return -1;
    };
    // SAFETY: the caller vouches for the set.
    let word = unsafe { (*set).word() };
    c_int::from(word & bit != 0)
}

/// 1 if `set` holds no signal, 0 if it holds one. A GNU extension.
///
/// # Safety
///
/// `set` must be valid for reads of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigisemptyset(set: *const SigSet) -> c_int {
    // SAFETY: the caller vouches for the set.
    let word = unsafe { (*set).word() };
    c_int::from(word == 0)
}

/// Stores the union of `left` and `right` in `dest`. A GNU extension.
///
/// # Safety
///
/// `left` and `right` must be valid for reads of a `sigset_t`, and `dest` for
/// writes of one. They may be the same set.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigorset(
    dest: *mut SigSet,
    left: *const SigSet,
    right: *const SigSet,
) -> c_int {
    // SAFETY: the caller vouches for the set.
    let left = unsafe { (*left).word() };
    // SAFETY: as above.
    let right = unsafe { (*right).word() };
    // SAFETY: as above; nothing refers to `left` or `right` any more.
    unsafe { dest.write(SigSet::from_word(left | right)) };
    0
}

/// Stores the intersection of `left` and `right` in `dest`. A GNU extension.
///
/// # Safety
///
/// As [`sigorset`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigandset(
    dest: *mut SigSet,
    left: *const SigSet,
    right: *const SigSet,
) -> c_int {
    // SAFETY: the caller vouches for the set.
    let left = unsafe { (*left).word() };
    // SAFETY: as above.
    let right = unsafe { (*right).word() };
    // SAFETY: as above; nothing refers to `left` or `right` any more.
    unsafe { dest.write(SigSet::from_word(left & right)) };
    0
}

/// Changes the calling thread's signal mask, returning an error number rather
/// than setting `errno`.
///
/// `how` is checked only when there is a set to apply, as the kernel does. The
/// old mask never shows the reserved signals.
///
/// Before there are threads, the calling thread's mask is the process's.
///
/// # Safety
///
/// `set` must be null or valid for reads of a `sigset_t`, and `old` null or
/// valid for writes of one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pthread_sigmask(
    how: c_int,
    set: *const SigSet,
    old: *mut SigSet,
) -> c_int {
    if !set.is_null() && !(SIG_BLOCK..=SIG_SETMASK).contains(&how) {
        return errno::EINVAL;
    }
    // Both pointers go to the kernel as they are, so that a bad one fails
    // with `EFAULT` instead of faulting here.
    // SAFETY: the caller vouches for `set` and `old`. The kernel reads the
    // first word of one and then writes the first word of the other. `how` is
    // in range, or ignored with no set.
    let ret = unsafe {
        syscall::syscall4(
            nr::RT_SIGPROCMASK,
            how as usize,
            set.addr(),
            old.addr(),
            KERNEL_SIGSET_SIZE,
        )
    };
    if let Err(error) = errno::decode(ret) {
        return error;
    }
    if !old.is_null() {
        // SAFETY: the caller vouches for `old`, and the kernel just wrote its
        // first word.
        let previous = unsafe { (*old).word() };
        // SAFETY: as above. The kernel has finished reading `set`.
        unsafe { old.write(SigSet::from_word(previous & !RESERVED_BITS)) };
    }
    0
}

/// Changes the process's signal mask. As [`pthread_sigmask`], but it reports
/// failure through `errno`.
///
/// # Safety
///
/// As [`pthread_sigmask`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigprocmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int {
    // SAFETY: the caller's promises are the same.
    match unsafe { pthread_sigmask(how, set, old) } {
        0 => 0,
        error => {
            errno::set(error);
            -1
        }
    }
}

/// Stores the signals that are blocked and waiting in `set`.
///
/// # Safety
///
/// `set` must be valid for writes of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigpending(set: *mut SigSet) -> c_int {
    // SAFETY: the caller vouches for the set, of which the kernel writes the
    // first word, or fails with `EFAULT`.
    let ret = unsafe { syscall::syscall2(nr::RT_SIGPENDING, set.addr(), KERNEL_SIGSET_SIZE) };
    if errno::from_syscall(ret) < 0 {
        return -1;
    }
    // SAFETY: the kernel just wrote the first word.
    let pending = unsafe { (*set).word() };
    // SAFETY: the caller vouches for the whole set.
    unsafe { set.write(SigSet::from_word(pending)) };
    0
}

/// Replaces the signal mask with `mask` and waits for a signal whose handler
/// runs, then puts the mask back. Always returns -1, with `errno` set to
/// `EINTR` once a handler has run.
///
/// # Safety
///
/// `mask` must be valid for reads of a `sigset_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sigsuspend(mask: *const SigSet) -> c_int {
    // SAFETY: the caller vouches for the set, of which the kernel reads one
    // word.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::RT_SIGSUSPEND,
            mask.addr(),
            KERNEL_SIGSET_SIZE,
            0,
            0,
            0,
            0,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Applies `how` with a set holding only `sig`, which must be one a program
/// may name.
fn change_one(how: c_int, sig: c_int) -> c_int {
    let mut set = SigSet::EMPTY;
    // SAFETY: `set` is a live local.
    if unsafe { sigaddset(&raw mut set, sig) } < 0 {
        return -1;
    }
    // SAFETY: `set` is a live local, and there is no old mask to write.
    unsafe { sigprocmask(how, &raw const set, core::ptr::null_mut()) }
}

/// Blocks `sig`. System V.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sighold(sig: c_int) -> c_int {
    change_one(SIG_BLOCK, sig)
}

/// Unblocks `sig`. System V.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sigrelse(sig: c_int) -> c_int {
    change_one(SIG_UNBLOCK, sig)
}

/// Unblocks `sig` and waits for a signal, as [`sigsuspend`]. System V.
///
/// As in musl, a `sig` that cannot be removed from a set leaves the mask as it
/// is while waiting.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sigpause(sig: c_int) -> c_int {
    let mut mask = SigSet::EMPTY;
    // SAFETY: `mask` is a live local, and there is no set to apply.
    if unsafe { sigprocmask(SIG_BLOCK, core::ptr::null(), &raw mut mask) } < 0 {
        return -1;
    }
    // SAFETY: `mask` is a live local.
    let _ = unsafe { sigdelset(&raw mut mask, sig) };
    // SAFETY: as above.
    unsafe { sigsuspend(&raw const mask) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errno() -> c_int {
        // SAFETY: the pointer is this thread's `errno`.
        unsafe { errno::__errno_location().read() }
    }

    #[test]
    fn filling_leaves_out_the_reserved_signals_and_emptying_clears_every_word() {
        let mut set = SigSet {
            word: [0; KERNEL_WORDS],
            upper: [7; WORDS - KERNEL_WORDS],
        };
        // SAFETY: `set` is a live local.
        assert_eq!(unsafe { sigfillset(&raw mut set) }, 0);
        assert_eq!(set.word(), 0xffff_fffc_7fff_ffff);
        assert_eq!(set.upper, [0; WORDS - KERNEL_WORDS]);
        for sig in 1..NSIG {
            // SAFETY: as above.
            let member = unsafe { sigismember(&raw const set, sig) };
            assert_eq!(member, c_int::from(!is_reserved(sig)), "signal {sig}");
        }
        // SAFETY: as above.
        assert_eq!(unsafe { sigisemptyset(&raw const set) }, 0);

        // SAFETY: as above.
        assert_eq!(unsafe { sigemptyset(&raw mut set) }, 0);
        assert_eq!(set, SigSet::EMPTY);
        // SAFETY: as above.
        assert_eq!(unsafe { sigisemptyset(&raw const set) }, 1);
    }

    #[test]
    fn adding_and_deleting_touch_one_bit() {
        let mut set = SigSet::EMPTY;
        // SAFETY: `set` is a live local.
        assert_eq!(unsafe { sigaddset(&raw mut set, 1) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { sigaddset(&raw mut set, 64) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { sigaddset(&raw mut set, 35) }, 0);
        assert_eq!(set.word(), 1 | 1 << 63 | 1 << 34);
        // SAFETY: as above.
        assert_eq!(unsafe { sigdelset(&raw mut set, 64) }, 0);
        assert_eq!(set.word(), 1 | 1 << 34);
    }

    #[test]
    fn a_signal_that_does_not_exist_or_is_reserved_is_refused() {
        let mut set = SigSet::EMPTY;
        for sig in [0, -1, 65, 1000, c_int::MIN, 32, 33, 34] {
            errno::set(0);
            // SAFETY: `set` is a live local.
            assert_eq!(unsafe { sigaddset(&raw mut set, sig) }, -1, "{sig}");
            assert_eq!(errno(), errno::EINVAL);
            errno::set(0);
            // SAFETY: as above.
            assert_eq!(unsafe { sigdelset(&raw mut set, sig) }, -1, "{sig}");
            assert_eq!(errno(), errno::EINVAL);
        }
        assert_eq!(set, SigSet::EMPTY);

        for sig in [0, 65, -5] {
            errno::set(0);
            // SAFETY: as above.
            assert_eq!(unsafe { sigismember(&raw const set, sig) }, -1);
            assert_eq!(errno(), errno::EINVAL);
        }
        // A reserved signal the kernel reported is visible.
        let set = SigSet::from_word(bit_of(33));
        // SAFETY: as above.
        assert_eq!(unsafe { sigismember(&raw const set, 33) }, 1);
    }

    #[test]
    fn union_and_intersection() {
        let left = SigSet::from_word(0b1100);
        let right = SigSet::from_word(0b1010);
        let mut dest = SigSet {
            word: [0; KERNEL_WORDS],
            upper: [9; WORDS - KERNEL_WORDS],
        };
        // SAFETY: all three are live locals.
        let ret = unsafe { sigorset(&raw mut dest, &raw const left, &raw const right) };
        assert_eq!(ret, 0);
        assert_eq!(dest, SigSet::from_word(0b1110));
        // SAFETY: as above.
        let ret = unsafe { sigandset(&raw mut dest, &raw const left, &raw const right) };
        assert_eq!(ret, 0);
        assert_eq!(dest, SigSet::from_word(0b1000));
    }

    #[test]
    fn a_bad_how_is_refused_only_with_a_set() {
        let set = SigSet::EMPTY;
        let mut old = SigSet::EMPTY;
        errno::set(0);
        // SAFETY: both are live locals.
        let ret = unsafe { pthread_sigmask(7, &raw const set, &raw mut old) };
        assert_eq!(ret, errno::EINVAL);
        // `pthread_sigmask` does not touch errno.
        assert_eq!(errno(), 0);
        // SAFETY: as above.
        assert_eq!(unsafe { sigprocmask(-1, &raw const set, &raw mut old) }, -1);
        assert_eq!(errno(), errno::EINVAL);
        // SAFETY: `old` is a live local, and there is no set.
        let ret = unsafe { sigprocmask(7, core::ptr::null(), &raw mut old) };
        assert_eq!(ret, 0);
    }
}
