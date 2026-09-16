//! `tanh` and `tanhf`: the hyperbolic tangent.
//!
//! Ported from musl 1.2.5's `tanh.c` and `tanhf.c` (MIT; see [`crate::math`]
//! for the notice). musl wrote these itself; they carry no other notice.
//!
//! # Method
//!
//! tanh(x) = (exp(2x) - 1)/(exp(2x) - 1 + 2), computed on |x| through
//! `expm1` in the form that loses least in each range: 1 - 2/(t + 2) above
//! log(3)/2, t/(t + 2) above log(5/3)/2, and -t/(t + 2) with t = expm1(-2x)
//! below. Above 20 (10 for `float`) the result is 1 without overflow, and a
//! subnormal x is its own result.

use crate::math::expm1::{expm1, expm1f};
use crate::math::support::force_evalf;

/// The hyperbolic tangent of `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tanh(x: f64) -> f64 {
    let bits = x.to_bits();
    let negative = bits >> 63 != 0;
    let x = f64::from_bits(bits & (u64::MAX >> 1));
    let w = (x.to_bits() >> 32) as u32;

    let t = if w > 0x3fe1_93ea {
        // |x| > log(3)/2 ~= 0.5493, or NaN.
        if w > 0x4034_0000 {
            // |x| > 20, or NaN: 1, without raising overflow, or the NaN.
            1.0 - 0.0 / x
        } else {
            let t = expm1(2.0 * x);
            1.0 - 2.0 / (t + 2.0)
        }
    } else if w > 0x3fd0_58ae {
        // |x| > log(5/3)/2 ~= 0.2554.
        let t = expm1(2.0 * x);
        t / (t + 2.0)
    } else if w >= 0x0010_0000 {
        // |x| >= 2^-1022, up to 2 ulps of error in [0.1, 0.2554].
        let t = expm1(-2.0 * x);
        -t / (t + 2.0)
    } else {
        // |x| is subnormal. The branch above would not raise underflow in
        // [2^-1023, 2^-1022).
        force_evalf(x as f32);
        x
    };
    if negative { -t } else { t }
}

/// [`tanh`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tanhf(x: f32) -> f32 {
    let bits = x.to_bits();
    let negative = bits >> 31 != 0;
    let w = bits & 0x7fff_ffff;
    let x = f32::from_bits(w);

    let t = if w > 0x3f0c_9f54 {
        // |x| > log(3)/2 ~= 0.5493, or NaN.
        if w > 0x4120_0000 {
            // |x| > 10, or NaN. musl writes 1 + 0/x.
            1.0 + 0.0 / x
        } else {
            let t = expm1f(2.0 * x);
            1.0 - 2.0 / (t + 2.0)
        }
    } else if w > 0x3e82_c578 {
        // |x| > log(5/3)/2 ~= 0.2554.
        let t = expm1f(2.0 * x);
        t / (t + 2.0)
    } else if w >= 0x0080_0000 {
        // |x| >= 2^-126.
        let t = expm1f(-2.0 * x);
        -t / (t + 2.0)
    } else {
        // |x| is subnormal.
        force_evalf(x * x);
        x
    };
    if negative { -t } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn tanh_matches_libc_test() {
        let files = ["ucb/tanh.h", "sanity/tanh.h", "special/tanh.h"];
        mtest::d_d("tanh", &files, |x| tanh(x), Rules::ULP, &[]);
    }

    #[test]
    fn tanhf_matches_libc_test() {
        let files = ["ucb/tanhf.h", "sanity/tanhf.h", "special/tanhf.h"];
        mtest::d_d("tanhf", &files, |x| tanhf(x), Rules::ULP, &[]);
    }
}
