//! `asin` and `asinf`, and the rational function of x² that they and `acos`
//! share.
//!
//! Ported from musl 1.2.5's `asin.c` and `asinf.c` (MIT; see [`crate::math`]
//! for the notice). musl took them from FreeBSD's msun; `asinf.c` was
//! converted to `float` by Ian Lance Taylor, Cygnus Support. The msun files
//! carry this notice:
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
//! musl writes these constants in decimal, with the bits of the `double` ones
//! in a comment. They are written here as bits, and the tests check them
//! against musl's decimals.
//!
//! # Method
//!
//! On [0, 0.5], asin(x) = x + x·x²·R(x²), where R is a rational approximation
//! of (asin(x) - x)/x³. On [0.5, 1], asin(x) = π/2 - 2·asin(√((1 - x)/2)),
//! and with z = (1 - x)/2 and s = √z, that is π/2 - 2·(s + s·z·R(z)). Below
//! 0.975, s is split into a head f and a correction c = (z - f²)/(s + f) so
//! that the subtraction loses less to rounding.

use crate::math::manipulate::{fabs, fabsf};
use crate::math::sqrt::sqrt;
use crate::math::support::{
    barrier, hexf64, high_word, invalid, invalidf, low_word, with_low_word,
};

/// The high part of π/2, which `acos` shares.
pub(crate) const PIO2_HI: f64 = f64::from_bits(0x3ff9_21fb_5444_2d18);
/// The part of π/2 that [`PIO2_HI`] misses.
pub(crate) const PIO2_LO: f64 = f64::from_bits(0x3c91_a626_3314_5c07);

// pS0 to pS5 and qS1 to qS4: R's numerator and denominator.
const PS0: f64 = f64::from_bits(0x3fc5_5555_5555_5555);
const PS1: f64 = f64::from_bits(0xbfd4_d612_03eb_6f7d);
const PS2: f64 = f64::from_bits(0x3fc9_c155_0e88_4455);
const PS3: f64 = f64::from_bits(0xbfa4_8228_b568_8f3b);
const PS4: f64 = f64::from_bits(0x3f49_efe0_7501_b288);
const PS5: f64 = f64::from_bits(0x3f02_3de1_0dfd_f709);
const QS1: f64 = f64::from_bits(0xc003_3a27_1c8a_2d4b);
const QS2: f64 = f64::from_bits(0x4000_2ae5_9c59_8ac8);
const QS3: f64 = f64::from_bits(0xbfe6_066c_1b8d_0159);
const QS4: f64 = f64::from_bits(0x3fb3_b8c5_b12e_9282);

/// `asinf`'s π/2, rounded to `double`.
const PIO2: f64 = f64::from_bits(0x3ff9_21fb_5444_2d18);

// The `float` R's numerator and denominator, which `acosf` shares.
const PS0_F: f32 = f32::from_bits(0x3e2a_aa75);
const PS1_F: f32 = f32::from_bits(0xbd2f_13ba);
const PS2_F: f32 = f32::from_bits(0xbc0d_d36b);
const QS1_F: f32 = f32::from_bits(0xbf34_e5ae);

/// musl's `R`: (asin(√z) - √z)/√z³, approximately.
pub(crate) fn ratio(z: f64) -> f64 {
    let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
    let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
    p / q
}

/// [`ratio`] for `float`, with fewer terms.
///
/// Never inlined: inlined into `acosf`, LLVM's vectoriser pairs its division
/// with `acosf`'s own in one `divps`, whose two unused lanes divide 0 by 0 and
/// raise invalid.
#[inline(never)]
pub(crate) fn ratiof(z: f32) -> f32 {
    let p = z * (PS0_F + z * (PS1_F + z * PS2_F));
    let q = 1.0 + z * QS1_F;
    p / q
}

/// The arcsine of `x`, in radians, in [-π/2, π/2].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn asin(x: f64) -> f64 {
    let hx = high_word(x);
    let ix = hx & 0x7fff_ffff;

    // |x| >= 1, or NaN.
    if ix >= 0x3ff0_0000 {
        if ((ix - 0x3ff0_0000) | low_word(x)) == 0 {
            // asin(±1) = ±π/2, inexact.
            return x * PIO2_HI + hexf64!("0x1p-120");
        }
        // NaN, raising invalid unless x is one.
        return invalid(x);
    }

    // |x| < 0.5.
    if ix < 0x3fe0_0000 {
        // 2^-1022 <= |x| < 2^-26: x, without raising underflow.
        if (0x0010_0000..0x3e50_0000).contains(&ix) {
            return x;
        }
        return x + x * ratio(x * x);
    }

    // 0.5 <= |x| < 1.
    let z = (1.0 - fabs(x)) * 0.5;
    let s = sqrt(z);
    let r = ratio(z);
    let y = if ix >= 0x3fef_3333 {
        // |x| > 0.975.
        PIO2_HI - (2.0 * (s + s * r) - PIO2_LO)
    } else {
        // f + c = √z.
        let f = with_low_word(s, 0);
        let c = (z - f * f) / (s + f);
        // LLVM would rewrite each `a - (b - c)` here as `a + (c - b)`, which
        // rounds the difference the other way when rounding up or down.
        let t = 2.0 * s * r - barrier(PIO2_LO - 2.0 * c) - barrier(0.5 * PIO2_HI - 2.0 * f);
        0.5 * PIO2_HI - barrier(t)
    };
    if hx >> 31 != 0 { -y } else { y }
}

/// [`asin`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn asinf(x: f32) -> f32 {
    let hx = x.to_bits();
    let ix = hx & 0x7fff_ffff;

    // |x| >= 1, or NaN.
    if ix >= 0x3f80_0000 {
        if ix == 0x3f80_0000 {
            // asinf(±1) = ±π/2, inexact.
            return (f64::from(x) * PIO2 + hexf64!("0x1p-120")) as f32;
        }
        // NaN, raising invalid unless x is one.
        return invalidf(x);
    }

    // |x| < 0.5.
    if ix < 0x3f00_0000 {
        // 2^-126 <= |x| < 2^-12: x, without raising underflow.
        if (0x0080_0000..0x3980_0000).contains(&ix) {
            return x;
        }
        return x + x * ratiof(x * x);
    }

    // 0.5 <= |x| < 1.
    let z = (1.0 - fabsf(x)) * 0.5;
    let s = sqrt(f64::from(z));
    let y = (PIO2 - 2.0 * (s + s * f64::from(ratiof(z)))) as f32;
    if hx >> 31 != 0 { -y } else { y }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn asin_matches_libc_test() {
        let files = [
            "crlibm/asin.h",
            "ucb/asin.h",
            "sanity/asin.h",
            "special/asin.h",
        ];
        mtest::d_d("asin", &files, |x| asin(x), Rules::ULP, &[]);
    }

    #[test]
    fn asinf_matches_libc_test() {
        let files = ["ucb/asinf.h", "sanity/asinf.h", "special/asinf.h"];
        mtest::d_d("asinf", &files, |x| asinf(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the `double` bits.
        let doubles = [
            (PIO2_HI, "1.57079632679489655800e+00"),
            (PIO2_LO, "6.12323399573676603587e-17"),
            (PS0, "1.66666666666666657415e-01"),
            (PS1, "-3.25565818622400915405e-01"),
            (PS2, "2.01212532134862925881e-01"),
            (PS3, "-4.00555345006794114027e-02"),
            (PS4, "7.91534994289814532176e-04"),
            (PS5, "3.47933107596021167570e-05"),
            (QS1, "-2.40339491173441421878e+00"),
            (QS2, "2.02094576023350569471e+00"),
            (QS3, "-6.88283971605453293030e-01"),
            (QS4, "7.70381505559019352791e-02"),
            (PIO2, "1.570796326794896558e+00"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
        let floats = [
            (PS0_F, "1.6666586697e-01"),
            (PS1_F, "-4.2743422091e-02"),
            (PS2_F, "-8.6563630030e-03"),
            (QS1_F, "-7.0662963390e-01"),
        ];
        for (value, text) in floats {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f32>().map(f32::to_bits),
                "{text}"
            );
        }
    }
}
