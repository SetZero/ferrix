//! What is left over from a division: `fmodl`, `remainderl` and `remquol`.
//!
//! All three are exact. `fmodl` takes the sign of the dividend and rounds the
//! quotient toward zero; `remainderl` rounds it to nearest, so its result can
//! have either sign and is never more than half the divisor in magnitude.
//! `remquol` is `remainderl` with the quotient's low three bits as well.
//!
//! Ported from musl 1.2.5's `x86_64/fmodl.c`, `x86_64/remainderl.c` and
//! `x86_64/remquol.c` (MIT; see [`crate::math`] for the notice). Each is one
//! `fprem` or `fprem1` repeated until the x87 says it finished, which
//! [`F80::fprem`] and [`F80::fprem1`] do.

use core::ffi::c_int;

use crate::math::ld80::{F80, export};

/// The remainder of `x` by `y` with the quotient rounded toward zero.
fn fmod_work(x: F80, y: F80) -> F80 {
    x.fprem(y)
}

/// The remainder of `x` by `y` with the quotient rounded to nearest.
fn remainder_work(x: F80, y: F80) -> F80 {
    x.fprem1(y).0
}

/// [`remainder_work`], and the quotient's low three bits through `quo`.
///
/// # Safety
///
/// `quo` must be a place a `c_int` may be written, as C's `int *` says.
unsafe fn remquo_work(x: F80, y: F80, quo: *mut c_int) -> F80 {
    let (remainder, status) = x.fprem1(y);
    // musl's remquol.c. The status word's condition codes carry the
    // quotient's low three bits, but not in order:
    //
    //   15 14 13 12 11 10  9  8
    //    . C3  .  .  . C2 C1 C0
    //    . b1  .  .  .  0 b0 b2
    //
    // Swapping the nibbles of the high byte puts {b0 b2 ? b1} in bits 5..2,
    // and the constant is a table of the eight answers read by that index.
    let byte = u8::try_from(status >> 8).unwrap_or(0);
    let swapped = byte.rotate_left(4);
    let bits = 0x7575_3131_6464_2020_u64 >> (swapped & 60);
    let bits = c_int::try_from(bits & 7).unwrap_or(0);
    let quotient = if x.is_negative() == y.is_negative() {
        bits
    } else {
        -bits
    };
    // SAFETY: the caller passed a place a `c_int` may be written.
    unsafe {
        *quo = quotient;
    }
    remainder
}

/// [`remquo_work`] as the adapter takes it, which is where the pointer stops
/// being C's and starts being this library's to trust.
fn remquo_at(x: F80, y: F80, quo: *mut c_int) -> F80 {
    // SAFETY: `quo` is the `int *` the caller of `remquol` passed, which C
    // requires to be a place an `int` may be written.
    unsafe { remquo_work(x, y, quo) }
}

export! {
    /// `fmodl`: the remainder of a division truncated toward zero.
    fn fmodl(long double, long double) -> long double = fmod_work;
}
export! {
    /// `remainderl`: the remainder of a division rounded to nearest.
    fn remainderl(long double, long double) -> long double = remainder_work;
}
export! {
    /// `remquol`: [`remainderl`], and the quotient's low three bits.
    fn remquol(long double, long double, quo: *mut c_int) -> long double = remquo_at;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fenv::{FE_INVALID, FE_TONEAREST};
    use crate::math::mtest;

    /// A `long double` from a `double`, for the cases a `double` says exactly.
    fn ld(x: f64) -> F80 {
        F80::from_f64(x)
    }

    #[test]
    fn fmod_keeps_the_dividend_sign_and_remainder_rounds_to_nearest() {
        assert_eq!(fmod_work(ld(7.0), ld(2.0)).to_f64(), 1.0);
        assert_eq!(fmod_work(ld(-7.0), ld(2.0)).to_f64(), -1.0);
        assert_eq!(fmod_work(ld(7.0), ld(-2.0)).to_f64(), 1.0);
        // 7 = 4*2 - 1 to nearest, so the remainder is negative where fmod's
        // is positive.
        assert_eq!(remainder_work(ld(7.0), ld(2.0)).to_f64(), -1.0);
        assert_eq!(remainder_work(ld(5.0), ld(2.0)).to_f64(), 1.0);
        assert_eq!(remainder_work(ld(-7.0), ld(2.0)).to_f64(), 1.0);
        // A zero divisor is invalid and gives a NaN.
        let (value, raised) = mtest::under(FE_TONEAREST, || fmod_work(ld(1.0), F80::ZERO));
        assert!(value.is_nan());
        assert_eq!(raised, FE_INVALID);
    }

    #[test]
    fn remquo_reports_the_quotients_low_bits() {
        // The sign of the quotient, and its low three bits, for a spread of
        // quotients: the remainder is checked beside each.
        for (x, y, quotient, remainder) in [
            (7.0, 2.0, 4, -1.0),
            (5.0, 2.0, 2, 1.0),
            (-7.0, 2.0, -4, 1.0),
            (7.0, -2.0, -4, -1.0),
            (3.0, 2.0, 2, -1.0),
            (1.0, 2.0, 0, 1.0),
            (11.0, 2.0, 6, -1.0),
            (13.0, 2.0, 6, 1.0),
        ] {
            let mut quo: c_int = 0xdead;
            let value = remquo_at(ld(x), ld(y), &raw mut quo);
            assert_eq!(value.to_f64(), remainder, "remquo({x}, {y}) remainder");
            // Only the low three bits are promised, with the quotient's sign.
            let expected = if quotient < 0 {
                -((-quotient) & 7)
            } else {
                quotient & 7
            };
            assert_eq!(quo, expected, "remquo({x}, {y}) quotient bits");
        }
    }
}
