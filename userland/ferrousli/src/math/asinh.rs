//! `asinh` and `asinhf`: the inverse hyperbolic sine, and the ln2 that they
//! and `acosh` add for large arguments.
//!
//! Ported from musl 1.2.5's `asinh.c` and `asinhf.c` (MIT; see
//! [`crate::math`] for the notice). musl wrote these itself; they carry no
//! other notice.
//!
//! # Method
//!
//! asinh(x) = ±log(|x| + √(x² + 1)), computed on |x|: as log(|x|) + ln2 from
//! 2^26 (2^12 for `float`), as log(2|x| + 1/(√(x² + 1) + |x|)) from 2, as
//! log1p(|x| + x²/(√(x² + 1) + 1)) from 2^-26 (2^-12), and below that as x.

use crate::math::log::log;
use crate::math::log1p::{log1p, log1pf};
use crate::math::logf::logf;
use crate::math::sqrt::{sqrt, sqrtf};
use crate::math::support::{force_eval, force_evalf, hexf32, hexf64};

/// ln2, which musl writes as `0.693147180559945309417232121458176568`,
/// rounded to `double`.
pub(crate) const LN2: f64 = hexf64!("0x1.62e42fefa39efp-1");
/// ln2 rounded to `float`.
pub(crate) const LN2_F: f32 = hexf32!("0x1.62e43p-1");

/// The inverse hyperbolic sine of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn asinh(x: f64) -> f64 {
    let bits = x.to_bits();
    let e = (bits >> 52 & 0x7ff) as u32;
    let negative = bits >> 63 != 0;
    let mut x = f64::from_bits(bits & (u64::MAX >> 1));

    if e >= 0x3ff + 26 {
        // |x| >= 2^26, or inf or NaN.
        x = log(x) + LN2;
    } else if e > 0x3ff {
        // |x| >= 2.
        x = log(2.0 * x + 1.0 / (sqrt(x * x + 1.0) + x));
    } else if e >= 0x3ff - 26 {
        // |x| >= 2^-26, up to 1.6 ulps of error in [0.125, 0.5].
        x = log1p(x + x * x / (sqrt(x * x + 1.0) + 1.0));
    } else {
        // |x| < 2^-26: raise inexact if x is not 0.
        force_eval(x + hexf64!("0x1p120"));
    }
    if negative { -x } else { x }
}

/// [`asinh`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn asinhf(x: f32) -> f32 {
    let bits = x.to_bits();
    let i = bits & 0x7fff_ffff;
    let negative = bits >> 31 != 0;
    let mut x = f32::from_bits(i);

    if i >= 0x3f80_0000 + (12 << 23) {
        // |x| >= 2^12, or inf or NaN.
        x = logf(x) + LN2_F;
    } else if i >= 0x3f80_0000 + (1 << 23) {
        // |x| >= 2.
        x = logf(2.0 * x + 1.0 / (sqrtf(x * x + 1.0) + x));
    } else if i >= 0x3f80_0000 - (12 << 23) {
        // |x| >= 2^-12, up to 1.6 ulps of error in [0.125, 0.5].
        x = log1pf(x + x * x / (sqrtf(x * x + 1.0) + 1.0));
    } else {
        // |x| < 2^-12: raise inexact if x is not 0.
        force_evalf(x + hexf32!("0x1p120"));
    }
    if negative { -x } else { x }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn asinh_matches_libc_test() {
        let files = ["sanity/asinh.h", "special/asinh.h"];
        // libc-test's `asinh.c` tolerates an error under 2 ulps.
        let rules = Rules::ULP.tolerate(2.0);
        mtest::d_d("asinh", &files, |x| asinh(x), rules, &[]);
    }

    #[test]
    fn asinhf_matches_libc_test() {
        let files = ["sanity/asinhf.h", "special/asinhf.h"];
        mtest::d_d("asinhf", &files, |x| asinhf(x), Rules::ULP, &[]);
    }

    #[test]
    fn ln2_is_musls_decimal() {
        let text = "0.693147180559945309417232121458176568";
        assert_eq!(text.parse::<f64>().map(f64::to_bits), Ok(LN2.to_bits()));
        assert_eq!(text.parse::<f32>().map(f32::to_bits), Ok(LN2_F.to_bits()));
    }
}
