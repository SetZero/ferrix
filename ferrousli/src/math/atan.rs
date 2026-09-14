//! `atan2`, and the `atan` it is built on.
//!
//! Ported from musl 1.2.5's `atan.c` and `atan2.c` (MIT; see [`crate::math`]
//! for the notice). musl took them from FreeBSD's msun, whose files carry this
//! notice:
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
//! musl writes these constants in decimal, with their bits in a comment. They
//! are written here as those bits.
//!
//! `atan` is not exported yet: busybox needs only `atan2`, and the rest of the
//! inverse trigonometric functions come with the rest of `math.h`.

use crate::math::support::{barrier, force_evalf, hexf64, high_word, is_nan, low_word};

/// atan(0.5), as a high and a low part.
const ATAN_HALF: (f64, f64) = (
    f64::from_bits(0x3fdd_ac67_0561_bb4f),
    f64::from_bits(0x3c7a_2b7f_222f_65e2),
);
/// atan(1).
const ATAN_ONE: (f64, f64) = (
    f64::from_bits(0x3fe9_21fb_5444_2d18),
    f64::from_bits(0x3c81_a626_3314_5c07),
);
/// atan(1.5).
const ATAN_THREE_HALVES: (f64, f64) = (
    f64::from_bits(0x3fef_730b_d281_f69b),
    f64::from_bits(0x3c70_0788_7af0_cbbd),
);
/// atan(∞).
const ATAN_INFINITY: (f64, f64) = (
    f64::from_bits(0x3ff9_21fb_5444_2d18),
    f64::from_bits(0x3c91_a626_3314_5c07),
);

// musl's aT[]: the coefficients of atan's odd polynomial.
const AT0: f64 = f64::from_bits(0x3fd5_5555_5555_550d);
const AT1: f64 = f64::from_bits(0xbfc9_9999_9998_ebc4);
const AT2: f64 = f64::from_bits(0x3fc2_4924_9200_83ff);
const AT3: f64 = f64::from_bits(0xbfbc_71c6_fe23_1671);
const AT4: f64 = f64::from_bits(0x3fb7_45cd_c54c_206e);
const AT5: f64 = f64::from_bits(0xbfb3_b0f2_af74_9a6d);
const AT6: f64 = f64::from_bits(0x3fb1_0d66_a0d0_3d51);
const AT7: f64 = f64::from_bits(0xbfad_de2d_52de_fd9a);
const AT8: f64 = f64::from_bits(0x3fa9_7b4b_2476_0deb);
const AT9: f64 = f64::from_bits(0xbfa2_b444_2c6a_6c2f);
const AT10: f64 = f64::from_bits(0x3f90_ad3a_e322_da11);

/// π.
const PI: f64 = f64::from_bits(0x4009_21fb_5444_2d18);
/// The part of π that [`PI`] misses.
const PI_LO: f64 = f64::from_bits(0x3ca1_a626_3314_5c07);

/// `x` without its sign, as musl's `fabs`.
const fn abs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & (u64::MAX >> 1))
}

/// The arctangent of `x`, in radians, in [-π/2, π/2].
pub(crate) fn atan(x: f64) -> f64 {
    let high = high_word(x);
    let negative = high >> 31 != 0;
    let ix = high & 0x7fff_ffff;

    // |x| >= 2^66.
    if ix >= 0x4410_0000 {
        if is_nan(x) {
            return x;
        }
        // Inexact, so musl adds at run time.
        let z = barrier(ATAN_INFINITY.0) + hexf64!("0x1p-120");
        return if negative { -z } else { z };
    }

    // Reduce the argument, choosing the constant to add back.
    let (x, part) = if ix < 0x3fdc_0000 {
        // |x| < 0.4375.
        if ix < 0x3e40_0000 {
            // |x| < 2^-27: raise underflow for subnormal x.
            if ix < 0x0010_0000 {
                force_evalf(x as f32);
            }
            return x;
        }
        (x, None)
    } else {
        let x = abs(x);
        if ix < 0x3ff3_0000 {
            if ix < 0x3fe6_0000 {
                // 7/16 <= |x| < 11/16.
                ((2.0 * x - 1.0) / (2.0 + x), Some(ATAN_HALF))
            } else {
                // 11/16 <= |x| < 19/16.
                ((x - 1.0) / (x + 1.0), Some(ATAN_ONE))
            }
        } else if ix < 0x4003_8000 {
            // |x| < 2.4375.
            ((x - 1.5) / (1.0 + 1.5 * x), Some(ATAN_THREE_HALVES))
        } else {
            // 2.4375 <= |x| < 2^66.
            (-1.0 / x, Some(ATAN_INFINITY))
        }
    };

    // The sum of aT[i]·z^(i+1), split into odd and even polynomials.
    let z = x * x;
    let w = z * z;
    let s1 = z * (AT0 + w * (AT2 + w * (AT4 + w * (AT6 + w * (AT8 + w * AT10)))));
    let s2 = w * (AT1 + w * (AT3 + w * (AT5 + w * (AT7 + w * AT9))));
    match part {
        None => x - x * (s1 + s2),
        Some((hi, lo)) => {
            let z = hi - (x * (s1 + s2) - lo - x);
            if negative { -z } else { z }
        }
    }
}

/// The angle from the positive x axis to the point (`x`, `y`), in radians, in
/// [-π, π].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn atan2(y: f64, x: f64) -> f64 {
    if is_nan(x) || is_nan(y) {
        return x + y;
    }
    let (ix, lx) = (high_word(x), low_word(x));
    let (iy, ly) = (high_word(y), low_word(y));
    // x = 1.0.
    if (ix.wrapping_sub(0x3ff0_0000) | lx) == 0 {
        return atan(y);
    }
    // 2·sign(x) + sign(y).
    let m = ((iy >> 31) & 1) | ((ix >> 30) & 2);
    let ix = ix & 0x7fff_ffff;
    let iy = iy & 0x7fff_ffff;

    // y = 0.
    if (iy | ly) == 0 {
        return match m {
            0 | 1 => y,
            2 => PI,
            _ => -PI,
        };
    }
    // x = 0.
    if (ix | lx) == 0 {
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 };
    }
    // x is infinite.
    if ix == 0x7ff0_0000 {
        return if iy == 0x7ff0_0000 {
            match m {
                0 => PI / 4.0,
                1 => -PI / 4.0,
                2 => 3.0 * PI / 4.0,
                _ => -3.0 * PI / 4.0,
            }
        } else {
            match m {
                0 => 0.0,
                1 => -0.0,
                2 => PI,
                _ => -PI,
            }
        };
    }
    // |y/x| > 2^64.
    if ix.wrapping_add(64 << 20) < iy || iy == 0x7ff0_0000 {
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 };
    }

    // z = atan(|y/x|), without a spurious underflow.
    let z = if m & 2 != 0 && iy.wrapping_add(64 << 20) < ix {
        // |y/x| < 2^-64 and x < 0.
        0.0
    } else {
        atan(abs(y / x))
    };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn atan_matches_libc_test() {
        let files = ["crlibm/atan.h", "ucb/atan.h", "sanity/atan.h", "special/atan.h"];
        mtest::d_d("atan", &files, atan, Rules::ULP, &[]);
    }

    #[test]
    fn atan2_matches_libc_test() {
        let files = ["ucb/atan2.h", "sanity/atan2.h", "special/atan2.h"];
        mtest::dd_d("atan2", &files, |y, x| atan2(y, x), Rules::ULP, &[]);
    }

    #[test]
    fn constant_multiples_of_pi_are_exact() {
        // musl leaves an inexact constant expression to run time. These are
        // exact, so folding them here gives the same bits in every mode.
        assert_eq!((3.0 * PI).to_bits(), 0x4022_d97c_7f33_21d2);
        assert_eq!((3.0 * PI / 4.0).to_bits(), 0x4002_d97c_7f33_21d2);
    }
}
