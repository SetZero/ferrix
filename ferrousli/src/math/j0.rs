//! `j0` and `y0`: the Bessel functions of the first and second kinds of order
//! zero.
//!
//! Ported from musl 1.2.5's `j0.c` (MIT; see [`crate::math`] for the notice).
//! musl took it from FreeBSD's msun, whose file carries this notice:
//!
//! ```text
//! Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//!
//! Developed at SunSoft, a Sun Microsystems, Inc. business.
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
//! Below 2, j0(x) = 1 - x²/4 + x⁴·R(x²)/S(x²), and y0(x) = U(x²)/V(x²) +
//! (2/π)·j0(x)·log(x). From 2 up, j0(x) = √(2/(πx))·(p0(x)·cos(x0) -
//! q0(x)·sin(x0)) and y0(x) = √(2/(πx))·(p0(x)·sin(x0) + q0(x)·cos(x0)), with
//! x0 = x - π/4 and rational approximations p0 and q0 in 1/x² on four
//! intervals. cos(x0) and sin(x0) are (sin(x) ± cos(x))/√2, and whichever of
//! the two cancels is computed as -cos(2x)/(sin(x) ∓ cos(x)) instead.

use crate::math::log::log;
use crate::math::manipulate::fabs;
use crate::math::sqrt::sqrt;
use crate::math::support::{barrier, divzero, high_word, low_word};
use crate::math::trig::{cos, sin};

/// 1/√π.
const INVSQRTPI: f64 = f64::from_bits(0x3fe2_0dd7_5042_9b6d);
/// 2/π.
const TPI: f64 = f64::from_bits(0x3fe4_5f30_6dc9_c883);

// j0 below 2: R's numerator and S's.
const R02: f64 = f64::from_bits(0x3f8f_ffff_ffff_fffd);
const R03: f64 = f64::from_bits(0xbf28_e6a5_b61a_c6e9);
const R04: f64 = f64::from_bits(0x3ebe_b1d1_0c50_3919);
const R05: f64 = f64::from_bits(0xbe33_d5e7_73d6_3fce);
const S01: f64 = f64::from_bits(0x3f8f_fce8_82c8_c2a4);
const S02: f64 = f64::from_bits(0x3f1e_a6d2_dd57_dbf4);
const S03: f64 = f64::from_bits(0x3ea1_3b54_ce84_d5a9);
const S04: f64 = f64::from_bits(0x3e14_08bc_f474_5d8f);

// y0 below 2: U's numerator and V's.
const U00: f64 = f64::from_bits(0xbfb2_e4d6_99cb_d01f);
const U01: f64 = f64::from_bits(0x3fc6_9d01_9de9_e3fc);
const U02: f64 = f64::from_bits(0xbf8c_4ce8_b16c_fa97);
const U03: f64 = f64::from_bits(0x3f36_c54d_20b2_9b6b);
const U04: f64 = f64::from_bits(0xbecf_fea7_73d2_5cad);
const U05: f64 = f64::from_bits(0x3e55_0057_3b4e_abd4);
const U06: f64 = f64::from_bits(0xbdc5_e43d_693f_b3c8);
const V01: f64 = f64::from_bits(0x3f8a_1270_91c9_c71a);
const V02: f64 = f64::from_bits(0x3f13_ecbb_f578_c6c1);
const V03: f64 = f64::from_bits(0x3e91_642d_7ff2_02fd);
const V04: f64 = f64::from_bits(0x3dfe_5018_3bd6_d9ef);

// p0(x) - 1 = R/S in 1/x², on [8, inf], [4.5454, 8], [2.8571, 4.5454] and
// [2, 2.8571].
const P_R8: [f64; 6] = [
    f64::from_bits(0x0000_0000_0000_0000),
    f64::from_bits(0xbfb1_ffff_ffff_fd32),
    f64::from_bits(0xc020_29d0_b44f_a779),
    f64::from_bits(0xc070_1102_7b19_e863),
    f64::from_bits(0xc0a3_6a6e_cd4d_cafc),
    f64::from_bits(0xc0b4_850b_36cc_643d),
];
const P_S8: [f64; 5] = [
    f64::from_bits(0x405d_2233_07a9_6751),
    f64::from_bits(0x40ad_f37d_5059_6938),
    f64::from_bits(0x40e3_d2bb_6eb6_b05f),
    f64::from_bits(0x40fc_810f_8f9f_a9bd),
    f64::from_bits(0x40e7_4177_4f2c_49dc),
];
const P_R5: [f64; 6] = [
    f64::from_bits(0xbda9_18b1_47e4_95cc),
    f64::from_bits(0xbfb1_ffff_e69a_fbc6),
    f64::from_bits(0xc010_a370_f90c_6bbf),
    f64::from_bits(0xc050_eb2f_5a7d_1783),
    f64::from_bits(0xc074_b3b3_6742_cc63),
    f64::from_bits(0xc075_a6ef_28a3_8bd7),
];
const P_S5: [f64; 5] = [
    f64::from_bits(0x404e_6081_0c98_c5de),
    f64::from_bits(0x4090_6d02_5c7e_2864),
    f64::from_bits(0x40b7_5af8_8fbe_1d60),
    f64::from_bits(0x40c2_ccb8_fa76_fa38),
    f64::from_bits(0x40a2_cc1d_c70b_e864),
];
const P_R3: [f64; 6] = [
    f64::from_bits(0xbe25_e103_6fe1_aa86),
    f64::from_bits(0xbfb1_fff6_f7c0_e24b),
    f64::from_bits(0xc003_45b2_aea4_8074),
    f64::from_bits(0xc035_f74a_4cb9_4e14),
    f64::from_bits(0xc04d_0a22_420a_1a45),
    f64::from_bits(0xc03f_72ac_a892_d80f),
];
const P_S3: [f64; 5] = [
    f64::from_bits(0x4041_ed92_8407_7dd3),
    f64::from_bits(0x4076_9839_464a_7c0e),
    f64::from_bits(0x4092_a66e_6d10_61d6),
    f64::from_bits(0x4091_9ffc_b8c3_9b7e),
    f64::from_bits(0x4065_b296_fc37_9081),
];
const P_R2: [f64; 6] = [
    f64::from_bits(0xbe77_d316_e927_026d),
    f64::from_bits(0xbfb1_ff62_495e_1e42),
    f64::from_bits(0xbff7_3639_8a24_a843),
    f64::from_bits(0xc01e_8af3_edaf_a7f3),
    f64::from_bits(0xc026_62e6_c524_6303),
    f64::from_bits(0xc009_de81_af8f_e70f),
];
const P_S2: [f64; 5] = [
    f64::from_bits(0x4036_3865_908b_5959),
    f64::from_bits(0x4061_069e_0ee8_878f),
    f64::from_bits(0x4070_e786_42ea_079b),
    f64::from_bits(0x4063_3c03_3ab6_faff),
    f64::from_bits(0x402d_50b3_4439_1809),
];

// q0(x)·x + 1/8 = R/S in 1/x², on the same intervals.
const Q_R8: [f64; 6] = [
    f64::from_bits(0x0000_0000_0000_0000),
    f64::from_bits(0x3fb2_bfff_ffff_fe2c),
    f64::from_bits(0x4027_8952_5bb3_34d6),
    f64::from_bits(0x4081_6d63_1530_1825),
    f64::from_bits(0x40c1_4d99_3e18_f46d),
    f64::from_bits(0x40e2_12d4_0e90_1566),
];
const Q_S8: [f64; 6] = [
    f64::from_bits(0x4064_78d5_365b_39bc),
    f64::from_bits(0x40bf_a258_4e6b_0563),
    f64::from_bits(0x4101_6652_54d3_8c3f),
    f64::from_bits(0x4128_83da_83a5_2b43),
    f64::from_bits(0x4129_a66b_28de_0b3d),
    f64::from_bits(0xc114_fd6d_2c95_30c5),
];
const Q_R5: [f64; 6] = [
    f64::from_bits(0x3db4_3d8f_29cc_8cd9),
    f64::from_bits(0x3fb2_bfff_d172_b04c),
    f64::from_bits(0x4017_57b0_b995_3dd3),
    f64::from_bits(0x4060_e392_0a87_88e9),
    f64::from_bits(0x4090_0cf9_9dc8_c481),
    f64::from_bits(0x409f_17e9_53c6_e3a6),
];
const Q_S5: [f64; 6] = [
    f64::from_bits(0x4054_b1b3_fb5e_1543),
    f64::from_bits(0x40a0_3ba0_da21_c0ce),
    f64::from_bits(0x40d2_67d2_7b59_1e6d),
    f64::from_bits(0x40eb_b5e3_97e0_2372),
    f64::from_bits(0x40e1_9118_1f7a_54a0),
    f64::from_bits(0xc0b4_ea57_bedb_c609),
];
const Q_R3: [f64; 6] = [
    f64::from_bits(0x3e32_cd03_6ade_cb82),
    f64::from_bits(0x3fb2_bfee_0e8d_0842),
    f64::from_bits(0x400a_c0fc_6114_9cf5),
    f64::from_bits(0x4045_4f98_962d_aedd),
    f64::from_bits(0x4065_59db_e25e_fd1f),
    f64::from_bits(0x4064_d77c_81fa_21e0),
];
const Q_S3: [f64; 6] = [
    f64::from_bits(0x4048_6122_bfe3_43a6),
    f64::from_bits(0x4086_2d83_8654_4eb3),
    f64::from_bits(0x40ac_f04b_e44d_fc63),
    f64::from_bits(0x40b9_3c6c_d7c7_6a28),
    f64::from_bits(0x40a3_a8aa_d94f_b1c0),
    f64::from_bits(0xc062_a7eb_201c_f40f),
];
const Q_R2: [f64; 6] = [
    f64::from_bits(0x3e84_313b_54f7_6bdb),
    f64::from_bits(0x3fb2_bec5_3e88_3e34),
    f64::from_bits(0x3fff_f897_e727_779c),
    f64::from_bits(0x402c_fdbf_aaf9_6fe5),
    f64::from_bits(0x403f_aa8e_29fb_dc4a),
    f64::from_bits(0x4030_40b1_7181_4bb4),
];
const Q_S2: [f64; 6] = [
    f64::from_bits(0x403e_5d96_f7c0_7aed),
    f64::from_bits(0x4070_d591_e4d1_4b40),
    f64::from_bits(0x408a_6645_22b3_bf22),
    f64::from_bits(0x408b_977c_9c5c_c214),
    f64::from_bits(0x406a_9553_0e00_1365),
    f64::from_bits(0xc015_3e6a_f8b3_2931),
];

/// j0(`x`), or y0(x) if `y0` is set, for `x` >= 2 whose high word is `ix`.
/// musl's `common`.
fn common(ix: u32, x: f64, y0: bool) -> f64 {
    let s = sin(x);
    let mut c = cos(x);
    if y0 {
        c = -c;
    }
    let mut cc = s + c;
    // 2x would overflow, and the ulp error is large from 2^1023 up anyway.
    if ix < 0x7fe0_0000 {
        let mut ss = s - c;
        let z = -cos(2.0 * x);
        if s * c < 0.0 {
            cc = z / ss;
        } else {
            ss = z / cc;
        }
        if ix < 0x4800_0000 {
            if y0 {
                ss = -ss;
            }
            cc = pzero(x) * cc - qzero(x) * ss;
        }
    }
    INVSQRTPI * cc / sqrt(x)
}

/// The Bessel function of the first kind of order zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn j0(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;

    // j0(±inf) is 0, j0(NaN) is NaN.
    if ix >= 0x7ff0_0000 {
        return 1.0 / (x * x);
    }
    let x = fabs(x);

    if ix >= 0x4000_0000 {
        // |x| >= 2. The ulp error is large near the zeros.
        return common(ix, x, false);
    }

    // 1 - x²/4 + x²·R(x²)/S(x²).
    if ix >= 0x3f20_0000 {
        // |x| >= 2^-13. Up to 4 ulps of error close to 2.
        let z = x * x;
        let r = z * (R02 + z * (R03 + z * (R04 + z * R05)));
        let s = 1.0 + z * (S01 + z * (S02 + z * (S03 + z * S04)));
        return (1.0 + x / 2.0) * (1.0 - x / 2.0) + z * (r / s);
    }

    // 1 - x²/4, avoiding underflow. As in musl, inexact is not raised for
    // every nonzero x.
    let x = if ix >= 0x3800_0000 {
        // |x| >= 2^-127.
        0.25 * x * x
    } else {
        x
    };
    1.0 - x
}

/// The Bessel function of the second kind of order zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn y0(x: f64) -> f64 {
    let ix = high_word(x);
    let lx = low_word(x);

    // y0(NaN) is NaN, y0(x < 0) is NaN, y0(0) is -inf and y0(inf) is 0.
    if (ix << 1 | lx) == 0 {
        return divzero(1);
    }
    if ix >> 31 != 0 {
        return barrier(0.0) / 0.0;
    }
    if ix >= 0x7ff0_0000 {
        return 1.0 / x;
    }

    if ix >= 0x4000_0000 {
        // x >= 2. The ulp error is large near the zeros.
        return common(ix, x, true);
    }

    // U(x²)/V(x²) + (2/π)·j0(x)·log(x).
    if ix >= 0x3e40_0000 {
        // x >= 2^-27. The ulp error is large near the first zero, 0.89.
        let z = x * x;
        let u = U00 + z * (U01 + z * (U02 + z * (U03 + z * (U04 + z * (U05 + z * U06)))));
        let v = 1.0 + z * (V01 + z * (V02 + z * (V03 + z * V04)));
        return u / v + TPI * (j0(x) * log(x));
    }
    U00 + TPI * log(x)
}

/// p0(`x`) for `x` >= 2. musl's `pzero`.
fn pzero(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4020_0000 {
        (&P_R8, &P_S8)
    } else if ix >= 0x4012_2e8b {
        (&P_R5, &P_S5)
    } else if ix >= 0x4006_db6d {
        (&P_R3, &P_S3)
    } else {
        (&P_R2, &P_S2)
    };
    let [p0, p1, p2, p3, p4, p5] = *p;
    let [q0, q1, q2, q3, q4] = *q;
    let z = 1.0 / (x * x);
    let r = p0 + z * (p1 + z * (p2 + z * (p3 + z * (p4 + z * p5))));
    let s = 1.0 + z * (q0 + z * (q1 + z * (q2 + z * (q3 + z * q4))));
    1.0 + r / s
}

/// q0(`x`) for `x` >= 2. musl's `qzero`.
fn qzero(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4020_0000 {
        (&Q_R8, &Q_S8)
    } else if ix >= 0x4012_2e8b {
        (&Q_R5, &Q_S5)
    } else if ix >= 0x4006_db6d {
        (&Q_R3, &Q_S3)
    } else {
        (&Q_R2, &Q_S2)
    };
    let [p0, p1, p2, p3, p4, p5] = *p;
    let [q0, q1, q2, q3, q4, q5] = *q;
    let z = 1.0 / (x * x);
    let r = p0 + z * (p1 + z * (p2 + z * (p3 + z * (p4 + z * p5))));
    let s = 1.0 + z * (q0 + z * (q1 + z * (q2 + z * (q3 + z * (q4 + z * q5)))));
    (-0.125 + r / s) / x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};
    use crate::math::support::hexf32;

    #[test]
    fn j0_matches_libc_test() {
        let files = ["sanity/j0.h", "special/j0.h"];
        // libc-test's `j0.c` tolerates an error under 2^52 ulps.
        let rules = Rules::ULP.tolerate(hexf32!("0x1p52"));
        mtest::d_d("j0", &files, |x| j0(x), rules, &[]);
    }

    #[test]
    fn y0_matches_libc_test() {
        let files = ["sanity/y0.h", "special/y0.h"];
        // libc-test's `y0.c` wants a NaN or -inf for a negative argument, and
        // tolerates an error under 2^52 ulps for the others.
        let rules = Rules::ULP.tolerate(hexf32!("0x1p52")).negative_domain();
        mtest::d_d("y0", &files, |x| y0(x), rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (INVSQRTPI, "5.64189583547756279280e-01"),
            (TPI, "6.36619772367581382433e-01"),
            (R02, "1.56249999999999947958e-02"),
            (R03, "-1.89979294238854721751e-04"),
            (R04, "1.82954049532700665670e-06"),
            (R05, "-4.61832688532103189199e-09"),
            (S01, "1.56191029464890010492e-02"),
            (S02, "1.16926784663337450260e-04"),
            (S03, "5.13546550207318111446e-07"),
            (S04, "1.16614003333790000205e-09"),
            (U00, "-7.38042951086872317523e-02"),
            (U01, "1.76666452509181115538e-01"),
            (U02, "-1.38185671945596898896e-02"),
            (U03, "3.47453432093683650238e-04"),
            (U04, "-3.81407053724364161125e-06"),
            (U05, "1.95590137035022920206e-08"),
            (U06, "-3.98205194132103398453e-11"),
            (V01, "1.27304834834123699328e-02"),
            (V02, "7.60068627350353253702e-05"),
            (V03, "2.59150851840457805467e-07"),
            (V04, "4.41110311332675467403e-10"),
            (P_R8[0], "0.00000000000000000000e+00"),
            (P_R8[1], "-7.03124999999900357484e-02"),
            (P_R8[2], "-8.08167041275349795626e+00"),
            (P_R8[3], "-2.57063105679704847262e+02"),
            (P_R8[4], "-2.48521641009428822144e+03"),
            (P_R8[5], "-5.25304380490729545272e+03"),
            (P_S8[0], "1.16534364619668181717e+02"),
            (P_S8[1], "3.83374475364121826715e+03"),
            (P_S8[2], "4.05978572648472545552e+04"),
            (P_S8[3], "1.16752972564375915681e+05"),
            (P_S8[4], "4.76277284146730962675e+04"),
            (P_R5[0], "-1.14125464691894502584e-11"),
            (P_R5[1], "-7.03124940873599280078e-02"),
            (P_R5[2], "-4.15961064470587782438e+00"),
            (P_R5[3], "-6.76747652265167261021e+01"),
            (P_R5[4], "-3.31231299649172967747e+02"),
            (P_R5[5], "-3.46433388365604912451e+02"),
            (P_S5[0], "6.07539382692300335975e+01"),
            (P_S5[1], "1.05125230595704579173e+03"),
            (P_S5[2], "5.97897094333855784498e+03"),
            (P_S5[3], "9.62544514357774460223e+03"),
            (P_S5[4], "2.40605815922939109441e+03"),
            (P_R3[0], "-2.54704601771951915620e-09"),
            (P_R3[1], "-7.03119616381481654654e-02"),
            (P_R3[2], "-2.40903221549529611423e+00"),
            (P_R3[3], "-2.19659774734883086467e+01"),
            (P_R3[4], "-5.80791704701737572236e+01"),
            (P_R3[5], "-3.14479470594888503854e+01"),
            (P_S3[0], "3.58560338055209726349e+01"),
            (P_S3[1], "3.61513983050303863820e+02"),
            (P_S3[2], "1.19360783792111533330e+03"),
            (P_S3[3], "1.12799679856907414432e+03"),
            (P_S3[4], "1.73580930813335754692e+02"),
            (P_R2[0], "-8.87534333032526411254e-08"),
            (P_R2[1], "-7.03030995483624743247e-02"),
            (P_R2[2], "-1.45073846780952986357e+00"),
            (P_R2[3], "-7.63569613823527770791e+00"),
            (P_R2[4], "-1.11931668860356747786e+01"),
            (P_R2[5], "-3.23364579351335335033e+00"),
            (P_S2[0], "2.22202997532088808441e+01"),
            (P_S2[1], "1.36206794218215208048e+02"),
            (P_S2[2], "2.70470278658083486789e+02"),
            (P_S2[3], "1.53875394208320329881e+02"),
            (P_S2[4], "1.46576176948256193810e+01"),
            (Q_R8[0], "0.00000000000000000000e+00"),
            (Q_R8[1], "7.32421874999935051953e-02"),
            (Q_R8[2], "1.17682064682252693899e+01"),
            (Q_R8[3], "5.57673380256401856059e+02"),
            (Q_R8[4], "8.85919720756468632317e+03"),
            (Q_R8[5], "3.70146267776887834771e+04"),
            (Q_S8[0], "1.63776026895689824414e+02"),
            (Q_S8[1], "8.09834494656449805916e+03"),
            (Q_S8[2], "1.42538291419120476348e+05"),
            (Q_S8[3], "8.03309257119514397345e+05"),
            (Q_S8[4], "8.40501579819060512818e+05"),
            (Q_S8[5], "-3.43899293537866615225e+05"),
            (Q_R5[0], "1.84085963594515531381e-11"),
            (Q_R5[1], "7.32421766612684765896e-02"),
            (Q_R5[2], "5.83563508962056953777e+00"),
            (Q_R5[3], "1.35111577286449829671e+02"),
            (Q_R5[4], "1.02724376596164097464e+03"),
            (Q_R5[5], "1.98997785864605384631e+03"),
            (Q_S5[0], "8.27766102236537761883e+01"),
            (Q_S5[1], "2.07781416421392987104e+03"),
            (Q_S5[2], "1.88472887785718085070e+04"),
            (Q_S5[3], "5.67511122894947329769e+04"),
            (Q_S5[4], "3.59767538425114471465e+04"),
            (Q_S5[5], "-5.35434275601944773371e+03"),
            (Q_R3[0], "4.37741014089738620906e-09"),
            (Q_R3[1], "7.32411180042911447163e-02"),
            (Q_R3[2], "3.34423137516170720929e+00"),
            (Q_R3[3], "4.26218440745412650017e+01"),
            (Q_R3[4], "1.70808091340565596283e+02"),
            (Q_R3[5], "1.66733948696651168575e+02"),
            (Q_S3[0], "4.87588729724587182091e+01"),
            (Q_S3[1], "7.09689221056606015736e+02"),
            (Q_S3[2], "3.70414822620111362994e+03"),
            (Q_S3[3], "6.46042516752568917582e+03"),
            (Q_S3[4], "2.51633368920368957333e+03"),
            (Q_S3[5], "-1.49247451836156386662e+02"),
            (Q_R2[0], "1.50444444886983272379e-07"),
            (Q_R2[1], "7.32234265963079278272e-02"),
            (Q_R2[2], "1.99819174093815998816e+00"),
            (Q_R2[3], "1.44956029347885735348e+01"),
            (Q_R2[4], "3.16662317504781540833e+01"),
            (Q_R2[5], "1.62527075710929267416e+01"),
            (Q_S2[0], "3.03655848355219184498e+01"),
            (Q_S2[1], "2.69348118608049844624e+02"),
            (Q_S2[2], "8.44783757595320139444e+02"),
            (Q_S2[3], "8.82935845112488550512e+02"),
            (Q_S2[4], "2.12666388511798828631e+02"),
            (Q_S2[5], "-5.31095493882666946917e+00"),
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
