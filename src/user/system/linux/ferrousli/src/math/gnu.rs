//! GNU's additions to `math.h` that Chrome and systemd call: `sincos`,
//! `sincosf`, `exp10` and `exp10f`.
//!
//! `exp10` and `exp10f` are ported from musl 1.2.5's `exp10.c` and
//! `exp10f.c` (MIT; see [`crate::math`] for the notice): an exact power of
//! ten from a table for the integer part, times `exp2` of the fraction
//! times log2(10).
//!
//! musl's `sincos` and `sincosf` reduce the argument once and run the same
//! kernels `sin`, `cos`, `sinf` and `cosf` do on it, so their results are
//! those functions' results, and the exceptions their union. Here they are
//! the two calls: the same bits, the same flags, and the reduction made
//! twice.

use core::ffi::c_int;

use crate::math::exp2::exp2;
use crate::math::expf::exp2f;
use crate::math::manipulate::{modf, modff};
use crate::math::pow::pow;
use crate::math::trig::{cos, sin};
use crate::math::trigf::{cosf, sinf};

/// log2(10): musl writes `3.32192809488736234787031942948939`, which rounds
/// to the same `double` as core's constant.
const LOG2_10: f64 = core::f64::consts::LOG2_10;
/// The same digits rounded straight to `float`, as musl's `f` literal and
/// core's `f32` constant are.
const LOG2_10F: f32 = core::f32::consts::LOG2_10;

/// `sin(x)` into `*s` and `cos(x)` into `*c`.
///
/// # Safety
///
/// `s` and `c` must be valid for writes of a `double`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sincos(x: f64, s: *mut f64, c: *mut f64) {
    // SAFETY: the caller vouches for `s`.
    unsafe { s.write(sin(x)) };
    // SAFETY: the caller vouches for `c`.
    unsafe { c.write(cos(x)) };
}

/// [`sincos`] for `float`.
///
/// # Safety
///
/// `s` and `c` must be valid for writes of a `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn sincosf(x: f32, s: *mut f32, c: *mut f32) {
    // SAFETY: the caller vouches for `s`.
    unsafe { s.write(sinf(x)) };
    // SAFETY: the caller vouches for `c`.
    unsafe { c.write(cosf(x)) };
}

/// 10 to the power `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn exp10(x: f64) -> f64 {
    const P10: [f64; 31] = [
        1e-15, 1e-14, 1e-13, 1e-12, 1e-11, 1e-10, 1e-9, 1e-8, 1e-7, 1e-6, 1e-5, 1e-4, 1e-3, 1e-2,
        1e-1, 1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
    ];
    let mut n = 0.0;
    // SAFETY: `n` is a live local.
    let y = unsafe { modf(x, &raw mut n) };
    // |n| < 16, without raising invalid for a NaN.
    if (n.to_bits() >> 52 & 0x7ff) < 0x3ff + 4 {
        let power = usize::try_from(n as c_int + 15)
            .ok()
            .and_then(|i| P10.get(i).copied());
        if let Some(power) = power {
            if y == 0.0 {
                return power;
            }
            return exp2(LOG2_10 * y) * power;
        }
    }
    pow(10.0, x)
}

/// [`exp10`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn exp10f(x: f32) -> f32 {
    const P10: [f32; 15] = [
        1e-7, 1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1, 1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7,
    ];
    let mut n = 0.0;
    // SAFETY: `n` is a live local.
    let y = unsafe { modff(x, &raw mut n) };
    // |n| < 8, without raising invalid for a NaN.
    if (n.to_bits() >> 23 & 0xff) < 0x7f + 3 {
        let power = usize::try_from(n as c_int + 7)
            .ok()
            .and_then(|i| P10.get(i).copied());
        if let Some(power) = power {
            if y == 0.0 {
                return power;
            }
            return exp2f(LOG2_10F * y) * power;
        }
    }
    exp2(LOG2_10 * f64::from(x)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn exp10_matches_libc_test() {
        let files = ["sanity/exp10.h", "special/exp10.h"];
        mtest::d_d("exp10", &files, |x| exp10(x), Rules::ULP.directed(), &[]);
        let files = ["sanity/exp10f.h", "special/exp10f.h"];
        mtest::d_d("exp10f", &files, |x| exp10f(x), Rules::ULP.directed(), &[]);
    }

    #[test]
    fn sincos_matches_libc_test_and_is_sin_and_cos() {
        let files = ["sanity/sincos.h", "special/sincos.h"];
        mtest::d_d(
            "sincos",
            &files,
            |x: f64| {
                let (mut s, mut c) = (0.0, 0.0);
                // SAFETY: two live locals.
                unsafe { sincos(x, &raw mut s, &raw mut c) };
                assert_eq!(c.to_bits(), cos(x).to_bits(), "cos({x})");
                s
            },
            Rules::ULP.directed(),
            &[],
        );
        let files = ["sanity/sincosf.h", "special/sincosf.h"];
        mtest::d_d(
            "sincosf",
            &files,
            |x: f32| {
                let (mut s, mut c) = (0.0, 0.0);
                // SAFETY: two live locals.
                unsafe { sincosf(x, &raw mut s, &raw mut c) };
                assert_eq!(c.to_bits(), cosf(x).to_bits(), "cosf({x})");
                s
            },
            Rules::ULP.directed(),
            &[],
        );
    }

    #[test]
    fn exp10_of_an_integer_is_the_exact_power() {
        assert_eq!(exp10(3.0), 1000.0);
        assert_eq!(exp10(-2.0), 0.01);
        assert_eq!(exp10f(5.0), 100_000.0);
        assert_eq!(exp10(400.0), f64::INFINITY);
    }
}
