//! `j0f` and `y0f`: [`j0`](crate::math::j0::j0) and
//! [`y0`](crate::math::j0::y0) for `float`.
//!
//! Ported from musl 1.2.5's `j0f.c` (MIT; see [`crate::math`] for the
//! notice). musl took it from FreeBSD's msun, where Ian Lance Taylor, Cygnus
//! Support, converted it to `float`. The msun file carries this notice:
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
//! As for `double`, in `float` arithmetic.

use core::hint::black_box;

use crate::math::logf::logf;
use crate::math::manipulate::fabsf;
use crate::math::sqrt::sqrtf;
use crate::math::support::{barrierf, divzerof};
use crate::math::trigf::{cosf, sinf};

/// 1/√π.
const INVSQRTPI: f32 = f32::from_bits(0x3f10_6ebb);
/// 2/π.
const TPI: f32 = f32::from_bits(0x3f22_f983);

// j0f below 2: R's numerator and S's.
const R02: f32 = f32::from_bits(0x3c80_0000);
const R03: f32 = f32::from_bits(0xb947_352e);
const R04: f32 = f32::from_bits(0x35f5_8e88);
const R05: f32 = f32::from_bits(0xb19e_af3c);
const S01: f32 = f32::from_bits(0x3c7f_e744);
const S02: f32 = f32::from_bits(0x38f5_3697);
const S03: f32 = f32::from_bits(0x3509_daa6);
const S04: f32 = f32::from_bits(0x30a0_45e8);

// y0f below 2: U's numerator and V's.
const U00: f32 = f32::from_bits(0xbd97_26b5);
const U01: f32 = f32::from_bits(0x3e34_e80d);
const U02: f32 = f32::from_bits(0xbc62_6746);
const U03: f32 = f32::from_bits(0x39b6_2a69);
const U04: f32 = f32::from_bits(0xb67f_f53c);
const U05: f32 = f32::from_bits(0x32a8_02ba);
const U06: f32 = f32::from_bits(0xae2f_21eb);
const V01: f32 = f32::from_bits(0x3c50_9385);
const V02: f32 = f32::from_bits(0x389f_65e0);
const V03: f32 = f32::from_bits(0x348b_216c);
const V04: f32 = f32::from_bits(0x2ff2_80c2);

// p0(x) - 1 = R/S in 1/x², on [8, inf], [4.5454, 8], [2.8571, 4.5454] and
// [2, 2.8571].
const P_R8: [f32; 6] = [
    f32::from_bits(0x0000_0000),
    f32::from_bits(0xbd90_0000),
    f32::from_bits(0xc101_4e86),
    f32::from_bits(0xc380_8814),
    f32::from_bits(0xc51b_5376),
    f32::from_bits(0xc5a4_285a),
];
const P_S8: [f32; 5] = [
    f32::from_bits(0x42e9_1198),
    f32::from_bits(0x456f_9beb),
    f32::from_bits(0x471e_95db),
    f32::from_bits(0x47e4_087c),
    f32::from_bits(0x473a_0bba),
];
const P_R5: [f32; 6] = [
    f32::from_bits(0xad48_c58a),
    f32::from_bits(0xbd8f_ffff),
    f32::from_bits(0xc085_1b88),
    f32::from_bits(0xc287_597b),
    f32::from_bits(0xc3a5_9d9b),
    f32::from_bits(0xc3ad_3779),
];
const P_S5: [f32; 5] = [
    f32::from_bits(0x4273_0408),
    f32::from_bits(0x4483_6813),
    f32::from_bits(0x45ba_d7c4),
    f32::from_bits(0x4616_65c8),
    f32::from_bits(0x4516_60ee),
];
const P_R3: [f32; 6] = [
    f32::from_bits(0xb12f_081b),
    f32::from_bits(0xbd8f_ffb8),
    f32::from_bits(0xc01a_2d95),
    f32::from_bits(0xc1af_ba52),
    f32::from_bits(0xc268_5112),
    f32::from_bits(0xc1fb_9565),
];
const P_S3: [f32; 5] = [
    f32::from_bits(0x420f_6c94),
    f32::from_bits(0x43b4_c1ca),
    f32::from_bits(0x4495_3373),
    f32::from_bits(0x448c_ffe6),
    f32::from_bits(0x432d_94b8),
];
const P_R2: [f32; 6] = [
    f32::from_bits(0xb3be_98b7),
    f32::from_bits(0xbd8f_fb12),
    f32::from_bits(0xbfb9_b1cc),
    f32::from_bits(0xc0f4_579f),
    f32::from_bits(0xc133_1736),
    f32::from_bits(0xc04e_f40d),
];
const P_S2: [f32; 5] = [
    f32::from_bits(0x41b1_c32d),
    f32::from_bits(0x4308_34f0),
    f32::from_bits(0x4387_3c32),
    f32::from_bits(0x4319_e01a),
    f32::from_bits(0x416a_859a),
];

// q0(x)·x + 1/8 = R/S in 1/x², on the same intervals.
const Q_R8: [f32; 6] = [
    f32::from_bits(0x0000_0000),
    f32::from_bits(0x3d96_0000),
    f32::from_bits(0x413c_4a93),
    f32::from_bits(0x440b_6b19),
    f32::from_bits(0x460a_6cca),
    f32::from_bits(0x4710_96a0),
];
const Q_S8: [f32; 6] = [
    f32::from_bits(0x4323_c6aa),
    f32::from_bits(0x45fd_12c2),
    f32::from_bits(0x480b_3293),
    f32::from_bits(0x4944_1ed4),
    f32::from_bits(0x494d_3359),
    f32::from_bits(0xc8a7_eb69),
];
const Q_R5: [f32; 6] = [
    f32::from_bits(0x2da1_ec79),
    f32::from_bits(0x3d95_ffff),
    f32::from_bits(0x40ba_bd86),
    f32::from_bits(0x4307_1c90),
    f32::from_bits(0x4480_67cd),
    f32::from_bits(0x44f8_bf4b),
];
const Q_S5: [f32; 6] = [
    f32::from_bits(0x42a5_8da0),
    f32::from_bits(0x4501_dd07),
    f32::from_bits(0x4693_3e94),
    f32::from_bits(0x475d_af1d),
    f32::from_bits(0x470c_88c1),
    f32::from_bits(0xc5a7_52be),
];
const Q_R3: [f32; 6] = [
    f32::from_bits(0x3196_681b),
    f32::from_bits(0x3d95_ff70),
    f32::from_bits(0x4056_07e3),
    f32::from_bits(0x422a_7cc5),
    f32::from_bits(0x432a_cedf),
    f32::from_bits(0x4326_bbe4),
];
const Q_S3: [f32; 6] = [
    f32::from_bits(0x4243_0916),
    f32::from_bits(0x4431_6c1c),
    f32::from_bits(0x4567_825f),
    f32::from_bits(0x45c9_e367),
    f32::from_bits(0x451d_4557),
    f32::from_bits(0xc315_3f59),
];
const Q_R2: [f32; 6] = [
    f32::from_bits(0x3421_89db),
    f32::from_bits(0x3d95_f62a),
    f32::from_bits(0x3fff_c4bf),
    f32::from_bits(0x4167_edfd),
    f32::from_bits(0x41fd_5471),
    f32::from_bits(0x4182_058c),
];
const Q_S2: [f32; 6] = [
    f32::from_bits(0x41f2_ecb8),
    f32::from_bits(0x4386_ac8f),
    f32::from_bits(0x4453_3229),
    f32::from_bits(0x445c_bbe5),
    f32::from_bits(0x4354_aa98),
    f32::from_bits(0xc0a9_f358),
];

/// `c[0] + z·(c[1] + z·(c[2] + ...))`: the polynomial with coefficients `c`
/// at `z`, by Horner's rule, in the order musl writes it out.
///
/// Never inlined: where two `float` polynomials in the same z are written out
/// side by side, LLVM's SLP vectoriser evaluates them in two lanes of one
/// `mulps` and `addps`. The other two lanes hold other coefficients or stale
/// register contents, and multiplying those by a small z again and again
/// raises underflow that musl does not. Evaluated here, one at a time, each
/// polynomial is a chain of scalar operations with nothing to pair.
#[inline(never)]
pub(crate) fn polyf(z: f32, c: &[f32]) -> f32 {
    let mut terms = c.iter().rev();
    let highest = terms.next().copied().unwrap_or(0.0);
    terms.fold(highest, |sum, &term| term + z * sum)
}

/// j0f(`x`), or y0f(x) if `y0` is set, for `x` >= 2 whose word is `ix`.
/// musl's `common`.
fn common(ix: u32, x: f32, y0: bool) -> f32 {
    let s = sinf(x);
    let mut c = cosf(x);
    if y0 {
        c = -c;
    }
    let mut cc = s + c;
    if ix < 0x7f00_0000 {
        let mut ss = s - c;
        let z = -cosf(2.0 * x);
        if s * c < 0.0 {
            cc = z / ss;
        } else {
            ss = z / cc;
        }
        if ix < 0x5880_0000 {
            if y0 {
                ss = -ss;
            }
            cc = pzerof(x) * cc - qzerof(x) * ss;
        }
    }
    INVSQRTPI * cc / sqrtf(x)
}

/// [`j0`](crate::math::j0::j0) for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn j0f(x: f32) -> f32 {
    let ix = x.to_bits() & 0x7fff_ffff;
    if ix >= 0x7f80_0000 {
        return 1.0 / (x * x);
    }
    let x = fabsf(x);

    if ix >= 0x4000_0000 {
        // |x| >= 2. The ulp error is large near the zeros.
        return common(ix, x, false);
    }
    if ix >= 0x3a00_0000 {
        // |x| >= 2^-11. Up to 4 ulps of error near 2.
        let z = x * x;
        let r = z * polyf(z, &[R02, R03, R04, R05]);
        let s = polyf(z, &[1.0, S01, S02, S03, S04]);
        return (1.0 + x / 2.0) * (1.0 - x / 2.0) + z * (r / s);
    }
    let x = if ix >= 0x2180_0000 {
        // |x| >= 2^-60.
        0.25 * x * x
    } else {
        x
    };
    1.0 - x
}

/// [`y0`](crate::math::j0::y0) for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn y0f(x: f32) -> f32 {
    // The bits go through `black_box`: LLVM would test them for ±0 with
    // `ucomiss`, which raises denormal for a subnormal x. See `crate::math`.
    let ix = black_box(x.to_bits());
    if ix & 0x7fff_ffff == 0 {
        return divzerof(1);
    }
    if ix >> 31 != 0 {
        return barrierf(0.0) / 0.0;
    }
    if ix >= 0x7f80_0000 {
        return 1.0 / x;
    }
    if ix >= 0x4000_0000 {
        // x >= 2. The ulp error is large near the zeros.
        return common(ix, x, true);
    }
    if ix >= 0x3900_0000 {
        // x >= 2^-13. The ulp error is large near 0.89.
        let z = x * x;
        let u = polyf(z, &[U00, U01, U02, U03, U04, U05, U06]);
        let v = polyf(z, &[1.0, V01, V02, V03, V04]);
        return u / v + TPI * (j0f(x) * logf(x));
    }
    U00 + TPI * logf(x)
}

/// p0(`x`) for `x` >= 2. musl's `pzerof`.
fn pzerof(x: f32) -> f32 {
    let ix = x.to_bits() & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4100_0000 {
        (&P_R8, &P_S8)
    } else if ix >= 0x4091_73eb {
        (&P_R5, &P_S5)
    } else if ix >= 0x4036_d917 {
        (&P_R3, &P_S3)
    } else {
        (&P_R2, &P_S2)
    };
    let [q0, q1, q2, q3, q4] = *q;
    let z = 1.0 / (x * x);
    let r = polyf(z, p);
    let s = polyf(z, &[1.0, q0, q1, q2, q3, q4]);
    1.0 + r / s
}

/// q0(`x`) for `x` >= 2. musl's `qzerof`.
fn qzerof(x: f32) -> f32 {
    let ix = x.to_bits() & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4100_0000 {
        (&Q_R8, &Q_S8)
    } else if ix >= 0x4091_73eb {
        (&Q_R5, &Q_S5)
    } else if ix >= 0x4036_d917 {
        (&Q_R3, &Q_S3)
    } else {
        (&Q_R2, &Q_S2)
    };
    let [q0, q1, q2, q3, q4, q5] = *q;
    let z = 1.0 / (x * x);
    let r = polyf(z, p);
    let s = polyf(z, &[1.0, q0, q1, q2, q3, q4, q5]);
    (-0.125 + r / s) / x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};
    use crate::math::support::hexf32;

    #[test]
    fn j0f_matches_libc_test() {
        let files = ["sanity/j0f.h", "special/j0f.h"];
        // libc-test's `j0f.c` tolerates an error under 2^23 ulps.
        let rules = Rules::ULP.tolerate(hexf32!("0x1p23"));
        mtest::d_d("j0f", &files, |x| j0f(x), rules, &[]);
    }

    #[test]
    fn y0f_matches_libc_test() {
        let files = ["sanity/y0f.h", "special/y0f.h"];
        // libc-test's `y0f.c` wants a NaN or -inf for a negative argument, and
        // tolerates an error under 2^23 ulps for the others.
        let rules = Rules::ULP.tolerate(hexf32!("0x1p23")).negative_domain();
        mtest::d_d("y0f", &files, |x| y0f(x), rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let floats = [
            (INVSQRTPI, "5.6418961287e-01"),
            (TPI, "6.3661974669e-01"),
            (R02, "1.5625000000e-02"),
            (R03, "-1.8997929874e-04"),
            (R04, "1.8295404516e-06"),
            (R05, "-4.6183270541e-09"),
            (S01, "1.5619102865e-02"),
            (S02, "1.1692678527e-04"),
            (S03, "5.1354652442e-07"),
            (S04, "1.1661400734e-09"),
            (U00, "-7.3804296553e-02"),
            (U01, "1.7666645348e-01"),
            (U02, "-1.3818567619e-02"),
            (U03, "3.4745343146e-04"),
            (U04, "-3.8140706238e-06"),
            (U05, "1.9559013964e-08"),
            (U06, "-3.9820518410e-11"),
            (V01, "1.2730483897e-02"),
            (V02, "7.6006865129e-05"),
            (V03, "2.5915085189e-07"),
            (V04, "4.4111031494e-10"),
            (P_R8[0], "0.0000000000e+00"),
            (P_R8[1], "-7.0312500000e-02"),
            (P_R8[2], "-8.0816707611e+00"),
            (P_R8[3], "-2.5706311035e+02"),
            (P_R8[4], "-2.4852163086e+03"),
            (P_R8[5], "-5.2530439453e+03"),
            (P_S8[0], "1.1653436279e+02"),
            (P_S8[1], "3.8337448730e+03"),
            (P_S8[2], "4.0597855469e+04"),
            (P_S8[3], "1.1675296875e+05"),
            (P_S8[4], "4.7627726562e+04"),
            (P_R5[0], "-1.1412546255e-11"),
            (P_R5[1], "-7.0312492549e-02"),
            (P_R5[2], "-4.1596107483e+00"),
            (P_R5[3], "-6.7674766541e+01"),
            (P_R5[4], "-3.3123129272e+02"),
            (P_R5[5], "-3.4643338013e+02"),
            (P_S5[0], "6.0753936768e+01"),
            (P_S5[1], "1.0512523193e+03"),
            (P_S5[2], "5.9789707031e+03"),
            (P_S5[3], "9.6254453125e+03"),
            (P_S5[4], "2.4060581055e+03"),
            (P_R3[0], "-2.5470459075e-09"),
            (P_R3[1], "-7.0311963558e-02"),
            (P_R3[2], "-2.4090321064e+00"),
            (P_R3[3], "-2.1965976715e+01"),
            (P_R3[4], "-5.8079170227e+01"),
            (P_R3[5], "-3.1447946548e+01"),
            (P_S3[0], "3.5856033325e+01"),
            (P_S3[1], "3.6151397705e+02"),
            (P_S3[2], "1.1936077881e+03"),
            (P_S3[3], "1.1279968262e+03"),
            (P_S3[4], "1.7358093262e+02"),
            (P_R2[0], "-8.8753431271e-08"),
            (P_R2[1], "-7.0303097367e-02"),
            (P_R2[2], "-1.4507384300e+00"),
            (P_R2[3], "-7.6356959343e+00"),
            (P_R2[4], "-1.1193166733e+01"),
            (P_R2[5], "-3.2336456776e+00"),
            (P_S2[0], "2.2220300674e+01"),
            (P_S2[1], "1.3620678711e+02"),
            (P_S2[2], "2.7047027588e+02"),
            (P_S2[3], "1.5387539673e+02"),
            (P_S2[4], "1.4657617569e+01"),
            (Q_R8[0], "0.0000000000e+00"),
            (Q_R8[1], "7.3242187500e-02"),
            (Q_R8[2], "1.1768206596e+01"),
            (Q_R8[3], "5.5767340088e+02"),
            (Q_R8[4], "8.8591972656e+03"),
            (Q_R8[5], "3.7014625000e+04"),
            (Q_S8[0], "1.6377603149e+02"),
            (Q_S8[1], "8.0983447266e+03"),
            (Q_S8[2], "1.4253829688e+05"),
            (Q_S8[3], "8.0330925000e+05"),
            (Q_S8[4], "8.4050156250e+05"),
            (Q_S8[5], "-3.4389928125e+05"),
            (Q_R5[0], "1.8408595828e-11"),
            (Q_R5[1], "7.3242180049e-02"),
            (Q_R5[2], "5.8356351852e+00"),
            (Q_R5[3], "1.3511157227e+02"),
            (Q_R5[4], "1.0272437744e+03"),
            (Q_R5[5], "1.9899779053e+03"),
            (Q_S5[0], "8.2776611328e+01"),
            (Q_S5[1], "2.0778142090e+03"),
            (Q_S5[2], "1.8847289062e+04"),
            (Q_S5[3], "5.6751113281e+04"),
            (Q_S5[4], "3.5976753906e+04"),
            (Q_S5[5], "-5.3543427734e+03"),
            (Q_R3[0], "4.3774099900e-09"),
            (Q_R3[1], "7.3241114616e-02"),
            (Q_R3[2], "3.3442313671e+00"),
            (Q_R3[3], "4.2621845245e+01"),
            (Q_R3[4], "1.7080809021e+02"),
            (Q_R3[5], "1.6673394775e+02"),
            (Q_S3[0], "4.8758872986e+01"),
            (Q_S3[1], "7.0968920898e+02"),
            (Q_S3[2], "3.7041481934e+03"),
            (Q_S3[3], "6.4604252930e+03"),
            (Q_S3[4], "2.5163337402e+03"),
            (Q_S3[5], "-1.4924745178e+02"),
            (Q_R2[0], "1.5044444979e-07"),
            (Q_R2[1], "7.3223426938e-02"),
            (Q_R2[2], "1.9981917143e+00"),
            (Q_R2[3], "1.4495602608e+01"),
            (Q_R2[4], "3.1666231155e+01"),
            (Q_R2[5], "1.6252708435e+01"),
            (Q_S2[0], "3.0365585327e+01"),
            (Q_S2[1], "2.6934811401e+02"),
            (Q_S2[2], "8.4478375244e+02"),
            (Q_S2[3], "8.8293585205e+02"),
            (Q_S2[4], "2.1266638184e+02"),
            (Q_S2[5], "-5.3109550476e+00"),
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
