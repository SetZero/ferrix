//! `cabs`, `carg`, `clog` and `cpow`, and their `float` forms: the modulus,
//! the argument, the logarithm and the power.
//!
//! Ported from musl 1.2.5's `cabs.c`, `carg.c`, `clog.c`, `cpow.c` and their
//! `f` forms (MIT; see [`crate::math`] for the notice). musl wrote these
//! itself; they carry no other notice.
//!
//! # Method
//!
//! |z| is `hypot` of the parts and arg z is `atan2` of them. log z =
//! log |z| + i arg z, and z^c = exp(c log z).
//!
//! C multiplies `c` by log z as (ac - bd) + i(ad + bc), and if either part
//! is a NaN, calls libgcc's `__muldc3` with log z first, which multiplies
//! again and then recovers the infinities a NaN may hide. That function is
//! the example in C11's Annex G.5.1, which [`multiply_recovering`] follows,
//! with each operation's operands in the order libgcc's code has them, so
//! that where two NaNs meet the same one wins, and each `isnan` and `isinf`
//! a comparison, so that a subnormal raises denormal where it does there.

use crate::complex::cexp::{cexp, cexpf};
use crate::complex::{
    Complex, ComplexF, add, addf, compares_inf, compares_inff, compares_nan, compares_nanf, mul,
    mulf, sub, subf, unordered, unorderedf,
};
use crate::math::atan::{atan2, atan2f};
use crate::math::hypot::{hypot, hypotf};
use crate::math::log::log;
use crate::math::logf::logf;
use crate::math::manipulate::{copysign, copysignf};

/// The modulus of `z`, √(re² + im²), without undue overflow or underflow.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cabs(z: Complex) -> f64 {
    hypot(z.re, z.im)
}

/// [`cabs`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cabsf(z: ComplexF) -> f32 {
    hypotf(z.re, z.im)
}

/// The argument of `z`, in [-π, π], with the branch cut along the negative
/// real axis.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn carg(z: Complex) -> f64 {
    atan2(z.im, z.re)
}

/// [`carg`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cargf(z: ComplexF) -> f32 {
    atan2f(z.im, z.re)
}

/// The natural logarithm of `z`, with the branch cut along the negative real
/// axis.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn clog(z: Complex) -> Complex {
    let r = cabs(z);
    let phi = carg(z);
    Complex::new(log(r), phi)
}

/// [`clog`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn clogf(z: ComplexF) -> ComplexF {
    let r = cabsf(z);
    let phi = cargf(z);
    ComplexF::new(logf(r), phi)
}

/// `z` raised to the power `c`, exp(c log z), with the branch cut of
/// [`clog`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cpow(z: Complex, c: Complex) -> Complex {
    let l = clog(z);
    // C's `c * l`: the plain product, tested with one `ucomisd` of its parts.
    let x = c.re * l.re - c.im * l.im;
    let y = c.re * l.im + c.im * l.re;
    let product = if unordered(x, y) {
        multiply_recovering(l, c)
    } else {
        Complex::new(x, y)
    };
    cexp(product)
}

/// [`cpow`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cpowf(z: ComplexF, c: ComplexF) -> ComplexF {
    let l = clogf(z);
    let x = c.re * l.re - c.im * l.im;
    let y = c.re * l.im + c.im * l.re;
    let product = if unorderedf(x, y) {
        multiply_recoveringf(l, c)
    } else {
        ComplexF::new(x, y)
    };
    cexpf(product)
}

/// `z` × `w`, with the infinities C's Annex G wants where the plain product
/// makes NaN + iNaN of them: libgcc's `__muldc3`.
#[inline(never)]
fn multiply_recovering(z: Complex, w: Complex) -> Complex {
    let (mut a, mut b, mut c, mut d) = (z.re, z.im, w.re, w.im);
    let ac = mul(a, c);
    let bd = mul(b, d);
    let ad = mul(a, d);
    let bc = mul(c, b);
    let mut x = sub(ac, bd);
    let mut y = add(ad, bc);
    if compares_nan(x) && compares_nan(y) {
        let mut recalc = false;
        if compares_inf(a) || compares_inf(b) {
            // z is infinite: box the infinity, and the other factor's NaNs
            // become zeros.
            a = copysign(if compares_inf(a) { 1.0 } else { 0.0 }, a);
            b = copysign(if compares_inf(b) { 1.0 } else { 0.0 }, b);
            if compares_nan(c) {
                c = copysign(0.0, c);
            }
            if compares_nan(d) {
                d = copysign(0.0, d);
            }
            recalc = true;
        }
        if compares_inf(c) || compares_inf(d) {
            // w is infinite.
            c = copysign(if compares_inf(c) { 1.0 } else { 0.0 }, c);
            d = copysign(if compares_inf(d) { 1.0 } else { 0.0 }, d);
            if compares_nan(a) {
                a = copysign(0.0, a);
            }
            if compares_nan(b) {
                b = copysign(0.0, b);
            }
            recalc = true;
        }
        if !recalc && (compares_inf(ac) || compares_inf(bd) || compares_inf(ad) || compares_inf(bc))
        {
            // An infinity from overflow.
            if compares_nan(a) {
                a = copysign(0.0, a);
            }
            if compares_nan(b) {
                b = copysign(0.0, b);
            }
            if compares_nan(c) {
                c = copysign(0.0, c);
            }
            if compares_nan(d) {
                d = copysign(0.0, d);
            }
            recalc = true;
        }
        if recalc {
            x = mul(sub(mul(a, c), mul(b, d)), f64::INFINITY);
            y = mul(f64::INFINITY, add(mul(a, d), mul(b, c)));
        }
    }
    Complex::new(x, y)
}

/// [`multiply_recovering`] for `float complex`: libgcc's `__mulsc3`.
#[inline(never)]
fn multiply_recoveringf(z: ComplexF, w: ComplexF) -> ComplexF {
    let (mut a, mut b, mut c, mut d) = (z.re, z.im, w.re, w.im);
    let ac = mulf(a, c);
    let bd = mulf(b, d);
    let ad = mulf(a, d);
    let bc = mulf(c, b);
    let mut x = subf(ac, bd);
    let mut y = addf(ad, bc);
    if compares_nanf(x) && compares_nanf(y) {
        let mut recalc = false;
        if compares_inff(a) || compares_inff(b) {
            a = copysignf(if compares_inff(a) { 1.0 } else { 0.0 }, a);
            b = copysignf(if compares_inff(b) { 1.0 } else { 0.0 }, b);
            if compares_nanf(c) {
                c = copysignf(0.0, c);
            }
            if compares_nanf(d) {
                d = copysignf(0.0, d);
            }
            recalc = true;
        }
        if compares_inff(c) || compares_inff(d) {
            c = copysignf(if compares_inff(c) { 1.0 } else { 0.0 }, c);
            d = copysignf(if compares_inff(d) { 1.0 } else { 0.0 }, d);
            if compares_nanf(a) {
                a = copysignf(0.0, a);
            }
            if compares_nanf(b) {
                b = copysignf(0.0, b);
            }
            recalc = true;
        }
        if !recalc
            && (compares_inff(ac) || compares_inff(bd) || compares_inff(ad) || compares_inff(bc))
        {
            if compares_nanf(a) {
                a = copysignf(0.0, a);
            }
            if compares_nanf(b) {
                b = copysignf(0.0, b);
            }
            if compares_nanf(c) {
                c = copysignf(0.0, c);
            }
            if compares_nanf(d) {
                d = copysignf(0.0, d);
            }
            recalc = true;
        }
        if recalc {
            x = mulf(subf(mulf(a, c), mulf(b, d)), f32::INFINITY);
            y = mulf(f32::INFINITY, addf(mulf(a, d), mulf(b, c)));
        }
    }
    ComplexF::new(x, y)
}
