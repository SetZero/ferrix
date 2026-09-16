//! `j1` and `y1`: the Bessel functions of the first and second kinds of order
//! one.
//!
//! Ported from musl 1.2.5's `j1.c` (MIT; see [`crate::math`] for the notice).
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
//! Below 2, j1(x) = x/2 + x·x²·R(x²)/S(x²), and y1(x) = x·U(x²)/V(x²) +
//! (2/π)·(j1(x)·log(x) - 1/x). From 2 up, j1(x) = √(2/(πx))·(p1(x)·cos(x1) -
//! q1(x)·sin(x1)) and y1(x) = √(2/(πx))·(p1(x)·sin(x1) + q1(x)·cos(x1)), with
//! x1 = x - 3π/4 and rational approximations p1 and q1 in 1/x² on four
//! intervals. cos(x1) = (sin(x) - cos(x))/√2 and sin(x1) = -(sin(x) +
//! cos(x))/√2, and whichever of the two cancels is computed from cos(2x)
//! instead.

use crate::math::log::log;
use crate::math::manipulate::fabs;
use crate::math::sqrt::sqrt;
use crate::math::support::{barrier, divzero, high_word, low_word};
use crate::math::trig::{cos, sin};

/// 1/√π.
const INVSQRTPI: f64 = f64::from_bits(0x3fe2_0dd7_5042_9b6d);
/// 2/π.
const TPI: f64 = f64::from_bits(0x3fe4_5f30_6dc9_c883);

// j1 below 2: R's numerator and S's.
const R00: f64 = f64::from_bits(0xbfb0_0000_0000_0000);
const R01: f64 = f64::from_bits(0x3f57_0d9f_9847_2c61);
const R02: f64 = f64::from_bits(0xbef0_c5c6_ba16_9668);
const R03: f64 = f64::from_bits(0x3e6a_aafa_46ca_0bd9);
const S01: f64 = f64::from_bits(0x3f93_9d0b_1263_7e53);
const S02: f64 = f64::from_bits(0x3f28_5f56_b9cd_f664);
const S03: f64 = f64::from_bits(0x3eb3_bff8_333f_8498);
const S04: f64 = f64::from_bits(0x3e35_ac88_c97d_ff2c);
const S05: f64 = f64::from_bits(0x3dab_2acf_cfb9_7ed8);

// y1 below 2: U's numerator and V's.
const U0: [f64; 5] = [
    f64::from_bits(0xbfc9_1866_143c_bc8a),
    f64::from_bits(0x3fa9_d3c7_7629_2cd1),
    f64::from_bits(0xbf5f_55e5_4844_f50f),
    f64::from_bits(0x3ef8_ab03_8fa6_b88e),
    f64::from_bits(0xbe78_ac00_5691_05b8),
];
const V0: [f64; 5] = [
    f64::from_bits(0x3f94_650d_3f4d_a9f0),
    f64::from_bits(0x3f2a_8c89_6c25_7764),
    f64::from_bits(0x3eb6_c05a_894e_8ca6),
    f64::from_bits(0x3e3a_bf1d_5ba6_9a86),
    f64::from_bits(0x3db2_5039_daca_772a),
];

// p1(x) - 1 = R/S in 1/x², on [8, inf], [4.5454, 8], [2.8571, 4.5454] and
// [2, 2.8571].
const PR8: [f64; 6] = [
    f64::from_bits(0x0000_0000_0000_0000),
    f64::from_bits(0x3fbd_ffff_ffff_fcce),
    f64::from_bits(0x402a_7a9d_357f_7fce),
    f64::from_bits(0x4079_c0d4_652e_a590),
    f64::from_bits(0x40ae_457d_a3a5_32cc),
    f64::from_bits(0x40be_ea7a_c327_82dd),
];
const PS8: [f64; 5] = [
    f64::from_bits(0x405c_8d45_8e65_6cac),
    f64::from_bits(0x40ac_85dc_964d_274f),
    f64::from_bits(0x40e2_0b86_97c5_bb7f),
    f64::from_bits(0x40f7_d42c_b28f_17bb),
    f64::from_bits(0x40de_1511_697a_0b2d),
];
const PR5: [f64; 6] = [
    f64::from_bits(0x3dad_0667_dae1_ca7d),
    f64::from_bits(0x3fbd_ffff_e2c1_0043),
    f64::from_bits(0x401b_3604_6e63_15e3),
    f64::from_bits(0x405b_13b9_4526_02ed),
    f64::from_bits(0x4080_2d16_d052_d649),
    f64::from_bits(0x4080_85b8_bb7e_0cb7),
];
const PS5: [f64; 5] = [
    f64::from_bits(0x404d_a3ea_a8af_633d),
    f64::from_bits(0x408e_fb36_1b06_6701),
    f64::from_bits(0x40b4_e944_5706_b6fb),
    f64::from_bits(0x40be_a4b0_b8a5_bb15),
    f64::from_bits(0x4097_8030_036f_5e51),
];
const PR3: [f64; 6] = [
    f64::from_bits(0x3e29_fc21_a7ad_9edd),
    f64::from_bits(0x3fbd_fff5_5b21_d17b),
    f64::from_bits(0x400f_76bc_e85e_ad8a),
    f64::from_bits(0x4041_8f48_9da6_d129),
    f64::from_bits(0x4056_c385_4d2c_1837),
    f64::from_bits(0x4048_478f_8ea8_3ee5),
];
const PS3: [f64; 5] = [
    f64::from_bits(0x4041_6549_a134_069c),
    f64::from_bits(0x4075_0c33_07f1_a75f),
    f64::from_bits(0x4090_5b7c_5037_d523),
    f64::from_bits(0x408b_d67d_a32e_31e9),
    f64::from_bits(0x4059_f26d_7c2e_ed53),
];
const PR2: [f64; 6] = [
    f64::from_bits(0x3e7c_e9d4_f655_44f4),
    f64::from_bits(0x3fbd_ff42_be76_0d83),
    f64::from_bits(0x4002_f2b7_f98f_aec0),
    f64::from_bits(0x4028_7c37_7f71_a964),
    f64::from_bits(0x4031_b1a8_177f_8ee2),
    f64::from_bits(0x4014_4b49_a574_c1fe),
];
const PS2: [f64; 5] = [
    f64::from_bits(0x4035_6fbd_8ad5_ecdc),
    f64::from_bits(0x405f_5293_14f9_2cd5),
    f64::from_bits(0x406d_08d8_d5a2_dbd9),
    f64::from_bits(0x405d_6b7a_da18_84a9),
    f64::from_bits(0x4020_bab1_f44e_5192),
];

// q1(x)·x - 3/8 = R/S in 1/x², on the same intervals.
const QR8: [f64; 6] = [
    f64::from_bits(0x0000_0000_0000_0000),
    f64::from_bits(0xbfba_3fff_ffff_fdf3),
    f64::from_bits(0xc030_4591_a267_79f7),
    f64::from_bits(0xc087_bcd0_53e4_b576),
    f64::from_bits(0xc0c7_24e7_40f8_7415),
    f64::from_bits(0xc0e7_a6d0_65d0_9c6a),
];
const QS8: [f64; 6] = [
    f64::from_bits(0x4064_2ca6_de5b_cde5),
    f64::from_bits(0x40be_9162_d0d8_8419),
    f64::from_bits(0x4100_579a_b0b7_5e98),
    f64::from_bits(0x4125_f653_7286_9c19),
    f64::from_bits(0x4124_57d2_7719_ad5c),
    f64::from_bits(0xc111_f969_0ea5_aa18),
];
const QR5: [f64; 6] = [
    f64::from_bits(0xbdb6_fa43_1aa1_a098),
    f64::from_bits(0xbfba_3fff_cb59_7fef),
    f64::from_bits(0xc020_1ce6_ca03_ad4b),
    f64::from_bits(0xc066_f56d_6ca7_b9b0),
    f64::from_bits(0xc095_74c6_6931_734f),
    f64::from_bits(0xc0a4_68e3_88fd_a79d),
];
const QS5: [f64; 6] = [
    f64::from_bits(0x4054_51b2_ff5a_11b2),
    f64::from_bits(0x409f_1f31_e77b_f839),
    f64::from_bits(0x40d1_0f1f_0d64_ce29),
    f64::from_bits(0x40e8_576d_aaba_d197),
    f64::from_bits(0x40db_4b04_cf7c_364b),
    f64::from_bits(0xc0b2_6f2e_fcff_a004),
];
const QR3: [f64; 6] = [
    f64::from_bits(0xbe35_cfa9_d38f_c84f),
    f64::from_bits(0xbfba_3feb_51ae_ed54),
    f64::from_bits(0xc012_70c2_3302_d9ff),
    f64::from_bits(0xc04c_ec71_c25d_16da),
    f64::from_bits(0xc06c_87d3_4718_d55f),
    f64::from_bits(0xc06b_66b9_5f5c_1bf6),
];
const QS3: [f64; 6] = [
    f64::from_bits(0x4047_d523_ccd3_67e4),
    f64::from_bits(0x4085_0eeb_c031_ee3e),
    f64::from_bits(0x40aa_684e_448e_7c9a),
    f64::from_bits(0x40b5_abba_a61d_54a6),
    f64::from_bits(0x409d_bc7a_0dd4_df4b),
    f64::from_bits(0xc060_e670_290a_311f),
];
const QR2: [f64; 6] = [
    f64::from_bits(0xbe87_f126_44c6_26d2),
    f64::from_bits(0xbfba_3e8e_9148_b010),
    f64::from_bits(0xc006_0484_69bb_4eda),
    f64::from_bits(0xc033_a9e2_c168_907f),
    f64::from_bits(0xc045_29a3_de10_4aaa),
    f64::from_bits(0xc035_5f36_39cf_6e52),
];
const QS2: [f64; 6] = [
    f64::from_bits(0x403d_888a_78ae_64ff),
    f64::from_bits(0x406f_9f68_db82_1cba),
    f64::from_bits(0x4087_ac05_ce49_a0f7),
    f64::from_bits(0x4087_1b25_48d4_c029),
    f64::from_bits(0x4063_7e5e_3c3e_d8d4),
    f64::from_bits(0xc013_d686_e71b_e86b),
];

/// ±j1(`x`), or y1(x) if `y1` is set, for `x` >= 2 whose high word is `ix`,
/// negated if `sign` is set. musl's `common`.
fn common(ix: u32, x: f64, y1: bool, sign: bool) -> f64 {
    let mut s = sin(x);
    if y1 {
        s = -s;
    }
    let c = cos(x);
    let mut cc = s - c;
    if ix < 0x7fe0_0000 {
        // Where 2x does not overflow.
        let mut ss = -s - c;
        let z = cos(2.0 * x);
        if s * c > 0.0 {
            cc = z / ss;
        } else {
            ss = z / cc;
        }
        if ix < 0x4800_0000 {
            if y1 {
                ss = -ss;
            }
            cc = pone(x) * cc - qone(x) * ss;
        }
    }
    if sign {
        cc = -cc;
    }
    INVSQRTPI * cc / sqrt(x)
}

/// The Bessel function of the first kind of order one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn j1(x: f64) -> f64 {
    let hx = high_word(x);
    let sign = hx >> 31 != 0;
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        return 1.0 / (x * x);
    }
    if ix >= 0x4000_0000 {
        // |x| >= 2.
        return common(ix, fabs(x), false, sign);
    }
    let z = if ix >= 0x3800_0000 {
        // |x| >= 2^-127.
        let z = x * x;
        let r = z * (R00 + z * (R01 + z * (R02 + z * R03)));
        let s = 1.0 + z * (S01 + z * (S02 + z * (S03 + z * (S04 + z * S05))));
        r / s
    } else {
        // Avoids underflow, and raises inexact if x is not 0.
        x
    };
    (0.5 + z) * x
}

/// The Bessel function of the second kind of order one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn y1(x: f64) -> f64 {
    let ix = high_word(x);
    let lx = low_word(x);

    // y1(NaN) is NaN, y1(x < 0) is NaN, y1(0) is -inf and y1(inf) is 0.
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
        // x >= 2.
        return common(ix, x, true, false);
    }
    if ix < 0x3c90_0000 {
        // x < 2^-54.
        return -TPI / x;
    }
    let [u0, u1, u2, u3, u4] = U0;
    let [v0, v1, v2, v3, v4] = V0;
    let z = x * x;
    let u = u0 + z * (u1 + z * (u2 + z * (u3 + z * u4)));
    let v = 1.0 + z * (v0 + z * (v1 + z * (v2 + z * (v3 + z * v4))));
    // LLVM would rewrite `a - 1.0 / x` as `a + -1.0 / x`, which rounds the
    // quotient the other way. See `crate::math`.
    x * (u / v) + TPI * (j1(x) * log(x) - barrier(1.0 / x))
}

/// p1(`x`) for `x` >= 2. musl's `pone`.
fn pone(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4020_0000 {
        (&PR8, &PS8)
    } else if ix >= 0x4012_2e8b {
        (&PR5, &PS5)
    } else if ix >= 0x4006_db6d {
        (&PR3, &PS3)
    } else {
        (&PR2, &PS2)
    };
    let [p0, p1, p2, p3, p4, p5] = *p;
    let [q0, q1, q2, q3, q4] = *q;
    let z = 1.0 / (x * x);
    let r = p0 + z * (p1 + z * (p2 + z * (p3 + z * (p4 + z * p5))));
    let s = 1.0 + z * (q0 + z * (q1 + z * (q2 + z * (q3 + z * q4))));
    1.0 + r / s
}

/// q1(`x`) for `x` >= 2. musl's `qone`.
fn qone(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4020_0000 {
        (&QR8, &QS8)
    } else if ix >= 0x4012_2e8b {
        (&QR5, &QS5)
    } else if ix >= 0x4006_db6d {
        (&QR3, &QS3)
    } else {
        (&QR2, &QS2)
    };
    let [p0, p1, p2, p3, p4, p5] = *p;
    let [q0, q1, q2, q3, q4, q5] = *q;
    let z = 1.0 / (x * x);
    let r = p0 + z * (p1 + z * (p2 + z * (p3 + z * (p4 + z * p5))));
    let s = 1.0 + z * (q0 + z * (q1 + z * (q2 + z * (q3 + z * (q4 + z * q5)))));
    (0.375 + r / s) / x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn j1_matches_libc_test() {
        let files = ["sanity/j1.h", "special/j1.h"];
        mtest::d_d("j1", &files, |x| j1(x), Rules::ULP, &[]);
    }

    #[test]
    fn y1_matches_libc_test() {
        let files = ["sanity/y1.h", "special/y1.h"];
        // libc-test's `y1.c` wants a NaN or -inf for a negative argument.
        let rules = Rules::ULP.negative_domain();
        mtest::d_d("y1", &files, |x| y1(x), rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (INVSQRTPI, "5.64189583547756279280e-01"),
            (TPI, "6.36619772367581382433e-01"),
            (R00, "-6.25000000000000000000e-02"),
            (R01, "1.40705666955189706048e-03"),
            (R02, "-1.59955631084035597520e-05"),
            (R03, "4.96727999609584448412e-08"),
            (S01, "1.91537599538363460805e-02"),
            (S02, "1.85946785588630915560e-04"),
            (S03, "1.17718464042623683263e-06"),
            (S04, "5.04636257076217042715e-09"),
            (S05, "1.23542274426137913908e-11"),
            (U0[0], "-1.96057090646238940668e-01"),
            (U0[1], "5.04438716639811282616e-02"),
            (U0[2], "-1.91256895875763547298e-03"),
            (U0[3], "2.35252600561610495928e-05"),
            (U0[4], "-9.19099158039878874504e-08"),
            (V0[0], "1.99167318236649903973e-02"),
            (V0[1], "2.02552581025135171496e-04"),
            (V0[2], "1.35608801097516229404e-06"),
            (V0[3], "6.22741452364621501295e-09"),
            (V0[4], "1.66559246207992079114e-11"),
            (PR8[0], "0.00000000000000000000e+00"),
            (PR8[1], "1.17187499999988647970e-01"),
            (PR8[2], "1.32394806593073575129e+01"),
            (PR8[3], "4.12051854307378562225e+02"),
            (PR8[4], "3.87474538913960532227e+03"),
            (PR8[5], "7.91447954031891731574e+03"),
            (PS8[0], "1.14207370375678408436e+02"),
            (PS8[1], "3.65093083420853463394e+03"),
            (PS8[2], "3.69562060269033463555e+04"),
            (PS8[3], "9.76027935934950801311e+04"),
            (PS8[4], "3.08042720627888811578e+04"),
            (PR5[0], "1.31990519556243522749e-11"),
            (PR5[1], "1.17187493190614097638e-01"),
            (PR5[2], "6.80275127868432871736e+00"),
            (PR5[3], "1.08308182990189109773e+02"),
            (PR5[4], "5.17636139533199752805e+02"),
            (PR5[5], "5.28715201363337541807e+02"),
            (PS5[0], "5.92805987221131331921e+01"),
            (PS5[1], "9.91401418733614377743e+02"),
            (PS5[2], "5.35326695291487976647e+03"),
            (PS5[3], "7.84469031749551231769e+03"),
            (PS5[4], "1.50404688810361062679e+03"),
            (PR3[0], "3.02503916137373618024e-09"),
            (PR3[1], "1.17186865567253592491e-01"),
            (PR3[2], "3.93297750033315640650e+00"),
            (PR3[3], "3.51194035591636932736e+01"),
            (PR3[4], "9.10550110750781271918e+01"),
            (PR3[5], "4.85590685197364919645e+01"),
            (PS3[0], "3.47913095001251519989e+01"),
            (PS3[1], "3.36762458747825746741e+02"),
            (PS3[2], "1.04687139975775130551e+03"),
            (PS3[3], "8.90811346398256432622e+02"),
            (PS3[4], "1.03787932439639277504e+02"),
            (PR2[0], "1.07710830106873743082e-07"),
            (PR2[1], "1.17176219462683348094e-01"),
            (PR2[2], "2.36851496667608785174e+00"),
            (PR2[3], "1.22426109148261232917e+01"),
            (PR2[4], "1.76939711271687727390e+01"),
            (PR2[5], "5.07352312588818499250e+00"),
            (PS2[0], "2.14364859363821409488e+01"),
            (PS2[1], "1.25290227168402751090e+02"),
            (PS2[2], "2.32276469057162813669e+02"),
            (PS2[3], "1.17679373287147100768e+02"),
            (PS2[4], "8.36463893371618283368e+00"),
            (QR8[0], "0.00000000000000000000e+00"),
            (QR8[1], "-1.02539062499992714161e-01"),
            (QR8[2], "-1.62717534544589987888e+01"),
            (QR8[3], "-7.59601722513950107896e+02"),
            (QR8[4], "-1.18498066702429587167e+04"),
            (QR8[5], "-4.84385124285750353010e+04"),
            (QS8[0], "1.61395369700722909556e+02"),
            (QS8[1], "7.82538599923348465381e+03"),
            (QS8[2], "1.33875336287249578163e+05"),
            (QS8[3], "7.19657723683240939863e+05"),
            (QS8[4], "6.66601232617776375264e+05"),
            (QS8[5], "-2.94490264303834643215e+05"),
            (QR5[0], "-2.08979931141764104297e-11"),
            (QR5[1], "-1.02539050241375426231e-01"),
            (QR5[2], "-8.05644828123936029840e+00"),
            (QR5[3], "-1.83669607474888380239e+02"),
            (QR5[4], "-1.37319376065508163265e+03"),
            (QR5[5], "-2.61244440453215656817e+03"),
            (QS5[0], "8.12765501384335777857e+01"),
            (QS5[1], "1.99179873460485964642e+03"),
            (QS5[2], "1.74684851924908907677e+04"),
            (QS5[3], "4.98514270910352279316e+04"),
            (QS5[4], "2.79480751638918118260e+04"),
            (QS5[5], "-4.71918354795128470869e+03"),
            (QR3[0], "-5.07831226461766561369e-09"),
            (QR3[1], "-1.02537829820837089745e-01"),
            (QR3[2], "-4.61011581139473403113e+00"),
            (QR3[3], "-5.78472216562783643212e+01"),
            (QR3[4], "-2.28244540737631695038e+02"),
            (QR3[5], "-2.19210128478909325622e+02"),
            (QS3[0], "4.76651550323729509273e+01"),
            (QS3[1], "6.73865112676699709482e+02"),
            (QS3[2], "3.38015286679526343505e+03"),
            (QS3[3], "5.54772909720722782367e+03"),
            (QS3[4], "1.90311919338810798763e+03"),
            (QS3[5], "-1.35201191444307340817e+02"),
            (QR2[0], "-1.78381727510958865572e-07"),
            (QR2[1], "-1.02517042607985553460e-01"),
            (QR2[2], "-2.75220568278187460720e+00"),
            (QR2[3], "-1.96636162643703720221e+01"),
            (QR2[4], "-4.23253133372830490089e+01"),
            (QR2[5], "-2.13719211703704061733e+01"),
            (QS2[0], "2.95333629060523854548e+01"),
            (QS2[1], "2.52981549982190529136e+02"),
            (QS2[2], "7.57502834868645436472e+02"),
            (QS2[3], "7.39393205320467245656e+02"),
            (QS2[4], "1.55949003336666123687e+02"),
            (QS2[5], "-4.95949898822628210127e+00"),
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
