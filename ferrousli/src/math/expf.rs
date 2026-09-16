//! `expf` and `exp2f`, and the table of 2^(k/32) that `powf` shares.
//!
//! Ported from musl 1.2.5's `expf.c`, `exp2f.c`, `exp2f_data.c` and
//! `exp2f_data.h` (MIT; see [`crate::math`] for the notice). musl took them
//! from ARM's optimized-routines, whose files carry this notice:
//!
//! ```text
//! Copyright (c) 2017-2018, Arm Limited.
//! SPDX-License-Identifier: MIT
//! ```
//!
//! The table was converted from `exp2f_data.c` by a script, digit for digit.
//! musl's x86-64 build has no `TOINT_INTRINSICS`, so the code kept is the code
//! without it. The arithmetic is in `double`, and only the result is rounded
//! to `float`.

use crate::math::support::{hexf32, hexf64, oflowf, uflowf};

/// `EXP2F_TABLE_BITS`: the table covers 2^(k/N) for N = 2^5.
pub(crate) const TABLE_BITS: u32 = 5;
/// `N`, the number of entries.
pub(crate) const N: u64 = 1 << TABLE_BITS;

/// `shift_scaled`: added before k's bits are read out of kd, in `exp2f`.
/// 0x1.8p52/N is exact.
pub(crate) const SHIFT_SCALED: f64 = hexf64!("0x1.8p52") / N as f64;
/// `shift`: the same, in `expf`.
const SHIFT: f64 = hexf64!("0x1.8p52");
/// `invln2_scaled`: N/ln2, exact as a product.
const INV_LN2_SCALED: f64 = hexf64!("0x1.71547652b82fep+0") * N as f64;

/// `poly[0]`: the coefficients of 2^r ~= C0·r³ + C1·r² + C2·r + 1.
pub(crate) const C0: f64 = hexf64!("0x1.c6af84b912394p-5");
/// `poly[1]`.
pub(crate) const C1: f64 = hexf64!("0x1.ebfce50fac4f3p-3");
/// `poly[2]`.
pub(crate) const C2: f64 = hexf64!("0x1.62e42ff0c52d6p-1");

// `poly_scaled`: the same, for r scaled by N. Dividing by a power of two is
// exact here.
const C0_SCALED: f64 = C0 / N as f64 / N as f64 / N as f64;
const C1_SCALED: f64 = C1 / N as f64 / N as f64;
const C2_SCALED: f64 = C2 / N as f64;

/// `tab`: the bits of 2^(i/N), less (i << 52)/N.
static TABLE: [u64; N as usize] = [
    0x3ff0000000000000,
    0x3fefd9b0d3158574,
    0x3fefb5586cf9890f,
    0x3fef9301d0125b51,
    0x3fef72b83c7d517b,
    0x3fef54873168b9aa,
    0x3fef387a6e756238,
    0x3fef1e9df51fdee1,
    0x3fef06fe0a31b715,
    0x3feef1a7373aa9cb,
    0x3feedea64c123422,
    0x3feece086061892d,
    0x3feebfdad5362a27,
    0x3feeb42b569d4f82,
    0x3feeab07dd485429,
    0x3feea47eb03a5585,
    0x3feea09e667f3bcd,
    0x3fee9f75e8ec5f74,
    0x3feea11473eb0187,
    0x3feea589994cce13,
    0x3feeace5422aa0db,
    0x3feeb737b0cdc5e5,
    0x3feec49182a3f090,
    0x3feed503b23e255d,
    0x3feee89f995ad3ad,
    0x3feeff76f2fb5e47,
    0x3fef199bdd85529c,
    0x3fef3720dcef9069,
    0x3fef5818dcfba487,
    0x3fef7c97337b9b5f,
    0x3fefa4afa2a490da,
    0x3fefd0765b6e4540,
];

/// Entry `index` of [`TABLE`], counting modulo N.
pub(crate) fn table(index: u64) -> u64 {
    usize::try_from(index % N)
        .ok()
        .and_then(|index| TABLE.get(index))
        .copied()
        .unwrap_or(0)
}

/// The top 12 bits of a `float`: its sign, exponent and top three fraction
/// bits.
pub(crate) const fn top12f(x: f32) -> u32 {
    x.to_bits() >> 20
}

/// e raised to the power `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn expf(x: f32) -> f32 {
    let xd = f64::from(x);
    let abstop = top12f(x) & 0x7ff;
    if abstop >= top12f(88.0) {
        // |x| >= 88, or x is NaN.
        if x.to_bits() == f32::NEG_INFINITY.to_bits() {
            return 0.0;
        }
        if abstop >= top12f(f32::INFINITY) {
            return x + x;
        }
        if x > hexf32!("0x1.62e42ep6") {
            // x > log(0x1p128) ~= 88.72.
            return oflowf(0);
        }
        if x < hexf32!("-0x1.9fe368p6") {
            // x < log(0x1p-150) ~= -103.97.
            return uflowf(0);
        }
    }

    // x·N/ln2 = k + r, with r in [-1/2, 1/2] and k an integer.
    let z = INV_LN2_SCALED * xd;
    // k is in [-150·N, 128·N].
    let mut kd = z + SHIFT;
    let ki = kd.to_bits();
    kd -= SHIFT;
    let r = z - kd;

    // exp(x) = 2^(k/N)·2^(r/N) ~= s·(C0·r³ + C1·r² + C2·r + 1).
    let t = table(ki).wrapping_add(ki << (52 - TABLE_BITS));
    let s = f64::from_bits(t);
    let z = C0_SCALED * r + C1_SCALED;
    let r2 = r * r;
    let mut y = C2_SCALED * r + 1.0;
    y += z * r2;
    y *= s;
    y as f32
}

/// 2 raised to the power `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn exp2f(x: f32) -> f32 {
    let xd = f64::from(x);
    let abstop = top12f(x) & 0x7ff;
    if abstop >= top12f(128.0) {
        // |x| >= 128, or x is NaN.
        if x.to_bits() == f32::NEG_INFINITY.to_bits() {
            return 0.0;
        }
        if abstop >= top12f(f32::INFINITY) {
            return x + x;
        }
        if x > 0.0 {
            return oflowf(0);
        }
        if x <= -150.0 {
            return uflowf(0);
        }
    }

    // x = k/N + r, with r in [-1/2N, 1/2N] and k an integer.
    let mut kd = xd + SHIFT_SCALED;
    let ki = kd.to_bits();
    // k/N.
    kd -= SHIFT_SCALED;
    let r = xd - kd;

    // exp2(x) = 2^(k/N)·2^r ~= s·(C0·r³ + C1·r² + C2·r + 1).
    let t = table(ki).wrapping_add(ki << (52 - TABLE_BITS));
    let s = f64::from_bits(t);
    let z = C0 * r + C1;
    let r2 = r * r;
    let mut y = C2 * r + 1.0;
    y += z * r2;
    y *= s;
    y as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn expf_matches_libc_test() {
        let files = ["ucb/expf.h", "sanity/expf.h", "special/expf.h"];
        mtest::d_d("expf", &files, |x| expf(x), Rules::ULP, &[]);
    }

    #[test]
    fn exp2f_matches_libc_test() {
        let files = ["sanity/exp2f.h", "special/exp2f.h"];
        mtest::d_d("exp2f", &files, |x| exp2f(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_scaled_constants_are_exact() {
        assert_eq!(SHIFT_SCALED, hexf64!("0x1.8p47"));
        assert_eq!(INV_LN2_SCALED, hexf64!("0x1.71547652b82fep+5"));
        assert_eq!(C0_SCALED, hexf64!("0x1.c6af84b912394p-20"));
        assert_eq!(C1_SCALED, hexf64!("0x1.ebfce50fac4f3p-13"));
        assert_eq!(C2_SCALED, hexf64!("0x1.62e42ff0c52d6p-6"));
    }

    #[test]
    fn the_table_starts_and_ends_as_musls() {
        assert_eq!(table(0), 0x3ff0_0000_0000_0000);
        assert_eq!(table(N - 1), 0x3fef_d076_5b6e_4540);
        assert_eq!(table(N), table(0));
    }
}
