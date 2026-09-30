//! `tan`, and the kernel that evaluates it within π/4 of zero.
//!
//! Ported from musl 1.2.5's `tan.c` and `__tan.c` (MIT; see [`crate::math`]
//! for the notice). musl took them from FreeBSD's msun, whose `s_tan.c`
//! carries this notice:
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
//! and whose `k_tan.c` carries this one:
//!
//! ```text
//! Copyright 2004 Sun Microsystems, Inc.  All Rights Reserved.
//!
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ```
//!
//! musl writes these constants in decimal, with their bits in a comment. They
//! are written here as those bits. The reduction by multiples of π/2 is
//! [`crate::math::trig`]'s, which `sin` and `cos` share.
//!
//! # Method
//!
//! x is reduced to y = x - n·π/2, |y| ~<= π/4, as a head and a tail, and
//! tan(x) is tan(y) for an even n and -1/tan(y) for an odd one. Near 0,
//! tan(y) is an odd polynomial of degree 27. Above 0.6744 it is computed
//! from tan(π/4 - y) instead, and -1/tan(y) is corrected for its rounding.

use crate::math::support::{barrier, force_eval, hexf64, high_word, with_low_word};
use crate::math::trig::rem_pio2;

// __tan.c's T[]: the coefficients of x³ to x^27 in tan(x)'s odd polynomial.
const T0: f64 = f64::from_bits(0x3fd5_5555_5555_5563);
const T1: f64 = f64::from_bits(0x3fc1_1111_1110_fe7a);
const T2: f64 = f64::from_bits(0x3fab_a1ba_1bb3_41fe);
const T3: f64 = f64::from_bits(0x3f96_64f4_8406_d637);
const T4: f64 = f64::from_bits(0x3f82_26e3_e96e_8493);
const T5: f64 = f64::from_bits(0x3f6d_6d22_c956_0328);
const T6: f64 = f64::from_bits(0x3f57_dbc8_fee0_8315);
const T7: f64 = f64::from_bits(0x3f43_44d8_f2f2_6501);
const T8: f64 = f64::from_bits(0x3f30_26f7_1a8d_1068);
const T9: f64 = f64::from_bits(0x3f14_7e88_a037_92a6);
const T10: f64 = f64::from_bits(0x3f12_b80f_32f0_a7e9);
const T11: f64 = f64::from_bits(0xbef3_75cb_db60_5373);
const T12: f64 = f64::from_bits(0x3efb_2a70_74bf_7ad4);

/// π/4.
const PIO4: f64 = f64::from_bits(0x3fe9_21fb_5444_2d18);
/// The part of π/4 that [`PIO4`] misses.
const PIO4LO: f64 = f64::from_bits(0x3c81_a626_3314_5c07);

/// The tangent of `x`, in radians.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tan(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;

    // |x| ~<= π/4.
    if ix <= 0x3fe9_21fb {
        if ix < 0x3e40_0000 {
            // |x| < 2^-27: raise inexact if x is not zero, and underflow if it
            // is subnormal.
            if ix < 0x0010_0000 {
                force_eval(x / hexf64!("0x1p120"));
            } else {
                force_eval(x + hexf64!("0x1p120"));
            }
            return x;
        }
        return kernel_tan(x, 0.0, false);
    }

    // tan(Inf or NaN) is NaN.
    if ix >= 0x7ff0_0000 {
        return barrier(x) - x;
    }

    let (n, y0, y1) = rem_pio2(x);
    kernel_tan(y0, y1, n & 1 != 0)
}

/// musl's `__tan`: tan(x + y) for |x| ~<= π/4, where `y` is the tail of `x`,
/// or -1/tan(x + y) if `odd` is set. The caller returns tan(-0) itself.
pub(crate) fn kernel_tan(x: f64, y: f64, odd: bool) -> f64 {
    let hx = high_word(x);
    let negative = hx >> 31 != 0;
    // |x| >= 0.6744: tan(x) = tan(π/4 - (π/4 - x)), from positive x.
    let big = (hx & 0x7fff_ffff) >= 0x3fe5_9428;
    let (x, y) = if big {
        let (x, y) = if negative { (-x, -y) } else { (x, y) };
        ((PIO4 - x) + (PIO4LO - y), 0.0)
    } else {
        (x, y)
    };

    // x⁵·(T1 + x²·T2 + ...), split into x⁵·(T1 + x⁴·T3 + ... + x^20·T11)
    // and x⁵·x²·(T2 + x⁴·T4 + ... + x^20·T12).
    let z = x * x;
    let w = z * z;
    let r = T1 + w * (T3 + w * (T5 + w * (T7 + w * (T9 + w * T11))));
    let v = z * (T2 + w * (T4 + w * (T6 + w * (T8 + w * (T10 + w * T12)))));
    let s = z * x;
    let r = y + z * (s * (r + v) + y) + s * T0;
    let w = x + r;

    if big {
        let s = if odd { -1.0 } else { 1.0 };
        let v = s - 2.0 * (x + (r - w * w / (w + s)));
        return if negative { -v } else { v };
    }
    if !odd {
        return w;
    }
    // -1/(x + r) has up to 2 ulps of error, so compute it accurately:
    // w0 + v = x + r, and a0 is -1/w cut to its high word.
    let w0 = with_low_word(w, 0);
    let v = r - (w0 - x);
    let a = -1.0 / w;
    let a0 = with_low_word(a, 0);
    a0 + a * (1.0 + a0 * w0 + a0 * v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    /// libc-test's `tan.c` counts an error in a rounding mode other than to
    /// nearest as tolerated.
    const RULES: Rules = Rules::ULP.directed();

    #[test]
    fn tan_matches_libc_test() {
        let files = ["crlibm/tan.h", "ucb/tan.h", "sanity/tan.h", "special/tan.h"];
        mtest::d_d("tan", &files, |x| tan(x), RULES, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (T0, "3.33333333333334091986e-01"),
            (T1, "1.33333333333201242699e-01"),
            (T2, "5.39682539762260521377e-02"),
            (T3, "2.18694882948595424599e-02"),
            (T4, "8.86323982359930005737e-03"),
            (T5, "3.59207910759131235356e-03"),
            (T6, "1.45620945432529025516e-03"),
            (T7, "5.88041240820264096874e-04"),
            (T8, "2.46463134818469906812e-04"),
            (T9, "7.81794442939557092300e-05"),
            (T10, "7.14072491382608190305e-05"),
            (T11, "-1.85586374855275456654e-05"),
            (T12, "2.59073051863633712884e-05"),
            (PIO4, "7.85398163397448278999e-01"),
            (PIO4LO, "3.06161699786838301793e-17"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
    }
}
