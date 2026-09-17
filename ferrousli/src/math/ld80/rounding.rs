//! Rounding a `long double` to an integer value: `ceill`, `floorl`, `truncl`,
//! `roundl`, `rintl`, `nearbyintl`, and the four that return an integer type,
//! `lrintl`, `llrintl`, `lroundl` and `llroundl`.
//!
//! `rintl` rounds in the current mode and raises inexact when it changes the
//! value; `nearbyintl` is the same rounding with the flag left as it was.
//! `ceill`, `floorl` and `truncl` round in one named direction whatever the
//! mode, and `roundl` rounds halves away from zero, which no x87 mode does.
//!
//! Ported from musl 1.2.5's `x86_64/rintl.c`, `x86_64/floorl.s` (which holds
//! `ceill` and `truncl` too), `x86_64/lrintl.c`, `x86_64/llrintl.c`,
//! `roundl.c`, `lroundl.c`, `llroundl.c` and `nearbyintl.c` (MIT; see
//! [`crate::math`] for the notice).

use core::ffi::{c_long, c_longlong};

use crate::fenv::{FE_INEXACT, feclearexcept, fetestexcept};
use crate::math::ld80::{F80, export, hexf80};

/// The control word's high byte for each named direction, as musl's
/// `floorl.s` loads it: the low byte stays as it was, so only the rounding
/// control changes and the precision stays at 64 bits.
const DOWN: u8 = 0x07;
/// Toward positive infinity.
const UP: u8 = 0x0b;
/// Toward zero.
const ZERO: u8 = 0x0f;

/// `1/LDBL_EPSILON`, which is `0x1p63`: adding it to a number smaller than it
/// and taking it away again rounds that number to an integer in the current
/// mode, which is how musl's `roundl` reaches the halfway case.
fn toint() -> F80 {
    hexf80!("0x1p63L")
}

/// One half, the size of the step `roundl` decides on.
fn half() -> F80 {
    hexf80!("0x1p-1L")
}

/// `x` rounded toward positive infinity.
fn ceil_work(x: F80) -> F80 {
    x.rndint_with_control(UP)
}

/// `x` rounded toward negative infinity.
fn floor_work(x: F80) -> F80 {
    x.rndint_with_control(DOWN)
}

/// `x` rounded toward zero.
fn trunc_work(x: F80) -> F80 {
    x.rndint_with_control(ZERO)
}

/// `x` rounded in the current mode, raising inexact when that changes it.
fn rint_work(x: F80) -> F80 {
    x.rndint()
}

/// [`rint_work`] with the inexact flag left as it was found.
fn nearbyint_work(x: F80) -> F80 {
    let inexact = fetestexcept(FE_INEXACT);
    let y = rint_work(x);
    if inexact == 0 {
        let _ = feclearexcept(FE_INEXACT);
    }
    y
}

/// `x` with halves rounded away from zero, whatever the current mode.
///
/// musl's `roundl.c`: a number with no fractional part is its own answer, one
/// below a half rounds to a zero of `x`'s sign, and the rest is decided by
/// `y`, the distance from `x` to the integer `toint` rounded it to.
fn round_work(x: F80) -> F80 {
    let exponent = x.exponent();
    // 0x3fff + 63: at that exponent and above the value is already an
    // integer, and adding `toint` would lose bits rather than reveal them.
    if exponent >= 0x403e {
        return x;
    }
    let negative = x.is_negative();
    let magnitude = if negative { x.neg() } else { x };
    if exponent < 0x3ffe {
        // Below a half: the answer is a zero of `x`'s sign, and the addition
        // is kept so that inexact is raised as the arithmetic would.
        let _ = magnitude.add(toint());
        return F80::ZERO.mul(x);
    }
    let rounded = magnitude.add(toint()).sub(toint());
    let mut y = rounded.sub(magnitude);
    y = if F80::fcomi(y, half()).above() {
        y.add(magnitude).sub(F80::ONE)
    } else if F80::fcomi(y, half().neg()).below_or_equal() {
        y.add(magnitude).add(F80::ONE)
    } else {
        y.add(magnitude)
    };
    if negative { y.neg() } else { y }
}

/// `x` rounded in the current mode to an integer, with `fistpll`.
fn lrint_work(x: F80) -> c_long {
    x.to_i64()
}

/// [`lrint_work`] for `long long`, which is the same width here.
fn llrint_work(x: F80) -> c_longlong {
    x.to_i64()
}

/// [`round_work`] converted to an integer, which is how musl's `lroundl.c`
/// returns it: the conversion truncates, and the value it is given has no
/// fractional part.
fn lround_work(x: F80) -> c_long {
    round_work(x).to_i64_truncating()
}

/// [`lround_work`] for `long long`, which is the same width here.
fn llround_work(x: F80) -> c_longlong {
    round_work(x).to_i64_truncating()
}

export! {
    /// `ceill`: the smallest integer value not less than a `long double`.
    fn ceill(long double) -> long double = ceil_work;
}
export! {
    /// `floorl`: the largest integer value not greater than a `long double`.
    fn floorl(long double) -> long double = floor_work;
}
export! {
    /// `truncl`: a `long double` with its fractional part removed.
    fn truncl(long double) -> long double = trunc_work;
}
export! {
    /// `rintl`: a `long double` rounded in the current mode, raising inexact.
    fn rintl(long double) -> long double = rint_work;
}
export! {
    /// `nearbyintl`: [`rintl`] leaving the inexact flag alone.
    fn nearbyintl(long double) -> long double = nearbyint_work;
}
export! {
    /// `roundl`: a `long double` with halves rounded away from zero.
    fn roundl(long double) -> long double = round_work;
}
export! {
    /// `lrintl`: a `long double` rounded in the current mode, as a `long`.
    fn lrintl(long double) -> c_long = lrint_work;
}
export! {
    /// `llrintl`: [`lrintl`] as a `long long`.
    fn llrintl(long double) -> c_longlong = llrint_work;
}
export! {
    /// `lroundl`: a `long double` with halves rounded away from zero, as a
    /// `long`.
    fn lroundl(long double) -> c_long = lround_work;
}
export! {
    /// `llroundl`: [`lroundl`] as a `long long`.
    fn llroundl(long double) -> c_longlong = llround_work;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fenv::{FE_TONEAREST, FE_UPWARD};
    use crate::math::mtest;

    /// A `long double` from a `double`, for the cases a `double` says exactly.
    fn ld(x: f64) -> F80 {
        F80::from_f64(x)
    }

    #[test]
    fn the_named_directions_ignore_the_current_mode() {
        for mode in [FE_TONEAREST, FE_UPWARD] {
            let (value, _) = mtest::under(mode, || ceil_work(ld(2.5)));
            assert_eq!(value.to_f64(), 3.0, "ceil");
            let (value, _) = mtest::under(mode, || floor_work(ld(2.5)));
            assert_eq!(value.to_f64(), 2.0, "floor");
            let (value, _) = mtest::under(mode, || trunc_work(ld(-2.5)));
            assert_eq!(value.to_f64(), -2.0, "trunc");
            let (value, _) = mtest::under(mode, || ceil_work(ld(-2.5)));
            assert_eq!(value.to_f64(), -2.0, "ceil of a negative");
            let (value, _) = mtest::under(mode, || floor_work(ld(-2.5)));
            assert_eq!(value.to_f64(), -3.0, "floor of a negative");
        }
    }

    #[test]
    fn rint_follows_the_mode_and_nearbyint_leaves_the_flag() {
        let (value, raised) = mtest::under(FE_TONEAREST, || rint_work(ld(2.5)));
        assert_eq!(value.to_f64(), 2.0, "halves go to even");
        assert_eq!(raised, FE_INEXACT);
        let (value, raised) = mtest::under(FE_UPWARD, || rint_work(ld(2.5)));
        assert_eq!(value.to_f64(), 3.0);
        assert_eq!(raised, FE_INEXACT);
        let (value, raised) = mtest::under(FE_TONEAREST, || nearbyint_work(ld(2.5)));
        assert_eq!(value.to_f64(), 2.0);
        assert_eq!(raised, 0, "nearbyint raises no inexact");
        // An integer argument raises nothing either way.
        let (value, raised) = mtest::under(FE_TONEAREST, || rint_work(ld(4.0)));
        assert_eq!(value.to_f64(), 4.0);
        assert_eq!(raised, 0);
    }

    #[test]
    fn round_takes_halves_away_from_zero() {
        for (given, expected) in [
            (0.5, 1.0),
            (-0.5, -1.0),
            (2.5, 3.0),
            (-2.5, -3.0),
            (2.4, 2.0),
            (-2.4, -2.0),
            (0.4, 0.0),
            (-0.4, -0.0),
            (0.0, 0.0),
            (-0.0, -0.0),
        ] {
            let (value, _) = mtest::under(FE_TONEAREST, || round_work(ld(given)));
            assert_eq!(value.to_f64(), expected, "round({given})");
            assert_eq!(
                value.is_negative(),
                expected.is_sign_negative(),
                "round({given}) sign"
            );
        }
        // Rounding away from zero holds in every mode, unlike rint.
        let (value, _) = mtest::under(FE_UPWARD, || round_work(ld(-2.5)));
        assert_eq!(value.to_f64(), -3.0);
        // A value with no fractional part comes back untouched.
        let big = hexf80!("0x1p+64L");
        assert_eq!(
            (round_work(big).mantissa(), round_work(big).sign_exponent()),
            (big.mantissa(), big.sign_exponent())
        );
    }

    #[test]
    fn the_integer_returns_round_as_their_names_say() {
        let (value, _) = mtest::under(FE_TONEAREST, || lrint_work(ld(2.5)));
        assert_eq!(value, 2, "lrint takes the mode's answer");
        let (value, _) = mtest::under(FE_UPWARD, || lrint_work(ld(2.5)));
        assert_eq!(value, 3);
        let (value, _) = mtest::under(FE_TONEAREST, || llrint_work(ld(-2.5)));
        assert_eq!(value, -2);
        assert_eq!(
            lround_work(ld(2.5)),
            3,
            "lround takes halves away from zero"
        );
        assert_eq!(lround_work(ld(-2.5)), -3);
        assert_eq!(llround_work(ld(0.5)), 1);
        assert_eq!(llround_work(ld(-0.4)), 0);
    }
}
