//! `csqrt` and `csqrtf`: the complex square root.
//!
//! Ported from musl 1.2.5's `csqrt.c` and `csqrtf.c` (MIT; see
//! [`crate::math`] for the notice), which came from FreeBSD's `s_csqrt.c` and
//! `s_csqrtf.c`, carrying this notice:
//!
//! ```text
//! Copyright (c) 2007 David Schultz <das@FreeBSD.ORG>
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
//! Algorithm 312, CACM vol 10, Oct 1967: with t = √((|a| + |z|) / 2), the
//! root of a + ib is t + ib/2t for a >= 0 and |b|/2t ± it otherwise. `csqrt`
//! quarters both parts first where |z| might overflow, and doubles the
//! result; `csqrtf` computes in `double`, where nothing overflows.

use crate::complex::{
    Complex, ComplexF, div, equals_zero, equals_zerof, is_inf, is_inff, ordered_ge, ordered_gef,
    sign_bit, sign_bitf,
};
use crate::math::hypot::hypot;
use crate::math::manipulate::{copysign, copysignf, fabs, fabsf};
use crate::math::sqrt::sqrt;
use crate::math::support::{barrier, barrierf, hexf64, is_nan, is_nanf};

/// Parts from here up risk overflow: `DBL_MAX` / (1 + √2).
const THRESH: f64 = hexf64!("0x1.a827999fcef32p+1022");

/// The square root of `z`, with the branch cut along the negative real axis
/// and a non-negative real part.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn csqrt(z: Complex) -> Complex {
    let mut a = z.re;
    let mut b = z.im;

    // Special cases.
    if equals_zero(a) && equals_zero(b) {
        return Complex::new(0.0, b);
    }
    if is_inf(b) {
        return Complex::new(f64::INFINITY, b);
    }
    if is_nan(a) {
        // Invalid unless b is a NaN.
        let t = (barrier(b) - b) / (barrier(b) - b);
        return Complex::new(a, t);
    }
    if is_inf(a) {
        // csqrt(inf + NaN i)  = inf +  NaN i
        // csqrt(inf + y i)    = inf +  0 i
        // csqrt(-inf + NaN i) = NaN +- inf i
        // csqrt(-inf + y i)   = 0   +  inf i
        if sign_bit(a) {
            return Complex::new(fabs(barrier(b) - b), copysign(a, b));
        } else {
            return Complex::new(a, copysign(barrier(b) - b, b));
        }
    }
    // A NaN b takes the normal path.

    // Scale to avoid overflow. GCC compares with `comisd`, which raises
    // invalid for a NaN b.
    let scale = if ordered_ge(fabs(a), THRESH) || ordered_ge(fabs(b), THRESH) {
        a *= 0.25;
        b *= 0.25;
        true
    } else {
        false
    };

    let result = if ordered_ge(a, 0.0) {
        let t = sqrt((a + hypot(a, b)) * 0.5);
        Complex::new(t, b / (2.0 * t))
    } else {
        let t = sqrt((-a + hypot(a, b)) * 0.5);
        Complex::new(div(fabs(b), 2.0 * t), copysign(t, b))
    };

    // Rescale.
    if scale {
        return Complex::new(result.re * 2.0, result.im * 2.0);
    }
    result
}

/// [`csqrt`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn csqrtf(z: ComplexF) -> ComplexF {
    let a = z.re;
    let b = z.im;

    if equals_zerof(a) && equals_zerof(b) {
        return ComplexF::new(0.0, b);
    }
    if is_inff(b) {
        return ComplexF::new(f32::INFINITY, b);
    }
    if is_nanf(a) {
        let t = (barrierf(b) - b) / (barrierf(b) - b);
        return ComplexF::new(a, t);
    }
    if is_inff(a) {
        if sign_bitf(a) {
            return ComplexF::new(fabsf(barrierf(b) - b), copysignf(a, b));
        } else {
            return ComplexF::new(a, copysignf(barrierf(b) - b, b));
        }
    }

    // t in `double` avoids overflow, and rounds correctly in nearly all
    // cases.
    if ordered_gef(a, 0.0) {
        let t = sqrt((f64::from(a) + hypot(f64::from(a), f64::from(b))) * 0.5);
        ComplexF::new(t as f32, (f64::from(b) / (2.0 * t)) as f32)
    } else {
        let t = sqrt((f64::from(-a) + hypot(f64::from(a), f64::from(b))) * 0.5);
        ComplexF::new(
            div(f64::from(fabsf(b)), 2.0 * t) as f32,
            copysignf(t as f32, b),
        )
    }
}
