//! The few pieces of arithmetic a pixel loop needs, without a call.
//!
//! `f32::round`, `f32::floor` and a comparison of two slices are each a call
//! into the C library on a baseline x86-64: the instruction that rounds in
//! one go is an extension the target does not assume, and `==` on bytes is
//! `memcmp`. On the machine this is developed on that is glibc, whose
//! versions are picked for the processor at load time and cost almost
//! nothing. On Ferrix the compositor is a static musl program, and musl's
//! are plain portable C: a `roundf` a channel a pixel made one window's
//! shadow 170 ms in a guest where the same frame's copy of that window was
//! 9, and its byte-at-a-time `memcmp` made keeping the backdrop up to date
//! forty times dearer than on the host. `blur::floor` found the same thing
//! first and says so.
//!
//! Each of these is the library's answer for every value it is given here,
//! not an approximation of it: the tests compare them with the library over
//! every value that could differ.

/// `value.round().clamp(0.0, 255.0) as u8`.
///
/// `as u8` truncates towards zero and holds what is out of range at the
/// ends, which is the whole part for anything a byte can hold. What is left
/// of it is exact -- two floats less than one apart subtract without
/// rounding -- so comparing it with a half is `round`'s own rule, half away
/// from zero. Below zero both give 0 and past 255 both give 255; a NaN is 0
/// either way.
pub(crate) fn byte(value: f32) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the cast is the point: it truncates, and holds the ends"
    )]
    let whole = value as u8;
    if value - f32::from(whole) >= 0.5 {
        whole.saturating_add(1)
    } else {
        whole
    }
}

/// `value.round() as usize`, for a position in a table of a few hundred
/// entries: exact below `2^24`, which is every whole number an `f32` holds.
pub(crate) fn nearest(value: f32) -> usize {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the cast is the point: it truncates, and holds the ends"
    )]
    let whole = value as usize;
    #[expect(
        clippy::cast_precision_loss,
        reason = "a place in a table, far inside what an f32 holds exactly"
    )]
    let left = value - whole as f32;
    if left >= 0.5 {
        whole.saturating_add(1)
    } else {
        whole
    }
}

/// Whether two runs of bytes are the same bytes, eight at a time.
pub(crate) fn same(one: &[u8], other: &[u8]) -> bool {
    if one.len() != other.len() {
        return false;
    }
    let (ones, others) = (one.chunks_exact(8), other.chunks_exact(8));
    let tails = ones.remainder().iter().eq(others.remainder());
    let word = |bytes: &[u8]| bytes.try_into().map_or(0, u64::from_ne_bytes);
    tails && ones.zip(others).all(|(a, b)| word(a) == word(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every float from below zero to past a byte's end in steps of a
    /// 256th, and the float either side of every half, which is where a
    /// rounding rule that was nearly right would show.
    #[test]
    fn a_byte_is_rounded_as_the_library_rounds_it() {
        let the_library = |value: f32| value.round().clamp(0.0, 255.0) as u8;
        for step in -1024_i32..=70_000 {
            let value = step as f32 / 256.0;
            for near in [value, value.next_down(), value.next_up()] {
                assert_eq!(byte(near), the_library(near), "{near:?}");
            }
        }
        for odd in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -0.0,
            1e30,
            -1e30,
        ] {
            assert_eq!(byte(odd), the_library(odd), "{odd:?}");
        }
    }

    #[test]
    fn a_place_is_rounded_as_the_library_rounds_it() {
        for step in -64_i32..=40_000 {
            let value = step as f32 / 32.0;
            for near in [value, value.next_down(), value.next_up()] {
                assert_eq!(nearest(near), near.round() as usize, "{near:?}");
            }
        }
        assert_eq!(nearest(f32::NAN), f32::NAN.round() as usize);
    }

    /// The same, a byte different anywhere -- in a whole word, in what is
    /// left after the words -- and a different length.
    #[test]
    fn bytes_are_the_same_or_they_are_not() {
        let bytes: Vec<u8> = (0..=255).cycle().take(1027).collect();
        assert!(same(&bytes, &bytes.clone()));
        assert!(same(&[], &[]));
        for at in [0, 7, 8, 511, 1023, 1024, 1026] {
            let mut other = bytes.clone();
            other[at] ^= 1;
            assert!(
                !same(&bytes, &other),
                "a different byte at {at} went unseen"
            );
        }
        assert!(!same(&bytes, &bytes[..1026]));
    }
}
