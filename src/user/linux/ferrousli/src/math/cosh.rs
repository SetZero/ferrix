//! `cosh` and `coshf`: the hyperbolic cosine.
//!
//! Ported from musl 1.2.5's `cosh.c` and `coshf.c` (MIT; see [`crate::math`]
//! for the notice). musl wrote these itself; they carry no other notice.
//!
//! # Method
//!
//! Below ln2, with t = expm1(|x|), cosh(x) = 1 + t²/(2·(1 + t)); below 2^-26
//! (2^-12 for `float`) it is 1, inexact. Up to log(`DBL_MAX`) it is
//! (exp(|x|) + 1/exp(|x|))/2, and above, `sinh`'s halved exponential.

use crate::math::exp::exp;
use crate::math::expf::expf;
use crate::math::expm1::{expm1, expm1f};
use crate::math::sinh::{expo2, expo2f};
use crate::math::support::{force_eval, force_evalf, hexf32, hexf64};

/// The hyperbolic cosine of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cosh(x: f64) -> f64 {
    let x = f64::from_bits(x.to_bits() & (u64::MAX >> 1));
    let w = (x.to_bits() >> 32) as u32;

    // |x| < ln2.
    if w < 0x3fe6_2e42 {
        if w < 0x3ff0_0000 - (26 << 20) {
            // Raise inexact if x is not 0.
            force_eval(x + hexf64!("0x1p120"));
            return 1.0;
        }
        let t = expm1(x);
        return 1.0 + t * t / (2.0 * (1.0 + t));
    }

    // |x| < log(DBL_MAX).
    if w < 0x4086_2e42 {
        let t = exp(x);
        // Above log(2^26), 1/t is not needed.
        return 0.5 * (t + 1.0 / t);
    }

    // |x| > log(DBL_MAX), or NaN.
    expo2(x, 1.0)
}

/// [`cosh`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn coshf(x: f32) -> f32 {
    let w = x.to_bits() & 0x7fff_ffff;
    let x = f32::from_bits(w);

    // |x| < ln2.
    if w < 0x3f31_7217 {
        if w < 0x3f80_0000 - (12 << 23) {
            // Raise inexact if x is not 0.
            force_evalf(x + hexf32!("0x1p120"));
            return 1.0;
        }
        let t = expm1f(x);
        return 1.0 + t * t / (2.0 * (1.0 + t));
    }

    // |x| < log(FLT_MAX).
    if w < 0x42b1_7217 {
        let t = expf(x);
        return 0.5 * (t + 1.0 / t);
    }

    // |x| > log(FLT_MAX), or NaN.
    expo2f(x, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn cosh_matches_libc_test() {
        let files = [
            "crlibm/cosh.h",
            "ucb/cosh.h",
            "sanity/cosh.h",
            "special/cosh.h",
        ];
        mtest::d_d("cosh", &files, |x| cosh(x), Rules::ULP, &[]);
    }

    #[test]
    fn coshf_matches_libc_test() {
        let files = ["ucb/coshf.h", "sanity/coshf.h", "special/coshf.h"];
        mtest::d_d("coshf", &files, |x| coshf(x), Rules::ULP, &[]);
    }
}
