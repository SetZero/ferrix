//! Taking a `long double` apart and putting one together: `fabsl`,
//! `copysignl`, `fmaxl`, `fminl`, `fdiml`, `nanl`, `frexpl`, `ldexpl`,
//! `scalbnl`, `scalblnl`, `logbl`, `ilogbl`, `modfl`, `nextafterl`,
//! `nexttowardl`, and `nexttoward` and `nexttowardf`, which take a
//! `long double` and return one of the narrower types.
//!
//! Every one is exact but for `scalbnl`, which overflows and underflows, and
//! `nextafterl`, which does when it steps to an infinity or below the normal
//! range. `ilogbl` and `logbl` raise invalid or divide-by-zero for a zero, an
//! infinity or a NaN.
//!
//! Ported from musl 1.2.5's `x86_64/fabsl.c`, `copysignl.c`, `fmaxl.c`,
//! `fminl.c`, `fdiml.c`, `nanl.c`, `frexpl.c`, `ldexpl.c`, `scalbnl.c`,
//! `scalblnl.c`, `logbl.c`, `ilogbl.c`, `modfl.c`, `nextafterl.c`,
//! `nexttowardl.c`, `nexttoward.c` and `nexttowardf.c` (MIT; see
//! [`crate::math`] for the notice).

use core::ffi::{c_char, c_int, c_long};

use crate::math::ld80::{F80, export, hexf80};
use crate::math::manipulate::{FP_ILOGB0, FP_ILOGBNAN};
use crate::math::support::{barrierf, force_eval, force_evalf};

/// The exponent field of a zero and of a subnormal.
const SUBNORMAL: u16 = 0;
/// The exponent field of an infinity and of a NaN.
const SPECIAL: u16 = 0x7fff;
/// The exponent field of `1.0`, which is the bias.
const BIAS: u16 = 0x3fff;
/// How many bits the significand holds, the explicit integer bit included.
const MANTISSA_BITS: u16 = 64;

/// `1/LDBL_EPSILON`, `0x1p63`: see [`super::rounding`] for what adding and
/// taking it away again does.
fn toint() -> F80 {
    hexf80!("0x1p63L")
}

/// Whether `x` is neither an infinity nor a NaN.
fn is_finite(x: F80) -> bool {
    x.exponent() != SPECIAL
}

/// Whether `x` is a zero of either sign.
fn is_zero(x: F80) -> bool {
    x.exponent() == SUBNORMAL && x.mantissa() == 0
}

/// A zero with `x`'s sign, which is what several of these return.
fn signed_zero(x: F80) -> F80 {
    if x.is_negative() {
        F80::ZERO.neg()
    } else {
        F80::ZERO
    }
}

/// `|x|`, with `fabs`.
fn fabs_work(x: F80) -> F80 {
    x.abs()
}

/// `x` with `y`'s sign.
fn copysign_work(x: F80, y: F80) -> F80 {
    let sign_exponent = (x.sign_exponent() & 0x7fff) | (y.sign_exponent() & 0x8000);
    F80::from_parts(x.mantissa(), sign_exponent)
}

/// The larger of `x` and `y`, a NaN losing to a number and `-0` to `+0`.
fn fmax_work(x: F80, y: F80) -> F80 {
    if x.is_nan() {
        return y;
    }
    if y.is_nan() {
        return x;
    }
    // Signed zeros, as C99 Annex F.9.9.2 has them: the comparison below calls
    // them equal, so the sign decides.
    if x.is_negative() != y.is_negative() {
        return if x.is_negative() { y } else { x };
    }
    if F80::fcomi(x, y).below() { y } else { x }
}

/// The smaller of `x` and `y`, a NaN losing to a number and `-0` to `+0`.
fn fmin_work(x: F80, y: F80) -> F80 {
    if x.is_nan() {
        return y;
    }
    if y.is_nan() {
        return x;
    }
    if x.is_negative() != y.is_negative() {
        return if x.is_negative() { x } else { y };
    }
    if F80::fcomi(x, y).below() { x } else { y }
}

/// `x - y` where that is positive, and `+0` where it is not.
fn fdim_work(x: F80, y: F80) -> F80 {
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return y;
    }
    if F80::fcomi(x, y).above() {
        x.sub(y)
    } else {
        F80::ZERO
    }
}

/// A NaN, whatever the string says, as musl's `nanl.c` returns one.
fn nan_work(_tag: *const c_char) -> F80 {
    F80::NAN
}

/// `x × 2^n`, overflowing and underflowing as the arithmetic does.
///
/// musl's `scalbnl.c`: `n` is brought inside the exponent range by at most two
/// multiplications at each end, so the last one — by a power of two made here
/// — rounds once, where a single huge step would round twice.
fn scalbn_work(x: F80, n: c_int) -> F80 {
    let mut x = x;
    let mut n = n;
    if n > 16383 {
        x = x.mul(hexf80!("0x1p16383L"));
        n -= 16383;
        if n > 16383 {
            x = x.mul(hexf80!("0x1p16383L"));
            n -= 16383;
            if n > 16383 {
                n = 16383;
            }
        }
    } else if n < -16382 {
        x = x.mul(hexf80!("0x1p-16382L")).mul(hexf80!("0x1p113L"));
        n += 16382 - 113;
        if n < -16382 {
            x = x.mul(hexf80!("0x1p-16382L")).mul(hexf80!("0x1p113L"));
            n += 16382 - 113;
            if n < -16382 {
                n = -16382;
            }
        }
    }
    // `1.0` with `n` added to its exponent, which is now in range.
    let exponent = u16::try_from(c_int::from(BIAS) + n).unwrap_or(BIAS);
    x.mul(F80::from_parts(1 << 63, exponent))
}

/// [`scalbn_work`] for a `long`, which musl's `scalblnl.c` clamps to an `int`
/// first: anything beyond that overflows or underflows either way.
fn scalbln_work(x: F80, n: c_long) -> F80 {
    let n = c_int::try_from(n).unwrap_or(if n > 0 { c_int::MAX } else { c_int::MIN });
    scalbn_work(x, n)
}

/// The significand of `x` in `[0.5, 1)`, with its exponent through `e`.
///
/// # Safety
///
/// `e` must be a place a `c_int` may be written, as C's `int *` says.
unsafe fn frexp_work(x: F80, e: *mut c_int) -> F80 {
    let exponent = x.exponent();
    if exponent == SUBNORMAL {
        if is_zero(x) {
            // SAFETY: the caller passed a place a `c_int` may be written.
            unsafe {
                *e = 0;
            }
            return x;
        }
        // A subnormal: scale it into the normal range, take it apart there,
        // and give back what the scaling added.
        // SAFETY: as above.
        let scaled = unsafe { frexp_work(x.mul(hexf80!("0x1p120L")), e) };
        // SAFETY: as above.
        unsafe {
            *e -= 120;
        }
        return scaled;
    }
    if exponent == SPECIAL {
        return x;
    }
    // SAFETY: as above.
    unsafe {
        *e = c_int::from(exponent) - c_int::from(BIAS - 1);
    }
    let sign_exponent = (x.sign_exponent() & 0x8000) | (BIAS - 1);
    F80::from_parts(x.mantissa(), sign_exponent)
}

/// [`frexp_work`] as the adapter takes it.
fn frexp_at(x: F80, e: *mut c_int) -> F80 {
    // SAFETY: `e` is the `int *` the caller of `frexpl` passed, which C
    // requires to be a place an `int` may be written.
    unsafe { frexp_work(x, e) }
}

/// The exponent of `x` as an integer, or what C names for the cases that have
/// none.
fn ilogb_work(x: F80) -> c_int {
    let exponent = x.exponent();
    let mut mantissa = x.mantissa();
    if exponent == SUBNORMAL {
        if mantissa == 0 {
            force_evalf(barrierf(0.0) / 0.0);
            return FP_ILOGB0;
        }
        // A subnormal: count the leading zeros of the significand off the
        // smallest normal exponent.
        let mut found = -c_int::from(BIAS) + 1;
        while mantissa >> 63 == 0 {
            found -= 1;
            mantissa <<= 1;
        }
        return found;
    }
    if exponent == SPECIAL {
        force_evalf(barrierf(0.0) / 0.0);
        // A NaN, or an infinity, which C says reports `INT_MAX`.
        return if mantissa << 1 != 0 {
            FP_ILOGBNAN
        } else {
            c_int::MAX
        };
    }
    c_int::from(exponent) - c_int::from(BIAS)
}

/// [`ilogb_work`] as a `long double`, with the exceptions C asks for.
fn logb_work(x: F80) -> F80 {
    if !is_finite(x) {
        // A NaN times itself is that NaN; an infinity is its own answer.
        return x.mul(x);
    }
    if is_zero(x) {
        // Divide-by-zero, and minus infinity.
        return F80::ONE.neg().div(x.mul(x));
    }
    F80::from_i32(ilogb_work(x))
}

/// The fractional part of `x`, with the integral part through `iptr`.
///
/// # Safety
///
/// `iptr` must be a place a `long double` may be written, as C's
/// `long double *` says.
unsafe fn modf_work(x: F80, iptr: *mut F80) -> F80 {
    let exponent = c_int::from(x.exponent()) - c_int::from(BIAS);
    let negative = x.is_negative();
    // No fractional part: every bit of the significand is an integer bit.
    if exponent >= c_int::from(MANTISSA_BITS) - 1 {
        // SAFETY: the caller passed a place a `long double` may be written.
        unsafe {
            *iptr = x;
        }
        if x.is_nan() {
            return x;
        }
        return signed_zero(x);
    }
    // No integral part.
    if exponent < 0 {
        // SAFETY: as above.
        unsafe {
            *iptr = signed_zero(x);
        }
        return x;
    }
    let magnitude = if negative { x.neg() } else { x };
    // Rounds to an integer in the current mode, and raises a spurious
    // inexact, which musl's `modfl.c` accepts as the price.
    let mut y = magnitude.add(toint()).sub(toint()).sub(magnitude);
    if is_zero(y) {
        // SAFETY: as above.
        unsafe {
            *iptr = x;
        }
        return signed_zero(x);
    }
    if F80::fcomi(y, F80::ZERO).above() {
        y = y.sub(F80::ONE);
    }
    if negative {
        y = y.neg();
    }
    // SAFETY: as above.
    unsafe {
        *iptr = x.add(y);
    }
    y.neg()
}

/// [`modf_work`] as the adapter takes it.
fn modf_at(x: F80, iptr: *mut F80) -> F80 {
    // SAFETY: `iptr` is the `long double *` the caller of `modfl` passed,
    // which C requires to be a place a `long double` may be written.
    unsafe { modf_work(x, iptr) }
}

/// The `long double` next to `x` in `y`'s direction.
///
/// musl's `nextafterl.c`: the step is one on the significand read as an
/// integer, which is what "next representable" means, with the carry between
/// the significand and the exponent written out.
fn nextafter_work(x: F80, y: F80) -> F80 {
    if x.is_nan() || y.is_nan() {
        return x.add(y);
    }
    if F80::fucomi(x, y).equal() {
        return y;
    }
    let mut mantissa = x.mantissa();
    let mut sign_exponent = x.sign_exponent();
    if is_zero(x) {
        // The smallest subnormal, with the direction's sign.
        mantissa = 1;
        sign_exponent = y.sign_exponent() & 0x8000;
    } else if F80::fcomi(x, y).below() == (sign_exponent & 0x8000 == 0) {
        // Away from zero.
        mantissa = mantissa.wrapping_add(1);
        if mantissa << 1 == 0 {
            mantissa = 1 << 63;
            sign_exponent = sign_exponent.wrapping_add(1);
        }
    } else {
        // Toward zero.
        if mantissa << 1 == 0 {
            sign_exponent = sign_exponent.wrapping_sub(1);
            if sign_exponent & 0x7fff != 0 {
                mantissa = 0;
            }
        }
        mantissa = mantissa.wrapping_sub(1);
    }
    let stepped = F80::from_parts(mantissa, sign_exponent);
    // Overflow, where the step reached an infinity from a finite number.
    if stepped.exponent() == SPECIAL {
        return x.add(x);
    }
    // Underflow, where it reached a subnormal or a zero.
    if stepped.exponent() == SUBNORMAL {
        let _ = x.mul(x).add(stepped.mul(stepped));
    }
    stepped
}

/// [`nextafter_work`] for a `double`, stepping toward a `long double`.
fn nexttoward_double(x: f64, y: F80) -> f64 {
    if x.is_nan() || y.is_nan() {
        return y.add_f64(x).to_f64();
    }
    let wide = F80::from_f64(x);
    if F80::fucomi(wide, y).equal() {
        return y.to_f64();
    }
    let mut bits = x.to_bits();
    if x == 0.0 {
        bits = 1;
        if y.is_negative() {
            bits |= 1 << 63;
        }
    } else if F80::fcomi(wide, y).below() {
        bits = if x.is_sign_negative() {
            bits.wrapping_sub(1)
        } else {
            bits.wrapping_add(1)
        };
    } else {
        bits = if x.is_sign_negative() {
            bits.wrapping_add(1)
        } else {
            bits.wrapping_sub(1)
        };
    }
    let stepped = f64::from_bits(bits);
    let exponent = bits >> 52 & 0x7ff;
    if exponent == 0x7ff {
        force_eval(x + x);
    }
    if exponent == 0 {
        force_eval(x * x + stepped * stepped);
    }
    stepped
}

/// [`nextafter_work`] for a `float`, stepping toward a `long double`.
fn nexttoward_float(x: f32, y: F80) -> f32 {
    if x.is_nan() || y.is_nan() {
        return y.add_f32(x).to_f32();
    }
    let wide = F80::from_f32(x);
    if F80::fucomi(wide, y).equal() {
        return y.to_f32();
    }
    let mut bits = x.to_bits();
    if x == 0.0 {
        bits = 1;
        if y.is_negative() {
            bits |= 0x8000_0000;
        }
    } else if F80::fcomi(wide, y).below() {
        bits = if x.is_sign_negative() {
            bits.wrapping_sub(1)
        } else {
            bits.wrapping_add(1)
        };
    } else {
        bits = if x.is_sign_negative() {
            bits.wrapping_add(1)
        } else {
            bits.wrapping_sub(1)
        };
    }
    let stepped = f32::from_bits(bits);
    let exponent = bits & 0x7f80_0000;
    if exponent == 0x7f80_0000 {
        force_evalf(x + x);
    }
    if exponent == 0 {
        force_evalf(x * x + stepped * stepped);
    }
    stepped
}

export! {
    /// `fabsl`: the magnitude of a `long double`.
    fn fabsl(long double) -> long double = fabs_work;
}
export! {
    /// `copysignl`: the first `long double` with the second's sign.
    fn copysignl(long double, long double) -> long double = copysign_work;
}
export! {
    /// `fmaxl`: the larger of two `long double`s.
    fn fmaxl(long double, long double) -> long double = fmax_work;
}
export! {
    /// `fminl`: the smaller of two `long double`s.
    fn fminl(long double, long double) -> long double = fmin_work;
}
export! {
    /// `fdiml`: the positive difference of two `long double`s.
    fn fdiml(long double, long double) -> long double = fdim_work;
}
export! {
    /// `nanl`: a quiet NaN.
    fn nanl(tag: *const c_char) -> long double = nan_work;
}
export! {
    /// `scalbnl`: a `long double` times a power of two.
    fn scalbnl(long double, n: c_int) -> long double = scalbn_work;
}
export! {
    /// `ldexpl`: [`scalbnl`] under its other name.
    fn ldexpl(long double, n: c_int) -> long double = scalbn_work;
}
export! {
    /// `scalblnl`: [`scalbnl`] taking a `long`.
    fn scalblnl(long double, n: c_long) -> long double = scalbln_work;
}
export! {
    /// `frexpl`: the significand and exponent of a `long double`.
    fn frexpl(long double, e: *mut c_int) -> long double = frexp_at;
}
export! {
    /// `logbl`: the exponent of a `long double`, as a `long double`.
    fn logbl(long double) -> long double = logb_work;
}
export! {
    /// `ilogbl`: the exponent of a `long double`, as an `int`.
    fn ilogbl(long double) -> c_int = ilogb_work;
}
export! {
    /// `modfl`: the fractional and integral parts of a `long double`.
    fn modfl(long double, iptr: *mut F80) -> long double = modf_at;
}
export! {
    /// `nextafterl`: the `long double` next to the first, toward the second.
    fn nextafterl(long double, long double) -> long double = nextafter_work;
}
export! {
    /// `nexttowardl`: [`nextafterl`], whose arguments are already both
    /// `long double`s here.
    fn nexttowardl(long double, long double) -> long double = nextafter_work;
}
export! {
    /// `nexttoward`: the `double` next to the first, toward a `long double`.
    fn nexttoward(x: f64, long double) -> f64 = nexttoward_double;
}
export! {
    /// `nexttowardf`: the `float` next to the first, toward a `long double`.
    fn nexttowardf(x: f32, long double) -> f32 = nexttoward_float;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fenv::{FE_DIVBYZERO, FE_INVALID, FE_OVERFLOW, FE_TONEAREST};
    use crate::math::mtest;

    /// A `long double` from a `double`, for the cases a `double` says exactly.
    fn ld(x: f64) -> F80 {
        F80::from_f64(x)
    }

    /// The bits of a `long double`, as significand and sign-and-exponent.
    fn parts(x: F80) -> (u64, u16) {
        (x.mantissa(), x.sign_exponent())
    }

    #[test]
    fn the_sign_functions_keep_the_signs_c_asks_for() {
        assert_eq!(parts(fabs_work(ld(-2.5))), parts(ld(2.5)));
        assert_eq!(parts(copysign_work(ld(2.5), ld(-1.0))), parts(ld(-2.5)));
        assert_eq!(parts(copysign_work(ld(-2.5), ld(1.0))), parts(ld(2.5)));
        // A zero's sign travels too.
        assert_eq!(
            parts(copysign_work(F80::ZERO, ld(-1.0))),
            parts(F80::ZERO.neg())
        );
    }

    #[test]
    fn the_extremes_prefer_a_number_to_a_nan_and_plus_zero_to_minus() {
        assert_eq!(parts(fmax_work(ld(1.0), ld(2.0))), parts(ld(2.0)));
        assert_eq!(parts(fmin_work(ld(1.0), ld(2.0))), parts(ld(1.0)));
        assert_eq!(parts(fmax_work(F80::NAN, ld(2.0))), parts(ld(2.0)));
        assert_eq!(parts(fmin_work(ld(2.0), F80::NAN)), parts(ld(2.0)));
        assert_eq!(
            parts(fmax_work(F80::ZERO.neg(), F80::ZERO)),
            parts(F80::ZERO)
        );
        assert_eq!(
            parts(fmin_work(F80::ZERO, F80::ZERO.neg())),
            parts(F80::ZERO.neg())
        );
        assert_eq!(fdim_work(ld(5.0), ld(2.0)).to_f64(), 3.0);
        assert_eq!(fdim_work(ld(2.0), ld(5.0)).to_f64(), 0.0);
        assert!(fdim_work(F80::NAN, ld(1.0)).is_nan());
    }

    #[test]
    fn scaling_by_a_power_of_two_is_exact_until_it_overflows() {
        assert_eq!(scalbn_work(ld(3.0), 4).to_f64(), 48.0);
        assert_eq!(scalbn_work(ld(3.0), -1).to_f64(), 1.5);
        assert_eq!(scalbn_work(F80::ZERO, 100).to_f64(), 0.0);
        assert_eq!(scalbln_work(ld(3.0), 4).to_f64(), 48.0);
        // Past the top the answer is an infinity, with overflow raised; the
        // two-step scaling is what keeps it from rounding twice on the way.
        let (value, raised) = mtest::under(FE_TONEAREST, || scalbn_work(ld(1.0), 40000));
        assert_eq!(value.exponent(), SPECIAL);
        assert_eq!(value.mantissa(), 1 << 63, "an infinity, not a NaN");
        assert!(raised & FE_OVERFLOW != 0);
        // And past the bottom, a zero.
        let (value, _) = mtest::under(FE_TONEAREST, || scalbn_work(ld(1.0), -40000));
        assert!(is_zero(value));
    }

    #[test]
    fn frexp_splits_into_a_half_open_significand_and_an_exponent() {
        let mut e: c_int = 0xdead;
        // 3 = 0.75 × 2^2.
        let significand = frexp_at(ld(3.0), &raw mut e);
        assert_eq!(significand.to_f64(), 0.75);
        assert_eq!(e, 2);
        // A zero reports an exponent of zero and keeps its sign.
        let zero = frexp_at(F80::ZERO.neg(), &raw mut e);
        assert_eq!(parts(zero), parts(F80::ZERO.neg()));
        assert_eq!(e, 0);
        // The smallest subnormal, which is scaled into range and back.
        let tiny = F80::from_parts(1, 0);
        let significand = frexp_at(tiny, &raw mut e);
        assert_eq!(significand.to_f64(), 0.5);
        assert_eq!(e, -16444);
        // A NaN comes back as it was.
        assert!(frexp_at(F80::NAN, &raw mut e).is_nan());
    }

    #[test]
    fn the_exponent_functions_agree_and_name_the_special_cases() {
        assert_eq!(ilogb_work(ld(3.0)), 1);
        assert_eq!(ilogb_work(ld(1.0)), 0);
        assert_eq!(ilogb_work(ld(0.5)), -1);
        assert_eq!(ilogb_work(F80::from_parts(1, 0)), -16445);
        let (value, raised) = mtest::under(FE_TONEAREST, || ilogb_work(F80::ZERO));
        assert_eq!(value, FP_ILOGB0);
        assert_eq!(raised, FE_INVALID);
        let (value, raised) = mtest::under(FE_TONEAREST, || ilogb_work(F80::NAN));
        assert_eq!(value, FP_ILOGBNAN);
        assert_eq!(raised, FE_INVALID);
        assert_eq!(logb_work(ld(3.0)).to_f64(), 1.0);
        let (value, raised) = mtest::under(FE_TONEAREST, || logb_work(F80::ZERO));
        assert_eq!(value.exponent(), SPECIAL);
        assert!(value.is_negative(), "minus infinity");
        assert_eq!(raised, FE_DIVBYZERO);
    }

    #[test]
    fn modf_splits_off_the_fraction_and_keeps_the_sign() {
        let mut integral = F80::NAN;
        assert_eq!(modf_at(ld(2.5), &raw mut integral).to_f64(), 0.5);
        assert_eq!(integral.to_f64(), 2.0);
        assert_eq!(modf_at(ld(-2.5), &raw mut integral).to_f64(), -0.5);
        assert_eq!(integral.to_f64(), -2.0);
        // No fractional part: a zero of the argument's sign.
        let fraction = modf_at(ld(-4.0), &raw mut integral);
        assert_eq!(parts(fraction), parts(F80::ZERO.neg()));
        assert_eq!(integral.to_f64(), -4.0);
        // No integral part.
        assert_eq!(modf_at(ld(0.25), &raw mut integral).to_f64(), 0.25);
        assert_eq!(parts(integral), parts(F80::ZERO));
    }

    #[test]
    fn nextafter_steps_one_representable_value() {
        let one = F80::ONE;
        let up = nextafter_work(one, ld(2.0));
        assert_eq!(parts(up), ((1 << 63) + 1, BIAS));
        let down = nextafter_work(one, F80::ZERO);
        assert_eq!(parts(down), (u64::MAX, BIAS - 1));
        // A step back is the value it came from.
        assert_eq!(parts(nextafter_work(up, F80::ZERO)), parts(one));
        // Equal arguments give the second, so the sign of a zero is the
        // destination's.
        assert_eq!(
            parts(nextafter_work(F80::ZERO, F80::ZERO.neg())),
            parts(F80::ZERO.neg())
        );
        // Away from zero, the first step is the smallest subnormal.
        assert_eq!(parts(nextafter_work(F80::ZERO, one)), (1, 0));
        assert!(nextafter_work(F80::NAN, one).is_nan());
        // The narrower forms step in their own type.
        assert_eq!(
            nexttoward_double(1.0, ld(2.0)),
            f64::from_bits(1.0_f64.to_bits() + 1)
        );
        assert_eq!(
            nexttoward_float(1.0, ld(2.0)),
            f32::from_bits(1.0_f32.to_bits() + 1)
        );
        assert_eq!(nexttoward_double(1.0, ld(1.0)), 1.0);
    }
}
