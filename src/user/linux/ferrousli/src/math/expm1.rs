//! `expm1` and `expm1f`: e raised to a power, less 1, accurate near 0.
//!
//! Ported from musl 1.2.5's `expm1.c` and `expm1f.c` (MIT; see
//! [`crate::math`] for the notice). musl took them from FreeBSD's msun;
//! `expm1f.c` was converted to `float` by Ian Lance Taylor, Cygnus Support.
//! The msun files carry this notice:
//!
//! ```text
//! Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//!
//! Developed at SunPro, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ```
//!
//! musl writes most of these constants in decimal, with their bits in a
//! comment. They are written here as those bits. The parts of ln2 are
//! `log1p`'s, which has the same ones.
//!
//! # Method
//!
//! x = k·ln2 + r with |r| <= ln2/2, and a correction c for r's rounding.
//! expm1(r) comes from a rational function of r whose even part is a
//! polynomial in r²/2, and 2^k·(expm1(r) + 1) - 1 is arranged for each k so
//! that it loses as little as it can to rounding.

use crate::math::log1p::{LN2_HI, LN2_HI_F, LN2_LO, LN2_LO_F};
use crate::math::support::{force_evalf, hexf32, hexf64, is_nan};

/// Above this, expm1(x) overflows.
const O_THRESHOLD: f64 = f64::from_bits(0x4086_2e42_fefa_39ef);
/// 1/ln2.
const INVLN2: f64 = f64::from_bits(0x3ff7_1547_652b_82fe);

// The coefficients of the even polynomial, scaled: Qn here is 2^n·Qn in
// musl's description, for R(2z) where z = x²/2.
const Q1: f64 = f64::from_bits(0xbfa1_1111_1111_10f4);
const Q2: f64 = f64::from_bits(0x3f5a_01a0_19fe_5585);
const Q3: f64 = f64::from_bits(0xbf14_ce19_9eaa_dbb7);
const Q4: f64 = f64::from_bits(0x3ed0_cfca_86e6_5239);
const Q5: f64 = f64::from_bits(0xbe8a_fdb7_6e09_c32d);

/// 1/ln2 for `float`.
const INVLN2_F: f32 = f32::from_bits(0x3fb8_aa3b);

// The `float` polynomial's coefficients, scaled as the `double` ones are.
const Q1_F: f32 = hexf32!("-0x888868.0p-28");
const Q2_F: f32 = hexf32!("0xcf3010.0p-33");

/// e raised to the power `x`, less 1.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn expm1(x: f64) -> f64 {
    let bits = x.to_bits();
    let hx = (bits >> 32) as u32 & 0x7fff_ffff;
    let negative = bits >> 63 != 0;
    let mut x = x;

    // Huge and non-finite arguments.
    if hx >= 0x4043_687a {
        // |x| >= 56·ln2.
        if is_nan(x) {
            return x;
        }
        if negative {
            return -1.0;
        }
        if x > O_THRESHOLD {
            return x * hexf64!("0x1p1023");
        }
    }

    // Reduce the argument.
    let k: i32;
    let c: f64;
    if hx > 0x3fd6_2e42 {
        // |x| > ln2/2.
        let (hi, lo);
        if hx < 0x3ff0_a2b2 {
            // And |x| < 3·ln2/2.
            if negative {
                hi = x + LN2_HI;
                lo = -LN2_LO;
                k = -1;
            } else {
                hi = x - LN2_HI;
                lo = LN2_LO;
                k = 1;
            }
        } else {
            k = (INVLN2 * x + if negative { -0.5 } else { 0.5 }) as i32;
            let t = f64::from(k);
            // Exact: |k| <= 1024 and `LN2_HI` has 32 significant bits. So it
            // does not matter that LLVM may write it as x + t·-LN2_HI.
            hi = x - t * LN2_HI;
            lo = t * LN2_LO;
        }
        x = hi - lo;
        c = (hi - x) - lo;
    } else if hx < 0x3c90_0000 {
        // |x| < 2^-54: raise underflow if x is subnormal.
        if hx < 0x0010_0000 {
            force_evalf(x as f32);
        }
        return x;
    } else {
        k = 0;
        c = 0.0;
    }

    // x is now in the primary range.
    let hfx = 0.5 * x;
    let hxs = x * hfx;
    let r1 = 1.0 + hxs * (Q1 + hxs * (Q2 + hxs * (Q3 + hxs * (Q4 + hxs * Q5))));
    let t = 3.0 - r1 * hfx;
    let mut e = hxs * ((r1 - t) / (6.0 - x * t));
    if k == 0 {
        // c is 0.
        return x - (x * e - hxs);
    }
    e = x * (e - c) - c;
    e -= hxs;
    // exp(x) ~= 2^k·(x_reduced - e + 1).
    if k == -1 {
        return 0.5 * (x - e) - 0.5;
    }
    if k == 1 {
        if x < -0.25 {
            return -2.0 * (e - (x + 0.5));
        }
        return 1.0 + 2.0 * (x - e);
    }
    // 2^k.
    let twopk = f64::from_bits(u64::from((0x3ff + k).unsigned_abs()) << 52);
    if !(0..=56).contains(&k) {
        // exp(x) - 1 is good enough.
        let mut y = x - e + 1.0;
        if k == 1024 {
            y = y * 2.0 * hexf64!("0x1p1023");
        } else {
            y *= twopk;
        }
        return y - 1.0;
    }
    // 2^-k.
    let twomk = f64::from_bits(u64::from((0x3ff - k).unsigned_abs()) << 52);
    if k < 20 {
        (x - e + (1.0 - twomk)) * twopk
    } else {
        (x - (e + twomk) + 1.0) * twopk
    }
}

/// [`expm1`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn expm1f(x: f32) -> f32 {
    let bits = x.to_bits();
    let hx = bits & 0x7fff_ffff;
    let negative = bits >> 31 != 0;
    let mut x = x;

    // Huge and non-finite arguments.
    if hx >= 0x4195_b844 {
        // |x| >= 27·ln2.
        if hx > 0x7f80_0000 {
            // NaN.
            return x;
        }
        if negative {
            return -1.0;
        }
        if hx > 0x42b1_7217 {
            // x > log(FLT_MAX).
            return x * hexf32!("0x1p127");
        }
    }

    // Reduce the argument.
    let k: i32;
    let c: f32;
    if hx > 0x3eb1_7218 {
        // |x| > ln2/2.
        let (hi, lo);
        if hx < 0x3f85_1592 {
            // And |x| < 3·ln2/2.
            if negative {
                hi = x + LN2_HI_F;
                lo = -LN2_LO_F;
                k = -1;
            } else {
                hi = x - LN2_HI_F;
                lo = LN2_LO_F;
                k = 1;
            }
        } else {
            k = (INVLN2_F * x + if negative { -0.5 } else { 0.5 }) as i32;
            let t = k as f32;
            // Exact: |k| <= 128 and `LN2_HI_F` has 17 significant bits. So it
            // does not matter that LLVM may write it as x + t·-LN2_HI_F.
            hi = x - t * LN2_HI_F;
            lo = t * LN2_LO_F;
        }
        x = hi - lo;
        c = (hi - x) - lo;
    } else if hx < 0x3300_0000 {
        // |x| < 2^-25: raise underflow if x is subnormal.
        if hx < 0x0080_0000 {
            force_evalf(x * x);
        }
        return x;
    } else {
        k = 0;
        c = 0.0;
    }

    // x is now in the primary range.
    let hfx = 0.5 * x;
    let hxs = x * hfx;
    let r1 = 1.0 + hxs * (Q1_F + hxs * Q2_F);
    let t = 3.0 - r1 * hfx;
    let mut e = hxs * ((r1 - t) / (6.0 - x * t));
    if k == 0 {
        // c is 0.
        return x - (x * e - hxs);
    }
    e = x * (e - c) - c;
    e -= hxs;
    // exp(x) ~= 2^k·(x_reduced - e + 1).
    if k == -1 {
        return 0.5 * (x - e) - 0.5;
    }
    if k == 1 {
        if x < -0.25 {
            return -2.0 * (e - (x + 0.5));
        }
        return 1.0 + 2.0 * (x - e);
    }
    // 2^k.
    let twopk = f32::from_bits((0x7f + k).unsigned_abs() << 23);
    if !(0..=56).contains(&k) {
        // exp(x) - 1 is good enough.
        let mut y = x - e + 1.0;
        if k == 128 {
            y = y * 2.0 * hexf32!("0x1p127");
        } else {
            y *= twopk;
        }
        return y - 1.0;
    }
    // 2^-k.
    let twomk = f32::from_bits((0x7f - k).unsigned_abs() << 23);
    if k < 23 {
        (x - e + (1.0 - twomk)) * twopk
    } else {
        (x - (e + twomk) + 1.0) * twopk
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn expm1_matches_libc_test() {
        let files = ["crlibm/expm1.h", "sanity/expm1.h", "special/expm1.h"];
        mtest::d_d("expm1", &files, |x| expm1(x), Rules::ULP, &[]);
    }

    #[test]
    fn expm1f_matches_libc_test() {
        let files = ["sanity/expm1f.h", "special/expm1f.h"];
        mtest::d_d("expm1f", &files, |x| expm1f(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (O_THRESHOLD, "7.09782712893383973096e+02"),
            (INVLN2, "1.44269504088896338700e+00"),
            (Q1, "-3.33333333333331316428e-02"),
            (Q2, "1.58730158725481460165e-03"),
            (Q3, "-7.93650757867487942473e-05"),
            (Q4, "4.00821782732936239552e-06"),
            (Q5, "-2.01099218183624371326e-07"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
        let floats = [
            (INVLN2_F, "1.4426950216e+00"),
            (Q1_F, "-3.3333212137e-2"),
            (Q2_F, "1.5807170421e-3"),
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
