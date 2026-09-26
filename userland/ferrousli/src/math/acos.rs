//! `acos` and `acosf`.
//!
//! Ported from musl 1.2.5's `acos.c` and `acosf.c` (MIT; see [`crate::math`]
//! for the notice). musl took them from FreeBSD's msun; `acosf.c` was
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
//! musl's `acos.c` and `acosf.c` repeat the rational function R and the parts
//! of π/2 from `asin.c` and `asinf.c`, the same constants digit for digit.
//! This module uses [`crate::math::asin`]'s.
//!
//! # Method
//!
//! acos(x) = π/2 - asin(x). On [-0.5, 0.5] that is
//! π/2 - (x + x·x²·R(x²)). Above 0.5, acos(x) = 2·asin(√((1 - x)/2)), which
//! with z = (1 - x)/2 and s = √z split into a head f and a correction c, is
//! 2·f + (2·c + 2·s·z·R(z)). Below -0.5, acos(x) = π - 2·asin(√((1 + x)/2)).

use crate::math::asin::{PIO2_HI, PIO2_LO, ratio, ratiof};
use crate::math::sqrt::{sqrt, sqrtf};
use crate::math::support::{
    barrier, barrierf, hexf32, hexf64, high_word, invalid, invalidf, low_word, with_low_word,
};

/// `acosf`'s high part of π/2.
const PIO2_HI_F: f32 = f32::from_bits(0x3fc9_0fda);
/// The part of π/2 that [`PIO2_HI_F`] misses.
const PIO2_LO_F: f32 = f32::from_bits(0x33a2_2168);

/// The arccosine of `x`, in radians, in [0, π].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn acos(x: f64) -> f64 {
    let hx = high_word(x);
    let ix = hx & 0x7fff_ffff;

    // |x| >= 1, or NaN.
    if ix >= 0x3ff0_0000 {
        if ((ix - 0x3ff0_0000) | low_word(x)) == 0 {
            // acos(1) = 0, and acos(-1) = π, inexact, which musl adds at run
            // time.
            if hx >> 31 != 0 {
                return barrier(2.0 * PIO2_HI) + hexf64!("0x1p-120");
            }
            return 0.0;
        }
        // NaN, raising invalid unless x is one.
        return invalid(x);
    }

    // |x| < 0.5.
    if ix < 0x3fe0_0000 {
        if ix <= 0x3c60_0000 {
            // |x| < 2^-57: π/2, inexact, added at run time.
            return barrier(PIO2_HI) + hexf64!("0x1p-120");
        }
        // LLVM would rewrite each `a - (b - c)` here as `a + (c - b)`, which
        // rounds the difference the other way when rounding up or down.
        return PIO2_HI - barrier(x - barrier(PIO2_LO - x * ratio(x * x)));
    }

    // x < -0.5.
    if hx >> 31 != 0 {
        let z = (1.0 + x) * 0.5;
        let s = sqrt(z);
        let w = ratio(z) * s - PIO2_LO;
        return 2.0 * (PIO2_HI - (s + w));
    }

    // x > 0.5.
    let z = (1.0 - x) * 0.5;
    let s = sqrt(z);
    let df = with_low_word(s, 0);
    let c = (z - df * df) / (s + df);
    let w = ratio(z) * s + c;
    2.0 * (df + w)
}

/// [`acos`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn acosf(x: f32) -> f32 {
    let hx = x.to_bits();
    let ix = hx & 0x7fff_ffff;

    // |x| >= 1, or NaN.
    if ix >= 0x3f80_0000 {
        if ix == 0x3f80_0000 {
            // acosf(1) = 0, and acosf(-1) = π, inexact, which musl adds at run
            // time.
            if hx >> 31 != 0 {
                return barrierf(2.0 * PIO2_HI_F) + hexf32!("0x1p-120");
            }
            return 0.0;
        }
        // NaN, raising invalid unless x is one.
        return invalidf(x);
    }

    // |x| < 0.5.
    if ix < 0x3f00_0000 {
        if ix <= 0x3280_0000 {
            // |x| < 2^-26: π/2, inexact, added at run time.
            return barrierf(PIO2_HI_F) + hexf32!("0x1p-120");
        }
        // As in `acos`.
        return PIO2_HI_F - barrierf(x - barrierf(PIO2_LO_F - x * ratiof(x * x)));
    }

    // x < -0.5.
    if hx >> 31 != 0 {
        let z = (1.0 + x) * 0.5;
        let s = sqrtf(z);
        let w = ratiof(z) * s - PIO2_LO_F;
        return 2.0 * (PIO2_HI_F - (s + w));
    }

    // x > 0.5.
    let z = (1.0 - x) * 0.5;
    let s = sqrtf(z);
    let df = f32::from_bits(s.to_bits() & 0xffff_f000);
    let c = (z - df * df) / (s + df);
    let w = ratiof(z) * s + c;
    2.0 * (df + w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn acos_matches_libc_test() {
        let files = [
            "crlibm/acos.h",
            "ucb/acos.h",
            "sanity/acos.h",
            "special/acos.h",
        ];
        mtest::d_d("acos", &files, |x| acos(x), Rules::ULP, &[]);
    }

    #[test]
    fn acosf_matches_libc_test() {
        let files = ["ucb/acosf.h", "sanity/acosf.h", "special/acosf.h"];
        mtest::d_d("acosf", &files, |x| acosf(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        let floats = [
            (PIO2_HI_F, "1.5707962513e+00"),
            (PIO2_LO_F, "7.5497894159e-08"),
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
