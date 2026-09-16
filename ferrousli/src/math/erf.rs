//! `erf` and `erfc`: the error function and its complement.
//!
//! Ported from musl 1.2.5's `erf.c` (MIT; see [`crate::math`] for the notice).
//! musl took it from FreeBSD's msun, whose file carries this notice:
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
//! are written here as bits, and the tests check them against musl's decimals.
//!
//! # Method
//!
//! On [0, 0.84375], erf(x) = x + x·R(x²) for a rational R, and erfc(x) is
//! 1 - erf(x), arranged to lose less above 1/4. On [0.84375, 1.25], with
//! s = |x| - 1, erf(x) = c + P(s)/Q(s) for c = erf(1) rounded to 24 bits. From
//! 1.25 to 28, erfc(x) = exp(-x² - 0.5625 + R(1/x²)/S(1/x²))/x, with one
//! rational function below 1/0.35 and another above it, and -x² split as
//! -z² + (z - x)(z + x) for a z with only the high word of x, whose square is
//! exact. erf(x) is 1 - erfc(x) there, and ±(1 - tiny) from 6 up. For
//! negative x, erf(-x) = -erf(x) and erfc(-x) = 2 - erfc(x).

use crate::math::exp::exp;
use crate::math::manipulate::fabs;
use crate::math::support::{barrier, hexf64, high_word, with_low_word};

/// erf(1), rounded to 24 bits.
const ERX: f64 = f64::from_bits(0x3feb_0ac1_6000_0000);

// erf on [0, 0.84375]: efx8 is 8·(2/√π - 1), and pp/qq are R's numerator
// and denominator.
const EFX8: f64 = f64::from_bits(0x3ff0_6eba_8214_db69);
const PP0: f64 = f64::from_bits(0x3fc0_6eba_8214_db68);
const PP1: f64 = f64::from_bits(0xbfd4_cd7d_691c_b913);
const PP2: f64 = f64::from_bits(0xbf9d_2a51_dbd7_194f);
const PP3: f64 = f64::from_bits(0xbf77_a291_2366_68e4);
const PP4: f64 = f64::from_bits(0xbef8_ead6_1200_16ac);
const QQ1: f64 = f64::from_bits(0x3fd9_7779_cdda_dc09);
const QQ2: f64 = f64::from_bits(0x3fb0_a54c_5536_ceba);
const QQ3: f64 = f64::from_bits(0x3f74_d022_c4d3_6b0f);
const QQ4: f64 = f64::from_bits(0x3f21_5dc9_221c_1a10);
const QQ5: f64 = f64::from_bits(0xbed0_9c43_42a2_6120);

// erf - erx on [0.84375, 1.25].
const PA0: f64 = f64::from_bits(0xbf63_59b8_bef7_7538);
const PA1: f64 = f64::from_bits(0x3fda_8d00_ad92_b34d);
const PA2: f64 = f64::from_bits(0xbfd7_d240_fbb8_c3f1);
const PA3: f64 = f64::from_bits(0x3fd4_5fca_8051_20e4);
const PA4: f64 = f64::from_bits(0xbfbc_6398_3d3e_28ec);
const PA5: f64 = f64::from_bits(0x3fa2_2a36_5997_95eb);
const PA6: f64 = f64::from_bits(0xbf61_bf38_0a96_073f);
const QA1: f64 = f64::from_bits(0x3fbb_3e66_18ee_e323);
const QA2: f64 = f64::from_bits(0x3fe1_4af0_92eb_6f33);
const QA3: f64 = f64::from_bits(0x3fb2_635c_d99f_e9a7);
const QA4: f64 = f64::from_bits(0x3fc0_2660_e763_351f);
const QA5: f64 = f64::from_bits(0x3f8b_edc2_6b51_dd1c);
const QA6: f64 = f64::from_bits(0x3f88_8b54_5735_151d);

// erfc on [1.25, 1/0.35].
const RA0: f64 = f64::from_bits(0xbf84_3412_600d_6435);
const RA1: f64 = f64::from_bits(0xbfe6_3416_e4ba_7360);
const RA2: f64 = f64::from_bits(0xc025_1e04_41b0_e726);
const RA3: f64 = f64::from_bits(0xc04f_300a_e4cb_a38d);
const RA4: f64 = f64::from_bits(0xc064_4cb1_8428_2266);
const RA5: f64 = f64::from_bits(0xc067_135c_ebcc_abb2);
const RA6: f64 = f64::from_bits(0xc054_5265_57e4_d2f2);
const RA7: f64 = f64::from_bits(0xc023_a0ef_c69a_c25c);
const SA1: f64 = f64::from_bits(0x4033_a6b9_bd70_7687);
const SA2: f64 = f64::from_bits(0x4061_350c_526a_e721);
const SA3: f64 = f64::from_bits(0x407b_290d_d58a_1a71);
const SA4: f64 = f64::from_bits(0x4084_2b19_21ec_2868);
const SA5: f64 = f64::from_bits(0x407a_d021_5770_0314);
const SA6: f64 = f64::from_bits(0x405b_28a3_ee48_ae2c);
const SA7: f64 = f64::from_bits(0x401a_47ef_8e48_4a93);
const SA8: f64 = f64::from_bits(0xbfae_eff2_ee74_9a62);

// erfc on [1/0.35, 28].
const RB0: f64 = f64::from_bits(0xbf84_3412_39e8_6f4a);
const RB1: f64 = f64::from_bits(0xbfe9_93ba_70c2_85de);
const RB2: f64 = f64::from_bits(0xc031_c209_555f_995a);
const RB3: f64 = f64::from_bits(0xc064_145d_43c5_ed98);
const RB4: f64 = f64::from_bits(0xc083_ec88_1375_f228);
const RB5: f64 = f64::from_bits(0xc090_0461_6a2e_5992);
const RB6: f64 = f64::from_bits(0xc07e_384e_9bdc_383f);
const SB1: f64 = f64::from_bits(0x403e_568b_261d_5190);
const SB2: f64 = f64::from_bits(0x4074_5cae_221b_9f0a);
const SB3: f64 = f64::from_bits(0x4098_02eb_189d_5118);
const SB4: f64 = f64::from_bits(0x40a8_ffb7_688c_246a);
const SB5: f64 = f64::from_bits(0x40a3_f219_cedf_3be6);
const SB6: f64 = f64::from_bits(0x407d_a874_e79f_e763);
const SB7: f64 = f64::from_bits(0xc036_70e2_4271_2d62);

/// The smallest normal `double`, a tiny amount for the results that round.
const TINY: f64 = hexf64!("0x1p-1022");

/// erfc(`x`) for 0.84375 <= |x| < 1.25. musl's `erfc1`.
fn erfc1(x: f64) -> f64 {
    let s = fabs(x) - 1.0;
    let p = PA0 + s * (PA1 + s * (PA2 + s * (PA3 + s * (PA4 + s * (PA5 + s * PA6)))));
    let q = 1.0 + s * (QA1 + s * (QA2 + s * (QA3 + s * (QA4 + s * (QA5 + s * QA6)))));
    1.0 - ERX - p / q
}

/// erfc(|`x`|) for 0.84375 <= |x| < 28, where `ix` is the high word of |x|.
/// musl's `erfc2`.
fn erfc2(ix: u32, x: f64) -> f64 {
    if ix < 0x3ff4_0000 {
        return erfc1(x);
    }

    let x = fabs(x);
    let s = 1.0 / (x * x);
    let (r, big_s) = if ix < 0x4006_db6d {
        // |x| < 1/0.35.
        (
            RA0 + s * (RA1 + s * (RA2 + s * (RA3 + s * (RA4 + s * (RA5 + s * (RA6 + s * RA7)))))),
            1.0 + s
                * (SA1
                    + s * (SA2
                        + s * (SA3 + s * (SA4 + s * (SA5 + s * (SA6 + s * (SA7 + s * SA8))))))),
        )
    } else {
        (
            RB0 + s * (RB1 + s * (RB2 + s * (RB3 + s * (RB4 + s * (RB5 + s * RB6))))),
            1.0 + s * (SB1 + s * (SB2 + s * (SB3 + s * (SB4 + s * (SB5 + s * (SB6 + s * SB7)))))),
        )
    };
    let z = with_low_word(x, 0);
    exp(-z * z - 0.5625) * exp((z - x) * (z + x) + r / big_s) / x
}

/// The error function: 2/√π times the integral of exp(-t²) from 0 to `x`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn erf(x: f64) -> f64 {
    let hx = high_word(x);
    let sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        // erf(NaN) is NaN, erf(±inf) is ±1.
        return f64::from(1 - 2 * sign as i32) + 1.0 / x;
    }
    if ix < 0x3feb_0000 {
        // |x| < 0.84375.
        if ix < 0x3e30_0000 {
            // |x| < 2^-28: arranged to avoid underflow.
            return 0.125 * (8.0 * x + EFX8 * x);
        }
        let z = x * x;
        let r = PP0 + z * (PP1 + z * (PP2 + z * (PP3 + z * PP4)));
        let s = 1.0 + z * (QQ1 + z * (QQ2 + z * (QQ3 + z * (QQ4 + z * QQ5))));
        let y = r / s;
        return x + x * y;
    }
    let y = if ix < 0x4018_0000 {
        // |x| < 6.
        1.0 - erfc2(ix, x)
    } else {
        // musl's `1 - 0x1p-1022`, which rounds, so it rounds at run time.
        barrier(1.0) - TINY
    };
    if sign != 0 { -y } else { y }
}

/// The complementary error function, 1 - erf(`x`), without the cancellation.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn erfc(x: f64) -> f64 {
    let hx = high_word(x);
    let sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        // erfc(NaN) is NaN, erfc(+inf) is 0 and erfc(-inf) is 2.
        return f64::from(2 * sign) + 1.0 / x;
    }
    if ix < 0x3feb_0000 {
        // |x| < 0.84375.
        if ix < 0x3c70_0000 {
            // |x| < 2^-56.
            return 1.0 - x;
        }
        let z = x * x;
        let r = PP0 + z * (PP1 + z * (PP2 + z * (PP3 + z * PP4)));
        let s = 1.0 + z * (QQ1 + z * (QQ2 + z * (QQ3 + z * (QQ4 + z * QQ5))));
        let y = r / s;
        if sign != 0 || ix < 0x3fd0_0000 {
            // x < 1/4.
            return 1.0 - (x + x * y);
        }
        return 0.5 - (x - 0.5 + x * y);
    }
    if ix < 0x403c_0000 {
        // |x| < 28.
        return if sign != 0 {
            2.0 - erfc2(ix, x)
        } else {
            erfc2(ix, x)
        };
    }
    // musl's `2 - 0x1p-1022` and `0x1p-1022*0x1p-1022`, which round and
    // underflow at run time.
    if sign != 0 {
        barrier(2.0) - TINY
    } else {
        barrier(TINY) * TINY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn erf_matches_libc_test() {
        let files = ["sanity/erf.h", "special/erf.h"];
        // libc-test's `erf.c` tolerates an error under 4 ulps.
        let rules = Rules::ULP.tolerate(4.0);
        mtest::d_d("erf", &files, |x| erf(x), rules, &[]);
    }

    #[test]
    fn erfc_matches_libc_test() {
        let files = ["sanity/erfc.h", "special/erfc.h"];
        // libc-test's `erfc.c` tolerates an error under 4 ulps.
        let rules = Rules::ULP.tolerate(4.0);
        mtest::d_d("erfc", &files, |x| erfc(x), rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (ERX, "8.45062911510467529297e-01"),
            (EFX8, "1.02703333676410069053e+00"),
            (PP0, "1.28379167095512558561e-01"),
            (PP1, "-3.25042107247001499370e-01"),
            (PP2, "-2.84817495755985104766e-02"),
            (PP3, "-5.77027029648944159157e-03"),
            (PP4, "-2.37630166566501626084e-05"),
            (QQ1, "3.97917223959155352819e-01"),
            (QQ2, "6.50222499887672944485e-02"),
            (QQ3, "5.08130628187576562776e-03"),
            (QQ4, "1.32494738004321644526e-04"),
            (QQ5, "-3.96022827877536812320e-06"),
            (PA0, "-2.36211856075265944077e-03"),
            (PA1, "4.14856118683748331666e-01"),
            (PA2, "-3.72207876035701323847e-01"),
            (PA3, "3.18346619901161753674e-01"),
            (PA4, "-1.10894694282396677476e-01"),
            (PA5, "3.54783043256182359371e-02"),
            (PA6, "-2.16637559486879084300e-03"),
            (QA1, "1.06420880400844228286e-01"),
            (QA2, "5.40397917702171048937e-01"),
            (QA3, "7.18286544141962662868e-02"),
            (QA4, "1.26171219808761642112e-01"),
            (QA5, "1.36370839120290507362e-02"),
            (QA6, "1.19844998467991074170e-02"),
            (RA0, "-9.86494403484714822705e-03"),
            (RA1, "-6.93858572707181764372e-01"),
            (RA2, "-1.05586262253232909814e+01"),
            (RA3, "-6.23753324503260060396e+01"),
            (RA4, "-1.62396669462573470355e+02"),
            (RA5, "-1.84605092906711035994e+02"),
            (RA6, "-8.12874355063065934246e+01"),
            (RA7, "-9.81432934416914548592e+00"),
            (SA1, "1.96512716674392571292e+01"),
            (SA2, "1.37657754143519042600e+02"),
            (SA3, "4.34565877475229228821e+02"),
            (SA4, "6.45387271733267880336e+02"),
            (SA5, "4.29008140027567833386e+02"),
            (SA6, "1.08635005541779435134e+02"),
            (SA7, "6.57024977031928170135e+00"),
            (SA8, "-6.04244152148580987438e-02"),
            (RB0, "-9.86494292470009928597e-03"),
            (RB1, "-7.99283237680523006574e-01"),
            (RB2, "-1.77579549177547519889e+01"),
            (RB3, "-1.60636384855821916062e+02"),
            (RB4, "-6.37566443368389627722e+02"),
            (RB5, "-1.02509513161107724954e+03"),
            (RB6, "-4.83519191608651397019e+02"),
            (SB1, "3.03380607434824582924e+01"),
            (SB2, "3.25792512996573918826e+02"),
            (SB3, "1.53672958608443695994e+03"),
            (SB4, "3.19985821950859553908e+03"),
            (SB5, "2.55305040643316442583e+03"),
            (SB6, "4.74528541206955367215e+02"),
            (SB7, "-2.24409524465858183362e+01"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
    }
}
