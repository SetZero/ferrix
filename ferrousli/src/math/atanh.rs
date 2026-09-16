//! `atanh` and `atanhf`: the inverse hyperbolic tangent.
//!
//! Ported from musl 1.2.5's `atanh.c` and `atanhf.c` (MIT; see
//! [`crate::math`] for the notice). musl wrote these itself; they carry no
//! other notice.
//!
//! # Method
//!
//! atanh(x) = ±log((1 + |x|)/(1 - |x|))/2 = ±log1p(2|x|/(1 - |x|))/2, written
//! as log1p(2|x| + 2x²/(1 - |x|)) below 0.5, and as x itself below 2^-32.
//! For |x| = 1 the division raises divide-by-zero, and above 1 `log1p`
//! raises invalid.

use crate::math::log1p::{log1p, log1pf};
use crate::math::support::force_evalf;

/// The inverse hyperbolic tangent of `x`, for |`x`| <= 1.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn atanh(x: f64) -> f64 {
    let bits = x.to_bits();
    let e = (bits >> 52 & 0x7ff) as u32;
    let negative = bits >> 63 != 0;
    let mut y = f64::from_bits(bits & (u64::MAX >> 1));

    if e < 0x3ff - 1 {
        if e < 0x3ff - 32 {
            // Raise underflow if x is subnormal.
            if e == 0 {
                force_evalf(y as f32);
            }
        } else {
            // |x| < 0.5, up to 1.7 ulps of error.
            y = 0.5 * log1p(2.0 * y + 2.0 * y * y / (1.0 - y));
        }
    } else {
        // Avoid overflow.
        y = 0.5 * log1p(2.0 * (y / (1.0 - y)));
    }
    if negative { -y } else { y }
}

/// [`atanh`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn atanhf(x: f32) -> f32 {
    let bits = x.to_bits();
    let negative = bits >> 31 != 0;
    let i = bits & 0x7fff_ffff;
    let mut y = f32::from_bits(i);

    if i < 0x3f80_0000 - (1 << 23) {
        if i < 0x3f80_0000 - (32 << 23) {
            // Raise underflow if x is subnormal.
            if i < 1 << 23 {
                force_evalf(y * y);
            }
        } else {
            // |x| < 0.5, up to 1.7 ulps of error.
            y = 0.5 * log1pf(2.0 * y + 2.0 * y * y / (1.0 - y));
        }
    } else {
        // Avoid overflow.
        y = 0.5 * log1pf(2.0 * (y / (1.0 - y)));
    }
    if negative { -y } else { y }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn atanh_matches_libc_test() {
        let files = ["sanity/atanh.h", "special/atanh.h"];
        mtest::d_d("atanh", &files, |x| atanh(x), Rules::ULP, &[]);
    }

    #[test]
    fn atanhf_matches_libc_test() {
        let files = ["sanity/atanhf.h", "special/atanhf.h"];
        mtest::d_d("atanhf", &files, |x| atanhf(x), Rules::ULP, &[]);
    }
}
