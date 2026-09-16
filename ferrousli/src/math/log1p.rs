//! `log1p` and `log1pf`: the natural logarithm of 1 + x, accurate near 0, and
//! the polynomial that they and `log10` share.
//!
//! Ported from musl 1.2.5's `log1p.c` and `log1pf.c` (MIT; see
//! [`crate::math`] for the notice). musl took them from FreeBSD's msun, whose
//! files carry this notice:
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
//! musl writes the `double` constants in decimal, with their bits in a
//! comment. They are written here as those bits.
//!
//! # Method
//!
//! 1 + x = 2^k·(1 + f) with √2/2 < 1 + f < √2. Where k is not 0, f may not be
//! exact, so c = ((1 + x) - u)/u corrects for u, 1 + x rounded. Then
//! log1p(x) = k·ln2 + log(1 + f) + c/u, where with s = f/(2 + f),
//! log(1 + f) = f - f²/2 + s·(f²/2 + R(s²)) and R is the polynomial here.

use crate::math::support::{barrier, barrierf, force_evalf, hexf32, high_word, with_high_word};

/// The high bits of ln2, which `expm1` shares.
pub(crate) const LN2_HI: f64 = f64::from_bits(0x3fe6_2e42_fee0_0000);
/// The rest of ln2.
pub(crate) const LN2_LO: f64 = f64::from_bits(0x3dea_39ef_3579_3c76);

// `Lg1` to `Lg7`: R's coefficients, of s² to s^14.
const LG1: f64 = f64::from_bits(0x3fe5_5555_5555_5593);
const LG2: f64 = f64::from_bits(0x3fd9_9999_9997_fa04);
const LG3: f64 = f64::from_bits(0x3fd2_4924_9422_9359);
const LG4: f64 = f64::from_bits(0x3fcc_71c5_1d8e_78af);
const LG5: f64 = f64::from_bits(0x3fc7_4664_96cb_03de);
const LG6: f64 = f64::from_bits(0x3fc3_9a09_d078_c69f);
const LG7: f64 = f64::from_bits(0x3fc2_f112_df3e_5244);

/// The high bits of ln2 for `float`, which `expm1f` shares.
pub(crate) const LN2_HI_F: f32 = f32::from_bits(0x3f31_7180);
/// The rest of ln2 for `float`.
pub(crate) const LN2_LO_F: f32 = f32::from_bits(0x3717_f7d1);

// R's coefficients for `float`, with |(log(1+s) - log(1-s))/s - Lg(s)| <
// 2^-34.24.
const LG1_F: f32 = hexf32!("0xaaaaaa.0p-24");
const LG2_F: f32 = hexf32!("0xccce13.0p-25");
const LG3_F: f32 = hexf32!("0x91e9ee.0p-25");
const LG4_F: f32 = hexf32!("0xf89e26.0p-26");

/// R(z) for z = s², the part of log(1 + f) that musl computes as `t2 + t1`.
#[inline]
pub(crate) fn polynomial(z: f64) -> f64 {
    let w = z * z;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    t2 + t1
}

/// [`polynomial`] for `float`.
#[inline]
pub(crate) fn polynomialf(z: f32) -> f32 {
    let w = z * z;
    let t1 = w * (LG2_F + w * LG4_F);
    let t2 = z * (LG1_F + w * LG3_F);
    t2 + t1
}

/// The natural logarithm of 1 + `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn log1p(x: f64) -> f64 {
    let hx = high_word(x);
    let mut k = 1;
    let mut c = 0.0;
    let mut f = 0.0;
    if hx < 0x3fda_827a || hx >> 31 != 0 {
        // 1 + x < √2, roughly.
        if hx >= 0xbff0_0000 {
            // x <= -1, or a NaN with its sign bit set.
            if x == -1.0 {
                // log1p(-1) is -inf. LLVM may put -1 for x here and fold the
                // division, losing divide-by-zero, without the barrier.
                return barrier(x) / 0.0;
            }
            // log1p(x < -1) is NaN. musl writes x - x, which clippy takes
            // for a mistake; the barrier changes nothing else.
            return (barrier(x) - x) / 0.0;
        }
        if hx << 1 < 0x3ca0_0000 << 1 {
            // |x| < 2^-53: raise underflow if x is subnormal.
            if hx & 0x7ff0_0000 == 0 {
                force_evalf(x as f32);
            }
            return x;
        }
        if hx <= 0xbfd2_bec4 {
            // √2/2 <= 1 + x < √2, roughly.
            k = 0;
            c = 0.0;
            f = x;
        }
    } else if hx >= 0x7ff0_0000 {
        return x;
    }
    if k != 0 {
        let u = 1.0 + x;
        let mut hu = high_word(u);
        hu += 0x3ff0_0000 - 0x3fe6_a09e;
        k = (hu >> 20) as i32 - 0x3ff;
        // The correction term, log(1 + x) - log(u), without underflow in c/u.
        if k < 54 {
            c = if k >= 2 { 1.0 - (u - x) } else { x - (u - 1.0) };
            c /= u;
        } else {
            c = 0.0;
        }
        // Reduce u into [√2/2, √2].
        hu = (hu & 0x000f_ffff) + 0x3fe6_a09e;
        f = with_high_word(u, hu) - 1.0;
    }
    let hfsq = 0.5 * f * f;
    let s = f / (2.0 + f);
    let r = polynomial(s * s);
    let dk = f64::from(k);
    s * (hfsq + r) + (dk * LN2_LO + c) - hfsq + f + dk * LN2_HI
}

/// [`log1p`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn log1pf(x: f32) -> f32 {
    let ix = x.to_bits();
    let mut k = 1;
    let mut c = 0.0;
    let mut f = 0.0;
    if ix < 0x3ed4_13d0 || ix >> 31 != 0 {
        // 1 + x < √2, roughly.
        if ix >= 0xbf80_0000 {
            // x <= -1, or a NaN with its sign bit set.
            if x == -1.0 {
                // log1pf(-1) is -inf. LLVM may put -1 for x here and fold the
                // division, losing divide-by-zero, without the barrier.
                return barrierf(x) / 0.0;
            }
            // log1pf(x < -1) is NaN. musl writes x - x, which clippy takes
            // for a mistake; the barrier changes nothing else.
            return (barrierf(x) - x) / 0.0;
        }
        if ix << 1 < 0x3380_0000 << 1 {
            // |x| < 2^-24: raise underflow if x is subnormal.
            if ix & 0x7f80_0000 == 0 {
                force_evalf(x * x);
            }
            return x;
        }
        if ix <= 0xbe95_f619 {
            // √2/2 <= 1 + x < √2, roughly.
            k = 0;
            c = 0.0;
            f = x;
        }
    } else if ix >= 0x7f80_0000 {
        return x;
    }
    if k != 0 {
        let u = 1.0 + x;
        let mut iu = u.to_bits();
        iu += 0x3f80_0000 - 0x3f35_04f3;
        k = (iu >> 23) as i32 - 0x7f;
        // The correction term, log(1 + x) - log(u), without underflow in c/u.
        if k < 25 {
            c = if k >= 2 { 1.0 - (u - x) } else { x - (u - 1.0) };
            c /= u;
        } else {
            c = 0.0;
        }
        // Reduce u into [√2/2, √2].
        iu = (iu & 0x007f_ffff) + 0x3f35_04f3;
        f = f32::from_bits(iu) - 1.0;
    }
    let s = f / (2.0 + f);
    let r = polynomialf(s * s);
    let hfsq = 0.5 * f * f;
    let dk = k as f32;
    s * (hfsq + r) + (dk * LN2_LO_F + c) - hfsq + f + dk * LN2_HI_F
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn log1p_matches_libc_test() {
        let files = ["crlibm/log1p.h", "sanity/log1p.h", "special/log1p.h"];
        mtest::d_d("log1p", &files, |x| log1p(x), Rules::ULP, &[]);
    }

    #[test]
    fn log1pf_matches_libc_test() {
        let files = ["sanity/log1pf.h", "special/log1pf.h"];
        mtest::d_d("log1pf", &files, |x| log1pf(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (LN2_HI, "6.93147180369123816490e-01"),
            (LN2_LO, "1.90821492927058770002e-10"),
            (LG1, "6.666666666666735130e-01"),
            (LG2, "3.999999999940941908e-01"),
            (LG3, "2.857142874366239149e-01"),
            (LG4, "2.222219843214978396e-01"),
            (LG5, "1.818357216161805012e-01"),
            (LG6, "1.531383769920937332e-01"),
            (LG7, "1.479819860511658591e-01"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
        let floats = [
            (LN2_HI_F, "6.9313812256e-01"),
            (LN2_LO_F, "9.0580006145e-06"),
            (LG1_F, "0.66666662693"),
            (LG2_F, "0.40000972152"),
            (LG3_F, "0.28498786688"),
            (LG4_F, "0.24279078841"),
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
