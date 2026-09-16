//! `sinf`, `cosf` and `tanf`, the kernels that evaluate them within π/4 of
//! zero, and the reduction of a `float` by multiples of π/2 that they share.
//!
//! Ported from musl 1.2.5's `sinf.c`, `cosf.c`, `tanf.c`, `__sindf.c`,
//! `__cosdf.c`, `__tandf.c` and `__rem_pio2f.c` (MIT; see [`crate::math`] for
//! the notice). musl took them from FreeBSD's msun, where Ian Lance Taylor,
//! Cygnus Support, converted them to `float` and Bruce D. Evans optimized and
//! debugged them. The msun files carry this notice:
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
//! except `k_tanf.c`, which carries this one:
//!
//! ```text
//! Copyright 2004 Sun Microsystems, Inc.  All Rights Reserved.
//!
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ```
//!
//! # Method
//!
//! The arithmetic is in `double`, and only the kernels' results are rounded
//! to `float`. An argument within 9π/4 of zero is reduced by the nearest of
//! the first four multiples of π/2, rounded to `double`; a larger one by
//! n·π/2 with 25 + 53 bits of π/2, or, from 2^28·π/2 up, by
//! [`crate::math::trig`]'s `__rem_pio2_large` in single precision.

use crate::math::support::{barrier, barrierf, force_evalf, hexf32, hexf64};
use crate::math::trig::rem_pio2_large;

// __sindf.c: sin(x)/x as a polynomial in x², within 2^-37.5.
const S1: f64 = hexf64!("-0x15555554cbac77.0p-55");
const S2: f64 = hexf64!("0x111110896efbb2.0p-59");
const S3: f64 = hexf64!("-0x1a00f9e2cae774.0p-65");
const S4: f64 = hexf64!("0x16cd878c3b46a7.0p-71");

// __cosdf.c: cos(x) as a polynomial in x², within 2^-34.1.
const C0: f64 = hexf64!("-0x1ffffffd0c5e81.0p-54");
const C1: f64 = hexf64!("0x155553e1053a42.0p-57");
const C2: f64 = hexf64!("-0x16c087e80f1e27.0p-62");
const C3: f64 = hexf64!("0x199342e0ee5069.0p-68");

// __tandf.c's T[]: tan(x)/x as a polynomial in x², within 2^-25.5.
const T0: f64 = hexf64!("0x15554d3418c99f.0p-54");
const T1: f64 = hexf64!("0x1112fd38999f72.0p-55");
const T2: f64 = hexf64!("0x1b54c91d865afe.0p-57");
const T3: f64 = hexf64!("0x191df3908c33ce.0p-58");
const T4: f64 = hexf64!("0x185dadfcecf44e.0p-61");
const T5: f64 = hexf64!("0x1362b9bf971bcd.0p-59");

// sinf.c, cosf.c and tanf.c: `1*M_PI_2` to `4*M_PI_2`, small multiples of π/2
// rounded to double precision, which musl's comments give the bits of.
const PIO2_1X: f64 = f64::from_bits(0x3ff9_21fb_5444_2d18);
const PIO2_2X: f64 = f64::from_bits(0x4009_21fb_5444_2d18);
const PIO2_3X: f64 = f64::from_bits(0x4012_d97c_7f33_21d2);
const PIO2_4X: f64 = f64::from_bits(0x4019_21fb_5444_2d18);

// __rem_pio2f.c.
/// `1.5/DBL_EPSILON`: adding and subtracting it rounds to an integer.
const TOINT: f64 = hexf64!("0x1.8p52");
/// π/4, rounded up to `float`.
const PIO4: f64 = hexf64!("0x1.921fb6p-1");
/// 53 bits of 2/π.
const INVPIO2: f64 = f64::from_bits(0x3fe4_5f30_6dc9_c883);
/// The first 25 bits of π/2.
const PIO2_1: f64 = f64::from_bits(0x3ff9_21fb_5000_0000);
/// π/2 - `PIO2_1`.
const PIO2_1T: f64 = f64::from_bits(0x3e51_10b4_611a_6263);

/// The sine of `x`, in radians.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sinf(x: f32) -> f32 {
    let bits = x.to_bits();
    let negative = bits >> 31 != 0;
    let ix = bits & 0x7fff_ffff;
    let xd = f64::from(x);

    // |x| ~<= π/4.
    if ix <= 0x3f49_0fda {
        if ix < 0x3980_0000 {
            // |x| < 2^-12: raise inexact if x is not zero, and underflow if it
            // is subnormal.
            if ix < 0x0080_0000 {
                force_evalf(x / hexf32!("0x1p120"));
            } else {
                force_evalf(x + hexf32!("0x1p120"));
            }
            return x;
        }
        return kernel_sinf(xd);
    }
    // |x| ~<= 5π/4.
    if ix <= 0x407b_53d1 {
        // |x| ~<= 3π/4.
        if ix <= 0x4016_cbe3 {
            return if negative {
                -kernel_cosf(xd + PIO2_1X)
            } else {
                kernel_cosf(xd - PIO2_1X)
            };
        }
        return kernel_sinf(if negative {
            -(xd + PIO2_2X)
        } else {
            -(xd - PIO2_2X)
        });
    }
    // |x| ~<= 9π/4.
    if ix <= 0x40e2_31d5 {
        // |x| ~<= 7π/4.
        if ix <= 0x40af_eddf {
            return if negative {
                kernel_cosf(xd + PIO2_3X)
            } else {
                -kernel_cosf(xd - PIO2_3X)
            };
        }
        return kernel_sinf(if negative { xd + PIO2_4X } else { xd - PIO2_4X });
    }

    // sin(Inf or NaN) is NaN.
    if ix >= 0x7f80_0000 {
        return barrierf(x) - x;
    }

    let (n, y) = rem_pio2f(x);
    match n & 3 {
        0 => kernel_sinf(y),
        1 => kernel_cosf(y),
        2 => kernel_sinf(-y),
        _ => -kernel_cosf(y),
    }
}

/// The cosine of `x`, in radians.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cosf(x: f32) -> f32 {
    let bits = x.to_bits();
    let negative = bits >> 31 != 0;
    let ix = bits & 0x7fff_ffff;
    let xd = f64::from(x);

    // |x| ~<= π/4.
    if ix <= 0x3f49_0fda {
        if ix < 0x3980_0000 {
            // |x| < 2^-12: raise inexact if x is not zero.
            force_evalf(x + hexf32!("0x1p120"));
            return 1.0;
        }
        return kernel_cosf(xd);
    }
    // |x| ~<= 5π/4.
    if ix <= 0x407b_53d1 {
        // |x| ~> 3π/4.
        if ix > 0x4016_cbe3 {
            return -kernel_cosf(if negative { xd + PIO2_2X } else { xd - PIO2_2X });
        }
        return if negative {
            kernel_sinf(xd + PIO2_1X)
        } else {
            kernel_sinf(PIO2_1X - xd)
        };
    }
    // |x| ~<= 9π/4.
    if ix <= 0x40e2_31d5 {
        // |x| ~> 7π/4.
        if ix > 0x40af_eddf {
            return kernel_cosf(if negative { xd + PIO2_4X } else { xd - PIO2_4X });
        }
        return if negative {
            kernel_sinf(f64::from(-x) - PIO2_3X)
        } else {
            kernel_sinf(xd - PIO2_3X)
        };
    }

    // cos(Inf or NaN) is NaN.
    if ix >= 0x7f80_0000 {
        return barrierf(x) - x;
    }

    let (n, y) = rem_pio2f(x);
    match n & 3 {
        0 => kernel_cosf(y),
        1 => kernel_sinf(-y),
        2 => -kernel_cosf(y),
        _ => kernel_sinf(y),
    }
}

/// The tangent of `x`, in radians.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tanf(x: f32) -> f32 {
    let bits = x.to_bits();
    let negative = bits >> 31 != 0;
    let ix = bits & 0x7fff_ffff;
    let xd = f64::from(x);

    // |x| ~<= π/4.
    if ix <= 0x3f49_0fda {
        if ix < 0x3980_0000 {
            // |x| < 2^-12: raise inexact if x is not zero, and underflow if it
            // is subnormal.
            if ix < 0x0080_0000 {
                force_evalf(x / hexf32!("0x1p120"));
            } else {
                force_evalf(x + hexf32!("0x1p120"));
            }
            return x;
        }
        return kernel_tanf(xd, false);
    }
    // |x| ~<= 5π/4.
    if ix <= 0x407b_53d1 {
        // |x| ~<= 3π/4.
        if ix <= 0x4016_cbe3 {
            return kernel_tanf(if negative { xd + PIO2_1X } else { xd - PIO2_1X }, true);
        }
        return kernel_tanf(if negative { xd + PIO2_2X } else { xd - PIO2_2X }, false);
    }
    // |x| ~<= 9π/4.
    if ix <= 0x40e2_31d5 {
        // |x| ~<= 7π/4.
        if ix <= 0x40af_eddf {
            return kernel_tanf(if negative { xd + PIO2_3X } else { xd - PIO2_3X }, true);
        }
        return kernel_tanf(if negative { xd + PIO2_4X } else { xd - PIO2_4X }, false);
    }

    // tan(Inf or NaN) is NaN.
    if ix >= 0x7f80_0000 {
        return barrierf(x) - x;
    }

    let (n, y) = rem_pio2f(x);
    kernel_tanf(y, n & 1 != 0)
}

/// musl's `__sindf`: sin(x) for |x| ~<= π/4, rounded to `float`. The caller
/// returns sin(-0) itself.
fn kernel_sinf(x: f64) -> f32 {
    let z = x * x;
    let w = z * z;
    let r = S3 + z * S4;
    let s = z * x;
    ((x + s * (S1 + z * S2)) + s * w * r) as f32
}

/// musl's `__cosdf`: cos(x) for |x| ~<= π/4, rounded to `float`.
fn kernel_cosf(x: f64) -> f32 {
    let z = x * x;
    let w = z * z;
    let r = C2 + z * C3;
    (((1.0 + z * C0) + w * C1) + (w * z) * r) as f32
}

/// musl's `__tandf`: tan(x) for |x| ~<= π/4, or -1/tan(x) if `odd` is set,
/// rounded to `float`. The caller returns tan(-0) itself.
fn kernel_tanf(x: f64, odd: bool) -> f32 {
    // The polynomial in small independent terms, added from the lowest
    // degree up, as musl splits it for parallel evaluation.
    let z = x * x;
    let r = T4 + z * T5;
    let t = T2 + z * T3;
    let w = z * z;
    let s = z * x;
    let u = T0 + z * T1;
    let r = (x + s * u) + (s * w) * (t + w * r);
    if odd { (-1.0 / r) as f32 } else { r as f32 }
}

/// musl's `__rem_pio2f`: `x` less the nearest multiple n·π/2, in a `double`,
/// and the n, whose last two bits are what the callers need. Returns
/// `(n, remainder)`. The callers handle |x| ~<= 9π/4 themselves.
fn rem_pio2f(x: f32) -> (i32, f64) {
    let bits = x.to_bits();
    let ix = bits & 0x7fff_ffff;

    // |x| ~< 2^28·π/2: 25 + 53 bits of π/2 are enough.
    if ix < 0x4dc9_0fdb {
        let xd = f64::from(x);
        let mut fnv = xd * INVPIO2 + TOINT - TOINT;
        let mut n = fnv as i32;
        // fn·pio2_1 is exact, but fn·pio2_1t is not, and LLVM would rewrite
        // `a - fn * PIO2_1T` as `a + fn * -PIO2_1T`, which rounds the product
        // the other way when rounding up or down. See `crate::math`.
        let mut y = xd - fnv * PIO2_1 - barrier(fnv * PIO2_1T);
        // This matters with directed rounding.
        if y < -PIO4 {
            n -= 1;
            fnv -= 1.0;
            y = xd - fnv * PIO2_1 - barrier(fnv * PIO2_1T);
        } else if y > PIO4 {
            n += 1;
            fnv += 1.0;
            y = xd - fnv * PIO2_1 - barrier(fnv * PIO2_1T);
        }
        return (n, y);
    }

    // x is infinite or NaN.
    if ix >= 0x7f80_0000 {
        return (0, f64::from(barrierf(x) - x));
    }

    // Scale |x| into [2^23, 2^24 - 1]: e0 = ilogb(|x|) - 23, which is
    // positive.
    let e0 = (ix >> 23) as i32 - (0x7f + 23);
    let tx = f64::from(f32::from_bits(ix - (e0.unsigned_abs() << 23)));
    let (n, [y, _]) = rem_pio2_large(&[tx, 0.0, 0.0], 1, e0, 0);
    if bits >> 31 != 0 { (-n, -y) } else { (n, y) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn sinf_matches_libc_test() {
        let files = ["ucb/sinf.h", "sanity/sinf.h", "special/sinf.h"];
        mtest::d_d("sinf", &files, |x| sinf(x), Rules::ULP, &[]);
    }

    #[test]
    fn cosf_matches_libc_test() {
        let files = ["ucb/cosf.h", "sanity/cosf.h", "special/cosf.h"];
        mtest::d_d("cosf", &files, |x| cosf(x), Rules::ULP, &[]);
    }

    #[test]
    fn tanf_matches_libc_test() {
        let files = ["ucb/tanf.h", "sanity/tanf.h", "special/tanf.h"];
        mtest::d_d("tanf", &files, |x| tanf(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls() {
        // musl initialises these from `M_PI_2` and gives their bits.
        let pio2: f64 = "1.57079632679489661923".parse().unwrap_or_default();
        assert_eq!(PIO2_1X, pio2);
        assert_eq!(PIO2_2X, 2.0 * pio2);
        assert_eq!(PIO2_3X, 3.0 * pio2);
        assert_eq!(PIO2_4X, 4.0 * pio2);
        let doubles = [
            (INVPIO2, "6.36619772367581382433e-01"),
            (PIO2_1, "1.57079631090164184570e+00"),
            (PIO2_1T, "1.58932547735281966916e-08"),
            (S1, "-0.166666666416265235595"),
            (C0, "-0.499999997251031003120"),
            (T0, "0.333331395030791399758"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
    }

    #[test]
    fn large_arguments_reduce_as_musl_does() {
        // 2^28·π/2 and up go through `__rem_pio2_large`; sin²+cos² stays 1.
        for bits in [0x4dc9_0fdb_u32, 0x5000_0000, 0x7f7f_ffff, 0xdead_beef] {
            let x = f32::from_bits(bits);
            let (s, c) = (f64::from(sinf(x)), f64::from(cosf(x)));
            assert!((s * s + c * c - 1.0).abs() < 1e-6, "{x:e}");
        }
    }
}
