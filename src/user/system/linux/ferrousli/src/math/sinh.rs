//! `sinh` and `sinhf`: the hyperbolic sine, and the halved exponential for
//! large arguments that they and `cosh` share.
//!
//! Ported from musl 1.2.5's `sinh.c`, `sinhf.c`, `__expo2.c` and `__expo2f.c`
//! (MIT; see [`crate::math`] for the notice). musl wrote these itself; they
//! carry no other notice.
//!
//! # Method
//!
//! With t = expm1(|x|), sinh(x) = ±(t + t/(t + 1))/2, which near 0 is
//! arranged as ±(2t - t²/(t + 1))/2 so that the subtraction loses less. Below
//! 2^-26 (2^-12 for `float`) the result is x. Where exp(|x|) overflows,
//! exp(|x|)/2 is exp(|x| - k·ln2)·2^(k-1) for an odd k chosen so that k·ln2
//! has a small relative error.

use crate::math::exp::exp;
use crate::math::expf::expf;
use crate::math::expm1::{expm1, expm1f};
use crate::math::support::{force_eval, force_evalf, hexf32, hexf64};

/// `__expo2`'s k·ln2, for k = 2043, rounded.
const KLN2: f64 = hexf64!("0x1.62066151add8bp+10");
/// 2^(k/2) for k = 2043, which musl builds from its words: `scale²` is
/// 2^(k-1), which overflows, so it multiplies by it twice.
const SCALE: f64 = hexf64!("0x1p1021");

/// `__expo2f`'s k·ln2, for k = 235, rounded.
const KLN2_F: f32 = hexf32!("0x1.45c778p+7");
/// 2^(k/2) for k = 235.
const SCALE_F: f32 = hexf32!("0x1p117");

/// exp(`x`)·`sign`/2 for `x` >= log(`DBL_MAX`), where exp(x) itself
/// overflows. musl's `__expo2`, which `cosh` shares.
pub(crate) fn expo2(x: f64, sign: f64) -> f64 {
    // exp(x - k·ln2)·2^(k-1). The sign goes on before the rounding or the
    // overflow, which matters when rounding upward or downward.
    exp(x - KLN2) * (sign * SCALE) * SCALE
}

/// [`expo2`] for `float`: musl's `__expo2f`.
pub(crate) fn expo2f(x: f32, sign: f32) -> f32 {
    expf(x - KLN2_F) * (sign * SCALE_F) * SCALE_F
}

/// The hyperbolic sine of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sinh(x: f64) -> f64 {
    let bits = x.to_bits();
    let h = if bits >> 63 != 0 { -0.5 } else { 0.5 };
    let absx = f64::from_bits(bits & (u64::MAX >> 1));
    let w = (absx.to_bits() >> 32) as u32;

    // |x| < log(DBL_MAX).
    if w < 0x4086_2e42 {
        let t = expm1(absx);
        if w < 0x3ff0_0000 {
            if w < 0x3ff0_0000 - (26 << 20) {
                // expm1 raised inexact and underflow; returning here avoids
                // a spurious underflow. musl computes t before this test, so
                // the exceptions are its; without the `force_eval` LLVM may
                // drop the unused call once it is inlined.
                force_eval(t);
                return x;
            }
            return h * (2.0 * t - t * t / (t + 1.0));
        }
        // Above log(2^26), h·exp(x) would do.
        return h * (t + t / (t + 1.0));
    }

    // |x| > log(DBL_MAX), or NaN.
    expo2(absx, 2.0 * h)
}

/// [`sinh`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sinhf(x: f32) -> f32 {
    let bits = x.to_bits();
    let h = if bits >> 31 != 0 { -0.5 } else { 0.5 };
    let w = bits & 0x7fff_ffff;
    let absx = f32::from_bits(w);

    // |x| < log(FLT_MAX).
    if w < 0x42b1_7217 {
        let t = expm1f(absx);
        if w < 0x3f80_0000 {
            if w < 0x3f80_0000 - (12 << 23) {
                // As in `sinh`: the exceptions are expm1f's.
                force_evalf(t);
                return x;
            }
            return h * (2.0 * t - t * t / (t + 1.0));
        }
        return h * (t + t / (t + 1.0));
    }

    // |x| > log(FLT_MAX), or NaN.
    expo2f(absx, 2.0 * h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn sinh_matches_libc_test() {
        let files = [
            "crlibm/sinh.h",
            "ucb/sinh.h",
            "sanity/sinh.h",
            "special/sinh.h",
        ];
        // libc-test's `sinh.c` tolerates an error under 2 ulps to nearest,
        // and any error in the other modes.
        let rules = Rules::ULP.tolerate(2.0).directed();
        mtest::d_d("sinh", &files, |x| sinh(x), rules, &[]);
    }

    #[test]
    fn sinhf_matches_libc_test() {
        let files = ["ucb/sinhf.h", "sanity/sinhf.h", "special/sinhf.h"];
        // libc-test's `sinhf.c` tolerates any error outside rounding to
        // nearest.
        let rules = Rules::ULP.directed();
        mtest::d_d("sinhf", &files, |x| sinhf(x), rules, &[]);
    }

    #[test]
    fn the_scales_are_musls_words() {
        // musl sets the high word to (0x3ff + k/2) << 20, or the `float`'s
        // word to (0x7f + k/2) << 23.
        assert_eq!(SCALE.to_bits(), ((0x3ff + 2043 / 2) << 20_u64) << 32);
        assert_eq!(SCALE_F.to_bits(), (0x7f + 235 / 2) << 23);
    }
}
