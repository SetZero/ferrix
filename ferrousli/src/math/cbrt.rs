//! `cbrt` and `cbrtf`: the cube root.
//!
//! Ported from musl 1.2.5's `cbrt.c` and `cbrtf.c` (MIT; see [`crate::math`]
//! for the notice), which come from FreeBSD's msun and carry its notice:
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
//! `cbrt` was optimized by Bruce D. Evans; `cbrtf` converted to `float` by
//! Ian Lance Taylor, Cygnus Support, and debugged and optimized by Bruce D.
//! Evans.
//!
//! Until now compiler_builtins answered these for Rust's own use, and C
//! programs linked against the shared library found neither.
//!
//! # Method
//!
//! A rough root to 5 bits from the exponent divided by three, a polynomial
//! step to 23 bits, and one Newton step: to 53 bits in `double` with an
//! error under 0.667 ulps, and for `float` two Newton steps in `double`,
//! rounded once.

use crate::math::support::{hexf32, hexf64};

/// `(1023 - 1023/3 - 0.03306235651) * 2^20`.
const B1: u32 = 715_094_163;
/// `(1023 - 1023/3 - 54/3 - 0.03306235651) * 2^20`, for subnormals.
const B2: u32 = 696_219_795;

/// |1/cbrt(x) - p(x)| < 2^-23.5.
const P0: f64 = f64::from_bits(0x3ffe_03e6_0f61_e692);
/// See [`P0`].
const P1: f64 = f64::from_bits(0xbffe_28e0_92f0_2420);
/// See [`P0`].
const P2: f64 = f64::from_bits(0x3ff9_f160_4a49_d6c2);
/// See [`P0`].
const P3: f64 = f64::from_bits(0xbfe8_44cb_bee7_51d9);
/// See [`P0`].
const P4: f64 = f64::from_bits(0x3fc2_b000_d4e4_edd7);

/// The real cube root of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cbrt(x: f64) -> f64 {
    let mut ui = x.to_bits();
    let mut hx = (ui >> 32) as u32 & 0x7fff_ffff;
    if hx >= 0x7ff0_0000 {
        // cbrt(NaN, inf) is itself.
        return x + x;
    }
    if hx < 0x0010_0000 {
        // Zero or subnormal.
        ui = (x * hexf64!("0x1p54")).to_bits();
        hx = (ui >> 32) as u32 & 0x7fff_ffff;
        if hx == 0 {
            return x;
        }
        hx = hx / 3 + B2;
    } else {
        hx = hx / 3 + B1;
    }
    ui &= 1 << 63;
    ui |= u64::from(hx) << 32;
    let mut t = f64::from_bits(ui);

    // New cbrt to 23 bits: t * P(t^3 / x).
    let r = (t * t) * (t / x);
    t *= (P0 + r * (P1 + r * P2)) + ((r * r) * r) * (P3 + r * P4);

    // Round t away from zero to 23 bits.
    t = f64::from_bits((t.to_bits().wrapping_add(0x8000_0000)) & 0xffff_ffff_c000_0000);

    // One Newton step to 53 bits.
    let s = t * t;
    let mut r = x / s;
    let w = t + t;
    r = (r - t) / (w + r);
    t + t * r
}

/// `(127 - 127/3 - 0.03306235651) * 2^23`.
const B1F: u32 = 709_958_130;
/// `(127 - 127/3 - 24/3 - 0.03306235651) * 2^23`, for subnormals.
const B2F: u32 = 642_849_266;

/// [`cbrt`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cbrtf(x: f32) -> f32 {
    let mut ui = x.to_bits();
    let mut hx = ui & 0x7fff_ffff;
    if hx >= 0x7f80_0000 {
        return x + x;
    }
    if hx < 0x0080_0000 {
        if hx == 0 {
            return x;
        }
        ui = (x * hexf32!("0x1p24")).to_bits();
        hx = ui & 0x7fff_ffff;
        hx = hx / 3 + B2F;
    } else {
        hx = hx / 3 + B1F;
    }
    ui &= 0x8000_0000;
    ui |= hx;

    // Two Newton steps in `double`, solving t*t - x/t == 0.
    let xd = f64::from(x);
    let mut t = f64::from(f32::from_bits(ui));
    let mut r = t * t * t;
    t = t * (xd + xd + r) / (xd + r + r);
    r = t * t * t;
    t = t * (xd + xd + r) / (xd + r + r);
    t as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn cbrt_matches_libc_test() {
        let files = ["sanity/cbrt.h", "special/cbrt.h"];
        mtest::d_d("cbrt", &files, |x| cbrt(x), Rules::ULP.directed(), &[]);
        let files = ["sanity/cbrtf.h", "special/cbrtf.h"];
        mtest::d_d("cbrtf", &files, |x| cbrtf(x), Rules::ULP.directed(), &[]);
    }

    #[test]
    fn exact_cubes_are_exact() {
        assert_eq!(cbrt(27.0), 3.0);
        assert_eq!(cbrt(-0.125), -0.5);
        assert_eq!(cbrtf(64.0), 4.0);
        assert_eq!(cbrt(-0.0).to_bits(), (-0.0f64).to_bits());
        assert!(cbrtf(f32::NAN).is_nan());
    }
}
