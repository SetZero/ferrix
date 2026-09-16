//! `hypot` and `hypotf`: √(x² + y²), without undue overflow or underflow.
//!
//! Ported from musl 1.2.5's `hypot.c` and `hypotf.c` (MIT; see
//! [`crate::math`] for the notice). musl wrote these itself; they carry no
//! other notice.
//!
//! # Method
//!
//! With |x| >= |y|: an infinity wins over a NaN, and where y is 0 or 2^64
//! (2^25 for `float`) times smaller than x, the result is x + y. Otherwise
//! both are scaled by 2^±700 (2^±90) if they are too large or too small to
//! square, and squared. `hypot` squares each exactly, as a head and a tail
//! from Dekker's split, and adds the four parts from the smallest; `hypotf`
//! squares in `double`, which is exact, and rounds the sum to `float`.

use crate::math::sqrt::{sqrt, sqrtf};
use crate::math::support::{hexf32, hexf64};

/// Where Dekker's product splits a `double`: 2^27 + 1.
const SPLIT: f64 = hexf64!("0x1.0000002p27");

/// `x²` as a head, `x·x` rounded, and a tail that is exact where rounding to
/// nearest. musl's `sq`.
fn sq(x: f64) -> (f64, f64) {
    let xc = x * SPLIT;
    let xh = x - xc + xc;
    let xl = x - xh;
    let hi = x * x;
    let lo = xh * xh - hi + 2.0 * xh * xl + xl * xl;
    (hi, lo)
}

/// √(`x`² + `y`²), the length of the hypotenuse.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn hypot(x: f64, y: f64) -> f64 {
    // Arrange |x| >= |y|.
    let mut ux = x.to_bits() & (u64::MAX >> 1);
    let mut uy = y.to_bits() & (u64::MAX >> 1);
    if ux < uy {
        core::mem::swap(&mut ux, &mut uy);
    }

    // Special cases.
    let ex = (ux >> 52) as i32;
    let ey = (uy >> 52) as i32;
    let mut x = f64::from_bits(ux);
    let mut y = f64::from_bits(uy);
    // hypot(inf, nan) is inf.
    if ey == 0x7ff {
        return y;
    }
    if ex == 0x7ff || uy == 0 {
        return x;
    }
    // hypot(x, y) ~= x + y²/x/2, inexact for a small y/x. A difference of
    // 64 is enough for a long double `double_t`.
    if ex - ey > 64 {
        return x + y;
    }

    // An exact argument for the root, when rounding to nearest, without
    // overflow: xh·xh must not overflow, nor xl·xl underflow, in `sq`.
    let mut z = 1.0;
    if ex > 0x3ff + 510 {
        z = hexf64!("0x1p700");
        x *= hexf64!("0x1p-700");
        y *= hexf64!("0x1p-700");
    } else if ey < 0x3ff - 450 {
        z = hexf64!("0x1p-700");
        x *= hexf64!("0x1p700");
        y *= hexf64!("0x1p700");
    }
    let (hx, lx) = sq(x);
    let (hy, ly) = sq(y);
    z * sqrt(ly + lx + hy + hx)
}

/// [`hypot`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn hypotf(x: f32, y: f32) -> f32 {
    let mut ux = x.to_bits() & (u32::MAX >> 1);
    let mut uy = y.to_bits() & (u32::MAX >> 1);
    if ux < uy {
        core::mem::swap(&mut ux, &mut uy);
    }

    let mut x = f32::from_bits(ux);
    let mut y = f32::from_bits(uy);
    if uy == 0xff << 23 {
        return y;
    }
    if ux >= 0xff << 23 || uy == 0 || ux - uy >= 25 << 23 {
        // GCC compiles musl's `x + y` as `addss` with `y` as the destination,
        // so where both are NaNs musl returns `y`'s; LLVM may pick either.
        return crate::complex::addf(y, x);
    }

    let mut z = 1.0;
    if ux >= (0x7f + 60) << 23 {
        z = hexf32!("0x1p90");
        x *= hexf32!("0x1p-90");
        y *= hexf32!("0x1p-90");
    } else if uy < (0x7f - 60) << 23 {
        z = hexf32!("0x1p-90");
        x *= hexf32!("0x1p90");
        y *= hexf32!("0x1p90");
    }
    // musl passes the `double` sum to `sqrtf`, which converts it to `float`
    // in the current rounding mode.
    let sum = f64::from(x) * f64::from(x) + f64::from(y) * f64::from(y);
    z * sqrtf(sum as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn hypot_matches_libc_test() {
        let files = ["ucb/hypot.h", "sanity/hypot.h", "special/hypot.h"];
        // libc-test's `hypot.c` also wants an error under 1 ulp to nearest.
        let rules = Rules::ULP.within_one_ulp();
        mtest::dd_d("hypot", &files, |x, y| hypot(x, y), rules, &[]);
    }

    #[test]
    fn hypotf_matches_libc_test() {
        let files = ["ucb/hypotf.h", "sanity/hypotf.h", "special/hypotf.h"];
        // As `hypot.c`.
        let rules = Rules::ULP.within_one_ulp();
        mtest::dd_d("hypotf", &files, |x, y| hypotf(x, y), rules, &[]);
    }

    #[test]
    fn the_split_is_musls() {
        assert_eq!(SPLIT, hexf64!("0x1p27") + 1.0);
    }
}
