//! `erff` and `erfcf`: [`erf`](crate::math::erf::erf) and
//! [`erfc`](crate::math::erf::erfc) for `float`.
//!
//! Ported from musl 1.2.5's `erff.c` (MIT; see [`crate::math`] for the
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
//! As for `double`, in `float` arithmetic. The z whose square is exact keeps
//! the top 10 bits of |x|'s fraction.

use crate::math::expf::expf;
use crate::math::manipulate::fabsf;
use crate::math::support::{barrierf, hexf32};

/// erf(1), rounded to 24 bits.
const ERX: f32 = f32::from_bits(0x3f58_560b);

// erf on [0, 0.84375].
const EFX8: f32 = f32::from_bits(0x3f83_75d4);
const PP0: f32 = f32::from_bits(0x3e03_75d4);
const PP1: f32 = f32::from_bits(0xbea6_6beb);
const PP2: f32 = f32::from_bits(0xbce9_528f);
const PP3: f32 = f32::from_bits(0xbbbd_1489);
const PP4: f32 = f32::from_bits(0xb7c7_56b1);
const QQ1: f32 = f32::from_bits(0x3ecb_bbce);
const QQ2: f32 = f32::from_bits(0x3d85_2a63);
const QQ3: f32 = f32::from_bits(0x3ba6_8116);
const QQ4: f32 = f32::from_bits(0x390a_ee49);
const QQ5: f32 = f32::from_bits(0xb684_e21a);

// erf - erx on [0.84375, 1.25].
const PA0: f32 = f32::from_bits(0xbb1a_cdc6);
const PA1: f32 = f32::from_bits(0x3ed4_6805);
const PA2: f32 = f32::from_bits(0xbebe_9208);
const PA3: f32 = f32::from_bits(0x3ea2_fe54);
const PA4: f32 = f32::from_bits(0xbde3_1cc2);
const PA5: f32 = f32::from_bits(0x3d11_51b3);
const PA6: f32 = f32::from_bits(0xbb0d_f9c0);
const QA1: f32 = f32::from_bits(0x3dd9_f331);
const QA2: f32 = f32::from_bits(0x3f0a_5785);
const QA3: f32 = f32::from_bits(0x3d93_1ae7);
const QA4: f32 = f32::from_bits(0x3e01_3307);
const QA5: f32 = f32::from_bits(0x3c5f_6e13);
const QA6: f32 = f32::from_bits(0x3c44_5aa3);

// erfc on [1.25, 1/0.35].
const RA0: f32 = f32::from_bits(0xbc21_a093);
const RA1: f32 = f32::from_bits(0xbf31_a0b7);
const RA2: f32 = f32::from_bits(0xc128_f022);
const RA3: f32 = f32::from_bits(0xc279_8057);
const RA4: f32 = f32::from_bits(0xc322_658c);
const RA5: f32 = f32::from_bits(0xc338_9ae7);
const RA6: f32 = f32::from_bits(0xc2a2_932b);
const RA7: f32 = f32::from_bits(0xc11d_077e);
const SA1: f32 = f32::from_bits(0x419d_35ce);
const SA2: f32 = f32::from_bits(0x4309_a863);
const SA3: f32 = f32::from_bits(0x43d9_486f);
const SA4: f32 = f32::from_bits(0x4421_58c9);
const SA5: f32 = f32::from_bits(0x43d6_810b);
const SA6: f32 = f32::from_bits(0x42d9_451f);
const SA7: f32 = f32::from_bits(0x40d2_3f7c);
const SA8: f32 = f32::from_bits(0xbd77_7f97);

// erfc on [1/0.35, 28].
const RB0: f32 = f32::from_bits(0xbc21_a092);
const RB1: f32 = f32::from_bits(0xbf4c_9dd4);
const RB2: f32 = f32::from_bits(0xc18e_104b);
const RB3: f32 = f32::from_bits(0xc320_a2ea);
const RB4: f32 = f32::from_bits(0xc41f_6441);
const RB5: f32 = f32::from_bits(0xc480_230b);
const RB6: f32 = f32::from_bits(0xc3f1_c275);
const SB1: f32 = f32::from_bits(0x41f2_b459);
const SB2: f32 = f32::from_bits(0x43a2_e571);
const SB3: f32 = f32::from_bits(0x44c0_1759);
const SB4: f32 = f32::from_bits(0x4547_fdbb);
const SB5: f32 = f32::from_bits(0x451f_90ce);
const SB6: f32 = f32::from_bits(0x43ed_43a7);
const SB7: f32 = f32::from_bits(0xc1b3_8712);

/// A tiny amount for the results that round.
const TINY: f32 = hexf32!("0x1p-120");

/// erfcf(`x`) for 0.84375 <= |x| < 1.25. musl's `erfc1`.
fn erfc1(x: f32) -> f32 {
    let s = fabsf(x) - 1.0;
    let p = PA0 + s * (PA1 + s * (PA2 + s * (PA3 + s * (PA4 + s * (PA5 + s * PA6)))));
    let q = 1.0 + s * (QA1 + s * (QA2 + s * (QA3 + s * (QA4 + s * (QA5 + s * QA6)))));
    1.0 - ERX - p / q
}

/// erfcf(|`x`|) for 0.84375 <= |x| < 28, where `ix` is the word of |x|.
/// musl's `erfc2`.
fn erfc2(ix: u32, x: f32) -> f32 {
    if ix < 0x3fa0_0000 {
        return erfc1(x);
    }

    let x = fabsf(x);
    let s = 1.0 / (x * x);
    let (r, big_s) = if ix < 0x4036_db6d {
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
    let z = f32::from_bits(x.to_bits() & 0xffff_e000);
    expf(-z * z - 0.5625) * expf((z - x) * (z + x) + r / big_s) / x
}

/// [`erf`](crate::math::erf::erf) for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn erff(x: f32) -> f32 {
    let hx = x.to_bits();
    let sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7f80_0000 {
        // erf(NaN) is NaN, erf(±inf) is ±1.
        return (1 - 2 * sign as i32) as f32 + 1.0 / x;
    }
    if ix < 0x3f58_0000 {
        // |x| < 0.84375.
        if ix < 0x3180_0000 {
            // |x| < 2^-28: arranged to avoid underflow.
            return 0.125 * (8.0 * x + EFX8 * x);
        }
        let z = x * x;
        let r = PP0 + z * (PP1 + z * (PP2 + z * (PP3 + z * PP4)));
        let s = 1.0 + z * (QQ1 + z * (QQ2 + z * (QQ3 + z * (QQ4 + z * QQ5))));
        let y = r / s;
        return x + x * y;
    }
    let y = if ix < 0x40c0_0000 {
        // |x| < 6.
        1.0 - erfc2(ix, x)
    } else {
        // musl's `1 - 0x1p-120f`, which rounds, so it rounds at run time.
        barrierf(1.0) - TINY
    };
    if sign != 0 { -y } else { y }
}

/// [`erfc`](crate::math::erf::erfc) for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn erfcf(x: f32) -> f32 {
    let hx = x.to_bits();
    let sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x7f80_0000 {
        // erfc(NaN) is NaN, erfc(+inf) is 0 and erfc(-inf) is 2.
        return (2 * sign) as f32 + 1.0 / x;
    }
    if ix < 0x3f58_0000 {
        // |x| < 0.84375.
        if ix < 0x2380_0000 {
            // |x| < 2^-56.
            return 1.0 - x;
        }
        let z = x * x;
        let r = PP0 + z * (PP1 + z * (PP2 + z * (PP3 + z * PP4)));
        let s = 1.0 + z * (QQ1 + z * (QQ2 + z * (QQ3 + z * (QQ4 + z * QQ5))));
        let y = r / s;
        if sign != 0 || ix < 0x3e80_0000 {
            // x < 1/4.
            return 1.0 - (x + x * y);
        }
        return 0.5 - (x - 0.5 + x * y);
    }
    if ix < 0x41e0_0000 {
        // |x| < 28.
        return if sign != 0 {
            2.0 - erfc2(ix, x)
        } else {
            erfc2(ix, x)
        };
    }
    // musl's `2 - 0x1p-120f` and `0x1p-120f*0x1p-120f`, which round and
    // underflow at run time.
    if sign != 0 {
        barrierf(2.0) - TINY
    } else {
        barrierf(TINY) * TINY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn erff_matches_libc_test() {
        let files = ["sanity/erff.h", "special/erff.h"];
        mtest::d_d("erff", &files, |x| erff(x), Rules::ULP, &[]);
    }

    #[test]
    fn erfcf_matches_libc_test() {
        let files = ["sanity/erfcf.h", "special/erfcf.h"];
        mtest::d_d("erfcf", &files, |x| erfcf(x), Rules::ULP, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let floats = [
            (ERX, "8.4506291151e-01"),
            (EFX8, "1.0270333290e+00"),
            (PP0, "1.2837916613e-01"),
            (PP1, "-3.2504209876e-01"),
            (PP2, "-2.8481749818e-02"),
            (PP3, "-5.7702702470e-03"),
            (PP4, "-2.3763017452e-05"),
            (QQ1, "3.9791721106e-01"),
            (QQ2, "6.5022252500e-02"),
            (QQ3, "5.0813062117e-03"),
            (QQ4, "1.3249473704e-04"),
            (QQ5, "-3.9602282413e-06"),
            (PA0, "-2.3621185683e-03"),
            (PA1, "4.1485610604e-01"),
            (PA2, "-3.7220788002e-01"),
            (PA3, "3.1834661961e-01"),
            (PA4, "-1.1089469492e-01"),
            (PA5, "3.5478305072e-02"),
            (PA6, "-2.1663755178e-03"),
            (QA1, "1.0642088205e-01"),
            (QA2, "5.4039794207e-01"),
            (QA3, "7.1828655899e-02"),
            (QA4, "1.2617121637e-01"),
            (QA5, "1.3637083583e-02"),
            (QA6, "1.1984500103e-02"),
            (RA0, "-9.8649440333e-03"),
            (RA1, "-6.9385856390e-01"),
            (RA2, "-1.0558626175e+01"),
            (RA3, "-6.2375331879e+01"),
            (RA4, "-1.6239666748e+02"),
            (RA5, "-1.8460508728e+02"),
            (RA6, "-8.1287437439e+01"),
            (RA7, "-9.8143291473e+00"),
            (SA1, "1.9651271820e+01"),
            (SA2, "1.3765776062e+02"),
            (SA3, "4.3456588745e+02"),
            (SA4, "6.4538726807e+02"),
            (SA5, "4.2900814819e+02"),
            (SA6, "1.0863500214e+02"),
            (SA7, "6.5702495575e+00"),
            (SA8, "-6.0424413532e-02"),
            (RB0, "-9.8649431020e-03"),
            (RB1, "-7.9928326607e-01"),
            (RB2, "-1.7757955551e+01"),
            (RB3, "-1.6063638306e+02"),
            (RB4, "-6.3756646729e+02"),
            (RB5, "-1.0250950928e+03"),
            (RB6, "-4.8351919556e+02"),
            (SB1, "3.0338060379e+01"),
            (SB2, "3.2579251099e+02"),
            (SB3, "1.5367296143e+03"),
            (SB4, "3.1998581543e+03"),
            (SB5, "2.5530502930e+03"),
            (SB6, "4.7452853394e+02"),
            (SB7, "-2.2440952301e+01"),
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
