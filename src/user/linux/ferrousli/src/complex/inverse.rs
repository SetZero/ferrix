//! `casin`, `cacos`, `casinh` and `cacosh`, and their `float` forms: the
//! inverse sine and cosine, circular and hyperbolic.
//!
//! Ported from musl 1.2.5's `casin.c`, `cacos.c`, `casinh.c`, `cacosh.c` and
//! their `f` forms (MIT; see [`crate::math`] for the notice). musl wrote these
//! itself; they carry no other notice.
//!
//! # Method
//!
//! asin z = -i log(iz + √(1 - z²)), acos z = π/2 - asin z, asinh z =
//! -i asin(iz), and acosh z = ±i acos z, the sign taken from z's imaginary
//! part.

use crate::complex::csqrt::{csqrt, csqrtf};
use crate::complex::log::{clog, clogf};
use crate::complex::{Complex, ComplexF, add, addf, mul, sign_bit, sign_bitf, sub, subf};
use crate::math::support::{hexf32, hexf64};

/// π/2, `M_PI_2`.
const PI_2: f64 = hexf64!("0x1.921fb54442d18p+0");

/// π/2 rounded to `float`, as musl's `float_pi_2` is initialised.
const PI_2_F: f32 = hexf32!("0x1.921fb6p+0");

/// The inverse sine of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn casin(z: Complex) -> Complex {
    let x = z.re;
    let y = z.im;
    // Operands in the order GCC's code has them, which says whose NaN wins.
    let w = Complex::new(1.0 - sub(x, y) * add(x, y), mul(mul(x, -2.0), y));
    let s = csqrt(w);
    // iz + s, added as GCC adds it.
    let r = clog(Complex::new(sub(s.re, y), add(s.im, x)));
    Complex::new(r.im, -r.re)
}

/// [`casin`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn casinf(z: ComplexF) -> ComplexF {
    let x = z.re;
    let y = z.im;
    // musl subtracts from `double` 1.0, which GCC does in `float`, the same;
    // the product -2xy it computes in `double`.
    let w = ComplexF::new(
        1.0 - subf(x, y) * addf(x, y),
        mul(mul(f64::from(x), -2.0), f64::from(y)) as f32,
    );
    let s = csqrtf(w);
    let r = clogf(ComplexF::new(subf(s.re, y), addf(s.im, x)));
    ComplexF::new(r.im, -r.re)
}

/// The inverse cosine of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn cacos(z: Complex) -> Complex {
    let z = casin(z);
    Complex::new(PI_2 - z.re, -z.im)
}

/// [`cacos`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn cacosf(z: ComplexF) -> ComplexF {
    let z = casinf(z);
    ComplexF::new(PI_2_F - z.re, -z.im)
}

/// The inverse hyperbolic sine of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn casinh(z: Complex) -> Complex {
    let z = casin(Complex::new(-z.im, z.re));
    Complex::new(z.im, -z.re)
}

/// [`casinh`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn casinhf(z: ComplexF) -> ComplexF {
    let z = casinf(ComplexF::new(-z.im, z.re));
    ComplexF::new(z.im, -z.re)
}

/// The inverse hyperbolic cosine of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cacosh(z: Complex) -> Complex {
    let negative = sign_bit(z.im);
    let z = cacos(z);
    if negative {
        Complex::new(z.im, -z.re)
    } else {
        Complex::new(-z.im, z.re)
    }
}

/// [`cacosh`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cacoshf(z: ComplexF) -> ComplexF {
    let negative = sign_bitf(z.im);
    let z = cacosf(z);
    if negative {
        ComplexF::new(z.im, -z.re)
    } else {
        ComplexF::new(-z.im, z.re)
    }
}
