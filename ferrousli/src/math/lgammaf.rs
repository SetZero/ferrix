//! `lgammaf` and `lgammaf_r`: [`lgamma`](crate::math::lgamma::lgamma) and
//! [`lgamma_r`](crate::math::lgamma::lgamma_r) for `float`.
//!
//! Ported from musl 1.2.5's `lgammaf.c` and `lgammaf_r.c` (MIT; see
//! [`crate::math`] for the notice). musl took `lgammaf_r.c` from FreeBSD's
//! msun, where Ian Lance Taylor, Cygnus Support, converted it to `float`. The
//! msun file carries this notice:
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
//! As with `lgamma_r`, musl's own name for the function, `__lgammaf_r`, is
//! hidden, and only `lgammaf_r` is exported.
//!
//! # Method
//!
//! As for `double`, in `float` arithmetic, except that sin(πx) for the
//! reflection is evaluated in `double`.

use core::ffi::c_int;
use core::sync::atomic::Ordering;

use crate::math::arch::trunc_to_i64f;
use crate::math::lgamma::signgam;
use crate::math::logf::logf;
use crate::math::rounding::floorf;
use crate::math::support::barrierf;
use crate::math::trigf::{kernel_cosf, kernel_sinf};

/// π, rounded.
const PI: f32 = f32::from_bits(0x4049_0fdb);

// lgammaf(x) around 2: a0 to a11.
const A0: f32 = f32::from_bits(0x3d9e_233f);
const A1: f32 = f32::from_bits(0x3ea5_1a66);
const A2: f32 = f32::from_bits(0x3d89_f001);
const A3: f32 = f32::from_bits(0x3ca8_9915);
const A4: f32 = f32::from_bits(0x3bf2_027e);
const A5: f32 = f32::from_bits(0x3b3d_6ec6);
const A6: f32 = f32::from_bits(0x3a9c_54a1);
const A7: f32 = f32::from_bits(0x3a05_b634);
const A8: f32 = f32::from_bits(0x3967_9767);
const A9: f32 = f32::from_bits(0x38e2_8445);
const A10: f32 = f32::from_bits(0x37d3_83a2);
const A11: f32 = f32::from_bits(0x383c_2c75);

/// Where lgamma has its minimum, rounded.
const TC: f32 = f32::from_bits(0x3fbb_16c3);
/// lgamma(tc), rounded.
const TF: f32 = f32::from_bits(0xbdf8_cdcd);
/// The error of `TF`, negated.
const TT: f32 = f32::from_bits(0x31e6_1c52);
// lgammaf(x) - tf around tc: t0 to t14.
const T0: f32 = f32::from_bits(0x3ef7_b95e);
const T1: f32 = f32::from_bits(0xbe17_213c);
const T2: f32 = f32::from_bits(0x3d84_5a15);
const T3: f32 = f32::from_bits(0xbd06_4d47);
const T4: f32 = f32::from_bits(0x3c93_373d);
const T5: f32 = f32::from_bits(0xbc28_fcfe);
const T6: f32 = f32::from_bits(0x3bc7_e707);
const T7: f32 = f32::from_bits(0xbb71_77fe);
const T8: f32 = f32::from_bits(0x3b14_1699);
const T9: f32 = f32::from_bits(0xbab7_f476);
const T10: f32 = f32::from_bits(0x3a66_f867);
const T11: f32 = f32::from_bits(0xba0d_3085);
const T12: f32 = f32::from_bits(0x39a5_7b6b);
const T13: f32 = f32::from_bits(0xb9a3_f927);
const T14: f32 = f32::from_bits(0x39af_e9f7);

// lgammaf(x) around 1: u0 to u5 over v1 to v5.
const U0: f32 = f32::from_bits(0xbd9e_233f);
const U1: f32 = f32::from_bits(0x3f22_00f4);
const U2: f32 = f32::from_bits(0x3fba_3ae7);
const U3: f32 = f32::from_bits(0x3f7a_4bb2);
const U4: f32 = f32::from_bits(0x3e6a_7578);
const U5: f32 = f32::from_bits(0x3c5b_3c5e);
const V1: f32 = f32::from_bits(0x401d_2ebe);
const V2: f32 = f32::from_bits(0x4008_392d);
const V3: f32 = f32::from_bits(0x3f44_efdf);
const V4: f32 = f32::from_bits(0x3dd5_72af);
const V5: f32 = f32::from_bits(0x3b52_d5db);

// lgammaf(2 + s) - s/2 on [0, 1): s0 to s6 over r1 to r6.
const S0: f32 = f32::from_bits(0xbd9e_233f);
const S1: f32 = f32::from_bits(0x3e5c_245a);
const S2: f32 = f32::from_bits(0x3ea6_cc7a);
const S3: f32 = f32::from_bits(0x3e15_dce6);
const S4: f32 = f32::from_bits(0x3cda_40e4);
const S5: f32 = f32::from_bits(0x3af1_35b4);
const S6: f32 = f32::from_bits(0x3805_ff67);
const R1: f32 = f32::from_bits(0x3fb2_2d3b);
const R2: f32 = f32::from_bits(0x3f38_d0c5);
const R3: f32 = f32::from_bits(0x3e30_0f6e);
const R4: f32 = f32::from_bits(0x3c98_bf54);
const R5: f32 = f32::from_bits(0x3a4b_eed6);
const R6: f32 = f32::from_bits(0x36f5_d7bd);

// lgammaf(x) - (x - 1/2)(log(x) - 1) from 8 up, in 1/x: w0 to w6.
const W0: f32 = f32::from_bits(0x3ed6_7f1d);
const W1: f32 = f32::from_bits(0x3daa_aaab);
const W2: f32 = f32::from_bits(0xbb36_0b61);
const W3: f32 = f32::from_bits(0x3a50_0cfd);
const W4: f32 = f32::from_bits(0xba1c_065c);
const W5: f32 = f32::from_bits(0x3a5b_3dd2);
const W6: f32 = f32::from_bits(0xbad5_c4e8);

/// π as a `double`: musl's `3.14159265358979323846`.
const PI_DOUBLE: f64 = f64::from_bits(0x4009_21fb_5444_2d18);

/// sin(π`x`) for `x` > 2^-100, rounded to `float`. Where it is 0, its sign is
/// arbitrary. musl's `sin_pi`.
fn sin_pi(x: f32) -> f32 {
    // x mod 2, with a spurious inexact for an odd integer.
    let x = 2.0 * (x * 0.5 - floorf(x * 0.5));

    // The conversion raises inexact as C's does.
    let n = trunc_to_i64f(x * 4.0) as i32;
    let n = (n + 1) / 2;
    // musl's `x - n*0.5f`, in `float`, and exact.
    let y = f64::from(x - n as f32 * 0.5);
    let y = y * PI_DOUBLE;

    match n {
        1 => kernel_cosf(y),
        2 => kernel_sinf(-y),
        3 => -kernel_cosf(y),
        _ => kernel_sinf(y),
    }
}

/// log|Γ(`x`)| and the sign of Γ(x). musl's `__lgammaf_r`, with the sign
/// returned rather than written.
pub(crate) fn lgammaf_sign(mut x: f32) -> (f32, c_int) {
    let u = x.to_bits();
    let mut sign_gamma = 1;
    let sign = u >> 31 != 0;
    let ix = u & 0x7fff_ffff;

    // ±inf, NaN, ±0, tiny and negative arguments.
    if ix >= 0x7f80_0000 {
        return (x * x, sign_gamma);
    }
    if ix < 0x3500_0000 {
        // |x| < 2^-21: -log(|x|).
        if sign {
            sign_gamma = -1;
            x = -x;
        }
        return (-logf(x), sign_gamma);
    }
    let mut nadj = 0.0;
    if sign {
        x = -x;
        let mut t = sin_pi(x);
        if t == 0.0 {
            // A negative integer: ±inf, as x - x is -0 when rounding
            // downward. The barrier only keeps clippy from calling x - x a
            // mistake.
            return (1.0 / (barrierf(x) - x), sign_gamma);
        }
        if t > 0.0 {
            sign_gamma = -1;
        } else {
            t = -t;
        }
        nadj = logf(PI / (t * x));
    }

    let mut r;
    if ix == 0x3f80_0000 || ix == 0x4000_0000 {
        // 1 and 2.
        r = 0.0;
    } else if ix < 0x4000_0000 {
        // x < 2.
        let (y, i);
        if ix <= 0x3f66_6666 {
            // lgamma(x) = lgamma(x + 1) - log(x).
            r = -logf(x);
            (y, i) = if ix >= 0x3f3b_4a20 {
                (1.0 - x, 0)
            } else if ix >= 0x3e6d_3308 {
                (x - (TC - 1.0), 1)
            } else {
                (x, 2)
            };
        } else {
            r = 0.0;
            (y, i) = if ix >= 0x3fdd_a618 {
                // [1.7316, 2].
                (2.0 - x, 0)
            } else if ix >= 0x3f9d_a620 {
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
                let p = z * p1 - barrierf(TT - w * (p2 + y * p3));
                r += TF + p;
            }
            _ => {
                let p1 = y * (U0 + y * (U1 + y * (U2 + y * (U3 + y * (U4 + y * U5)))));
                let p2 = 1.0 + y * (V1 + y * (V2 + y * (V3 + y * (V4 + y * V5))));
                r += -0.5 * y + p1 / p2;
            }
        }
    } else if ix < 0x4100_0000 {
        // x < 8. The conversion raises inexact as C's does.
        let i = trunc_to_i64f(x) as i32;
        let y = x - i as f32;
        let p = y * (S0 + y * (S1 + y * (S2 + y * (S3 + y * (S4 + y * (S5 + y * S6))))));
        let q = 1.0 + y * (R1 + y * (R2 + y * (R3 + y * (R4 + y * (R5 + y * R6)))));
        r = 0.5 * y + p / q;
        // lgamma(1 + s) = log(s) + lgamma(s): musl's switch falls through
        // from case i down to case 3, multiplying by y + i - 1 and down.
        if i >= 3 {
            let mut z = 1.0;
            let mut k = i;
            while k >= 3 {
                z *= y + (k - 1) as f32;
                k -= 1;
            }
            r += logf(z);
        }
    } else if ix < 0x5c80_0000 {
        // 8 <= x < 2^58.
        let t = logf(x);
        let z = 1.0 / x;
        let y = z * z;
        let w = W0 + z * (W1 + y * (W2 + y * (W3 + y * (W4 + y * (W5 + y * W6)))));
        r = (x - 0.5) * (t - 1.0) + w;
    } else {
        // 2^58 <= x.
        r = x * (logf(x) - 1.0);
    }
    if sign {
        r = nadj - r;
    }
    (r, sign_gamma)
}

/// [`lgamma`](crate::math::lgamma::lgamma) for `float`, leaving the sign of
/// Γ(`x`) in [`signgam`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lgammaf(x: f32) -> f32 {
    let (y, sign) = lgammaf_sign(x);
    signgam.store(sign, Ordering::Relaxed);
    y
}

/// [`lgammaf`], writing the sign of Γ(`x`) to `*signgamp` instead.
///
/// # Safety
///
/// `signgamp` must be valid for writing an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lgammaf_r(x: f32, signgamp: *mut c_int) -> f32 {
    let (y, sign) = lgammaf_sign(x);
    // SAFETY: the caller passes a pointer valid for writing an `int`.
    unsafe { signgamp.write(sign) };
    y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::lgamma::SIGNGAM_TESTS;
    use crate::math::mtest::{self, Rules};

    #[test]
    fn lgammaf_matches_libc_test() {
        let _serial = SIGNGAM_TESTS.lock();
        let files = ["sanity/lgammaf.h", "special/lgammaf.h"];
        // libc-test's `lgammaf.c` tolerates an error under 2 ulps, but never
        // a wrong sign.
        let rules = Rules::ULP.tolerate(2.0);
        let call = |x| {
            let y = lgammaf(x);
            (y, signgam.load(Ordering::Relaxed))
        };
        mtest::d_di("lgammaf", &files, call, rules, &[]);
    }

    #[test]
    fn lgammaf_r_matches_libc_test() {
        let files = ["sanity/lgammaf_r.h", "special/lgammaf_r.h"];
        // As `lgammaf.c`.
        let rules = Rules::ULP.tolerate(2.0);
        let call = |x| {
            let mut sign = 0;
            // SAFETY: `sign` is an `int` to write.
            let y = unsafe { lgammaf_r(x, &raw mut sign) };
            (y, sign)
        };
        mtest::d_di("lgammaf_r", &files, call, rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        // musl writes these in decimal; its comments give the bits.
        let floats = [
            (PI, "3.1415927410e+00"),
            (A0, "7.7215664089e-02"),
            (A1, "3.2246702909e-01"),
            (A2, "6.7352302372e-02"),
            (A3, "2.0580807701e-02"),
            (A4, "7.3855509982e-03"),
            (A5, "2.8905137442e-03"),
            (A6, "1.1927076848e-03"),
            (A7, "5.1006977446e-04"),
            (A8, "2.2086278477e-04"),
            (A9, "1.0801156895e-04"),
            (A10, "2.5214456400e-05"),
            (A11, "4.4864096708e-05"),
            (TC, "1.4616321325e+00"),
            (TF, "-1.2148628384e-01"),
            (TT, "6.6971006518e-09"),
            (T0, "4.8383611441e-01"),
            (T1, "-1.4758771658e-01"),
            (T2, "6.4624942839e-02"),
            (T3, "-3.2788541168e-02"),
            (T4, "1.7970675603e-02"),
            (T5, "-1.0314224288e-02"),
            (T6, "6.1005386524e-03"),
            (T7, "-3.6845202558e-03"),
            (T8, "2.2596477065e-03"),
            (T9, "-1.4034647029e-03"),
            (T10, "8.8108185446e-04"),
            (T11, "-5.3859531181e-04"),
            (T12, "3.1563205994e-04"),
            (T13, "-3.1275415677e-04"),
            (T14, "3.3552918467e-04"),
            (U0, "-7.7215664089e-02"),
            (U1, "6.3282704353e-01"),
            (U2, "1.4549225569e+00"),
            (U3, "9.7771751881e-01"),
            (U4, "2.2896373272e-01"),
            (U5, "1.3381091878e-02"),
            (V1, "2.4559779167e+00"),
            (V2, "2.1284897327e+00"),
            (V3, "7.6928514242e-01"),
            (V4, "1.0422264785e-01"),
            (V5, "3.2170924824e-03"),
            (S0, "-7.7215664089e-02"),
            (S1, "2.1498242021e-01"),
            (S2, "3.2577878237e-01"),
            (S3, "1.4635047317e-01"),
            (S4, "2.6642270386e-02"),
            (S5, "1.8402845599e-03"),
            (S6, "3.1947532989e-05"),
            (R1, "1.3920053244e+00"),
            (R2, "7.2193557024e-01"),
            (R3, "1.7193385959e-01"),
            (R4, "1.8645919859e-02"),
            (R5, "7.7794247773e-04"),
            (R6, "7.3266842264e-06"),
            (W0, "4.1893854737e-01"),
            (W1, "8.3333335817e-02"),
            (W2, "-2.7777778450e-03"),
            (W3, "7.9365057172e-04"),
            (W4, "-5.9518753551e-04"),
            (W5, "8.3633989561e-04"),
            (W6, "-1.6309292987e-03"),
        ];
        for (value, text) in floats {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f32>().map(f32::to_bits),
                "{text}"
            );
        }
        assert_eq!(Ok(PI_DOUBLE), "3.14159265358979323846".parse());
    }
}
