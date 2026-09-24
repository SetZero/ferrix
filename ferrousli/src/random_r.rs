//! glibc's reentrant `random`: `random_r`, `srandom_r`, `initstate_r` and
//! `setstate_r`, over a `struct random_data` the caller owns. Chrome calls
//! `initstate_r` and `random_r`.
//!
//! musl has none of them. The generator is the BSD additive feedback
//! generator `random` has always been, with glibc's state layout and
//! seeding, so a seed gives the sequence it gives under glibc; the unit
//! tests run both side by side. It is written from that description, not
//! from glibc's source, which is LGPL.
//!
//! A state buffer holds a header word, then the generator's words. The
//! header says which generator and where the rear pointer is:
//! `TYPE_0`, or `rear * 5 + type`. The buffer's size picks the type: under
//! 8 bytes is refused, then 8, 32, 64, 128 and 256 bytes give degrees 0, 7,
//! 15, 31 and 63, with separations 0, 3, 1, 3 and 1. Type 0 is the linear
//! congruential generator `x * 1103515245 + 12345`, kept to 31 bits.

use core::ffi::{c_char, c_int, c_long, c_uint};

use crate::errno;

/// How many generator types there are.
const MAX_TYPES: i32 = 5;
/// Each type's degree: the words of state it keeps.
const DEGREES: [i32; 5] = [0, 7, 15, 31, 63];
/// Each type's separation between the front and rear pointers.
const SEPARATIONS: [i32; 5] = [0, 3, 1, 3, 1];

/// glibc's `struct random_data`.
#[repr(C)]
#[derive(Debug)]
pub struct RandomData {
    /// The front pointer.
    pub fptr: *mut i32,
    /// The rear pointer.
    pub rptr: *mut i32,
    /// The generator's words, after the header word.
    pub state: *mut i32,
    /// Which generator, 0 to 4.
    pub rand_type: c_int,
    /// Its degree.
    pub rand_deg: c_int,
    /// Its separation.
    pub rand_sep: c_int,
    /// One past the last word.
    pub end_ptr: *mut i32,
}

/// Fails with `EINVAL`, returning -1.
fn invalid() -> c_int {
    errno::set(errno::EINVAL);
    -1
}

/// The table entry for generator `kind`, or `None` for one out of range.
fn parameters(kind: i32) -> Option<(i32, i32)> {
    let index = usize::try_from(kind).ok()?;
    Some((*DEGREES.get(index)?, *SEPARATIONS.get(index)?))
}

/// Records in the header word below `buf`'s state where its rear pointer
/// is, so `setstate_r` can resume it.
///
/// # Safety
///
/// `buf.state` must be null or a state set up by `initstate_r`.
unsafe fn save_position(buf: &RandomData) {
    if buf.state.is_null() {
        return;
    }
    let header = if buf.rand_type == 0 {
        0
    } else {
        // SAFETY: `rptr` points into the same state.
        let rear = unsafe { buf.rptr.offset_from(buf.state) } as i32;
        MAX_TYPES * rear + buf.rand_type
    };
    // SAFETY: the header word is the one before the state.
    unsafe { buf.state.wrapping_sub(1).write(header) };
}

/// The next number, from 0 to 2^31 - 1, into `*result`.
///
/// # Safety
///
/// `buf` must be null or a `struct random_data` set up by `initstate_r` or
/// `setstate_r`, and `result` null or valid for a write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn random_r(buf: *mut RandomData, result: *mut i32) -> c_int {
    if buf.is_null() || result.is_null() {
        return invalid();
    }
    // SAFETY: the caller vouches for `buf`.
    let buf = unsafe { &mut *buf };
    let state = buf.state;
    let value = if buf.rand_type == 0 {
        // SAFETY: type 0 keeps its one word at `state`.
        let word = unsafe { state.read() } as u32;
        let next = (word.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7fff_ffff) as i32;
        // SAFETY: as above.
        unsafe { state.write(next) };
        next
    } else {
        let (mut front, mut rear) = (buf.fptr, buf.rptr);
        // SAFETY: both pointers are inside the state.
        let sum = unsafe { front.read() as u32 }.wrapping_add(unsafe { rear.read() } as u32);
        // SAFETY: as above.
        unsafe { front.write(sum as i32) };
        front = front.wrapping_add(1);
        if front >= buf.end_ptr {
            front = state;
            rear = rear.wrapping_add(1);
        } else {
            rear = rear.wrapping_add(1);
            if rear >= buf.end_ptr {
                rear = state;
            }
        }
        buf.fptr = front;
        buf.rptr = rear;
        // The least random bit is dropped.
        (sum >> 1) as i32
    };
    // SAFETY: the caller vouches for `result`.
    unsafe { result.write(value) };
    0
}

/// Seeds the generator in `buf` with `seed`, 0 counting as 1.
///
/// # Safety
///
/// As [`random_r`], for `buf`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn srandom_r(seed: c_uint, buf: *mut RandomData) -> c_int {
    if buf.is_null() {
        return invalid();
    }
    // SAFETY: the caller vouches for `buf`.
    let data = unsafe { &mut *buf };
    let Some((degree, separation)) = parameters(data.rand_type) else {
        return invalid();
    };
    let state = data.state;
    let seed = if seed == 0 { 1 } else { seed };
    // SAFETY: the state holds at least one word.
    unsafe { state.write(seed as i32) };
    if data.rand_type == 0 {
        return 0;
    }
    // Each word is 16807 times the one before, modulo 2^31 - 1, computed
    // in a `long` without overflow (Schrage's method).
    let mut word = c_long::from(seed as i32);
    for i in 1..degree as usize {
        let hi = word / 127_773;
        let lo = word % 127_773;
        word = 16_807 * lo - 2_836 * hi;
        if word < 0 {
            word += 2_147_483_647;
        }
        // SAFETY: `i` is below the degree, inside the state.
        unsafe { state.wrapping_add(i).write(word as i32) };
    }
    data.fptr = state.wrapping_add(separation as usize);
    data.rptr = state;
    let mut discard = 0;
    for _ in 0..degree * 10 {
        // SAFETY: `buf` is set up now, and `discard` a live local.
        let _ = unsafe { random_r(buf, &raw mut discard) };
    }
    0
}

/// Sets `buf` up to use the `size` bytes at `state` as its generator,
/// seeded with `seed`. The generator's type follows from `size`. As in
/// glibc, the state `buf` used before, if any, first has its position
/// saved in its header, so `buf` must start zeroed.
///
/// # Safety
///
/// `state` must be valid for `size` bytes and aligned for `int32_t`, and
/// live as long as `buf` uses it; `buf` must be zeroed or set up already.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(
    clippy::cast_ptr_alignment,
    reason = "the caller's state is aligned for int32_t, as glibc requires"
)]
pub unsafe extern "C" fn initstate_r(
    seed: c_uint,
    state: *mut c_char,
    size: usize,
    buf: *mut RandomData,
) -> c_int {
    if buf.is_null() {
        return invalid();
    }
    // SAFETY: the caller vouches for `buf`.
    let data = unsafe { &mut *buf };
    // SAFETY: as above.
    unsafe { save_position(data) };
    let kind = match size {
        256.. => 4,
        128.. => 3,
        64.. => 2,
        32.. => 1,
        8.. => 0,
        _ => return invalid(),
    };
    let Some((degree, separation)) = parameters(kind) else {
        return invalid();
    };
    let words = state.cast::<i32>().wrapping_add(1);
    data.rand_type = kind;
    data.rand_deg = degree;
    data.rand_sep = separation;
    data.state = words;
    data.end_ptr = words.wrapping_add(degree as usize);
    // SAFETY: `buf` is set up for its state.
    let _ = unsafe { srandom_r(seed, buf) };
    // SAFETY: the caller vouches for `buf`.
    let data = unsafe { &*buf };
    // SAFETY: its state is the one just set up.
    unsafe { save_position(data) };
    0
}

/// Switches `buf` to the state at `state`, set up by `initstate_r` and
/// perhaps used with another `struct random_data`, resuming it where its
/// header says.
///
/// # Safety
///
/// `state` must be such a state, and `buf` set up.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(
    clippy::cast_ptr_alignment,
    reason = "the caller's state is aligned for int32_t, as glibc requires"
)]
pub unsafe extern "C" fn setstate_r(state: *mut c_char, buf: *mut RandomData) -> c_int {
    if state.is_null() || buf.is_null() {
        return invalid();
    }
    // SAFETY: the caller vouches for `buf`.
    let data = unsafe { &mut *buf };
    // SAFETY: as above.
    unsafe { save_position(data) };
    let words = state.cast::<i32>().wrapping_add(1);
    // SAFETY: the caller's state begins with its header word.
    let header = unsafe { state.cast::<i32>().read() };
    let kind = header % MAX_TYPES;
    let Some((degree, separation)) = parameters(kind) else {
        return invalid();
    };
    data.rand_type = kind;
    data.rand_deg = degree;
    data.rand_sep = separation;
    if kind != 0 {
        let rear = header / MAX_TYPES;
        data.rptr = words.wrapping_add(rear as usize);
        data.fptr = words.wrapping_add(((rear + separation) % degree) as usize);
    }
    data.state = words;
    data.end_ptr = words.wrapping_add(degree as usize);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr::null_mut;

    unsafe extern "C" {
        #[link_name = "initstate_r"]
        fn host_initstate_r(
            seed: c_uint,
            state: *mut c_char,
            size: usize,
            buf: *mut RandomData,
        ) -> c_int;
        #[link_name = "random_r"]
        fn host_random_r(buf: *mut RandomData, result: *mut i32) -> c_int;
    }

    /// A zeroed `struct random_data`.
    fn zeroed() -> RandomData {
        RandomData {
            fptr: null_mut(),
            rptr: null_mut(),
            state: null_mut(),
            rand_type: 0,
            rand_deg: 0,
            rand_sep: 0,
            end_ptr: null_mut(),
        }
    }

    #[test]
    fn every_state_size_gives_the_sequence_the_host_glibc_gives() {
        for size in [8, 32, 64, 128, 256, 300] {
            for seed in [0, 1, 42, 0x7fff_ffff, 0xdead_beef] {
                let mut ours_state = [0_i32; 80];
                let mut host_state = [0_i32; 80];
                let (mut ours, mut host) = (zeroed(), zeroed());
                // SAFETY: each state holds `size` bytes, and each buffer is
                // zeroed.
                unsafe {
                    assert_eq!(
                        initstate_r(seed, ours_state.as_mut_ptr().cast(), size, &raw mut ours),
                        0
                    );
                }
                // SAFETY: as above.
                unsafe {
                    assert_eq!(
                        host_initstate_r(seed, host_state.as_mut_ptr().cast(), size, &raw mut host),
                        0
                    );
                }
                for step in 0..200 {
                    let (mut a, mut b) = (0, 0);
                    // SAFETY: set up above.
                    unsafe { assert_eq!(random_r(&raw mut ours, &raw mut a), 0) };
                    // SAFETY: as above.
                    unsafe { assert_eq!(host_random_r(&raw mut host, &raw mut b), 0) };
                    assert_eq!(a, b, "size {size} seed {seed} step {step}");
                }
                assert_eq!(
                    ours_state[0], host_state[0],
                    "header, size {size} seed {seed}"
                );
            }
        }
    }

    #[test]
    fn a_saved_state_resumes_where_it_stopped() {
        let mut state = [0_i32; 32];
        let mut buf = zeroed();
        // SAFETY: 128 bytes, and a zeroed buffer.
        unsafe {
            assert_eq!(
                initstate_r(7, state.as_mut_ptr().cast(), 128, &raw mut buf),
                0
            )
        };
        let mut first = [0; 5];
        for value in &mut first {
            // SAFETY: set up.
            unsafe { assert_eq!(random_r(&raw mut buf, value), 0) };
        }
        let mut other_state = [0_i32; 2];
        let mut other = zeroed();
        // SAFETY: switching `buf` away saves its position in `state`.
        unsafe {
            assert_eq!(
                initstate_r(1, other_state.as_mut_ptr().cast(), 8, &raw mut buf),
                0
            )
        };
        // SAFETY: `state` was set up by `initstate_r`.
        unsafe { assert_eq!(setstate_r(state.as_mut_ptr().cast(), &raw mut other), 0) };
        let mut resumed = 0;
        let mut expected = 0;
        // SAFETY: set up.
        unsafe { assert_eq!(random_r(&raw mut other, &raw mut resumed), 0) };
        let mut fresh_state = [0_i32; 32];
        let mut fresh = zeroed();
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                initstate_r(7, fresh_state.as_mut_ptr().cast(), 128, &raw mut fresh),
                0
            )
        };
        for _ in 0..6 {
            // SAFETY: set up.
            unsafe { assert_eq!(random_r(&raw mut fresh, &raw mut expected), 0) };
        }
        assert_eq!(resumed, expected);
        // SAFETY: too small a state, refused before anything is written.
        assert_eq!(
            unsafe { initstate_r(1, state.as_mut_ptr().cast(), 7, &raw mut fresh) },
            -1
        );
    }
}
