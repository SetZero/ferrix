//! `creal`, `cimag`, `conj` and `cproj`, and their `float` forms: taking a
//! complex number apart and putting it back together.
//!
//! Ported from musl 1.2.5's `creal.c`, `cimag.c`, `conj.c`, `cproj.c` and
//! their `f` forms (MIT; see [`crate::math`] for the notice). musl wrote these
//! itself; they carry no other notice.

use crate::complex::{Complex, ComplexF, is_inf, is_inff};
use crate::math::manipulate::{copysign, copysignf};

/// The real part of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn creal(z: Complex) -> f64 {
    z.re
}

/// [`creal`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn crealf(z: ComplexF) -> f32 {
    z.re
}

/// The imaginary part of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cimag(z: Complex) -> f64 {
    z.im
}

/// [`cimag`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cimagf(z: ComplexF) -> f32 {
    z.im
}

/// The complex conjugate of `z`: its imaginary part negated, a NaN's sign
/// included.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn conj(z: Complex) -> Complex {
    Complex::new(z.re, -z.im)
}

/// [`conj`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn conjf(z: ComplexF) -> ComplexF {
    ComplexF::new(z.re, -z.im)
}

/// `z` projected onto the Riemann sphere: every infinity, whatever the other
/// part, becomes +∞ with an imaginary zero of the imaginary part's sign, and
/// anything else is `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cproj(z: Complex) -> Complex {
    if is_inf(z.re) || is_inf(z.im) {
        return Complex::new(f64::INFINITY, copysign(0.0, z.im));
    }
    z
}

/// [`cproj`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn cprojf(z: ComplexF) -> ComplexF {
    if is_inff(z.re) || is_inff(z.im) {
        return ComplexF::new(f32::INFINITY, copysignf(0.0, z.im));
    }
    z
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complex::tests::{bits, bitsf};

    #[test]
    fn parts_come_apart_and_back_bit_for_bit() {
        let nan = f64::from_bits(0x7ff4_0000_0000_0001);
        let z = Complex::new(-0.0, nan);
        assert_eq!(creal(z).to_bits(), (-0.0f64).to_bits());
        assert_eq!(cimag(z).to_bits(), nan.to_bits());
        assert_eq!(bits(conj(z)), bits(Complex::new(-0.0, -nan)));
        let z = ComplexF::new(1.5, -0.0);
        assert_eq!(crealf(z), 1.5);
        assert_eq!(cimagf(z).to_bits(), (-0.0f32).to_bits());
        assert_eq!(bitsf(conjf(z)), bitsf(ComplexF::new(1.5, 0.0)));
    }

    #[test]
    fn cproj_sends_every_infinity_to_positive_infinity() {
        let inf = f64::INFINITY;
        let cases = [
            (Complex::new(-inf, f64::NAN), Complex::new(inf, 0.0)),
            (Complex::new(f64::NAN, -inf), Complex::new(inf, -0.0)),
            (Complex::new(1.0, -2.0), Complex::new(1.0, -2.0)),
            (Complex::new(f64::NAN, -0.0), Complex::new(f64::NAN, -0.0)),
        ];
        for (z, want) in cases {
            assert_eq!(bits(cproj(z)), bits(want), "cproj({z:?})");
        }
        let inf = f32::INFINITY;
        let z = ComplexF::new(3.0, -inf);
        assert_eq!(bitsf(cprojf(z)), bitsf(ComplexF::new(inf, -0.0)));
        let z = ComplexF::new(-inf, 1.0);
        assert_eq!(bitsf(cprojf(z)), bitsf(ComplexF::new(inf, 0.0)));
    }
}
