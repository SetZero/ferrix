//! `log10` and `log10f`, the base-10 logarithms.
//!
//! Ported from musl 1.2.5's `log10.c` and `log10f.c` (MIT; see
//! [`crate::math`] for the notice), with the polynomial from
//! [`crate::math::log1p`]. musl took them from FreeBSD's msun, whose files
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
//! musl writes these constants in decimal, with their bits in a comment. They
//! are written here as those bits.
//!
//! # Method
//!
//! As `log1p`: x = 2^k·(1 + f), and log(1 + f) = f - f²/2 + r. Then
//! log10(x) = (f - f²/2 + r)/ln10 + k·log10(2), combined in extra precision.

use crate::math::log1p::{polynomial, polynomialf};
use crate::math::support::{barrier, barrierf, hexf32, hexf64};

/// The high bits of 1/ln10.
const IVLN10HI: f64 = f64::from_bits(0x3fdb_cb7b_1520_0000);
/// The rest of 1/ln10.
const IVLN10LO: f64 = f64::from_bits(0x3dbb_9438_ca9a_add5);
/// The high bits of log10(2).
const LOG10_2HI: f64 = f64::from_bits(0x3fd3_4413_509f_6000);
/// The rest of log10(2).
const LOG10_2LO: f64 = f64::from_bits(0x3d59_fef3_11f1_2b36);

/// The high bits of 1/ln10 for `float`.
const IVLN10HI_F: f32 = f32::from_bits(0x3ede_6000);
/// The rest of 1/ln10 for `float`.
const IVLN10LO_F: f32 = f32::from_bits(0xb804_ead9);
/// The high bits of log10(2) for `float`.
const LOG10_2HI_F: f32 = f32::from_bits(0x3e9a_2080);
/// The rest of log10(2) for `float`.
const LOG10_2LO_F: f32 = f32::from_bits(0x3554_27db);

/// The base-10 logarithm of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn log10(x: f64) -> f64 {
    let mut x = x;
    let mut bits = x.to_bits();
    let mut hx = (bits >> 32) as u32;
    let mut k = 0;
    if hx < 0x0010_0000 || hx >> 31 != 0 {
        if bits << 1 == 0 {
            // log(±0) is -inf. The barrier keeps LLVM from folding the
            // division, and divide-by-zero with it, if it works out that x is
            // zero here.
            return -1.0 / barrier(x * x);
        }
        if hx >> 31 != 0 {
            // log(-x) is NaN. musl writes x - x, which clippy takes
            // for a mistake; the barrier changes nothing else.
            return (barrier(x) - x) / 0.0;
        }
        // x is subnormal: scale it up.
        k -= 54;
        x *= hexf64!("0x1p54");
        bits = x.to_bits();
        hx = (bits >> 32) as u32;
    } else if hx >= 0x7ff0_0000 {
        return x;
    } else if hx == 0x3ff0_0000 && bits << 32 == 0 {
        return 0.0;
    }

    // Reduce x into [√2/2, √2].
    hx += 0x3ff0_0000 - 0x3fe6_a09e;
    k += (hx >> 20) as i32 - 0x3ff;
    hx = (hx & 0x000f_ffff) + 0x3fe6_a09e;
    let x = f64::from_bits(u64::from(hx) << 32 | (bits & 0xffff_ffff));

    let f = x - 1.0;
    let hfsq = 0.5 * f * f;
    let s = f / (2.0 + f);
    let r = polynomial(s * s);

    // hi + lo = f - hfsq + s·(hfsq + R) ~= log(1 + f), with hi's low word
    // cleared, as `log2` does.
    let hi = f - hfsq;
    let hi = f64::from_bits(hi.to_bits() & (u64::MAX << 32));
    let lo = f - hi - hfsq + s * (hfsq + r);

    // val_hi + val_lo ~= log10(1 + f) + k·log10(2).
    let mut val_hi = hi * IVLN10HI;
    let dk = f64::from(k);
    let y = dk * LOG10_2HI;
    let mut val_lo = dk * LOG10_2LO + (lo + hi) * IVLN10LO + lo * IVLN10HI;

    // Adding y in extra precision is not strictly needed, since there is no
    // large cancellation near x = √2 or √2/2, but it costs little and reduces
    // the error for many arguments.
    let w = y + val_hi;
    val_lo += (y - w) + val_hi;
    val_hi = w;

    val_lo + val_hi
}

/// [`log10`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn log10f(x: f32) -> f32 {
    let mut x = x;
    let mut ix = x.to_bits();
    let mut k = 0;
    if ix < 0x0080_0000 || ix >> 31 != 0 {
        // x < 2^-126.
        if ix << 1 == 0 {
            // log(±0) is -inf. The barrier keeps LLVM from folding the
            // division, and divide-by-zero with it, if it works out that x is
            // zero here.
            return -1.0 / barrierf(x * x);
        }
        if ix >> 31 != 0 {
            // log(-x) is NaN. musl writes x - x, which clippy takes
            // for a mistake; the barrier changes nothing else.
            return (barrierf(x) - x) / 0.0;
        }
        // x is subnormal: scale it up.
        k -= 25;
        x *= hexf32!("0x1p25");
        ix = x.to_bits();
    } else if ix >= 0x7f80_0000 {
        return x;
    } else if ix == 0x3f80_0000 {
        return 0.0;
    }

    // Reduce x into [√2/2, √2].
    ix += 0x3f80_0000 - 0x3f35_04f3;
    k += (ix >> 23) as i32 - 0x7f;
    ix = (ix & 0x007f_ffff) + 0x3f35_04f3;
    let x = f32::from_bits(ix);

    let f = x - 1.0;
    let s = f / (2.0 + f);
    let r = polynomialf(s * s);
    let hfsq = 0.5 * f * f;

    let hi = f - hfsq;
    let hi = f32::from_bits(hi.to_bits() & 0xffff_f000);
    let lo = f - hi - hfsq + s * (hfsq + r);
    let dk = k as f32;
    dk * LOG10_2LO_F + (lo + hi) * IVLN10LO_F + lo * IVLN10HI_F + hi * IVLN10HI_F + dk * LOG10_2HI_F
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn log10_matches_libc_test() {
        let files = [
            "crlibm/log10.h",
            "ucb/log10.h",
            "sanity/log10.h",
            "special/log10.h",
        ];
        mtest::d_d("log10", &files, |x| log10(x), Rules::ULP, &[]);
    }

    #[test]
    fn log10f_matches_libc_test() {
        let files = ["ucb/log10f.h", "sanity/log10f.h", "special/log10f.h"];
        mtest::d_d("log10f", &files, |x| log10f(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (IVLN10HI, "4.34294481878168880939e-01"),
            (IVLN10LO, "2.50829467116452752298e-11"),
            (LOG10_2HI, "3.01029995663611771306e-01"),
            (LOG10_2LO, "3.69423907715893078616e-13"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
        let floats = [
            (IVLN10HI_F, "4.3432617188e-01"),
            (IVLN10LO_F, "-3.1689971365e-05"),
            (LOG10_2HI_F, "3.0102920532e-01"),
            (LOG10_2LO_F, "7.9034151668e-07"),
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
