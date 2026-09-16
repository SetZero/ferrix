//! `lgamma` and `lgamma_r`: the logarithm of the magnitude of the gamma
//! function, and `signgam`, where `lgamma` and `lgammaf` leave its sign.
//!
//! Ported from musl 1.2.5's `lgamma.c`, `lgamma_r.c` and `signgam.c` (MIT;
//! see [`crate::math`] for the notice). musl took `lgamma_r.c` from FreeBSD's
//! msun, whose file carries this notice:
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
//! musl names its function `__lgamma_r`, a hidden symbol, and exports it as
//! the weak alias `lgamma_r`; its `signgam` is the weak alias of
//! `__signgam`. Only the names programs use are exported here.
//!
//! # Method
//!
//! Below 8, lgamma(x + 1) = log(x) + lgamma(x) reduces x to [2, 3), where
//! lgamma(2 + s) = s/2 + s·P(s)/Q(s), or, below 2, to one of three
//! approximations: around 1, around the minimum at 1.4616..., and around 2.
//! From 8 up, lgamma(x) = (x - 1/2)(log(x) - 1) + w(1/x) for a polynomial w,
//! and from 2^58 up, x(log(x) - 1). A negative x reflects:
//! lgamma(x) = log(π/|x·sin(πx)|) - lgamma(-x), with the sign of sin(πx).

use core::ffi::c_int;
use core::sync::atomic::{AtomicI32, Ordering};

use crate::math::arch::trunc_to_i64;
use crate::math::log::log;
use crate::math::rounding::floor;
use crate::math::support::barrier;
use crate::math::trig::{kernel_cos, kernel_sin};

/// `signgam`: the sign of Γ(x), 1 or -1, for the x of the last call to
/// [`lgamma`] or [`lgammaf`](crate::math::lgammaf::lgammaf). `AtomicI32` has
/// an `int`'s layout.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static signgam: AtomicI32 = AtomicI32::new(0);

/// π, rounded.
const PI: f64 = f64::from_bits(0x4009_21fb_5444_2d18);

// lgamma(x) around 2: a0 to a11.
const A0: f64 = f64::from_bits(0x3fb3_c467_e37d_b0c8);
const A1: f64 = f64::from_bits(0x3fd4_a34c_c4a6_0fad);
const A2: f64 = f64::from_bits(0x3fb1_3e00_1a55_62a7);
const A3: f64 = f64::from_bits(0x3f95_1322_ac92_547b);
const A4: f64 = f64::from_bits(0x3f7e_404f_b68f_efe8);
const A5: f64 = f64::from_bits(0x3f67_add8_ccb7_926b);
const A6: f64 = f64::from_bits(0x3f53_8a94_116f_3f5d);
const A7: f64 = f64::from_bits(0x3f40_b6c6_89b9_9c00);
const A8: f64 = f64::from_bits(0x3f2c_f2ec_ed10_e54d);
const A9: f64 = f64::from_bits(0x3f1c_5088_987d_fb07);
const A10: f64 = f64::from_bits(0x3efa_7074_428c_fa52);
const A11: f64 = f64::from_bits(0x3f07_858e_90a4_5837);

/// Where lgamma has its minimum, rounded.
const TC: f64 = f64::from_bits(0x3ff7_62d8_6356_be3f);
/// lgamma(tc), rounded.
const TF: f64 = f64::from_bits(0xbfbf_19b9_bcc3_8a42);
/// The error of `TF`, negated.
const TT: f64 = f64::from_bits(0xbc50_c7ca_a48a_971f);
// lgamma(x) - tf around tc: t0 to t14.
const T0: f64 = f64::from_bits(0x3fde_f72b_c8ee_38a2);
const T1: f64 = f64::from_bits(0xbfc2_e427_8dc6_c509);
const T2: f64 = f64::from_bits(0x3fb0_8b42_94d5_419b);
const T3: f64 = f64::from_bits(0xbfa0_c9a8_df35_b713);
const T4: f64 = f64::from_bits(0x3f92_66e7_970a_f9ec);
const T5: f64 = f64::from_bits(0xbf85_1f9f_ba91_ec6a);
const T6: f64 = f64::from_bits(0x3f78_fce0_e370_e344);
const T7: f64 = f64::from_bits(0xbf6e_2eff_b3e9_14d7);
const T8: f64 = f64::from_bits(0x3f62_82d3_2e15_c915);
const T9: f64 = f64::from_bits(0xbf56_fe8e_bf2d_1af1);
const T10: f64 = f64::from_bits(0x3f4c_df0c_ef61_a8e9);
const T11: f64 = f64::from_bits(0xbf41_a610_9c73_e0ec);
const T12: f64 = f64::from_bits(0x3f34_af6d_6c0e_bbf7);
const T13: f64 = f64::from_bits(0xbf34_7f24_ecc3_8c38);
const T14: f64 = f64::from_bits(0x3f35_fd3e_e8c2_d3f4);

// lgamma(x) around 1: u0 to u5 over v1 to v5.
const U0: f64 = f64::from_bits(0xbfb3_c467_e37d_b0c8);
const U1: f64 = f64::from_bits(0x3fe4_401e_8b00_5dff);
const U2: f64 = f64::from_bits(0x3ff7_475c_d119_bd6f);
const U3: f64 = f64::from_bits(0x3fef_4976_44ea_8450);
const U4: f64 = f64::from_bits(0x3fcd_4eae_f601_0924);
const U5: f64 = f64::from_bits(0x3f8b_678b_bf2b_ab09);
const V1: f64 = f64::from_bits(0x4003_a5d7_c2bd_619c);
const V2: f64 = f64::from_bits(0x4001_0725_a42b_18f5);
const V3: f64 = f64::from_bits(0x3fe8_9dfb_e450_50af);
const V4: f64 = f64::from_bits(0x3fba_ae55_d653_7c88);
const V5: f64 = f64::from_bits(0x3f6a_5abb_57d0_cf61);

// lgamma(2 + s) - s/2 on [0, 1): s0 to s6 over r1 to r6.
const S0: f64 = f64::from_bits(0xbfb3_c467_e37d_b0c8);
const S1: f64 = f64::from_bits(0x3fcb_848b_36e2_0878);
const S2: f64 = f64::from_bits(0x3fd4_d98f_4f13_9f59);
const S3: f64 = f64::from_bits(0x3fc2_bb9c_bee5_f2f7);
const S4: f64 = f64::from_bits(0x3f9b_481c_7e93_9961);
const S5: f64 = f64::from_bits(0x3f5e_26b6_7368_f239);
const S6: f64 = f64::from_bits(0x3f00_bfec_dd17_e945);
const R1: f64 = f64::from_bits(0x3ff6_45a7_62c4_ab74);
const R2: f64 = f64::from_bits(0x3fe7_1a18_93d3_dcdc);
const R3: f64 = f64::from_bits(0x3fc6_01ed_ccfb_df27);
const R4: f64 = f64::from_bits(0x3f93_17ea_742e_d475);
const R5: f64 = f64::from_bits(0x3f49_7dda_ca41_a95b);
const R6: f64 = f64::from_bits(0x3ede_baf7_a5b3_8140);

// lgamma(x) - (x - 1/2)(log(x) - 1) from 8 up, in 1/x: w0 to w6.
const W0: f64 = f64::from_bits(0x3fda_cfe3_90c9_7d69);
const W1: f64 = f64::from_bits(0x3fb5_5555_5555_553b);
const W2: f64 = f64::from_bits(0xbf66_c16c_16b0_2e5c);
const W3: f64 = f64::from_bits(0x3f4a_019f_98cf_38b6);
const W4: f64 = f64::from_bits(0xbf43_80cb_8c0f_e741);
const W5: f64 = f64::from_bits(0x3f4b_67ba_4cda_d5d1);
const W6: f64 = f64::from_bits(0xbf5a_b89d_0b9e_43e4);

/// sin(π`x`) for `x` > 2^-100. Where it is 0, its sign is arbitrary. musl's
/// `sin_pi`.
fn sin_pi(x: f64) -> f64 {
    // x mod 2, with a spurious inexact for an odd integer.
    let x = 2.0 * (x * 0.5 - floor(x * 0.5));

    // The conversion raises inexact as C's does.
    let n = trunc_to_i64(x * 4.0) as i32;
    let n = (n + 1) / 2;
    // musl's `n*0.5f`, exact.
    let x = x - f64::from(n) * 0.5;
    let x = x * PI;

    match n {
        1 => kernel_cos(x, 0.0),
        2 => kernel_sin(-x, 0.0, false),
        3 => -kernel_cos(x, 0.0),
        _ => kernel_sin(x, 0.0, false),
    }
}

/// log|Γ(`x`)| and the sign of Γ(x). musl's `__lgamma_r`, with the sign
/// returned rather than written.
pub(crate) fn lgamma_sign(mut x: f64) -> (f64, c_int) {
    let u = x.to_bits();
    let mut sign_gamma = 1;
    let sign = u >> 63 != 0;
    let ix = (u >> 32) as u32 & 0x7fff_ffff;

    // ±inf, NaN, ±0, tiny and negative arguments.
    if ix >= 0x7ff0_0000 {
        return (x * x, sign_gamma);
    }
    if ix < (0x3ff - 70) << 20 {
        // |x| < 2^-70: -log(|x|).
        if sign {
            x = -x;
            sign_gamma = -1;
        }
        return (-log(x), sign_gamma);
    }
    let mut nadj = 0.0;
    if sign {
        x = -x;
        let mut t = sin_pi(x);
        if t == 0.0 {
            // A negative integer: ±inf, as x - x is -0 when rounding
            // downward. The barrier only keeps clippy from calling x - x a
            // mistake.
            return (1.0 / (barrier(x) - x), sign_gamma);
        }
        if t > 0.0 {
            sign_gamma = -1;
        } else {
            t = -t;
        }
        nadj = log(PI / (t * x));
    }

    let mut r;
    if (ix == 0x3ff0_0000 || ix == 0x4000_0000) && u as u32 == 0 {
        // 1 and 2.
        r = 0.0;
    } else if ix < 0x4000_0000 {
        // x < 2.
        let (y, i);
        if ix <= 0x3fec_cccc {
            // lgamma(x) = lgamma(x + 1) - log(x).
            r = -log(x);
            (y, i) = if ix >= 0x3fe7_6944 {
                (1.0 - x, 0)
            } else if ix >= 0x3fcd_a661 {
                (x - (TC - 1.0), 1)
            } else {
                (x, 2)
            };
        } else {
            r = 0.0;
            (y, i) = if ix >= 0x3ffb_b4c3 {
                // [1.7316, 2].
                (2.0 - x, 0)
            } else if ix >= 0x3ff3_b4c4 {
                // [1.23, 1.73].
                (x - TC, 1)
            } else {
                (x - 1.0, 2)
            };
        }
        match i {
            0 => {
                let z = y * y;
                let p1 = A0 + z * (A2 + z * (A4 + z * (A6 + z * (A8 + z * A10))));
                let p2 = z * (A1 + z * (A3 + z * (A5 + z * (A7 + z * (A9 + z * A11)))));
                let p = y * p1 + p2;
                r += p - 0.5 * y;
            }
            1 => {
                let z = y * y;
                let w = z * y;
                // Split for evaluation in parallel.
                let p1 = T0 + w * (T3 + w * (T6 + w * (T9 + w * T12)));
                let p2 = T1 + w * (T4 + w * (T7 + w * (T10 + w * T13)));
                let p3 = T2 + w * (T5 + w * (T8 + w * (T11 + w * T14)));
                // LLVM would rewrite `a - (b - c)` as `a + (c - b)`, which
                // rounds the difference the other way. See `crate::math`.
                let p = z * p1 - barrier(TT - w * (p2 + y * p3));
                r += TF + p;
            }
            _ => {
                let p1 = y * (U0 + y * (U1 + y * (U2 + y * (U3 + y * (U4 + y * U5)))));
                let p2 = 1.0 + y * (V1 + y * (V2 + y * (V3 + y * (V4 + y * V5))));
                r += -0.5 * y + p1 / p2;
            }
        }
    } else if ix < 0x4020_0000 {
        // x < 8. The conversion raises inexact as C's does.
        let i = trunc_to_i64(x) as i32;
        let y = x - f64::from(i);
        let p = y * (S0 + y * (S1 + y * (S2 + y * (S3 + y * (S4 + y * (S5 + y * S6))))));
        let q = 1.0 + y * (R1 + y * (R2 + y * (R3 + y * (R4 + y * (R5 + y * R6)))));
        r = 0.5 * y + p / q;
        // lgamma(1 + s) = log(s) + lgamma(s): musl's switch falls through
        // from case i down to case 3, multiplying by y + i - 1 and down.
        if i >= 3 {
            let mut z = 1.0;
            let mut k = i;
            while k >= 3 {
                z *= y + f64::from(k - 1);
                k -= 1;
            }
            r += log(z);
        }
    } else if ix < 0x4390_0000 {
        // 8 <= x < 2^58.
        let t = log(x);
        let z = 1.0 / x;
        let y = z * z;
        let w = W0 + z * (W1 + y * (W2 + y * (W3 + y * (W4 + y * (W5 + y * W6)))));
        r = (x - 0.5) * (t - 1.0) + w;
    } else {
        // 2^58 <= x.
        r = x * (log(x) - 1.0);
    }
    if sign {
        r = nadj - r;
    }
    (r, sign_gamma)
}

/// log|Γ(`x`)|, the logarithm of the magnitude of the gamma function, leaving
/// the sign of Γ(x) in [`signgam`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lgamma(x: f64) -> f64 {
    let (y, sign) = lgamma_sign(x);
    signgam.store(sign, Ordering::Relaxed);
    y
}

/// [`lgamma`], writing the sign of Γ(`x`) to `*signgamp` instead.
///
/// # Safety
///
/// `signgamp` must be valid for writing an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lgamma_r(x: f64, signgamp: *mut c_int) -> f64 {
    let (y, sign) = lgamma_sign(x);
    // SAFETY: the caller passes a pointer valid for writing an `int`.
    unsafe { signgamp.write(sign) };
    y
}

/// Serialises the tests that read [`signgam`], which is shared.
#[cfg(test)]
pub(crate) static SIGNGAM_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn lgamma_matches_libc_test() {
        let _serial = SIGNGAM_TESTS.lock();
        let files = ["sanity/lgamma.h", "special/lgamma.h"];
        // libc-test's `lgamma.c` tolerates an error under 11 ulps, but never
        // a wrong sign.
        let rules = Rules::ULP.tolerate(11.0);
        let call = |x| {
            let y = lgamma(x);
            (y, signgam.load(Ordering::Relaxed))
        };
        mtest::d_di("lgamma", &files, call, rules, &[]);
    }

    #[test]
    fn lgamma_r_matches_libc_test() {
        let files = ["sanity/lgamma_r.h", "special/lgamma_r.h"];
        // As `lgamma.c`.
        let rules = Rules::ULP.tolerate(11.0);
        let call = |x| {
            let mut sign = 0;
            // SAFETY: `sign` is an `int` to write.
            let y = unsafe { lgamma_r(x, &raw mut sign) };
            (y, sign)
        };
        mtest::d_di("lgamma_r", &files, call, rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let doubles = [
            (PI, "3.14159265358979311600e+00"),
            (A0, "7.72156649015328655494e-02"),
            (A1, "3.22467033424113591611e-01"),
            (A2, "6.73523010531292681824e-02"),
            (A3, "2.05808084325167332806e-02"),
            (A4, "7.38555086081402883957e-03"),
            (A5, "2.89051383673415629091e-03"),
            (A6, "1.19270763183362067845e-03"),
            (A7, "5.10069792153511336608e-04"),
            (A8, "2.20862790713908385557e-04"),
            (A9, "1.08011567247583939954e-04"),
            (A10, "2.52144565451257326939e-05"),
            (A11, "4.48640949618915160150e-05"),
            (TC, "1.46163214496836224576e+00"),
            (TF, "-1.21486290535849611461e-01"),
            (TT, "-3.63867699703950536541e-18"),
            (T0, "4.83836122723810047042e-01"),
            (T1, "-1.47587722994593911752e-01"),
            (T2, "6.46249402391333854778e-02"),
            (T3, "-3.27885410759859649565e-02"),
            (T4, "1.79706750811820387126e-02"),
            (T5, "-1.03142241298341437450e-02"),
            (T6, "6.10053870246291332635e-03"),
            (T7, "-3.68452016781138256760e-03"),
            (T8, "2.25964780900612472250e-03"),
            (T9, "-1.40346469989232843813e-03"),
            (T10, "8.81081882437654011382e-04"),
            (T11, "-5.38595305356740546715e-04"),
            (T12, "3.15632070903625950361e-04"),
            (T13, "-3.12754168375120860518e-04"),
            (T14, "3.35529192635519073543e-04"),
            (U0, "-7.72156649015328655494e-02"),
            (U1, "6.32827064025093366517e-01"),
            (U2, "1.45492250137234768737e+00"),
            (U3, "9.77717527963372745603e-01"),
            (U4, "2.28963728064692451092e-01"),
            (U5, "1.33810918536787660377e-02"),
            (V1, "2.45597793713041134822e+00"),
            (V2, "2.12848976379893395361e+00"),
            (V3, "7.69285150456672783825e-01"),
            (V4, "1.04222645593369134254e-01"),
            (V5, "3.21709242282423911810e-03"),
            (S0, "-7.72156649015328655494e-02"),
            (S1, "2.14982415960608852501e-01"),
            (S2, "3.25778796408930981787e-01"),
            (S3, "1.46350472652464452805e-01"),
            (S4, "2.66422703033638609560e-02"),
            (S5, "1.84028451407337715652e-03"),
            (S6, "3.19475326584100867617e-05"),
            (R1, "1.39200533467621045958e+00"),
            (R2, "7.21935547567138069525e-01"),
            (R3, "1.71933865632803078993e-01"),
            (R4, "1.86459191715652901344e-02"),
            (R5, "7.77942496381893596434e-04"),
            (R6, "7.32668430744625636189e-06"),
            (W0, "4.18938533204672725052e-01"),
            (W1, "8.33333333333329678849e-02"),
            (W2, "-2.77777777728775536470e-03"),
            (W3, "7.93650558643019558500e-04"),
            (W4, "-5.95187557450339963135e-04"),
            (W5, "8.36339918996282139126e-04"),
            (W6, "-1.63092934096575273989e-03"),
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
