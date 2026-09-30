//! `jn`, `yn`, `jnf` and `ynf`: the Bessel functions of the first and second
//! kinds of integer order.
//!
//! Ported from musl 1.2.5's `jn.c` and `jnf.c` (MIT; see [`crate::math`] for
//! the notice). musl took them from FreeBSD's msun; `jnf.c` was converted to
//! `float` by Ian Lance Taylor, Cygnus Support. The msun files carry this
//! notice:
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
//! musl writes these constants in decimal. They are written here as bits, and
//! the tests check them against musl's decimals.
//!
//! # Method
//!
//! J(-n, x) = J(n, -x) = (-1)^n·J(n, x), and orders 0 and 1 are `j0` and `j1`.
//! Where n - 1 < x, the forward recurrence J(n + 1, x) = (2n/x)·J(n, x) -
//! J(n - 1, x) starts from j0 and j1, and above 2^302 (`double` only) the
//! asymptotic cos(x - (2n + 1)π/4)·√(2/(πx)) is used. Otherwise a tiny x
//! takes the first term of the Taylor series, (x/2)^n/n!, and any other x a
//! continued fraction for J(n, x)/J(n - 1, x), a backward recurrence from it,
//! and a scale that makes the result agree with j0 or j1, whichever is
//! larger. Y(n, x) always takes the forward recurrence from y0 and y1, which
//! stops at -inf.

use core::ffi::c_int;

use crate::math::j0::{j0, y0};
use crate::math::j0f::{j0f, y0f};
use crate::math::j1::{j1, y1};
use crate::math::j1f::{j1f, y1f};
use crate::math::log::log;
use crate::math::logf::logf;
use crate::math::manipulate::{fabs, fabsf};
use crate::math::sqrt::sqrt;
use crate::math::support::{barrier, barrierf, hexf32, hexf64, high_word, low_word};
use crate::math::trig::{cos, sin};

/// 1/√π.
const INVSQRTPI: f64 = f64::from_bits(0x3fe2_0dd7_5042_9b6d);

/// Where `jn`'s backward recurrence may overflow: log(`DBL_MAX`), about.
const OVERFLOW_LOG: f64 = f64::from_bits(0x4086_2e42_fefa_39ef);

/// Where `jnf`'s backward recurrence may overflow: log(`FLT_MAX`), about.
const OVERFLOW_LOG_F: f32 = f32::from_bits(0x42b1_7180);

/// The Bessel function of the first kind of order `n`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn jn(n: c_int, x: f64) -> f64 {
    let hx = high_word(x);
    let lx = low_word(x);
    let mut sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;

    if (ix | (lx | lx.wrapping_neg()) >> 31) > 0x7ff0_0000 {
        // NaN.
        return x;
    }

    // J(-n, x) = (-1)^n·J(n, x) and J(n, -x) = (-1)^n·J(n, x), so
    // J(-n, x) = J(n, -x). nm1 = |n| - 1 handles `INT_MIN`.
    if n == 0 {
        return j0(x);
    }
    let (nm1, x) = if n < 0 {
        sign ^= 1;
        (-(n + 1), -x)
    } else {
        (n - 1, x)
    };
    if nm1 == 0 {
        return j1(x);
    }

    // Even n: 0; odd n: the sign of x.
    sign &= n as u32;
    let x = fabs(x);
    let b = if (ix | lx) == 0 || ix == 0x7ff0_0000 {
        // x is 0 or inf.
        0.0
    } else if f64::from(nm1) < x {
        // J(n + 1, x) = 2n/x·J(n, x) - J(n - 1, x) is safe.
        if ix >= 0x52d0_0000 {
            // x > 2^302, so x >> n²: J(n, x) = cos(x - (2n + 1)π/4)·√(2/(πx)),
            // where √2·cos(x - (2n + 1)π/4) is, by n mod 4, c + s, -c + s,
            // -c - s or c - s, for s = sin(x) and c = cos(x). The arms are
            // keyed by n - 1.
            let temp = match nm1 & 3 {
                0 => -cos(x) + sin(x),
                1 => -cos(x) - sin(x),
                2 => cos(x) - sin(x),
                _ => cos(x) + sin(x),
            };
            INVSQRTPI * temp / sqrt(x)
        } else {
            let mut a = j0(x);
            let mut b = j1(x);
            let mut i = 0;
            while i < nm1 {
                i += 1;
                let temp = b;
                // Arranged to avoid underflow.
                b = b * (2.0 * f64::from(i) / x) - a;
                a = temp;
            }
            b
        }
    } else if ix < 0x3e10_0000 {
        // x < 2^-29: the first term of the Taylor series, (x/2)^n/n!.
        if nm1 > 32 {
            // Underflow.
            0.0
        } else {
            let temp = x * 0.5;
            let mut b = temp;
            let mut a = 1.0;
            for i in 2..=nm1 + 1 {
                a *= f64::from(i);
                b *= temp;
            }
            b / a
        }
    } else {
        // The backward recurrence, from the continued fraction
        // J(n, x)/J(n - 1, x) = 1/(w - 1/(w + h - 1/(w + 2h - ...))), with
        // w = 2n/x and h = 2/x. Q(0) = w, Q(1) = w(w + h) - 1 and
        // Q(k) = (w + kh)·Q(k - 1) - Q(k - 2): k terms are enough for
        // `double` once Q(k) > 10^9.
        let nf = f64::from(nm1) + 1.0;
        let w = 2.0 * nf / x;
        let h = 2.0 / x;
        let mut z = w + h;
        let mut q0 = w;
        let mut q1 = w * z - 1.0;
        let mut k = 1;
        while q1 < 1.0e9 {
            k += 1;
            z += h;
            let tmp = z * q1 - q0;
            q0 = q1;
            q1 = tmp;
        }
        let mut t = 0.0;
        let mut i = k;
        while i >= 0 {
            t = 1.0 / (2.0 * (f64::from(i) + nf) / x - t);
            i -= 1;
        }
        let mut a = t;
        let mut b = 1.0;
        // log((2/x)^n·n!) ~ n·log(2n/x). Beyond log(DBL_MAX) the recurrence
        // may overflow, and the result likely underflows to zero.
        let tmp = nf * log(fabs(w));
        if tmp < OVERFLOW_LOG {
            let mut i = nm1;
            while i > 0 {
                let temp = b;
                b = b * (2.0 * f64::from(i)) / x - a;
                a = temp;
                i -= 1;
            }
        } else {
            let mut i = nm1;
            while i > 0 {
                let temp = b;
                b = b * (2.0 * f64::from(i)) / x - a;
                a = temp;
                // Scale b down to avoid a spurious overflow.
                if b > hexf64!("0x1p500") {
                    a /= b;
                    t /= b;
                    b = 1.0;
                }
                i -= 1;
            }
        }
        let z = j0(x);
        let w = j1(x);
        if fabs(z) >= fabs(w) {
            t * z / b
        } else {
            t * w / a
        }
    };
    if sign != 0 { -b } else { b }
}

/// The Bessel function of the second kind of order `n`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn yn(n: c_int, x: f64) -> f64 {
    let hx = high_word(x);
    let lx = low_word(x);
    let sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;

    if (ix | (lx | lx.wrapping_neg()) >> 31) > 0x7ff0_0000 {
        // NaN.
        return x;
    }
    if sign != 0 && (ix | lx) != 0 {
        // x < 0.
        return barrier(0.0) / 0.0;
    }
    if ix == 0x7ff0_0000 {
        return 0.0;
    }

    if n == 0 {
        return y0(x);
    }
    let (nm1, negative) = if n < 0 {
        (-(n + 1), n & 1 != 0)
    } else {
        (n - 1, false)
    };
    if nm1 == 0 {
        return if negative { -y1(x) } else { y1(x) };
    }

    let b = if ix >= 0x52d0_0000 {
        // x > 2^302, so x >> n²: Y(n, x) = sin(x - (2n + 1)π/4)·√(2/(πx)),
        // where √2·sin(x - (2n + 1)π/4) is, by n mod 4, s - c, -s - c, -s + c
        // or s + c. The arms are keyed by n - 1.
        let temp = match nm1 & 3 {
            0 => -sin(x) - cos(x),
            1 => -sin(x) + cos(x),
            2 => sin(x) + cos(x),
            _ => sin(x) - cos(x),
        };
        INVSQRTPI * temp / sqrt(x)
    } else {
        let mut a = y0(x);
        let mut b = y1(x);
        // Stop once b is -inf.
        let mut ib = high_word(b);
        let mut i = 0;
        while i < nm1 && ib != 0xfff0_0000 {
            i += 1;
            let temp = b;
            b = (2.0 * f64::from(i) / x) * b - a;
            ib = high_word(b);
            a = temp;
        }
        b
    };
    if negative { -b } else { b }
}

/// [`jn`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn jnf(n: c_int, x: f32) -> f32 {
    let hx = x.to_bits();
    let mut sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;
    if ix > 0x7f80_0000 {
        // NaN.
        return x;
    }

    // J(-n, x) = J(n, -x); nm1 = |n| - 1 handles `INT_MIN`.
    if n == 0 {
        return j0f(x);
    }
    let (nm1, x) = if n < 0 {
        sign ^= 1;
        (-(n + 1), -x)
    } else {
        (n - 1, x)
    };
    if nm1 == 0 {
        return j1f(x);
    }

    // Even n: 0; odd n: the sign of x.
    sign &= n as u32;
    let x = fabsf(x);
    // The conversions of integers to `float` below round, in the current
    // mode, as C's do.
    let b = if ix == 0 || ix == 0x7f80_0000 {
        // x is 0 or inf.
        0.0
    } else if (nm1 as f32) < x {
        // J(n + 1, x) = 2n/x·J(n, x) - J(n - 1, x) is safe.
        let mut a = j0f(x);
        let mut b = j1f(x);
        let mut i = 0;
        while i < nm1 {
            i += 1;
            let temp = b;
            b = b * (2.0 * i as f32 / x) - a;
            a = temp;
        }
        b
    } else if ix < 0x3580_0000 {
        // x < 2^-20: the first term of the Taylor series, (x/2)^n/n!, which
        // underflows from n = 9 up.
        let nm1 = nm1.min(8);
        let temp = 0.5 * x;
        let mut b = temp;
        let mut a = 1.0;
        for i in 2..=nm1 + 1 {
            a *= i as f32;
            b *= temp;
        }
        b / a
    } else {
        // The backward recurrence, as in `jn`, with k terms enough for
        // `float` once Q(k) > 10^4.
        let nf = nm1 as f32 + 1.0;
        let w = 2.0 * nf / x;
        let h = 2.0 / x;
        let mut z = w + h;
        let mut q0 = w;
        let mut q1 = w * z - 1.0;
        let mut k = 1;
        while q1 < 1.0e4 {
            k += 1;
            z += h;
            let tmp = z * q1 - q0;
            q0 = q1;
            q1 = tmp;
        }
        let mut t = 0.0;
        let mut i = k;
        while i >= 0 {
            t = 1.0 / (2.0 * (i as f32 + nf) / x - t);
            i -= 1;
        }
        let mut a = t;
        let mut b = 1.0;
        // Beyond log(FLT_MAX) the recurrence may overflow.
        let tmp = nf * logf(fabsf(w));
        if tmp < OVERFLOW_LOG_F {
            let mut i = nm1;
            while i > 0 {
                let temp = b;
                b = 2.0 * i as f32 * b / x - a;
                a = temp;
                i -= 1;
            }
        } else {
            let mut i = nm1;
            while i > 0 {
                let temp = b;
                b = 2.0 * i as f32 * b / x - a;
                a = temp;
                // Scale b down to avoid a spurious overflow.
                if b > hexf32!("0x1p60") {
                    a /= b;
                    t /= b;
                    b = 1.0;
                }
                i -= 1;
            }
        }
        let z = j0f(x);
        let w = j1f(x);
        if fabsf(z) >= fabsf(w) {
            t * z / b
        } else {
            t * w / a
        }
    };
    if sign != 0 { -b } else { b }
}

/// [`yn`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ynf(n: c_int, x: f32) -> f32 {
    let hx = x.to_bits();
    let sign = hx >> 31;
    let ix = hx & 0x7fff_ffff;
    if ix > 0x7f80_0000 {
        // NaN.
        return x;
    }
    if sign != 0 && ix != 0 {
        // x < 0.
        return barrierf(0.0) / 0.0;
    }
    if ix == 0x7f80_0000 {
        return 0.0;
    }

    if n == 0 {
        return y0f(x);
    }
    let (nm1, negative) = if n < 0 {
        (-(n + 1), n & 1 != 0)
    } else {
        (n - 1, false)
    };
    if nm1 == 0 {
        return if negative { -y1f(x) } else { y1f(x) };
    }

    let mut a = y0f(x);
    let mut b = y1f(x);
    // Stop once b is -inf. The conversion of i rounds as C's does.
    let mut ib = b.to_bits();
    let mut i = 0;
    while i < nm1 && ib != 0xff80_0000 {
        i += 1;
        let temp = b;
        b = (2.0 * i as f32 / x) * b - a;
        ib = b.to_bits();
        a = temp;
    }
    if negative { -b } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::mtest::{self, Rules};

    /// An order from a table, which libc-test passes to an `int` parameter.
    fn order(n: i64) -> c_int {
        c_int::try_from(n).expect("an order that fits an int")
    }

    #[test]
    fn jn_matches_libc_test() {
        let files = ["sanity/jn.h", "special/jn.h"];
        // libc-test's `jn.c` tolerates an error under 3 ulps.
        let rules = Rules::ULP.tolerate(3.0);
        mtest::di_d("jn", &files, |x, n| jn(order(n), x), rules, &[]);
    }

    #[test]
    fn yn_matches_libc_test() {
        let files = ["sanity/yn.h", "special/yn.h"];
        // libc-test's `yn.c` wants a NaN or -inf for a negative argument.
        let rules = Rules::ULP.negative_domain();
        mtest::di_d("yn", &files, |x, n| yn(order(n), x), rules, &[]);
    }

    #[test]
    fn jnf_matches_libc_test() {
        let files = ["sanity/jnf.h", "special/jnf.h"];
        // libc-test's `jnf.c` tolerates an error under 3 ulps.
        let rules = Rules::ULP.tolerate(3.0);
        mtest::di_d("jnf", &files, |x, n| jnf(order(n), x), rules, &[]);
    }

    #[test]
    fn ynf_matches_libc_test() {
        let files = ["sanity/ynf.h", "special/ynf.h"];
        // libc-test's `ynf.c` wants a NaN or -inf for a negative argument,
        // and tolerates an error under 2.5 ulps for the others.
        let rules = Rules::ULP.tolerate(2.5).negative_domain();
        mtest::di_d("ynf", &files, |x, n| ynf(order(n), x), rules, &[]);
    }

    #[test]
    fn the_constants_are_musls_decimals() {
        let doubles = [
            (INVSQRTPI, "5.64189583547756279280e-01"),
            (OVERFLOW_LOG, "7.09782712893383973096e+02"),
        ];
        for (value, text) in doubles {
            assert_eq!(
                Ok(value.to_bits()),
                text.parse::<f64>().map(f64::to_bits),
                "{text}"
            );
        }
        assert_eq!(
            Ok(OVERFLOW_LOG_F.to_bits()),
            "88.721679688".parse::<f32>().map(f32::to_bits)
        );
    }
}
