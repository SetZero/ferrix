//! `powf`.
//!
//! Ported from musl 1.2.5's `powf.c`, `powf_data.c` and `powf_data.h` (MIT;
//! see [`crate::math`] for the notice), with the tables of `exp2f` from
//! [`crate::math::expf`] and of `log2f` from [`crate::math::logf`]. musl took
//! them from ARM's optimized-routines, whose files carry this notice:
//!
//! ```text
//! Copyright (c) 2017-2018, Arm Limited.
//! SPDX-License-Identifier: MIT
//! ```
//!
//! musl's x86-64 build has no `TOINT_INTRINSICS`, so `POWF_SCALE` is 1 and
//! the code kept is the code without them. With a scale of 1, the table in
//! `powf_data.c` is `log2f_data.c`'s digit for digit, so this module reads
//! that one instead of keeping a copy. `WANT_SNAN` is 0, so the tests for
//! signalling NaNs are left out. The worst-case error is 0.82 ulp.

use crate::math::expf::{self, C0, C1, C2, SHIFT_SCALED};
use crate::math::logf::{self, OFF, log2_row};
use crate::math::support::{barrierf, hexf32, hexf64, invalidf, oflowf, uflowf};

// `poly`: log1p(r)/ln2's coefficients of r⁵ to r.
const A0: f64 = hexf64!("0x1.27616c9496e0bp-2");
const A1: f64 = hexf64!("-0x1.71969a075c67ap-2");
const A2: f64 = hexf64!("0x1.ec70a6ca7baddp-2");
const A3: f64 = hexf64!("-0x1.7154748bef6c8p-1");
const A4: f64 = hexf64!("0x1.71547652ab82bp0");

/// Added to k to make the result negative.
const SIGN_BIAS: u32 = 1 << (expf::TABLE_BITS + 11);

/// log2(x), where `ix` is x's bits, normalised in the subnormal range so the
/// exponent is negative.
fn log2_inline(ix: u32) -> f64 {
    // x = 2^k·z, where z is in [OFF, 2·OFF] and exact. z's range is split into
    // N subintervals, and c is near the middle of z's.
    let tmp = ix.wrapping_sub(OFF);
    let i = (tmp >> (23 - logf::TABLE_BITS)) % logf::N;
    let top = tmp & 0xff80_0000;
    let iz = ix.wrapping_sub(top);
    // An arithmetic shift; `POWF_SCALE_BITS` is 0.
    let k = top.cast_signed() >> 23;
    let (invc, logc) = log2_row(i);
    let z = f64::from(f32::from_bits(iz));

    // log2(x) = log1p(z/c - 1)/ln2 + log2(c) + k.
    let r = z * invc - 1.0;
    let y0 = logc + f64::from(k);

    // log1p(r)/ln2, evaluated in pieces for a pipelined processor.
    let r2 = r * r;
    let y = A0 * r + A1;
    let p = A2 * r + A3;
    let r4 = r2 * r2;
    let mut q = A4 * r + y0;
    q += p * r2;
    y * r4 + q
}

/// sign·2^`xd`, where `xd` is in [-1021, 1023] and `sign_bias` is
/// [`SIGN_BIAS`] for a negative sign or 0 for a positive one.
fn exp2_inline(xd: f64, sign_bias: u32) -> f32 {
    // x = k/N + r, with r in [-1/2N, 1/2N].
    let mut kd = xd + SHIFT_SCALED;
    let ki = kd.to_bits();
    // k/N.
    kd -= SHIFT_SCALED;
    let r = xd - kd;

    // exp2(x) = 2^(k/N)·2^r ~= s·(C0·r³ + C1·r² + C2·r + 1).
    let ski = ki.wrapping_add(u64::from(sign_bias));
    let t = expf::table(ki).wrapping_add(ski << (52 - expf::TABLE_BITS));
    let s = f64::from_bits(t);
    let z = C0 * r + C1;
    let r2 = r * r;
    let mut y = C2 * r + 1.0;
    y += z * r2;
    y *= s;
    y as f32
}

/// 0 if `iy`, the bits of a nonzero finite `float`, is not an integer, 1 if
/// it is an odd one, and 2 if an even one.
fn checkint(iy: u32) -> i32 {
    let e = iy >> 23 & 0xff;
    if e < 0x7f {
        return 0;
    }
    if e > 0x7f + 23 {
        return 2;
    }
    let unit = 1u32 << (0x7f + 23 - e);
    if iy & (unit - 1) != 0 {
        return 0;
    }
    if iy & unit != 0 {
        return 1;
    }
    2
}

/// Whether `i` is the bits of a zero, an infinity or a NaN.
const fn zeroinfnan(i: u32) -> bool {
    i.wrapping_mul(2).wrapping_sub(1) >= 2 * 0x7f80_0000 - 1
}

/// `x` raised to the power `y`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn powf(x: f32, y: f32) -> f32 {
    let mut sign_bias = 0;
    let mut ix = x.to_bits();
    let iy = y.to_bits();
    if ix.wrapping_sub(0x0080_0000) >= 0x7f80_0000 - 0x0080_0000 || zeroinfnan(iy) {
        // x < 2^-126, infinite or NaN, or y is 0, infinite or NaN.
        if zeroinfnan(iy) {
            if iy.wrapping_mul(2) == 0 {
                return 1.0;
            }
            if ix == 0x3f80_0000 {
                return 1.0;
            }
            if ix.wrapping_mul(2) > 2 * 0x7f80_0000 || iy.wrapping_mul(2) > 2 * 0x7f80_0000 {
                return x + y;
            }
            if ix.wrapping_mul(2) == 2 * 0x3f80_0000 {
                return 1.0;
            }
            if (ix.wrapping_mul(2) < 2 * 0x3f80_0000) == (iy & 0x8000_0000 == 0) {
                // |x| < 1 and y is +inf, or |x| > 1 and y is -inf.
                return 0.0;
            }
            return y * y;
        }
        if zeroinfnan(ix) {
            let mut x2 = x * x;
            if ix & 0x8000_0000 != 0 && checkint(iy) == 1 {
                x2 = -x2;
            }
            // Without the barrier, some compilers hoist 1/x2 out of the branch
            // and raise divide-by-zero spuriously.
            return if iy & 0x8000_0000 != 0 {
                barrierf(1.0 / x2)
            } else {
                x2
            };
        }
        // x and y are nonzero and finite.
        if ix & 0x8000_0000 != 0 {
            // x < 0.
            let yint = checkint(iy);
            if yint == 0 {
                return invalidf(x);
            }
            if yint == 1 {
                sign_bias = SIGN_BIAS;
            }
            ix &= 0x7fff_ffff;
        }
        if ix < 0x0080_0000 {
            // Normalise subnormal x so its exponent becomes negative.
            ix = (x * hexf32!("0x1p23")).to_bits();
            ix &= 0x7fff_ffff;
            ix = ix.wrapping_sub(23 << 23);
        }
    }
    let logx = log2_inline(ix);
    // Cannot overflow: y is a `float`.
    let ylogx = f64::from(y) * logx;
    if (ylogx.to_bits() >> 47 & 0xffff) >= 126f64.to_bits() >> 47 {
        // |y·log2(x)| >= 126.
        if ylogx > hexf64!("0x1.fffffffd1d571p+6") {
            return oflowf(sign_bias);
        }
        if ylogx <= -150.0 {
            return uflowf(sign_bias);
        }
    }
    exp2_inline(ylogx, sign_bias)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn powf_matches_libc_test() {
        let files = ["ucb/powf.h", "sanity/powf.h", "special/powf.h"];
        // libc-test's `powf.c` tolerates wrong exceptions for a result below
        // the normal range that raised underflow. The one case allowed was
        // run against musl 1.2.5 itself, which gives inf with overflow and
        // inexact too.
        let allow = [mtest::Allow {
            place: "ucb/powf.h:103",
            reason: "powf(FLT_MAX, 1) rounding upward: musl's `double` \
                     approximation of 2^log2(FLT_MAX) lies just above FLT_MAX, \
                     so rounding it to `float` overflows to inf, as in musl",
        }];
        mtest::dd_d(
            "powf",
            &files,
            |x, y| powf(x, y),
            Rules::ULP.underflow(),
            &allow,
        );
    }

    #[test]
    fn integers_are_recognised() {
        assert_eq!(checkint(3f32.to_bits()), 1);
        assert_eq!(checkint(4f32.to_bits()), 2);
        assert_eq!(checkint(0.5f32.to_bits()), 0);
        assert_eq!(checkint(hexf32!("0x1p30").to_bits()), 2);
        assert!(zeroinfnan(0));
        assert!(zeroinfnan((-0.0f32).to_bits()));
        assert!(zeroinfnan(f32::NAN.to_bits()));
        assert!(!zeroinfnan(1));
    }
}
