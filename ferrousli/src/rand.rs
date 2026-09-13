//! `stdlib.h`: pseudo-random numbers.
//!
//! Three generators, each with the algorithm and state layout of musl 1.2.5's
//! `src/prng` (MIT):
//!
//! * `rand` and `srand`: a 64-bit linear congruential generator, returning its
//!   top 31 bits. `rand_r` keeps a 32-bit state in the caller's `unsigned` and
//!   tempers it.
//! * `random`, `srandom`, `initstate` and `setstate`: BSD's additive lagged
//!   Fibonacci generator, with musl's seeding. A state buffer holds a header
//!   word `n << 16 | i << 8 | j` followed by `n` words; `n` is 0, 7, 15, 31 or
//!   63 by the buffer's size.
//! * The `rand48` family: POSIX's 48-bit linear congruential generator, whose
//!   sequences POSIX specifies exactly. Unlike musl, and as POSIX requires,
//!   `srand48` and `seed48` restore the multiplier and addend that `lcong48`
//!   may have changed.
//!
//! The process-wide states are guarded by spin locks. None is held while
//! calling out.

use core::ffi::{c_char, c_double, c_int, c_long, c_uint, c_ushort};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::lock::SpinLock;

/// The 64-bit multiplier both musl generators seed with.
const LCG64: u64 = 6_364_136_223_846_793_005;

// ---------------------------------------------------------------------------
// rand
// ---------------------------------------------------------------------------

/// `rand`'s state.
static RAND_STATE: AtomicU64 = AtomicU64::new(0);

/// Seeds `rand`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn srand(seed: c_uint) {
    RAND_STATE.store(u64::from(seed.wrapping_sub(1)), Ordering::Relaxed);
}

/// A pseudo-random number from 0 to `RAND_MAX`, 2^31 - 1.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn rand() -> c_int {
    let step = |state: u64| state.wrapping_mul(LCG64).wrapping_add(1);
    // The closure always returns a value, so the update cannot fail.
    let old = match RAND_STATE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |s| Some(step(s)))
    {
        Ok(old) | Err(old) => old,
    };
    // 31 bits.
    (step(old) >> 33) as c_int
}

/// A pseudo-random number from 0 to `RAND_MAX`, from and advancing the state
/// at `seed`.
///
/// # Safety
///
/// `seed` must be valid to read and write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn rand_r(seed: *mut c_uint) -> c_int {
    // SAFETY: the caller vouches for `seed`.
    let state = unsafe { seed.read() }
        .wrapping_mul(1_103_515_245)
        .wrapping_add(12345);
    // SAFETY: as above.
    unsafe { seed.write(state) };
    let mut x = state;
    x ^= x >> 11;
    x ^= (x << 7) & 0x9d2c_5680;
    x ^= (x << 15) & 0xefc6_0000;
    x ^= x >> 18;
    // Halved, so 31 bits.
    (x / 2) as c_int
}

// ---------------------------------------------------------------------------
// random
// ---------------------------------------------------------------------------

/// The default state: a header word, then the 31 words `srandom(1)` produces.
static RANDOM_DEFAULT: [AtomicU32; 32] = [
    AtomicU32::new(0x0000_0000),
    AtomicU32::new(0x5851_f42d),
    AtomicU32::new(0xc0b1_8ccf),
    AtomicU32::new(0xcbb5_f646),
    AtomicU32::new(0xc703_3129),
    AtomicU32::new(0x3070_5b04),
    AtomicU32::new(0x20fd_5db4),
    AtomicU32::new(0x9a8b_7f78),
    AtomicU32::new(0x5029_59d8),
    AtomicU32::new(0xab89_4868),
    AtomicU32::new(0x6c03_56a7),
    AtomicU32::new(0x88cd_b7ff),
    AtomicU32::new(0xb477_d43f),
    AtomicU32::new(0x70a3_a52b),
    AtomicU32::new(0xa8e4_baf1),
    AtomicU32::new(0xfd83_41fc),
    AtomicU32::new(0x8ae1_6fd9),
    AtomicU32::new(0x742d_2f7a),
    AtomicU32::new(0x0d1f_0796),
    AtomicU32::new(0x7603_5e09),
    AtomicU32::new(0x40f7_702c),
    AtomicU32::new(0x6fa7_2ca5),
    AtomicU32::new(0xaaa8_4157),
    AtomicU32::new(0x58a0_df74),
    AtomicU32::new(0xc74a_0364),
    AtomicU32::new(0xae53_3cc4),
    AtomicU32::new(0x0418_5faf),
    AtomicU32::new(0x6de3_b115),
    AtomicU32::new(0x0cab_8628),
    AtomicU32::new(0xf043_bfa4),
    AtomicU32::new(0x3981_50e9),
    AtomicU32::new(0x3752_1657),
];

/// Guards the `random` state below.
static RANDOM_LOCK: SpinLock = SpinLock::new();
/// The state's first word after the header: musl's `x`. It points into
/// [`RANDOM_DEFAULT`] or into a buffer the program gave `initstate` or
/// `setstate`, which may not be aligned.
static RANDOM_WORDS: AtomicPtr<u8> =
    AtomicPtr::new(RANDOM_DEFAULT.as_ptr().wrapping_add(1).cast_mut().cast());
/// How many words the state has.
static RANDOM_N: AtomicU8 = AtomicU8::new(31);
/// The index of the word the next number updates.
static RANDOM_I: AtomicU8 = AtomicU8::new(3);
/// The index of the word added to it.
static RANDOM_J: AtomicU8 = AtomicU8::new(0);

/// The `random` state, read out while [`RANDOM_LOCK`] is held.
#[derive(Debug, Clone, Copy)]
struct Random {
    /// The first word after the header, as bytes, since it may be unaligned.
    words: *mut u8,
    n: u8,
    i: u8,
    j: u8,
}

impl Random {
    /// Reads the state. The lock must be held.
    fn load() -> Self {
        Self {
            words: RANDOM_WORDS.load(Ordering::Relaxed),
            n: RANDOM_N.load(Ordering::Relaxed),
            i: RANDOM_I.load(Ordering::Relaxed),
            j: RANDOM_J.load(Ordering::Relaxed),
        }
    }

    /// Writes the state back. The lock must be held.
    fn store(self) {
        RANDOM_WORDS.store(self.words, Ordering::Relaxed);
        RANDOM_N.store(self.n, Ordering::Relaxed);
        RANDOM_I.store(self.i, Ordering::Relaxed);
        RANDOM_J.store(self.j, Ordering::Relaxed);
    }

    /// Reads word `k`, where -1 is the header.
    ///
    /// # Safety
    ///
    /// The state's buffer must hold that word.
    unsafe fn word(self, k: isize) -> u32 {
        // SAFETY: the caller vouches for the word; the buffer may be a `char`
        // array, so it is read unaligned.
        unsafe {
            self.words
                .wrapping_offset(4 * k)
                .cast::<u32>()
                .read_unaligned()
        }
    }

    /// Writes word `k`, where -1 is the header.
    ///
    /// # Safety
    ///
    /// As [`Random::word`].
    unsafe fn set_word(self, k: isize, value: u32) {
        // SAFETY: as in `word`.
        unsafe {
            self.words
                .wrapping_offset(4 * k)
                .cast::<u32>()
                .write_unaligned(value)
        };
    }

    /// Records `n`, `i` and `j` in the header, and returns the buffer's
    /// start, as `initstate` and `setstate` return it.
    ///
    /// # Safety
    ///
    /// As [`Random::word`].
    unsafe fn save(self) -> *mut c_char {
        let header = u32::from(self.n) << 16 | u32::from(self.i) << 8 | u32::from(self.j);
        // SAFETY: the header is part of every state.
        unsafe { self.set_word(-1, header) };
        self.words.wrapping_sub(4).cast()
    }

    /// Seeds the state's words.
    ///
    /// # Safety
    ///
    /// As [`Random::word`], for all `n` words.
    unsafe fn seed(&mut self, seed: c_uint) {
        if self.n == 0 {
            // SAFETY: a state of size 0 still has one word.
            unsafe { self.set_word(0, seed) };
            return;
        }
        self.i = if self.n == 31 || self.n == 7 { 3 } else { 1 };
        self.j = 0;
        let mut s = u64::from(seed);
        for k in 0..isize::from(self.n) {
            s = s.wrapping_mul(LCG64).wrapping_add(1);
            // SAFETY: `k < n`. The high half is kept.
            unsafe { self.set_word(k, (s >> 32) as u32) };
        }
        // At least one odd word, or the low bits never change.
        // SAFETY: word 0 exists.
        let first = unsafe { self.word(0) };
        // SAFETY: as above.
        unsafe { self.set_word(0, first | 1) };
    }
}

/// Seeds `random`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn srandom(seed: c_uint) {
    let _guard = RANDOM_LOCK.lock();
    let mut state = Random::load();
    // SAFETY: the current state's buffer holds its `n` words.
    unsafe { state.seed(seed) };
    state.store();
}

/// Makes the `size` bytes at `buffer` the `random` state, seeded with `seed`,
/// and returns the previous state. Fewer than 8 bytes returns null and changes
/// nothing.
///
/// # Safety
///
/// `buffer` must be valid for `size` bytes for as long as it is the state.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn initstate(seed: c_uint, buffer: *mut c_char, size: usize) -> *mut c_char {
    if size < 8 {
        return null_mut();
    }
    let _guard = RANDOM_LOCK.lock();
    let old = Random::load();
    // SAFETY: the current state's buffer holds its header.
    let previous = unsafe { old.save() };
    let mut state = Random {
        words: buffer.wrapping_add(4).cast(),
        n: match size {
            ..32 => 0,
            32..64 => 7,
            64..128 => 15,
            128..256 => 31,
            _ => 63,
        },
        i: 0,
        j: 0,
    };
    // SAFETY: the buffer holds a header and `n` words for this size.
    unsafe { state.seed(seed) };
    // SAFETY: as above.
    let _ = unsafe { state.save() };
    state.store();
    previous
}

/// Makes the state `initstate` or `setstate` returned the `random` state again,
/// and returns the previous one.
///
/// # Safety
///
/// `buffer` must be such a state, still valid.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setstate(buffer: *mut c_char) -> *mut c_char {
    let _guard = RANDOM_LOCK.lock();
    let old = Random::load();
    // SAFETY: the current state's buffer holds its header.
    let previous = unsafe { old.save() };
    let words = buffer.wrapping_add(4).cast::<u8>();
    // SAFETY: the caller passes a state, which starts with its header.
    let header = unsafe { buffer.cast::<u32>().read_unaligned() };
    // Each field is a byte of the header.
    Random {
        words,
        n: (header >> 16) as u8,
        i: (header >> 8) as u8,
        j: header as u8,
    }
    .store();
    previous
}

/// A pseudo-random number from 0 to 2^31 - 1.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn random() -> c_long {
    let _guard = RANDOM_LOCK.lock();
    let mut state = Random::load();
    if state.n == 0 {
        // SAFETY: a state of size 0 has one word.
        let word = unsafe { state.word(0) };
        let next = word.wrapping_mul(1_103_515_245).wrapping_add(12345) & 0x7fff_ffff;
        // SAFETY: as above.
        unsafe { state.set_word(0, next) };
        return c_long::from(next);
    }
    let (i, j) = (isize::from(state.i), isize::from(state.j));
    // SAFETY: `i` and `j` are below `n`, and the buffer holds `n` words.
    let sum = unsafe { state.word(i) }.wrapping_add(unsafe { state.word(j) });
    // SAFETY: as above.
    unsafe { state.set_word(i, sum) };
    state.i = if state.i + 1 == state.n {
        0
    } else {
        state.i + 1
    };
    state.j = if state.j + 1 == state.n {
        0
    } else {
        state.j + 1
    };
    state.store();
    c_long::from(sum >> 1)
}

// ---------------------------------------------------------------------------
// rand48
// ---------------------------------------------------------------------------

/// POSIX's multiplier `a`, `0x5DEECE66D`, as three 16-bit words, low first.
const A48: [u16; 3] = [0xe66d, 0xdeec, 0x5];
/// POSIX's addend `c`.
const C48: u16 = 0xb;

/// Guards [`SEED48`] and [`SEED48_PREVIOUS`].
static LOCK48: SpinLock = SpinLock::new();
/// `X` in three words, low first, then `a` in three and `c`: `lcong48`'s
/// layout.
static SEED48: [AtomicU16; 7] = [
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(A48[0]),
    AtomicU16::new(A48[1]),
    AtomicU16::new(A48[2]),
    AtomicU16::new(C48),
];
/// The `X` that `seed48` replaced, which it returns a pointer to.
static SEED48_PREVIOUS: [AtomicU16; 3] = [const { AtomicU16::new(0) }; 3];

/// Three 16-bit words, low first, as one 48-bit number.
fn join48(words: [u16; 3]) -> u64 {
    u64::from(words[0]) | u64::from(words[1]) << 16 | u64::from(words[2]) << 32
}

/// A 48-bit number as three 16-bit words.
const fn split48(x: u64) -> [u16; 3] {
    // Each word keeps its 16 bits.
    [x as u16, (x >> 16) as u16, (x >> 32) as u16]
}

/// Advances `x` once with the current `a` and `c`, returning the new 48-bit
/// value.
fn step48(x: [u16; 3]) -> ([u16; 3], u64) {
    let load = |k: usize| SEED48.get(k).map_or(0, |w| w.load(Ordering::Relaxed));
    let a = join48([load(3), load(4), load(5)]);
    let c = u64::from(load(6));
    let next = a.wrapping_mul(join48(x)).wrapping_add(c) & ((1 << 48) - 1);
    (split48(next), next)
}

/// Advances the global `X`.
fn step_global() -> u64 {
    let _guard = LOCK48.lock();
    let load = |k: usize| SEED48.get(k).map_or(0, |w| w.load(Ordering::Relaxed));
    let (x, value) = step48([load(0), load(1), load(2)]);
    for (word, value) in SEED48.iter().zip(x) {
        word.store(value, Ordering::Relaxed);
    }
    value
}

/// Advances the caller's `X`.
///
/// # Safety
///
/// `xsubi` must be valid to read and write three `unsigned short`s.
unsafe fn step_caller(xsubi: *mut c_ushort) -> u64 {
    let _guard = LOCK48.lock();
    // SAFETY: the caller vouches for three words.
    let x = unsafe { xsubi.cast::<[u16; 3]>().read() };
    let (x, value) = step48(x);
    // SAFETY: as above.
    unsafe { xsubi.cast::<[u16; 3]>().write(x) };
    value
}

/// 48 random bits as a `double` in `[0, 1)`: exactly `x * 2^-48`.
fn to_double(x: u64) -> c_double {
    f64::from_bits(0x3ff0_0000_0000_0000 | x << 4) - 1.0
}

/// A pseudo-random `double` in `[0, 1)`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn drand48() -> c_double {
    to_double(step_global())
}

/// A pseudo-random `double` in `[0, 1)`, from and advancing `xsubi`.
///
/// # Safety
///
/// `xsubi` must be valid to read and write three `unsigned short`s.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn erand48(xsubi: *mut c_ushort) -> c_double {
    // SAFETY: the caller's contract is `step_caller`'s.
    to_double(unsafe { step_caller(xsubi) })
}

/// A pseudo-random `long` in `[0, 2^31)`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lrand48() -> c_long {
    // The top 31 of 48 bits.
    (step_global() >> 17) as c_long
}

/// A pseudo-random `long` in `[0, 2^31)`, from and advancing `xsubi`.
///
/// # Safety
///
/// As [`erand48`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn nrand48(xsubi: *mut c_ushort) -> c_long {
    // SAFETY: the caller's contract is `step_caller`'s.
    (unsafe { step_caller(xsubi) } >> 17) as c_long
}

/// A pseudo-random `long` in `[-2^31, 2^31)`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn mrand48() -> c_long {
    // The top 32 of 48 bits, as a signed 32-bit number.
    c_long::from((step_global() >> 16) as u32 as i32)
}

/// A pseudo-random `long` in `[-2^31, 2^31)`, from and advancing `xsubi`.
///
/// # Safety
///
/// As [`erand48`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn jrand48(xsubi: *mut c_ushort) -> c_long {
    // SAFETY: the caller's contract is `step_caller`'s.
    c_long::from((unsafe { step_caller(xsubi) } >> 16) as u32 as i32)
}

/// Sets `X` and restores the default `a` and `c`. The lock must be held.
fn reseed48(x: [u16; 3]) {
    let values = [x[0], x[1], x[2], A48[0], A48[1], A48[2], C48];
    for (word, value) in SEED48.iter().zip(values) {
        word.store(value, Ordering::Relaxed);
    }
}

/// Seeds `X` with `0x330E` below the low 32 bits of `seed`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn srand48(seed: c_long) {
    let _guard = LOCK48.lock();
    // The low 32 bits, in two words.
    reseed48([0x330e, seed as u16, (seed >> 16) as u16]);
}

/// Sets `X` from `seed16v` and returns a pointer to the `X` it replaced, in a
/// buffer the next call overwrites.
///
/// # Safety
///
/// `seed16v` must be valid to read three `unsigned short`s.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn seed48(seed16v: *mut c_ushort) -> *mut c_ushort {
    // SAFETY: the caller vouches for three words.
    let x = unsafe { seed16v.cast::<[u16; 3]>().read() };
    let _guard = LOCK48.lock();
    for (previous, current) in SEED48_PREVIOUS.iter().zip(&SEED48) {
        previous.store(current.load(Ordering::Relaxed), Ordering::Relaxed);
    }
    reseed48(x);
    // `AtomicU16` has `u16`'s layout, and the interior mutability lets C write
    // through the pointer.
    SEED48_PREVIOUS.as_ptr().cast::<c_ushort>().cast_mut()
}

/// Sets `X`, `a` and `c` from the seven words at `param`, in `SEED48`'s order.
///
/// # Safety
///
/// `param` must be valid to read seven `unsigned short`s.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lcong48(param: *mut c_ushort) {
    // SAFETY: the caller vouches for seven words.
    let values = unsafe { param.cast::<[u16; 7]>().read() };
    let _guard = LOCK48.lock();
    for (word, value) in SEED48.iter().zip(values) {
        word.store(value, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The global generators share state, so their tests run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn rand_follows_musls_sequence() {
        let _serial = SERIAL.lock();
        srand(1);
        let mut state = 0_u64;
        for _ in 0..5 {
            state = state.wrapping_mul(LCG64).wrapping_add(1);
            assert_eq!(rand(), (state >> 33) as c_int);
        }
        srand(0);
        assert_eq!(
            rand(),
            (0xffff_ffff_u64.wrapping_mul(LCG64).wrapping_add(1) >> 33) as c_int
        );
    }

    #[test]
    fn rand_r_tempers_its_state() {
        let mut seed: c_uint = 1;
        // SAFETY: `seed` is a local.
        let first = unsafe { rand_r(&raw mut seed) };
        assert_eq!(seed, 1_103_527_590);
        assert!((0..=0x7fff_ffff).contains(&first));
    }

    #[test]
    fn random_default_state_is_srandom_1_and_setstate_round_trips() {
        let _serial = SERIAL.lock();
        srandom(1);
        let first: Vec<c_long> = (0..50).map(|_| random()).collect();
        let mut buffer = [0_u8; 128];
        // SAFETY: `buffer` outlives its use as the state, which ends below.
        let previous = unsafe { initstate(1, buffer.as_mut_ptr().cast(), buffer.len()) };
        let again: Vec<c_long> = (0..50).map(|_| random()).collect();
        assert_eq!(first, again);
        // SAFETY: `previous` is the default state.
        let ours = unsafe { setstate(previous) };
        assert_eq!(ours.cast::<u8>(), buffer.as_mut_ptr());
        // The header records n = 31, i = 3 + 50 mod 31, j = 50 mod 31.
        assert_eq!(
            u32::from_ne_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]),
            31 << 16 | 22 << 8 | 19
        );
        // SAFETY: a buffer under 8 bytes is refused before it is used.
        assert!(unsafe { initstate(1, buffer.as_mut_ptr().cast(), 7) }.is_null());
        srandom(1);
    }

    #[test]
    fn a_tiny_state_is_a_plain_lcg() {
        let _serial = SERIAL.lock();
        let mut buffer = [0_u8; 16];
        // SAFETY: `buffer` is restored away from before the test returns.
        let previous = unsafe { initstate(5, buffer.as_mut_ptr().cast(), buffer.len()) };
        assert_eq!(
            random(),
            (5_u32.wrapping_mul(1_103_515_245).wrapping_add(12345) & 0x7fff_ffff).into()
        );
        // SAFETY: `previous` is the state before.
        let _ = unsafe { setstate(previous) };
    }

    #[test]
    fn rand48_matches_posix() {
        let _serial = SERIAL.lock();
        let _g = LOCK48.lock();
        reseed48([0, 0, 0]);
        drop(_g);
        assert_eq!(lrand48(), 0);
        assert_eq!(lrand48(), 2_116_118);
        assert_eq!(lrand48(), 89_401_895);

        srand48(0x1234_5678);
        let x = join48([0x330e, 0x5678, 0x1234]);
        let next = (0x5_deec_e66d_u64.wrapping_mul(x) + 0xb) & ((1 << 48) - 1);
        assert_eq!(mrand48(), c_long::from((next >> 16) as u32 as i32));

        let mut xsubi: [c_ushort; 3] = [1, 2, 3];
        // SAFETY: three words.
        let value = unsafe { erand48(xsubi.as_mut_ptr()) };
        let expected = (0x5_deec_e66d_u64.wrapping_mul(join48([1, 2, 3])) + 0xb) & ((1 << 48) - 1);
        assert_eq!(value, expected as f64 / (1_u64 << 48) as f64);
        assert_eq!(xsubi, split48(expected));

        let mut params: [c_ushort; 7] = [1, 0, 0, 2, 0, 0, 3];
        // SAFETY: seven words.
        unsafe { lcong48(params.as_mut_ptr()) };
        // Now a = 2 and c = 3 for every generator.
        let want = c_long::from(((2 * expected + 3) >> 16) as u32 as i32);
        // SAFETY: three words.
        assert_eq!(unsafe { jrand48(xsubi.as_mut_ptr()) }, want);
        let mut seed: [c_ushort; 3] = [7, 8, 9];
        // SAFETY: three words.
        let previous = unsafe { seed48(seed.as_mut_ptr()) };
        // `lcong48` set the global X to 1 as well.
        // SAFETY: `seed48` returns three words.
        assert_eq!(unsafe { previous.cast::<[u16; 3]>().read() }, [1, 0, 0]);
        // `seed48` restored the default `a` and `c`.
        let after = (0x5_deec_e66d_u64.wrapping_mul(join48([7, 8, 9])) + 0xb) & ((1 << 48) - 1);
        assert_eq!(lrand48(), (after >> 17) as c_long);
    }
}
