//! `j1f` and `y1f`: [`j1`](crate::math::j1::j1) and
//! [`y1`](crate::math::j1::y1) for `float`.
//!
//! Ported from musl 1.2.5's `j1f.c` (MIT; see [`crate::math`] for the
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
//! As for `double`, in `float` arithmetic, except that musl combines sin(x),
//! cos(x) and cos(2x) for |x| >= 2 in `double`.

use core::hint::black_box;

use crate::math::j0f::polyf;
use crate::math::logf::logf;
use crate::math::manipulate::fabsf;
use crate::math::sqrt::sqrtf;
use crate::math::support::{barrierf, divzerof};
use crate::math::trigf::{cosf, sinf};

/// 1/√π.
const INVSQRTPI: f32 = f32::from_bits(0x3f10_6ebb);
/// 2/π.
const TPI: f32 = f32::from_bits(0x3f22_f983);

// j1f below 2: R's numerator and S's.
const R00: f32 = f32::from_bits(0xbd80_0000);
const R01: f32 = f32::from_bits(0x3ab8_6cfd);
const R02: f32 = f32::from_bits(0xb786_2e36);
const R03: f32 = f32::from_bits(0x3355_57d2);
const S01: f32 = f32::from_bits(0x3c9c_e859);
const S02: f32 = f32::from_bits(0x3942_fab6);
const S03: f32 = f32::from_bits(0x359d_ffc2);
const S04: f32 = f32::from_bits(0x31ad_6446);
const S05: f32 = f32::from_bits(0x2d59_567e);

// y1f below 2: U's numerator and V's.
const U0: [f32; 5] = [
    f32::from_bits(0xbe48_c331),
    f32::from_bits(0x3d4e_9e3c),
    f32::from_bits(0xbafa_af2a),
    f32::from_bits(0x37c5_581c),
    f32::from_bits(0xb3c5_6003),
];
const V0: [f32; 5] = [
    f32::from_bits(0x3ca3_286a),
    f32::from_bits(0x3954_644b),
    f32::from_bits(0x35b6_02d4),
    f32::from_bits(0x31d5_f8eb),
    f32::from_bits(0x2d92_81cf),
];

// p1(x) - 1 = R/S in 1/x², on [8, inf], [4.5454, 8], [2.8571, 4.5454] and
// [2, 2.8571].
const PR8: [f32; 6] = [
    f32::from_bits(0x0000_0000),
    f32::from_bits(0x3df0_0000),
    f32::from_bits(0x4153_d4ea),
    f32::from_bits(0x43ce_06a3),
    f32::from_bits(0x4572_2bed),
    f32::from_bits(0x45f7_53d6),
];
const PS8: [f32; 5] = [
    f32::from_bits(0x42e4_6a2c),
    f32::from_bits(0x4564_2ee5),
    f32::from_bits(0x4710_5c35),
    f32::from_bits(0x47be_a166),
    f32::from_bits(0x46f0_a88b),
];
const PR5: [f32; 6] = [
    f32::from_bits(0x2d68_333f),
    f32::from_bits(0x3def_ffff),
    f32::from_bits(0x40d9_b023),
    f32::from_bits(0x42d8_9dca),
    f32::from_bits(0x4401_68b7),
    f32::from_bits(0x4404_2dc6),
];
const PS5: [f32; 5] = [
    f32::from_bits(0x426d_1f55),
    f32::from_bits(0x4477_d9b1),
    f32::from_bits(0x45a7_4a23),
    f32::from_bits(0x45f5_2586),
    f32::from_bits(0x44bc_0180),
];
const PR3: [f32; 6] = [
    f32::from_bits(0x314f_e10d),
    f32::from_bits(0x3def_ffab),
    f32::from_bits(0x407b_b5e7),
    f32::from_bits(0x420c_7a45),
    f32::from_bits(0x42b6_1c2a),
    f32::from_bits(0x4242_3c7c),
];
const PS3: [f32; 5] = [
    f32::from_bits(0x420b_2a4d),
    f32::from_bits(0x43a8_6198),
    f32::from_bits(0x4482_dbe3),
    f32::from_bits(0x445e_b3ed),
    f32::from_bits(0x42cf_936c),
];
const PR2: [f32; 6] = [
    f32::from_bits(0x33e7_4ea8),
    f32::from_bits(0x3def_fa16),
    f32::from_bits(0x4017_95c0),
    f32::from_bits(0x4143_e1bc),
    f32::from_bits(0x418d_8d41),
    f32::from_bits(0x40a2_5a4d),
];
const PS2: [f32; 5] = [
    f32::from_bits(0x41ab_7dec),
    f32::from_bits(0x42fa_9499),
    f32::from_bits(0x4368_46c7),
    f32::from_bits(0x42eb_5bd7),
    f32::from_bits(0x4105_d590),
];

// q1(x)·x - 3/8 = R/S in 1/x², on the same intervals.
const QR8: [f32; 6] = [
    f32::from_bits(0x0000_0000),
    f32::from_bits(0xbdd2_0000),
    f32::from_bits(0xc182_2c8d),
    f32::from_bits(0xc43d_e683),
    f32::from_bits(0xc639_273a),
    f32::from_bits(0xc73d_3683),
];
const QS8: [f32; 6] = [
    f32::from_bits(0x4321_6537),
    f32::from_bits(0x45f4_8b17),
    f32::from_bits(0x4802_bcd6),
    f32::from_bits(0x492f_b29c),
    f32::from_bits(0x4922_be94),
    f32::from_bits(0xc88f_cb48),
];
const QR5: [f32; 6] = [
    f32::from_bits(0xadb7_d219),
    f32::from_bits(0xbdd1_fffe),
    f32::from_bits(0xc100_e736),
    f32::from_bits(0xc337_ab6b),
    f32::from_bits(0xc4ab_a633),
    f32::from_bits(0xc523_471c),
];
const QS5: [f32; 6] = [
    f32::from_bits(0x42a2_8d98),
    f32::from_bits(0x44f8_f98f),
    f32::from_bits(0x4688_78f8),
    f32::from_bits(0x4742_bb6d),
    f32::from_bits(0x46da_5826),
    f32::from_bits(0xc593_7978),
];
const QR3: [f32; 6] = [
    f32::from_bits(0xb1ae_7d4f),
    f32::from_bits(0xbdd1_ff5b),
    f32::from_bits(0xc093_8612),
    f32::from_bits(0xc267_638e),
    f32::from_bits(0xc364_3e9a),
    f32::from_bits(0xc35b_35cb),
];
const QS3: [f32; 6] = [
    f32::from_bits(0x423e_a91e),
    f32::from_bits(0x4428_775e),
    f32::from_bits(0x4553_4272),
    f32::from_bits(0x45ad_5dd5),
    f32::from_bits(0x44ed_e3d0),
    f32::from_bits(0xc307_3381),
];
const QR2: [f32; 6] = [
    f32::from_bits(0xb43f_8932),
    f32::from_bits(0xbdd1_f475),
    f32::from_bits(0xc030_2423),
    f32::from_bits(0xc19d_4f16),
    f32::from_bits(0xc229_4d1f),
    f32::from_bits(0xc1aa_f9b2),
];
const QS2: [f32; 6] = [
    f32::from_bits(0x41ec_4454),
    f32::from_bits(0x437c_fb47),
    f32::from_bits(0x443d_602e),
    f32::from_bits(0x4438_d92a),
    f32::from_bits(0x431b_f2f2),
    f32::from_bits(0xc09e_b437),
];

/// ±j1f(`x`), or y1f(x) if `y1` is set, for `x` >= 2 whose word is `ix`,
/// negated if `sign` is set. musl's `common`, which works in `double`.
fn common(ix: u32, x: f32, y1: bool, sign: bool) -> f32 {
    let mut s = f64::from(sinf(x));
    if y1 {
        s = -s;
    }
    let c = f64::from(cosf(x));
    let mut cc = s - c;
    if ix < 0x7f00_0000 {
        let mut ss = -s - c;
        let z = f64::from(cosf(2.0 * x));
        if s * c > 0.0 {
            cc = z / ss;
        } else {
            ss = z / cc;
        }
        if ix < 0x5880_0000 {
            if y1 {
                ss = -ss;
            }
            cc = f64::from(ponef(x)) * cc - f64::from(qonef(x)) * ss;
        }
    }
    if sign {
        cc = -cc;
    }
    (f64::from(INVSQRTPI) * cc / f64::from(sqrtf(x))) as f32
}

/// [`j1`](crate::math::j1::j1) for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn j1f(x: f32) -> f32 {
    let hx = x.to_bits();
    let sign = hx >> 31 != 0;
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7f80_0000 {
        return 1.0 / (x * x);
    }
    if ix >= 0x4000_0000 {
        // |x| >= 2.
        return common(ix, fabsf(x), false, sign);
    }
    let z = if ix >= 0x3900_0000 {
        // |x| >= 2^-13.
        let z = x * x;
        let r = z * polyf(z, &[R00, R01, R02, R03]);
        let s = polyf(z, &[1.0, S01, S02, S03, S04, S05]);
        0.5 + r / s
    } else {
        0.5
    };
    z * x
}

/// [`y1`](crate::math::j1::y1) for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn y1f(x: f32) -> f32 {
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
        // x >= 2.
        return common(ix, x, true, false);
    }
    if ix < 0x3300_0000 {
        // x < 2^-25.
        return -TPI / x;
    }
    let [v0, v1, v2, v3, v4] = V0;
    let z = x * x;
    let u = polyf(z, &U0);
    let v = polyf(z, &[1.0, v0, v1, v2, v3, v4]);
    // LLVM would rewrite `a - 1.0 / x` as `a + -1.0 / x`, which rounds the
    // quotient the other way. See `crate::math`.
    x * (u / v) + TPI * (j1f(x) * logf(x) - barrierf(1.0 / x))
}

/// p1(`x`) for `x` >= 2. musl's `ponef`.
fn ponef(x: f32) -> f32 {
    let ix = x.to_bits() & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4100_0000 {
        (&PR8, &PS8)
    } else if ix >= 0x4091_73eb {
        (&PR5, &PS5)
    } else if ix >= 0x4036_d917 {
        (&PR3, &PS3)
    } else {
        (&PR2, &PS2)
    };
    let [q0, q1, q2, q3, q4] = *q;
    let z = 1.0 / (x * x);
    let r = polyf(z, p);
    let s = polyf(z, &[1.0, q0, q1, q2, q3, q4]);
    1.0 + r / s
}

/// q1(`x`) for `x` >= 2. musl's `qonef`.
fn qonef(x: f32) -> f32 {
    let ix = x.to_bits() & 0x7fff_ffff;
    let (p, q) = if ix >= 0x4100_0000 {
        (&QR8, &QS8)
    } else if ix >= 0x4091_73eb {
        (&QR5, &QS5)
    } else if ix >= 0x4036_d917 {
        (&QR3, &QS3)
    } else {
        (&QR2, &QS2)
    };
    let [q0, q1, q2, q3, q4, q5] = *q;
    let z = 1.0 / (x * x);
    let r = polyf(z, p);
    let s = polyf(z, &[1.0, q0, q1, q2, q3, q4, q5]);
    (0.375 + r / s) / x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn j1f_matches_libc_test() {
        let files = ["sanity/j1f.h", "special/j1f.h"];
        mtest::d_d("j1f", &files, |x| j1f(x), Rules::ULP, &[]);
    }

    #[test]
    fn y1f_matches_libc_test() {
        let files = ["sanity/y1f.h", "special/y1f.h"];
        // libc-test's `y1f.c` wants a NaN or -inf for a negative argument.
        let rules = Rules::ULP.negative_domain();
        mtest::d_d("y1f", &files, |x| y1f(x), rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let floats = [
            (INVSQRTPI, "5.6418961287e-01"),
            (TPI, "6.3661974669e-01"),
            (R00, "-6.2500000000e-02"),
            (R01, "1.4070566976e-03"),
            (R02, "-1.5995563444e-05"),
            (R03, "4.9672799207e-08"),
            (S01, "1.9153760746e-02"),
            (S02, "1.8594678841e-04"),
            (S03, "1.1771846857e-06"),
            (S04, "5.0463624390e-09"),
            (S05, "1.2354227016e-11"),
            (U0[0], "-1.9605709612e-01"),
            (U0[1], "5.0443872809e-02"),
            (U0[2], "-1.9125689287e-03"),
            (U0[3], "2.3525259166e-05"),
            (U0[4], "-9.1909917899e-08"),
            (V0[0], "1.9916731864e-02"),
            (V0[1], "2.0255257550e-04"),
            (V0[2], "1.3560879779e-06"),
            (V0[3], "6.2274145840e-09"),
            (V0[4], "1.6655924903e-11"),
            (PR8[0], "0.0000000000e+00"),
            (PR8[1], "1.1718750000e-01"),
            (PR8[2], "1.3239480972e+01"),
            (PR8[3], "4.1205184937e+02"),
            (PR8[4], "3.8747453613e+03"),
            (PR8[5], "7.9144794922e+03"),
            (PS8[0], "1.1420736694e+02"),
            (PS8[1], "3.6509309082e+03"),
            (PS8[2], "3.6956207031e+04"),
            (PS8[3], "9.7602796875e+04"),
            (PS8[4], "3.0804271484e+04"),
            (PR5[0], "1.3199052094e-11"),
            (PR5[1], "1.1718749255e-01"),
            (PR5[2], "6.8027510643e+00"),
            (PR5[3], "1.0830818176e+02"),
            (PR5[4], "5.1763616943e+02"),
            (PR5[5], "5.2871520996e+02"),
            (PS5[0], "5.9280597687e+01"),
            (PS5[1], "9.9140142822e+02"),
            (PS5[2], "5.3532670898e+03"),
            (PS5[3], "7.8446904297e+03"),
            (PS5[4], "1.5040468750e+03"),
            (PR3[0], "3.0250391081e-09"),
            (PR3[1], "1.1718686670e-01"),
            (PR3[2], "3.9329774380e+00"),
            (PR3[3], "3.5119403839e+01"),
            (PR3[4], "9.1055007935e+01"),
            (PR3[5], "4.8559066772e+01"),
            (PS3[0], "3.4791309357e+01"),
            (PS3[1], "3.3676245117e+02"),
            (PS3[2], "1.0468714600e+03"),
            (PS3[3], "8.9081134033e+02"),
            (PS3[4], "1.0378793335e+02"),
            (PR2[0], "1.0771083225e-07"),
            (PR2[1], "1.1717621982e-01"),
            (PR2[2], "2.3685150146e+00"),
            (PR2[3], "1.2242610931e+01"),
            (PR2[4], "1.7693971634e+01"),
            (PR2[5], "5.0735230446e+00"),
            (PS2[0], "2.1436485291e+01"),
            (PS2[1], "1.2529022980e+02"),
            (PS2[2], "2.3227647400e+02"),
            (PS2[3], "1.1767937469e+02"),
            (PS2[4], "8.3646392822e+00"),
            (QR8[0], "0.0000000000e+00"),
            (QR8[1], "-1.0253906250e-01"),
            (QR8[2], "-1.6271753311e+01"),
            (QR8[3], "-7.5960174561e+02"),
            (QR8[4], "-1.1849806641e+04"),
            (QR8[5], "-4.8438511719e+04"),
            (QS8[0], "1.6139537048e+02"),
            (QS8[1], "7.8253862305e+03"),
            (QS8[2], "1.3387534375e+05"),
            (QS8[3], "7.1965775000e+05"),
            (QS8[4], "6.6660125000e+05"),
            (QS8[5], "-2.9449025000e+05"),
            (QR5[0], "-2.0897993405e-11"),
            (QR5[1], "-1.0253904760e-01"),
            (QR5[2], "-8.0564479828e+00"),
            (QR5[3], "-1.8366960144e+02"),
            (QR5[4], "-1.3731937256e+03"),
            (QR5[5], "-2.6124443359e+03"),
            (QS5[0], "8.1276550293e+01"),
            (QS5[1], "1.9917987061e+03"),
            (QS5[2], "1.7468484375e+04"),
            (QS5[3], "4.9851425781e+04"),
            (QS5[4], "2.7948074219e+04"),
            (QS5[5], "-4.7191835938e+03"),
            (QR3[0], "-5.0783124372e-09"),
            (QR3[1], "-1.0253783315e-01"),
            (QR3[2], "-4.6101160049e+00"),
            (QR3[3], "-5.7847221375e+01"),
            (QR3[4], "-2.2824453735e+02"),
            (QR3[5], "-2.1921012878e+02"),
            (QS3[0], "4.7665153503e+01"),
            (QS3[1], "6.7386511230e+02"),
            (QS3[2], "3.3801528320e+03"),
            (QS3[3], "5.5477290039e+03"),
            (QS3[4], "1.9031191406e+03"),
            (QS3[5], "-1.3520118713e+02"),
            (QR2[0], "-1.7838172539e-07"),
            (QR2[1], "-1.0251704603e-01"),
            (QR2[2], "-2.7522056103e+00"),
            (QR2[3], "-1.9663616180e+01"),
            (QR2[4], "-4.2325313568e+01"),
            (QR2[5], "-2.1371921539e+01"),
            (QS2[0], "2.9533363342e+01"),
            (QS2[1], "2.5298155212e+02"),
            (QS2[2], "7.5750280762e+02"),
            (QS2[3], "7.3939318848e+02"),
            (QS2[4], "1.5594900513e+02"),
            (QS2[5], "-4.9594988823e+00"),
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
