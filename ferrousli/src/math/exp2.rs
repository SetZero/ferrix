//! `exp2`, 2 raised to a power.
//!
//! Ported from musl 1.2.5's `exp2.c`, and the part of `exp_data.c` and
//! `exp_data.h` that is `exp2`'s own (MIT; see [`crate::math`] for the
//! notice), with the table of 2^(k/128) from [`crate::math::exp`]. musl took
//! them from ARM's optimized-routines, whose files carry this notice:
//!
//! ```text
//! Copyright (c) 2018, Arm Limited.
//! SPDX-License-Identifier: MIT
//! ```
//!
//! musl's x86-64 build has no `__FP_FAST_FMA` and no `TOINT_INTRINSICS`, so
//! the code kept is the code without them.

use crate::math::exp::{N, TABLE_BITS, scale_down, table, top12};
use crate::math::support::{hexf64, oflow, uflow};

/// `exp2_shift`: added before k's bits are read out of kd. 0x1.8p52/N is
/// exact.
const SHIFT: f64 = hexf64!("0x1.8p52") / N as f64;

// `exp2_poly`: the coefficients of 2^r - 1, from r's.
const C1: f64 = hexf64!("0x1.62e42fefa39efp-1");
const C2: f64 = hexf64!("0x1.ebfbdff82c424p-3");
const C3: f64 = hexf64!("0x1.c6b08d70cf4b5p-5");
const C4: f64 = hexf64!("0x1.3b2abd24650ccp-7");
const C5: f64 = hexf64!("0x1.5d7e09b4e3a84p-10");

/// The cases that may overflow or underflow in computing scale·(1 + tmp)
/// without rounding in between. `sbits` holds scale's bits, but its computed
/// exponent may have overflowed into the sign bit. `ki` is the k of the
/// reduction: positive means the result may overflow, negative that it may
/// underflow.
fn specialcase(tmp: f64, sbits: u64, ki: u64) -> f64 {
    if ki & 0x8000_0000 == 0 {
        // k > 0: scale's exponent may have overflowed by 1.
        let scale = f64::from_bits(sbits.wrapping_sub(1 << 52));
        return 2.0 * (scale + scale * tmp);
    }
    // k < 0, as in `exp`.
    scale_down(tmp, sbits)
}

/// 2 raised to the power `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn exp2(x: f64) -> f64 {
    let mut abstop = top12(x) & 0x7ff;
    if abstop.wrapping_sub(top12(hexf64!("0x1p-54")))
        >= top12(512.0).wrapping_sub(top12(hexf64!("0x1p-54")))
    {
        if abstop.wrapping_sub(top12(hexf64!("0x1p-54"))) >= 0x8000_0000 {
            // Tiny x, including 0, without a spurious underflow.
            return 1.0 + x;
        }
        if abstop >= top12(1024.0) {
            if x.to_bits() == f64::NEG_INFINITY.to_bits() {
                return 0.0;
            }
            if abstop >= top12(f64::INFINITY) {
                return 1.0 + x;
            }
            if x.to_bits() >> 63 == 0 {
                return oflow(0);
            } else if x.to_bits() >= (-1075.0f64).to_bits() {
                return uflow(0);
            }
        }
        if x.to_bits().wrapping_mul(2) > 928f64.to_bits().wrapping_mul(2) {
            // Large x is handled by `specialcase` below.
            abstop = 0;
        }
    }

    // exp2(x) = 2^(k/N)·2^r, with 2^r in [2^(-1/2N), 2^(1/2N)] and
    // x = k/N + r.
    let mut kd = x + SHIFT;
    let ki = kd.to_bits();
    // k/N for an integer k.
    kd -= SHIFT;
    let r = x - kd;
    // 2^(k/N) ~= scale·(1 + tail).
    let index = 2 * (ki % N);
    let top = ki << (52 - TABLE_BITS);
    let tail = f64::from_bits(table(index));
    // Only a valid scale when -1023·N < k < 1024·N.
    let sbits = table(index + 1).wrapping_add(top);
    // exp2(x) ~= scale + scale·(tail + 2^r - 1). Without fma the worst-case
    // error is 0.5/N ulp larger.
    let r2 = r * r;
    let tmp = tail + r * C1 + r2 * (C2 + r * C3) + r2 * r2 * (C4 + r * C5);
    if abstop == 0 {
        return specialcase(tmp, sbits, ki);
    }
    let scale = f64::from_bits(sbits);
    // tmp is 0 or |tmp| > 2^-65, and scale > 2^-928, so there is no spurious
    // underflow here.
    scale + scale * tmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn exp2_matches_libc_test() {
        let files = ["sanity/exp2.h", "special/exp2.h"];
        // libc-test's `exp2.c` tolerates wrong exceptions for a result below
        // the normal range that raised underflow.
        mtest::d_d("exp2", &files, |x| exp2(x), Rules::ULP.underflow(), &[]);
    }

    #[test]
    fn the_shift_is_exact() {
        assert_eq!(SHIFT, hexf64!("0x1.8p45"));
    }
}
