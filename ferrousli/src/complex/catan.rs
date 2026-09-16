//! `catan` and `catanh`, and their `float` forms: the inverse tangent,
//! circular and hyperbolic.
//!
//! Ported from musl 1.2.5's `catan.c`, `catanf.c`, `catanh.c` and `catanhf.c`
//! (MIT; see [`crate::math`] for the notice). `catanh` musl wrote itself.
//! `catan` came from OpenBSD's `s_catan.c` and `s_catanf.c`, carrying this
//! notice:
//!
//! ```text
//! Copyright (c) 2008 Stephen L. Moshier <steve@moshier.net>
//!
//! Permission to use, copy, modify, and distribute this software for any
//! purpose with or without fee is hereby granted, provided that the above
//! copyright notice and this permission notice appear in all copies.
//!
//! THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
//! WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
//! MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
//! ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
//! WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
//! ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
//! OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//! ```
//!
//! # Method
//!
//! For z = x + iy,
//!
//! ```text
//! Re atan z = ½ atan(2x / (1 - x² - y²)) + kπ
//! Im atan z = ¼ log((x² + (y + 1)²) / (x² + (y - 1)²))
//! ```
//!
//! with the real part brought back by the nearest multiple of π, subtracted
//! in three pieces. atanh z = -i atan(iz).

use crate::complex::{Complex, ComplexF, add, addf, ordered_ge, ordered_gef, sub, subf};
use crate::math::arch::{trunc_to_i64, trunc_to_i64f};
use crate::math::atan::{atan2, atan2f};
use crate::math::log::log;
use crate::math::logf::logf;
use crate::math::support::{hexf32, hexf64};

/// π, `M_PI`.
const PI: f64 = hexf64!("0x1.921fb54442d18p+1");

/// π in three pieces, 3.14159265160560607910: the first.
const DP1: f64 = hexf64!("0x1.921fb54p+1");

/// The second piece of π, 1.98418714791870343106e-9.
const DP2: f64 = hexf64!("0x1.10b461p-29");

/// The third piece of π, 1.14423774522196636802e-17.
const DP3: f64 = hexf64!("0x1.a62633145c06ep-57");

/// π rounded to `float`, as musl's `float_pi` is initialised.
const PI_F: f32 = hexf32!("0x1.921fb6p+1");

/// [`DP1`] for `float`, 3.140625, still a `double`.
const DP1_F: f64 = hexf64!("0x1.92p+1");

/// [`DP2`] for `float`, 9.67502593994140625e-4.
const DP2_F: f64 = hexf64!("0x1.fb4p-11");

/// [`DP3`] for `float`, 1.509957990978376432e-7.
const DP3_F: f64 = hexf64!("0x1.4442d18469899p-23");

/// `x` less the multiple of π nearest it. musl's `_redupi`.
fn redupi(x: f64) -> f64 {
    let mut t = x / PI;
    // `comisd` raises invalid for a NaN, and so does the conversion.
    if ordered_ge(t, 0.0) {
        t += 0.5;
    } else {
        t -= 0.5;
    }

    // The multiple, truncated as C converts: `i64::MIN` for a NaN.
    let t = trunc_to_i64(t) as f64;
    // Each product of a small integer and a piece is exact.
    ((x - t * DP1) - t * DP2) - t * DP3
}

/// [`redupi`] for `float`, subtracting in `double`. musl's `_redupif`.
fn redupif(x: f32) -> f32 {
    let mut t = x / PI_F;
    if ordered_gef(t, 0.0) {
        t += 0.5;
    } else {
        t -= 0.5;
    }

    let t = f64::from(trunc_to_i64f(t) as f32);
    (((f64::from(x) - t * DP1_F) - t * DP2_F) - t * DP3_F) as f32
}

/// The inverse tangent of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn catan(z: Complex) -> Complex {
    let x = z.re;
    let y = z.im;

    let x2 = x * x;
    // Operands in the order GCC's code has them, which says whose NaN wins.
    let a = sub(1.0 - x2, y * y);

    let t = 0.5 * atan2(2.0 * x, a);
    let w = redupi(t);

    let t = y - 1.0;
    let a = add(t * t, x2);

    let t = y + 1.0;
    let a = add(t * t, x2) / a;
    Complex::new(w, 0.25 * log(a))
}

/// [`catan`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn catanf(z: ComplexF) -> ComplexF {
    let x = z.re;
    let y = z.im;

    let x2 = x * x;
    let a = subf(1.0 - x2, y * y);

    let t = 0.5 * atan2f(2.0 * x, a);
    let w = redupif(t);

    let t = y - 1.0;
    let a = addf(t * t, x2);

    let t = y + 1.0;
    let a = addf(t * t, x2) / a;
    ComplexF::new(w, 0.25 * logf(a))
}

/// The inverse hyperbolic tangent of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn catanh(z: Complex) -> Complex {
    let z = catan(Complex::new(-z.im, z.re));
    Complex::new(z.im, -z.re)
}

/// [`catanh`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn catanhf(z: ComplexF) -> ComplexF {
    let z = catanf(ComplexF::new(-z.im, z.re));
    ComplexF::new(z.im, -z.re)
}
