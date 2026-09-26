//! Taking a floating-point number apart and putting one together: `frexp`,
//! `ldexp`, `scalbn`, `scalbln`, `scalb`, `significand`, `modf`, `logb`,
//! `ilogb`, `copysign`, `nan`, `nextafter`, `fdim`, `fmax`, `fmin` and `fabs`,
//! for `double` and `float`.
//!
//! Every one is exact, and raises only the exceptions C names for it:
//! `scalbn` overflows and underflows, `nextafter` does when it steps to an
//! infinity or below the normal range, and `ilogb` and `logb` raise invalid or
//! divide-by-zero for zeros, infinities and NaNs.
//!
//! Ported from musl 1.2.5's `frexp.c`, `ldexp.c`, `scalbn.c`, `scalbln.c`,
//! `scalb.c`, `significand.c`, `modf.c`, `logb.c`, `ilogb.c`, `copysign.c`,
//! `nan.c`, `nextafter.c`, `fdim.c`, `fmax.c`, `fmin.c`, `x86_64/fabs.c` and
//! their `float` versions (MIT; see [`crate::math`] for the notice). `scalb.c`
//! and `scalbf.c` came to musl from FreeBSD's msun, and carry this notice:
//!
//! ```text
//! Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//!
//! Developed at SunSoft, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ```
//!
//! `scalbf.c` adds: "Conversion to float by Ian Lance Taylor, Cygnus Support,
//! ian@cygnus.com."

use core::ffi::{c_char, c_int, c_long};

use crate::math::rounding::{rint, rintf};
use crate::math::support::{
    barrierf, force_eval, force_evalf, hexf32, hexf64, invalid, invalidf, is_finite, is_finitef,
    is_nan, is_nanf,
};

/// What `ilogb` returns for a NaN: `INT_MIN`.
pub const FP_ILOGBNAN: c_int = c_int::MIN;
/// What `ilogb` returns for a zero, which musl makes the same as for a NaN.
pub const FP_ILOGB0: c_int = FP_ILOGBNAN;

/// The sign bit of a `double`.
const SIGN: u64 = 1 << 63;
/// The sign bit of a `float`.
const SIGNF: u32 = 1 << 31;

/// `frexp`'s two results: `x` scaled into [0.5, 1), and the power of two.
/// A zero, an infinity or a NaN is returned unchanged, with a power of zero.
fn frexp_parts(x: f64) -> (f64, c_int) {
    let mut bits = x.to_bits();
    let mut adjust = 0;
    if bits >> 52 & 0x7ff == 0 {
        if x == 0.0 {
            return (x, 0);
        }
        // Normalise a subnormal. The product is exact.
        bits = (x * hexf64!("0x1p64")).to_bits();
        adjust = 64;
    }
    let e = (bits >> 52 & 0x7ff) as c_int;
    if e == 0x7ff {
        return (x, 0);
    }
    let fraction = f64::from_bits(bits & 0x800f_ffff_ffff_ffff | 0x3fe0_0000_0000_0000);
    (fraction, e - 0x3fe - adjust)
}

/// [`frexp_parts`] for `float`.
fn frexpf_parts(x: f32) -> (f32, c_int) {
    let mut bits = x.to_bits();
    let mut adjust = 0;
    if bits >> 23 & 0xff == 0 {
        if x == 0.0 {
            return (x, 0);
        }
        bits = (x * hexf32!("0x1p64")).to_bits();
        adjust = 64;
    }
    let e = (bits >> 23 & 0xff) as c_int;
    if e == 0xff {
        return (x, 0);
    }
    let fraction = f32::from_bits(bits & 0x807f_ffff | 0x3f00_0000);
    (fraction, e - 0x7e - adjust)
}

/// `x` as a fraction in [0.5, 1) with the sign of `x`, times two to the power
/// stored in `*e`. A zero, an infinity or a NaN is returned unchanged, and
/// `*e` is set to zero.
///
/// # Safety
///
/// `e` must be valid for writing an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn frexp(x: f64, e: *mut c_int) -> f64 {
    let (fraction, power) = frexp_parts(x);
    // SAFETY: the caller vouches for `e`.
    unsafe { e.write(power) };
    fraction
}

/// [`frexp`] for `float`.
///
/// # Safety
///
/// `e` must be valid for writing an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn frexpf(x: f32, e: *mut c_int) -> f32 {
    let (fraction, power) = frexpf_parts(x);
    // SAFETY: the caller vouches for `e`.
    unsafe { e.write(power) };
    fraction
}

/// `x × 2^n`, correctly rounded, overflowing or underflowing as it should.
///
/// The scaling is done in at most three multiplications, arranged so that
/// only the last can round: rounding twice in the subnormal range would give
/// a wrong result.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn scalbn(x: f64, n: c_int) -> f64 {
    let mut y = x;
    let mut n = n;
    if n > 1023 {
        y *= hexf64!("0x1p1023");
        n -= 1023;
        if n > 1023 {
            y *= hexf64!("0x1p1023");
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        // Make sure the final n < -53, to avoid rounding twice in the
        // subnormal range.
        y *= hexf64!("0x1p-969");
        n += 1022 - 53;
        if n < -1022 {
            y *= hexf64!("0x1p-969");
            n += 1022 - 53;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    // n is now between -1022 and 1023, so the biased exponent is positive.
    y * f64::from_bits(((0x3ff + n) as u64) << 52)
}

/// [`scalbn`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn scalbnf(x: f32, n: c_int) -> f32 {
    let mut y = x;
    let mut n = n;
    if n > 127 {
        y *= hexf32!("0x1p127");
        n -= 127;
        if n > 127 {
            y *= hexf32!("0x1p127");
            n -= 127;
            if n > 127 {
                n = 127;
            }
        }
    } else if n < -126 {
        y *= hexf32!("0x1p-102");
        n += 126 - 24;
        if n < -126 {
            y *= hexf32!("0x1p-102");
            n += 126 - 24;
            if n < -126 {
                n = -126;
            }
        }
    }
    y * f32::from_bits(((0x7f + n) as u32) << 23)
}

/// [`scalbn`]: `x × 2^n`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ldexp(x: f64, n: c_int) -> f64 {
    scalbn(x, n)
}

/// [`scalbnf`]: `x × 2^n`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ldexpf(x: f32, n: c_int) -> f32 {
    scalbnf(x, n)
}

/// `n` clamped to the range of an `int`. Beyond it every result has already
/// overflowed or underflowed.
fn clamp_to_int(n: c_long) -> c_int {
    // The clamp makes the conversion exact.
    n.clamp(c_int::MIN.into(), c_int::MAX.into()) as c_int
}

/// [`scalbn`] with a `long` power.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn scalbln(x: f64, n: c_long) -> f64 {
    scalbn(x, clamp_to_int(n))
}

/// [`scalbnf`] with a `long` power.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn scalblnf(x: f32, n: c_long) -> f32 {
    scalbnf(x, clamp_to_int(n))
}

/// The obsolete `scalb`: `x × 2^n` for a `double` power, which must be an
/// integer or an infinity. Any other `n` is invalid.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn scalb(x: f64, n: f64) -> f64 {
    if is_nan(x) || is_nan(n) {
        return x * n;
    }
    if !is_finite(n) {
        return if n > 0.0 { x * n } else { x / -n };
    }
    if rint(n) != n {
        return invalid(n);
    }
    if n > 65000.0 {
        return scalbn(x, 65000);
    }
    if -n > 65000.0 {
        return scalbn(x, -65000);
    }
    // n is an integer within ±65000.
    scalbn(x, n as c_int)
}

/// [`scalb`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn scalbf(x: f32, n: f32) -> f32 {
    if is_nanf(x) || is_nanf(n) {
        return x * n;
    }
    if !is_finitef(n) {
        return if n > 0.0 { x * n } else { x / -n };
    }
    if rintf(n) != n {
        return invalidf(n);
    }
    if n > 65000.0 {
        return scalbnf(x, 65000);
    }
    if -n > 65000.0 {
        return scalbnf(x, -65000);
    }
    // n is an integer within ±65000.
    scalbnf(x, n as c_int)
}

/// The exponent of `x` as an integer: `floor(log2(|x|))` for a finite nonzero
/// `x`. A zero or a NaN gives `FP_ILOGB0` or `FP_ILOGBNAN`, and an infinity
/// `INT_MAX`; all three raise invalid.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ilogb(x: f64) -> c_int {
    let bits = x.to_bits();
    let e = (bits >> 52 & 0x7ff) as c_int;
    if e == 0 {
        let fraction = bits << 12;
        if fraction == 0 {
            force_evalf(barrierf(0.0) / 0.0);
            return FP_ILOGB0;
        }
        // A subnormal: count down past the fraction's leading zeros.
        return -0x3ff - fraction.leading_zeros() as c_int;
    }
    if e == 0x7ff {
        force_evalf(barrierf(0.0) / 0.0);
        return if bits << 12 != 0 {
            FP_ILOGBNAN
        } else {
            c_int::MAX
        };
    }
    e - 0x3ff
}

/// [`ilogb`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ilogbf(x: f32) -> c_int {
    let bits = x.to_bits();
    let e = (bits >> 23 & 0xff) as c_int;
    if e == 0 {
        let fraction = bits << 9;
        if fraction == 0 {
            force_evalf(barrierf(0.0) / 0.0);
            return FP_ILOGB0;
        }
        return -0x7f - fraction.leading_zeros() as c_int;
    }
    if e == 0xff {
        force_evalf(barrierf(0.0) / 0.0);
        return if bits << 9 != 0 {
            FP_ILOGBNAN
        } else {
            c_int::MAX
        };
    }
    e - 0x7f
}

/// The exponent of `x` as a `double`: `-inf` with divide-by-zero for a zero,
/// `+inf` for an infinity, and a NaN for a NaN.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn logb(x: f64) -> f64 {
    if !is_finite(x) {
        return x * x;
    }
    if x == 0.0 {
        return -1.0 / (x * x);
    }
    f64::from(ilogb(x))
}

/// [`logb`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn logbf(x: f32) -> f32 {
    if !is_finitef(x) {
        return x * x;
    }
    if x == 0.0 {
        return -1.0 / (x * x);
    }
    // Every exponent a float has is exact in a float.
    ilogbf(x) as f32
}

/// The obsolete `significand`: `x` scaled into [1, 2).
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn significand(x: f64) -> f64 {
    // For a zero or a NaN `ilogb` gives `INT_MIN`, which C negates to itself.
    scalbn(x, ilogb(x).wrapping_neg())
}

/// [`significand`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn significandf(x: f32) -> f32 {
    scalbnf(x, ilogbf(x).wrapping_neg())
}

/// `modf`'s two results: the fraction and the integer part, each with the
/// sign of `x`.
fn modf_parts(x: f64) -> (f64, f64) {
    let bits = x.to_bits();
    let sign = f64::from_bits(bits & SIGN);
    let e = (bits >> 52 & 0x7ff) as i32 - 0x3ff;
    // No fractional part.
    if e >= 52 {
        if e == 0x400 && bits << 12 != 0 {
            return (x, x);
        }
        return (sign, x);
    }
    // No integral part.
    if e < 0 {
        return (x, sign);
    }
    let mask = u64::MAX >> 12 >> e;
    if bits & mask == 0 {
        return (sign, x);
    }
    let integer = f64::from_bits(bits & !mask);
    (x - integer, integer)
}

/// [`modf_parts`] for `float`.
fn modff_parts(x: f32) -> (f32, f32) {
    let bits = x.to_bits();
    let sign = f32::from_bits(bits & SIGNF);
    let e = (bits >> 23 & 0xff) as i32 - 0x7f;
    if e >= 23 {
        if e == 0x80 && bits << 9 != 0 {
            return (x, x);
        }
        return (sign, x);
    }
    if e < 0 {
        return (x, sign);
    }
    let mask = 0x007f_ffff >> e;
    if bits & mask == 0 {
        return (sign, x);
    }
    let integer = f32::from_bits(bits & !mask);
    (x - integer, integer)
}

/// The fractional part of `x`, storing its integer part in `*iptr`. Both
/// have the sign of `x`.
///
/// # Safety
///
/// `iptr` must be valid for writing a `double`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn modf(x: f64, iptr: *mut f64) -> f64 {
    let (fraction, integer) = modf_parts(x);
    // SAFETY: the caller vouches for `iptr`.
    unsafe { iptr.write(integer) };
    fraction
}

/// [`modf`] for `float`.
///
/// # Safety
///
/// `iptr` must be valid for writing a `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn modff(x: f32, iptr: *mut f32) -> f32 {
    let (fraction, integer) = modff_parts(x);
    // SAFETY: the caller vouches for `iptr`.
    unsafe { iptr.write(integer) };
    fraction
}

/// `x`'s magnitude with `y`'s sign.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn copysign(x: f64, y: f64) -> f64 {
    f64::from_bits(x.to_bits() & !SIGN | y.to_bits() & SIGN)
}

/// [`copysign`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn copysignf(x: f32, y: f32) -> f32 {
    f32::from_bits(x.to_bits() & !SIGNF | y.to_bits() & SIGNF)
}

/// The magnitude of `x`, for any `x`, a NaN included.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fabs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !SIGN)
}

/// [`fabs`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fabsf(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & !SIGNF)
}

/// A quiet NaN. As in musl, the tag, which C lets an implementation use to
/// choose among NaNs, is ignored and never read.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn nan(_tag: *const c_char) -> f64 {
    f64::NAN
}

/// [`nan`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn nanf(_tag: *const c_char) -> f32 {
    f32::NAN
}

/// The next `double` after `x` in the direction of `y`, or `y` if they are
/// equal. Stepping from a finite `x` to an infinity overflows, and stepping to
/// a subnormal or a zero underflows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn nextafter(x: f64, y: f64) -> f64 {
    if is_nan(x) || is_nan(y) {
        return x + y;
    }
    let ux = x.to_bits();
    let uy = y.to_bits();
    if ux == uy {
        return y;
    }
    let ax = ux & !SIGN;
    let ay = uy & !SIGN;
    let next = if ax == 0 {
        if ay == 0 {
            return y;
        }
        uy & SIGN | 1
    } else if ax > ay || (ux ^ uy) & SIGN != 0 {
        ux - 1
    } else {
        ux + 1
    };
    let result = f64::from_bits(next);
    let e = next >> 52 & 0x7ff;
    // Raise overflow if the result is infinite and x is finite.
    if e == 0x7ff {
        force_eval(x + x);
    }
    // Raise underflow if the result is subnormal or zero.
    if e == 0 {
        force_eval(x * x + result * result);
    }
    result
}

/// [`nextafter`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn nextafterf(x: f32, y: f32) -> f32 {
    if is_nanf(x) || is_nanf(y) {
        return x + y;
    }
    let ux = x.to_bits();
    let uy = y.to_bits();
    if ux == uy {
        return y;
    }
    let ax = ux & !SIGNF;
    let ay = uy & !SIGNF;
    let next = if ax == 0 {
        if ay == 0 {
            return y;
        }
        uy & SIGNF | 1
    } else if ax > ay || (ux ^ uy) & SIGNF != 0 {
        ux - 1
    } else {
        ux + 1
    };
    let result = f32::from_bits(next);
    let e = next & 0x7f80_0000;
    if e == 0x7f80_0000 {
        force_evalf(x + x);
    }
    if e == 0 {
        force_evalf(x * x + result * result);
    }
    result
}

/// `x - y` if that is positive, and +0 otherwise. A NaN argument is returned.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fdim(x: f64, y: f64) -> f64 {
    if is_nan(x) {
        return x;
    }
    if is_nan(y) {
        return y;
    }
    if x > y { x - y } else { 0.0 }
}

/// [`fdim`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fdimf(x: f32, y: f32) -> f32 {
    if is_nanf(x) {
        return x;
    }
    if is_nanf(y) {
        return y;
    }
    if x > y { x - y } else { 0.0 }
}

/// The larger of `x` and `y`, ignoring a NaN, and taking +0 as larger than -0
/// as C's Annex F asks.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fmax(x: f64, y: f64) -> f64 {
    if is_nan(x) {
        return y;
    }
    if is_nan(y) {
        return x;
    }
    let x_negative = x.to_bits() & SIGN != 0;
    if x_negative != (y.to_bits() & SIGN != 0) {
        return if x_negative { y } else { x };
    }
    if x < y { y } else { x }
}

/// [`fmax`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fmaxf(x: f32, y: f32) -> f32 {
    if is_nanf(x) {
        return y;
    }
    if is_nanf(y) {
        return x;
    }
    let x_negative = x.to_bits() & SIGNF != 0;
    if x_negative != (y.to_bits() & SIGNF != 0) {
        return if x_negative { y } else { x };
    }
    if x < y { y } else { x }
}

/// The smaller of `x` and `y`, ignoring a NaN, and taking -0 as smaller than
/// +0.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fmin(x: f64, y: f64) -> f64 {
    if is_nan(x) {
        return y;
    }
    if is_nan(y) {
        return x;
    }
    let x_negative = x.to_bits() & SIGN != 0;
    if x_negative != (y.to_bits() & SIGN != 0) {
        return if x_negative { x } else { y };
    }
    if x < y { x } else { y }
}

/// [`fmin`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fminf(x: f32, y: f32) -> f32 {
    if is_nanf(x) {
        return y;
    }
    if is_nanf(y) {
        return x;
    }
    let x_negative = x.to_bits() & SIGNF != 0;
    if x_negative != (y.to_bits() & SIGNF != 0) {
        return if x_negative { x } else { y };
    }
    if x < y { x } else { y }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Row, Rules, Verdict};

    /// A table's integer as an `int`.
    fn int(n: i64) -> c_int {
        c_int::try_from(n).expect("the table's power fits an int")
    }

    /// Adds to `verdict` the check of a second result, which must be exact.
    fn second<F: mtest::Float>(
        verdict: Verdict,
        row: &Row,
        what: &str,
        got: F,
        want: F,
    ) -> Verdict {
        if mtest::checkcr(got, want) {
            return verdict;
        }
        let message = format!(
            "{}: {what} want {} got {}",
            row.place,
            mtest::hex(want),
            mtest::hex(got)
        );
        verdict.and(Verdict::Fail(message))
    }

    #[test]
    fn frexp_matches_libc_test() {
        let files = ["sanity/frexp.h", "special/frexp.h"];
        mtest::run("frexp", &files, &[], |row| {
            let (x, want, dy, power) = (row.f64(0), row.f64(1), row.f32(2), row.int(3));
            let mut got_power = 0;
            // SAFETY: `got_power` is a local `int`.
            let (got, raised) = mtest::under(row.mode, || unsafe { frexp(x, &raw mut got_power) });
            let verdict = mtest::judge(&Rules::EXACT, row, raised, got, want, dy, || {
                format!("frexp({})", mtest::hex(x))
            });
            if x.is_finite() && i64::from(got_power) != power {
                let message = format!("{}: power want {power} got {got_power}", row.place);
                return verdict.and(Verdict::Fail(message));
            }
            verdict
        });
        let files = ["sanity/frexpf.h", "special/frexpf.h"];
        mtest::run("frexpf", &files, &[], |row| {
            let (x, want, dy, power) = (row.f32(0), row.f32(1), row.f32(2), row.int(3));
            let mut got_power = 0;
            // SAFETY: `got_power` is a local `int`.
            let (got, raised) = mtest::under(row.mode, || unsafe { frexpf(x, &raw mut got_power) });
            let verdict = mtest::judge(&Rules::EXACT, row, raised, got, want, dy, || {
                format!("frexpf({})", mtest::hex(x))
            });
            if x.is_finite() && i64::from(got_power) != power {
                let message = format!("{}: power want {power} got {got_power}", row.place);
                return verdict.and(Verdict::Fail(message));
            }
            verdict
        });
    }

    #[test]
    fn modf_matches_libc_test() {
        // libc-test's `modf` programs ignore inexact.
        let rules = Rules::EXACT.ignore_inexact();
        let files = ["sanity/modf.h", "special/modf.h"];
        mtest::run("modf", &files, &[], |row| {
            let (x, want, dy, want_integer) = (row.f64(0), row.f64(1), row.f32(2), row.f64(3));
            let mut integer = 0.0;
            // SAFETY: `integer` is a local `double`.
            let (got, raised) = mtest::under(row.mode, || unsafe { modf(x, &raw mut integer) });
            let verdict = mtest::judge(&rules, row, raised, got, want, dy, || {
                format!("modf({})", mtest::hex(x))
            });
            second(verdict, row, "integer part", integer, want_integer)
        });
        let files = ["sanity/modff.h", "special/modff.h"];
        mtest::run("modff", &files, &[], |row| {
            let (x, want, dy, want_integer) = (row.f32(0), row.f32(1), row.f32(2), row.f32(3));
            let mut integer = 0.0;
            // SAFETY: `integer` is a local `float`.
            let (got, raised) = mtest::under(row.mode, || unsafe { modff(x, &raw mut integer) });
            let verdict = mtest::judge(&rules, row, raised, got, want, dy, || {
                format!("modff({})", mtest::hex(x))
            });
            second(verdict, row, "integer part", integer, want_integer)
        });
    }

    #[test]
    fn scaling_matches_libc_test() {
        let exact = Rules::EXACT;
        let files = ["sanity/ldexp.h", "special/ldexp.h"];
        mtest::di_d("ldexp", &files, |x, n| ldexp(x, int(n)), exact, &[]);
        let files = ["sanity/ldexpf.h", "special/ldexpf.h"];
        mtest::di_d("ldexpf", &files, |x, n| ldexpf(x, int(n)), exact, &[]);
        let files = ["sanity/scalbn.h", "special/scalbn.h"];
        mtest::di_d("scalbn", &files, |x, n| scalbn(x, int(n)), exact, &[]);
        let files = ["sanity/scalbnf.h", "special/scalbnf.h"];
        mtest::di_d("scalbnf", &files, |x, n| scalbnf(x, int(n)), exact, &[]);
        let files = ["sanity/scalbln.h", "special/scalbln.h"];
        // A 32-bit `long` holds every exponent that matters; past it the
        // result is the same overflow or underflow.
        let long =
            |n: i64| c_long::try_from(n).unwrap_or(if n < 0 { c_long::MIN } else { c_long::MAX });
        mtest::di_d("scalbln", &files, |x, n| scalbln(x, long(n)), exact, &[]);
        let files = ["sanity/scalblnf.h", "special/scalblnf.h"];
        mtest::di_d("scalblnf", &files, |x, n| scalblnf(x, long(n)), exact, &[]);
        let files = ["sanity/scalb.h", "special/scalb.h"];
        mtest::dd_d("scalb", &files, |x, y| scalb(x, y), exact, &[]);
        let files = ["sanity/scalbf.h", "special/scalbf.h"];
        mtest::dd_d("scalbf", &files, |x, y| scalbf(x, y), exact, &[]);
    }

    #[test]
    fn exponents_match_libc_test() {
        let files = ["sanity/logb.h", "special/logb.h"];
        mtest::d_d("logb", &files, |x| logb(x), Rules::EXACT, &[]);
        let files = ["sanity/logbf.h", "special/logbf.h"];
        mtest::d_d("logbf", &files, |x| logbf(x), Rules::EXACT, &[]);
        let files = ["sanity/ilogb.h", "special/ilogb.h"];
        mtest::d_i(
            "ilogb",
            &files,
            |x| i64::from(ilogb(x)),
            Rules::INTEGER,
            &[],
        );
        let files = ["sanity/ilogbf.h", "special/ilogbf.h"];
        mtest::d_i(
            "ilogbf",
            &files,
            |x| i64::from(ilogbf(x)),
            Rules::INTEGER,
            &[],
        );
    }

    #[test]
    fn signs_and_neighbours_match_libc_test() {
        let exact = Rules::EXACT;
        let files = ["sanity/copysign.h", "special/copysign.h"];
        mtest::dd_d("copysign", &files, |x, y| copysign(x, y), exact, &[]);
        let files = ["sanity/copysignf.h", "special/copysignf.h"];
        mtest::dd_d("copysignf", &files, |x, y| copysignf(x, y), exact, &[]);
        let files = ["ucb/fabs.h", "sanity/fabs.h", "special/fabs.h"];
        mtest::d_d("fabs", &files, |x| fabs(x), exact, &[]);
        let files = ["ucb/fabsf.h", "sanity/fabsf.h", "special/fabsf.h"];
        mtest::d_d("fabsf", &files, |x| fabsf(x), exact, &[]);
        let files = ["sanity/nextafter.h", "special/nextafter.h"];
        mtest::dd_d("nextafter", &files, |x, y| nextafter(x, y), exact, &[]);
        let files = ["sanity/nextafterf.h", "special/nextafterf.h"];
        mtest::dd_d("nextafterf", &files, |x, y| nextafterf(x, y), exact, &[]);
    }

    #[test]
    fn differences_and_extremes_match_libc_test() {
        let exact = Rules::EXACT;
        mtest::dd_d(
            "fdim",
            &["sanity/fdim.h", "special/fdim.h"],
            |x, y| fdim(x, y),
            exact,
            &[],
        );
        mtest::dd_d(
            "fdimf",
            &["sanity/fdimf.h", "special/fdimf.h"],
            |x, y| fdimf(x, y),
            exact,
            &[],
        );
        mtest::dd_d(
            "fmax",
            &["sanity/fmax.h", "special/fmax.h"],
            |x, y| fmax(x, y),
            exact,
            &[],
        );
        mtest::dd_d(
            "fmaxf",
            &["sanity/fmaxf.h", "special/fmaxf.h"],
            |x, y| fmaxf(x, y),
            exact,
            &[],
        );
        mtest::dd_d(
            "fmin",
            &["sanity/fmin.h", "special/fmin.h"],
            |x, y| fmin(x, y),
            exact,
            &[],
        );
        mtest::dd_d(
            "fminf",
            &["sanity/fminf.h", "special/fminf.h"],
            |x, y| fminf(x, y),
            exact,
            &[],
        );
    }

    #[test]
    fn significand_and_nan() {
        assert_eq!(significand(12.0), 1.5);
        assert_eq!(significandf(-0.375), -1.5);
        assert_eq!(significand(f64::from_bits(1)), 1.0);
        assert_eq!(significand(0.0).to_bits(), 0);
        assert!(significand(f64::NAN).is_nan());
        assert_eq!(significand(f64::INFINITY), f64::INFINITY);
        assert_eq!(nan(c"".as_ptr()).to_bits(), 0x7ff8_0000_0000_0000);
        assert_eq!(nanf(c"0x12".as_ptr()).to_bits(), 0x7fc0_0000);
    }
}
