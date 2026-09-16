//! `ccosh`, `csinh`, `ctanh`, `ccos`, `csin` and `ctan`, and their `float`
//! forms: the hyperbolic and circular cosine, sine and tangent.
//!
//! Ported from musl 1.2.5's `ccosh.c`, `csinh.c`, `ctanh.c`, `ccos.c`,
//! `csin.c`, `ctan.c` and their `f` forms (MIT; see [`crate::math`] for the
//! notice). The circular functions musl wrote itself. The hyperbolic ones
//! came from FreeBSD. `s_ccosh.c`, `s_ccoshf.c`, `s_csinh.c` and `s_csinhf.c`
//! carry this notice:
//!
//! ```text
//! Copyright (c) 2005 Bruce D. Evans and Steven G. Kargl
//! All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions
//! are met:
//! 1. Redistributions of source code must retain the above copyright
//!    notice unmodified, this list of conditions, and the following
//!    disclaimer.
//! 2. Redistributions in binary form must reproduce the above copyright
//!    notice, this list of conditions and the following disclaimer in the
//!    documentation and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
//! IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
//! OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
//! IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
//! INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
//! NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
//! DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
//! THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
//! (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
//! THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//! ```
//!
//! `s_ctanh.c` and `s_ctanhf.c` carry this one:
//!
//! ```text
//! Copyright (c) 2011 David Schultz
//! All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions
//! are met:
//! 1. Redistributions of source code must retain the above copyright
//!    notice unmodified, this list of conditions, and the following
//!    disclaimer.
//! 2. Redistributions in binary form must reproduce the above copyright
//!    notice, this list of conditions and the following disclaimer in the
//!    documentation and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
//! IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
//! OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
//! IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
//! INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
//! NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
//! DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
//! THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
//! (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
//! THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//! ```
//!
//! # Method
//!
//! cosh(x + iy) = cosh(x) cos(y) + i sinh(x) sin(y) and sinh(x + iy) =
//! sinh(x) cos(y) + i cosh(x) sin(y). From |x| = 22 (9 for `float`) cosh
//! and sinh are both e^|x|/2, and past ln(`DBL_MAX`) that is scaled as in
//! `cexp`. The special values are C's Annex G.
//!
//! tanh is Kahan's, from "Branch Cuts for Complex Elementary Functions or
//! Much Ado About Nothing's Sign Bit": with t = tan(y), β = 1 + t², s =
//! sinh(x) and ρ = √(1 + s²), tanh(z) = (βρs + it) / (1 + βs²). From x = 22
//! (11) it is ±1 + i 4 sin(y) cos(y) e^-2|x|.
//!
//! cos z = cosh(iz), sin z = -i sinh(iz) and tan z = -i tanh(iz).

use crate::complex::cexp::{ldexp_cexp, ldexp_cexpf};
use crate::complex::{Complex, ComplexF, equals_zero, equals_zerof, is_inf, is_inff, mul, mulf};
use crate::math::cosh::{cosh, coshf};
use crate::math::exp::exp;
use crate::math::expf::expf;
use crate::math::manipulate::{copysign, copysignf, fabs, fabsf};
use crate::math::sinh::{sinh, sinhf};
use crate::math::sqrt::{sqrt, sqrtf};
use crate::math::support::{
    barrier, barrierf, hexf32, hexf64, high_word, is_finite, is_finitef, low_word,
};
use crate::math::tan::tan;
use crate::math::trig::{cos, sin};
use crate::math::trigf::{cosf, sinf, tanf};

/// 2^1023: its square overflows.
const HUGE: f64 = hexf64!("0x1p1023");

/// 2^127, for `float`.
const HUGE_F: f32 = hexf32!("0x1p127");

/// The hyperbolic cosine of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn ccosh(z: Complex) -> Complex {
    let x = z.re;
    let y = z.im;

    let hx = high_word(x) as i32;
    let lx = low_word(x) as i32;
    let hy = high_word(y) as i32;
    let ly = low_word(y) as i32;

    let ix = 0x7fff_ffff & hx;
    let iy = 0x7fff_ffff & hy;

    // The nearly unexceptional cases, where x and y are finite.
    if ix < 0x7ff0_0000 && iy < 0x7ff0_0000 {
        if iy | ly == 0 {
            return Complex::new(cosh(x), x * y);
        }
        if ix < 0x4036_0000 {
            // Small x: the normal case.
            return Complex::new(cosh(x) * cos(y), sinh(x) * sin(y));
        }

        // |x| >= 22, so cosh(x) ≈ exp(|x|).
        if ix < 0x4086_2e42 {
            // x < 710: exp(|x|) does not overflow.
            let h = exp(fabs(x)) * 0.5;
            return Complex::new(h * cos(y), copysign(h, x) * sin(y));
        } else if ix < 0x4096_bbaa {
            // x < 1455: scale to avoid overflow.
            let z = ldexp_cexp(Complex::new(fabs(x), y), -1);
            return Complex::new(z.re, z.im * copysign(1.0, x));
        } else {
            // x >= 1455: the result always overflows.
            let h = HUGE * x;
            return Complex::new(h * h * cos(y), h * sin(y));
        }
    }

    // cosh(±0 ± i Inf) = dNaN + i sign(d(±0, dNaN))0, raising invalid.
    // cosh(±0 ± i NaN) = d(NaN) + i sign(d(±0, NaN))0.
    if ix | lx == 0 && iy >= 0x7ff0_0000 {
        return Complex::new(barrier(y) - y, copysign(0.0, x * (barrier(y) - y)));
    }

    // cosh(±Inf ± i0) = +Inf + i (±)(±)0.
    // cosh(NaN ± i0)   = d(NaN) + i sign(d(NaN, ±0))0.
    if iy | ly == 0 && ix >= 0x7ff0_0000 {
        if (hx & 0xfffff) | lx == 0 {
            return Complex::new(x * x, copysign(0.0, x) * y);
        }
        return Complex::new(x * x, copysign(0.0, (x + x) * y));
    }

    // cosh(x ± i Inf) = dNaN + i dNaN, raising invalid for finite nonzero x.
    // cosh(x + i NaN) = d(NaN) + i d(NaN).
    if ix < 0x7ff0_0000 && iy >= 0x7ff0_0000 {
        return Complex::new(barrier(y) - y, x * (barrier(y) - y));
    }

    // cosh(±Inf + i NaN)  = +Inf + i d(NaN).
    // cosh(±Inf ± i Inf) = +Inf + i dNaN, raising invalid.
    // cosh(±Inf + iy)    = +Inf cos(y) ± i Inf sin(y).
    if ix >= 0x7ff0_0000 && (hx & 0xfffff) | lx == 0 {
        if iy >= 0x7ff0_0000 {
            return Complex::new(x * x, x * (barrier(y) - y));
        }
        return Complex::new((x * x) * cos(y), x * sin(y));
    }

    // cosh(NaN + i NaN)  = d(NaN) + i d(NaN).
    // cosh(NaN ± i Inf) = d(NaN) + i d(NaN), raising invalid.
    // cosh(NaN + iy)    = d(NaN) + i d(NaN).
    // Where both are NaNs, x's wins, as in GCC's code.
    Complex::new(mul(x * x, barrier(y) - y), mul(x + x, barrier(y) - y))
}

/// [`ccosh`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn ccoshf(z: ComplexF) -> ComplexF {
    let x = z.re;
    let y = z.im;

    let hx = x.to_bits() as i32;
    let hy = y.to_bits() as i32;

    let ix = 0x7fff_ffff & hx;
    let iy = 0x7fff_ffff & hy;

    if ix < 0x7f80_0000 && iy < 0x7f80_0000 {
        if iy == 0 {
            return ComplexF::new(coshf(x), x * y);
        }
        if ix < 0x4110_0000 {
            // Small x: the normal case.
            return ComplexF::new(coshf(x) * cosf(y), sinhf(x) * sinf(y));
        }

        // |x| >= 9, so cosh(x) ≈ exp(|x|).
        if ix < 0x42b1_7218 {
            // x < 88.7: expf(|x|) does not overflow.
            let h = expf(fabsf(x)) * 0.5;
            return ComplexF::new(h * cosf(y), copysignf(h, x) * sinf(y));
        } else if ix < 0x4340_b1e7 {
            // x < 192.7: scale to avoid overflow.
            let z = ldexp_cexpf(ComplexF::new(fabsf(x), y), -1);
            return ComplexF::new(z.re, z.im * copysignf(1.0, x));
        } else {
            // x >= 192.7: the result always overflows.
            let h = HUGE_F * x;
            return ComplexF::new(h * h * cosf(y), h * sinf(y));
        }
    }

    if ix == 0 && iy >= 0x7f80_0000 {
        return ComplexF::new(barrierf(y) - y, copysignf(0.0, x * (barrierf(y) - y)));
    }

    if iy == 0 && ix >= 0x7f80_0000 {
        if hx & 0x7f_ffff == 0 {
            return ComplexF::new(x * x, copysignf(0.0, x) * y);
        }
        return ComplexF::new(x * x, copysignf(0.0, (x + x) * y));
    }

    if ix < 0x7f80_0000 && iy >= 0x7f80_0000 {
        return ComplexF::new(barrierf(y) - y, x * (barrierf(y) - y));
    }

    if ix >= 0x7f80_0000 && hx & 0x7f_ffff == 0 {
        if iy >= 0x7f80_0000 {
            return ComplexF::new(x * x, x * (barrierf(y) - y));
        }
        return ComplexF::new((x * x) * cosf(y), x * sinf(y));
    }

    ComplexF::new(mulf(x * x, barrierf(y) - y), mulf(x + x, barrierf(y) - y))
}

/// The hyperbolic sine of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn csinh(z: Complex) -> Complex {
    let x = z.re;
    let y = z.im;

    let hx = high_word(x) as i32;
    let lx = low_word(x) as i32;
    let hy = high_word(y) as i32;
    let ly = low_word(y) as i32;

    let ix = 0x7fff_ffff & hx;
    let iy = 0x7fff_ffff & hy;

    // The nearly unexceptional cases, where x and y are finite.
    if ix < 0x7ff0_0000 && iy < 0x7ff0_0000 {
        if iy | ly == 0 {
            return Complex::new(sinh(x), y);
        }
        if ix < 0x4036_0000 {
            // Small x: the normal case.
            return Complex::new(sinh(x) * cos(y), cosh(x) * sin(y));
        }

        // |x| >= 22, so cosh(x) ≈ exp(|x|).
        if ix < 0x4086_2e42 {
            // x < 710: exp(|x|) does not overflow.
            let h = exp(fabs(x)) * 0.5;
            return Complex::new(copysign(h, x) * cos(y), h * sin(y));
        } else if ix < 0x4096_bbaa {
            // x < 1455: scale to avoid overflow.
            let z = ldexp_cexp(Complex::new(fabs(x), y), -1);
            return Complex::new(z.re * copysign(1.0, x), z.im);
        } else {
            // x >= 1455: the result always overflows.
            let h = HUGE * x;
            return Complex::new(h * cos(y), h * h * sin(y));
        }
    }

    // sinh(±0 ± i Inf) = sign(d(±0, dNaN))0 + i dNaN, raising invalid.
    // sinh(±0 ± i NaN) = sign(d(±0, NaN))0 + i d(NaN).
    if ix | lx == 0 && iy >= 0x7ff0_0000 {
        return Complex::new(copysign(0.0, x * (barrier(y) - y)), barrier(y) - y);
    }

    // sinh(±Inf ± i0) = ±Inf + i ±0.
    // sinh(NaN ± i0)   = d(NaN) + i ±0.
    if iy | ly == 0 && ix >= 0x7ff0_0000 {
        if (hx & 0xfffff) | lx == 0 {
            return Complex::new(x, y);
        }
        return Complex::new(x, copysign(0.0, y));
    }

    // sinh(x ± i Inf) = dNaN + i dNaN, raising invalid for finite nonzero x.
    // sinh(x + i NaN) = d(NaN) + i d(NaN).
    if ix < 0x7ff0_0000 && iy >= 0x7ff0_0000 {
        return Complex::new(barrier(y) - y, x * (barrier(y) - y));
    }

    // sinh(±Inf + i NaN)  = ±Inf + i d(NaN).
    // sinh(±Inf ± i Inf) = +Inf + i dNaN, raising invalid.
    // sinh(±Inf + iy)    = ±Inf cos(y) + i Inf sin(y).
    if ix >= 0x7ff0_0000 && (hx & 0xfffff) | lx == 0 {
        if iy >= 0x7ff0_0000 {
            return Complex::new(x * x, x * (barrier(y) - y));
        }
        return Complex::new(x * cos(y), f64::INFINITY * sin(y));
    }

    // sinh(NaN + i NaN)  = d(NaN) + i d(NaN).
    // sinh(NaN ± i Inf) = d(NaN) + i d(NaN), raising invalid.
    // sinh(NaN + iy)    = d(NaN) + i d(NaN).
    // Where both are NaNs, x's wins in the real part and y's in the imaginary
    // part, as in GCC's code.
    Complex::new(mul(x * x, barrier(y) - y), mul(barrier(y) - y, x + x))
}

/// [`csinh`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn csinhf(z: ComplexF) -> ComplexF {
    let x = z.re;
    let y = z.im;

    let hx = x.to_bits() as i32;
    let hy = y.to_bits() as i32;

    let ix = 0x7fff_ffff & hx;
    let iy = 0x7fff_ffff & hy;

    if ix < 0x7f80_0000 && iy < 0x7f80_0000 {
        if iy == 0 {
            return ComplexF::new(sinhf(x), y);
        }
        if ix < 0x4110_0000 {
            // Small x: the normal case.
            return ComplexF::new(sinhf(x) * cosf(y), coshf(x) * sinf(y));
        }

        // |x| >= 9, so cosh(x) ≈ exp(|x|).
        if ix < 0x42b1_7218 {
            // x < 88.7: expf(|x|) does not overflow.
            let h = expf(fabsf(x)) * 0.5;
            return ComplexF::new(copysignf(h, x) * cosf(y), h * sinf(y));
        } else if ix < 0x4340_b1e7 {
            // x < 192.7: scale to avoid overflow.
            let z = ldexp_cexpf(ComplexF::new(fabsf(x), y), -1);
            return ComplexF::new(z.re * copysignf(1.0, x), z.im);
        } else {
            // x >= 192.7: the result always overflows.
            let h = HUGE_F * x;
            return ComplexF::new(h * cosf(y), h * h * sinf(y));
        }
    }

    if ix == 0 && iy >= 0x7f80_0000 {
        return ComplexF::new(copysignf(0.0, x * (barrierf(y) - y)), barrierf(y) - y);
    }

    if iy == 0 && ix >= 0x7f80_0000 {
        if hx & 0x7f_ffff == 0 {
            return ComplexF::new(x, y);
        }
        return ComplexF::new(x, copysignf(0.0, y));
    }

    if ix < 0x7f80_0000 && iy >= 0x7f80_0000 {
        return ComplexF::new(barrierf(y) - y, x * (barrierf(y) - y));
    }

    if ix >= 0x7f80_0000 && hx & 0x7f_ffff == 0 {
        if iy >= 0x7f80_0000 {
            return ComplexF::new(x * x, x * (barrierf(y) - y));
        }
        return ComplexF::new(x * cosf(y), f32::INFINITY * sinf(y));
    }

    ComplexF::new(mulf(x * x, barrierf(y) - y), mulf(x + x, barrierf(y) - y))
}

/// The hyperbolic tangent of `z`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn ctanh(z: Complex) -> Complex {
    let x = z.re;
    let y = z.im;

    let hx = high_word(x);
    let lx = low_word(x);
    let ix = hx & 0x7fff_ffff;

    // ctanh(NaN + i0) = NaN + i0
    // ctanh(NaN + iy) = NaN + iNaN for y != 0
    // ctanh(±Inf ± i Inf) = ±1 ± 0
    // ctanh(±Inf + iy) = ±1 + 0 sin(2y) for y finite
    // The sign of the imaginary zero is unspecified; the special case only
    // avoids a spurious invalid when y is infinite.
    if ix >= 0x7ff0_0000 {
        if (ix & 0xfffff) | lx != 0 {
            // x is a NaN.
            return Complex::new(x, if equals_zero(y) { y } else { mul(x, y) });
        }
        // copysign(1, x)
        let x = f64::from_bits(u64::from(hx.wrapping_sub(0x4000_0000)) << 32 | u64::from(lx));
        let sign = if is_inf(y) { y } else { sin(y) * cos(y) };
        return Complex::new(x, copysign(0.0, sign));
    }

    // ctanh(±0 + i NaN) = ±0 + i NaN
    // ctanh(±0 ± i Inf) = ±0 + i NaN
    // ctanh(x + i NaN) = NaN + i NaN
    // ctanh(x ± i Inf) = NaN + i NaN
    if !is_finite(y) {
        let re = if equals_zero(x) { x } else { barrier(y) - y };
        return Complex::new(re, barrier(y) - y);
    }

    // ctanh(±huge ± iy) ≈ ±1 ± i 2sin(2y)/exp(2x), from sinh²(huge) ≈
    // exp(2 huge)/4, written to avoid a spurious overflow.
    if ix >= 0x4036_0000 {
        // x >= 22
        let exp_mx = exp(-fabs(x));
        return Complex::new(copysign(1.0, x), 4.0 * sin(y) * cos(y) * exp_mx * exp_mx);
    }

    // Kahan's algorithm.
    let t = tan(y);
    let beta = 1.0 + t * t; // 1 / cos²(y)
    let s = sinh(x);
    let rho = sqrt(1.0 + s * s); // cosh(x)
    let denom = 1.0 + beta * s * s;
    Complex::new((beta * rho * s) / denom, t / denom)
}

/// [`ctanh`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[inline(never)]
pub extern "C" fn ctanhf(z: ComplexF) -> ComplexF {
    let x = z.re;
    let y = z.im;

    let hx = x.to_bits();
    let ix = hx & 0x7fff_ffff;

    if ix >= 0x7f80_0000 {
        if ix & 0x7f_ffff != 0 {
            return ComplexF::new(x, if equals_zerof(y) { y } else { mulf(y, x) });
        }
        let x = f32::from_bits(hx.wrapping_sub(0x4000_0000));
        let sign = if is_inff(y) { y } else { sinf(y) * cosf(y) };
        return ComplexF::new(x, copysignf(0.0, sign));
    }

    if !is_finitef(y) {
        return ComplexF::new(if ix != 0 { barrierf(y) - y } else { x }, barrierf(y) - y);
    }

    if ix >= 0x4130_0000 {
        // x >= 11
        let exp_mx = expf(-fabsf(x));
        return ComplexF::new(copysignf(1.0, x), 4.0 * sinf(y) * cosf(y) * exp_mx * exp_mx);
    }

    let t = tanf(y);
    // musl adds `double` 1.0 here, which GCC does in `float`, the same.
    let beta = 1.0 + t * t;
    let s = sinhf(x);
    let rho = sqrtf(1.0 + s * s);
    let denom = 1.0 + beta * s * s;
    ComplexF::new((beta * rho * s) / denom, t / denom)
}

/// The cosine of `z`, cosh(iz).
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ccos(z: Complex) -> Complex {
    ccosh(Complex::new(-z.im, z.re))
}

/// [`ccos`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ccosf(z: ComplexF) -> ComplexF {
    ccoshf(ComplexF::new(-z.im, z.re))
}

/// The sine of `z`, -i sinh(iz).
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn csin(z: Complex) -> Complex {
    let z = csinh(Complex::new(-z.im, z.re));
    Complex::new(z.im, -z.re)
}

/// [`csin`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn csinf(z: ComplexF) -> ComplexF {
    let z = csinhf(ComplexF::new(-z.im, z.re));
    ComplexF::new(z.im, -z.re)
}

/// The tangent of `z`, -i tanh(iz).
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ctan(z: Complex) -> Complex {
    let z = ctanh(Complex::new(-z.im, z.re));
    Complex::new(z.im, -z.re)
}

/// [`ctan`] for `float complex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ctanf(z: ComplexF) -> ComplexF {
    let z = ctanhf(ComplexF::new(-z.im, z.re));
    ComplexF::new(z.im, -z.re)
}
