//! `cexp` and `cexpf`: the complex exponential, and the scaled exponential
//! `ccosh` and `csinh` share with them.
//!
//! Ported from musl 1.2.5's `cexp.c`, `cexpf.c`, `__cexp.c` and `__cexpf.c`
//! (MIT; see [`crate::math`] for the notice), which came from FreeBSD's
//! `s_cexp.c`, `s_cexpf.c`, `k_exp.c` and `k_expf.c`, carrying this notice:
//!
//! ```text
//! Copyright (c) 2011 David Schultz <das@FreeBSD.ORG>
//! All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions
//! are met:
//! 1. Redistributions of source code must retain the above copyright
//!    notice, this list of conditions and the following disclaimer.
//! 2. Redistributions in binary form must reproduce the above copyright
//!    notice, this list of conditions and the following disclaimer in the
//!    documentation and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE AUTHOR AND CONTRIBUTORS ``AS IS'' AND
//! ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
//! IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
//! ARE DISCLAIMED.  IN NO EVENT SHALL THE AUTHOR OR CONTRIBUTORS BE LIABLE
//! FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
//! DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS
//! OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
//! HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT
//! LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY
//! OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF
//! SUCH DAMAGE.
//! ```
//!
//! # Method
//!
//! exp(x + iy) = exp(x) cos(y) + i exp(x) sin(y). Where exp(x) overflows but
//! the product need not, from ln(`DBL_MAX`) to ln(2 `DBL_MAX` /
//! `DBL_TRUE_MIN`), exp(x) is taken as exp(x - k ln2) 2^k and the power of two
//! is applied last, in two halves.

use crate::complex::{Complex, ComplexF};
use crate::math::exp::exp;
use crate::math::expf::expf;
use crate::math::support::{
    barrier, barrierf, from_words, hexf32, hexf64, high_word, low_word, with_high_word,
};
use crate::math::trig::{cos, sin};
use crate::math::trigf::{cosf, sinf};

/// The high word of ln(`DBL_MAX`) ≈ 709.78: where exp(x) overflows.
const EXP_OVFL: u32 = 0x4086_2e42;

/// The high word of ln(2 `DBL_MAX` / `DBL_TRUE_MIN`) ≈ 1454.9: where
/// exp(x) cos(y) overflows whatever y is.
const CEXP_OVFL: u32 = 0x4096_b8e4;

/// [`EXP_OVFL`] for `float`: ln(`FLT_MAX`) ≈ 88.72.
const EXP_OVFL_F: u32 = 0x42b1_7218;

/// [`CEXP_OVFL`] for `float`.
const CEXP_OVFL_F: u32 = 0x4340_0074;

/// The power of two taken out of exp(x): exp(k ln2) is close to 2^k.
const K: u32 = 1799;

/// k ln2, as musl writes it, 1246.97177782734161156.
const KLN2: f64 = hexf64!("0x1.37be319ba0da4p+10");

/// [`K`] for `float`.
const K_F: u32 = 235;

/// [`KLN2`] for `float`, 162.88958740.
const KLN2_F: f32 = hexf32!("0x1.45c778p+7");

/// e raised to `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn cexp(z: Complex) -> Complex {
    let x = z.re;
    let y = z.im;

    let hy = high_word(y) & 0x7fff_ffff;
    let ly = low_word(y);

    // cexp(x + i0) = exp(x) + i0
    if hy | ly == 0 {
        return Complex::new(exp(x), y);
    }
    let hx = high_word(x);
    let lx = low_word(x);
    // cexp(0 + iy) = cos(y) + i sin(y)
    if (hx & 0x7fff_ffff) | lx == 0 {
        return Complex::new(cos(y), sin(y));
    }

    if hy >= 0x7ff0_0000 {
        if lx != 0 || (hx & 0x7fff_ffff) != 0x7ff0_0000 {
            // cexp(finite|NaN ± i Inf|NaN) = NaN + iNaN
            return Complex::new(barrier(y) - y, barrier(y) - y);
        } else if hx & 0x8000_0000 != 0 {
            // cexp(-Inf ± i Inf|NaN) = 0 + i0
            return Complex::new(0.0, 0.0);
        } else {
            // cexp(+Inf ± i Inf|NaN) = Inf + iNaN
            return Complex::new(x, barrier(y) - y);
        }
    }

    if (EXP_OVFL..=CEXP_OVFL).contains(&hx) {
        // x is between 709.7 and 1454.3: scale to avoid overflow in exp(x).
        ldexp_cexp(z, 0)
    } else {
        // x < 709.7, where exp(x) does not overflow; or x > 1454.3, where
        // the product overflows whatever y is; or x is ±Inf or NaN.
        let exp_x = exp(x);
        Complex::new(exp_x * cos(y), exp_x * sin(y))
    }
}

/// [`cexp`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn cexpf(z: ComplexF) -> ComplexF {
    let x = z.re;
    let y = z.im;

    let hy = y.to_bits() & 0x7fff_ffff;

    if hy == 0 {
        return ComplexF::new(expf(x), y);
    }
    let hx = x.to_bits();
    if hx & 0x7fff_ffff == 0 {
        return ComplexF::new(cosf(y), sinf(y));
    }

    if hy >= 0x7f80_0000 {
        if hx & 0x7fff_ffff != 0x7f80_0000 {
            return ComplexF::new(barrierf(y) - y, barrierf(y) - y);
        } else if hx & 0x8000_0000 != 0 {
            return ComplexF::new(0.0, 0.0);
        } else {
            return ComplexF::new(x, barrierf(y) - y);
        }
    }

    if (EXP_OVFL_F..=CEXP_OVFL_F).contains(&hx) {
        // x is between 88.7 and 192.
        ldexp_cexpf(z, 0)
    } else {
        let exp_x = expf(x);
        ComplexF::new(exp_x * cosf(y), exp_x * sinf(y))
    }
}

/// exp(`x`) as a number in [2^1023, 2^1024) and a power of two, for x from
/// ln(`DBL_MAX`) to 1454.9. musl's `__frexp_exp`.
fn frexp_exp(x: f64) -> (f64, i32) {
    // exp(x) = exp(x - k ln2) 2^k, and the result's exponent is set to the
    // largest, so a tiny factor can scale it down without losing bits to
    // subnormals.
    let exp_x = exp(x - KLN2);
    let hx = high_word(exp_x);
    let expt = (hx >> 20).wrapping_sub(0x3ff + 1023).wrapping_add(K) as i32;
    let exp_x = with_high_word(exp_x, (hx & 0xfffff) | ((0x3ff + 1023) << 20));
    (exp_x, expt)
}

/// [`frexp_exp`] for `float`: a number in [2^127, 2^128) and a power of two.
fn frexp_expf(x: f32) -> (f32, i32) {
    let exp_x = expf(x - KLN2_F);
    let hx = exp_x.to_bits();
    let expt = (hx >> 23).wrapping_sub(0x7f + 127).wrapping_add(K_F) as i32;
    let exp_x = f32::from_bits((hx & 0x7f_ffff) | ((0x7f + 127) << 23));
    (exp_x, expt)
}

/// exp(`z`) × 2^`expt`, for a real part from ln(`DBL_MAX`) up, where
/// exp(z) would overflow, and a small `expt`. musl's `__ldexp_cexp`.
pub(crate) fn ldexp_cexp(z: Complex, expt: i32) -> Complex {
    let x = z.re;
    let y = z.im;
    let (exp_x, ex_expt) = frexp_exp(x);
    let expt = expt.wrapping_add(ex_expt);

    // scale1 × scale2 = 2^expt, which is faster than `scalbn`.
    let half_expt = expt / 2;
    let scale1 = from_words((0x3ff_i32.wrapping_add(half_expt) << 20) as u32, 0);
    let half_expt = expt.wrapping_sub(half_expt);
    let scale2 = from_words((0x3ff_i32.wrapping_add(half_expt) << 20) as u32, 0);

    Complex::new(
        cos(y) * exp_x * scale1 * scale2,
        sin(y) * exp_x * scale1 * scale2,
    )
}

/// [`ldexp_cexp`] for `float complex`. musl's `__ldexp_cexpf`.
pub(crate) fn ldexp_cexpf(z: ComplexF, expt: i32) -> ComplexF {
    let x = z.re;
    let y = z.im;
    let (exp_x, ex_expt) = frexp_expf(x);
    let expt = expt.wrapping_add(ex_expt);

    let half_expt = expt / 2;
    let scale1 = f32::from_bits((0x7f_i32.wrapping_add(half_expt) << 23) as u32);
    let half_expt = expt.wrapping_sub(half_expt);
    let scale2 = f32::from_bits((0x7f_i32.wrapping_add(half_expt) << 23) as u32);

    ComplexF::new(
        cosf(y) * exp_x * scale1 * scale2,
        sinf(y) * exp_x * scale1 * scale2,
    )
}
