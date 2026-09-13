//! Classifying a `double` or `float`.
//!
//! `math.h`'s `fpclassify` and `signbit` macros call `__fpclassify` and
//! `__signbit` for the types they do not open-code. Programs built against
//! glibc also call the older `__isnan`, `__isinf` and `__finite`, and the BSD
//! `isnan`, `isinf` and `finite` functions, which C can still reach by
//! undefining the macros.
//!
//! `__fpclassify`, `__signbit` and `finite` are ported from musl 1.2.5 (MIT;
//! see [`crate::math`] for the notice). musl has no `__isnan` or `__isinf`;
//! they follow glibc's ABI, in which `isinf` returns -1 for negative infinity.

use core::ffi::c_int;

/// `FP_NAN`: not a number.
pub const FP_NAN: c_int = 0;
/// `FP_INFINITE`: an infinity.
pub const FP_INFINITE: c_int = 1;
/// `FP_ZERO`: a zero of either sign.
pub const FP_ZERO: c_int = 2;
/// `FP_SUBNORMAL`: nonzero, and smaller than the smallest normal number.
pub const FP_SUBNORMAL: c_int = 3;
/// `FP_NORMAL`: any other finite number.
pub const FP_NORMAL: c_int = 4;

/// Which of the five classes `x` is in.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __fpclassify(x: f64) -> c_int {
    let bits = x.to_bits();
    match bits >> 52 & 0x7ff {
        0 if bits << 1 == 0 => FP_ZERO,
        0 => FP_SUBNORMAL,
        0x7ff if bits << 12 == 0 => FP_INFINITE,
        0x7ff => FP_NAN,
        _ => FP_NORMAL,
    }
}

/// [`__fpclassify`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __fpclassifyf(x: f32) -> c_int {
    let bits = x.to_bits();
    match bits >> 23 & 0xff {
        0 if bits << 1 == 0 => FP_ZERO,
        0 => FP_SUBNORMAL,
        0xff if bits << 9 == 0 => FP_INFINITE,
        0xff => FP_NAN,
        _ => FP_NORMAL,
    }
}

/// 1 if `x`'s sign bit is set, which it is for -0 and for some NaNs, and 0
/// otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __signbit(x: f64) -> c_int {
    (x.to_bits() >> 63) as c_int
}

/// [`__signbit`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __signbitf(x: f32) -> c_int {
    (x.to_bits() >> 31) as c_int
}

/// Whether `x` is a NaN.
fn nan_bits(x: f64) -> bool {
    x.to_bits() << 1 > 0x7ff << 53
}

/// Whether `x` is a NaN.
fn nan_bitsf(x: f32) -> bool {
    x.to_bits() << 1 > 0xff << 24
}

/// 1 for positive infinity, -1 for negative infinity, 0 otherwise.
fn infinity_sign(bits: u64) -> c_int {
    if bits << 1 != 0x7ff << 53 {
        0
    } else if bits >> 63 != 0 {
        -1
    } else {
        1
    }
}

/// 1 for positive infinity, -1 for negative infinity, 0 otherwise.
fn infinity_signf(bits: u32) -> c_int {
    if bits << 1 != 0xff << 24 {
        0
    } else if bits >> 31 != 0 {
        -1
    } else {
        1
    }
}

/// Whether `x` is finite.
fn finite_bits(x: f64) -> bool {
    x.to_bits() << 1 < 0x7ff << 53
}

/// Whether `x` is finite.
fn finite_bitsf(x: f32) -> bool {
    x.to_bits() << 1 < 0xff << 24
}

/// glibc's `__isnan`: 1 if `x` is a NaN, 0 otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __isnan(x: f64) -> c_int {
    c_int::from(nan_bits(x))
}

/// glibc's `__isnanf`: 1 if `x` is a NaN, 0 otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __isnanf(x: f32) -> c_int {
    c_int::from(nan_bitsf(x))
}

/// The BSD `isnan` function: 1 if `x` is a NaN, 0 otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isnan(x: f64) -> c_int {
    __isnan(x)
}

/// The BSD `isnanf` function: 1 if `x` is a NaN, 0 otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isnanf(x: f32) -> c_int {
    __isnanf(x)
}

/// glibc's `__isinf`: 1 for positive infinity, -1 for negative infinity, and 0
/// otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __isinf(x: f64) -> c_int {
    infinity_sign(x.to_bits())
}

/// glibc's `__isinff`, as [`__isinf`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __isinff(x: f32) -> c_int {
    infinity_signf(x.to_bits())
}

/// The BSD `isinf` function, as [`__isinf`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isinf(x: f64) -> c_int {
    __isinf(x)
}

/// The BSD `isinff` function, as [`__isinf`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isinff(x: f32) -> c_int {
    __isinff(x)
}

/// glibc's `__finite`: 1 if `x` is neither infinite nor a NaN, 0 otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __finite(x: f64) -> c_int {
    c_int::from(finite_bits(x))
}

/// glibc's `__finitef`, as [`__finite`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __finitef(x: f32) -> c_int {
    c_int::from(finite_bitsf(x))
}

/// The BSD `finite` function, as [`__finite`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn finite(x: f64) -> c_int {
    __finite(x)
}

/// The BSD `finitef` function, as [`__finite`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn finitef(x: f32) -> c_int {
    __finitef(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doubles in each class, adapted from libc-test's `fpclassify.c` (MIT).
    const DOUBLES: &[(f64, c_int)] = &[
        (0.0, FP_ZERO),
        (-0.0, FP_ZERO),
        (f64::MIN_POSITIVE, FP_NORMAL),
        (f64::MIN_POSITIVE / 2.0, FP_SUBNORMAL),
        (-f64::from_bits(1), FP_SUBNORMAL),
        (f64::from_bits(0x000f_ffff_ffff_ffff), FP_SUBNORMAL),
        (1.0, FP_NORMAL),
        (-f64::MAX, FP_NORMAL),
        (f64::INFINITY, FP_INFINITE),
        (f64::NEG_INFINITY, FP_INFINITE),
        (f64::NAN, FP_NAN),
        (-f64::NAN, FP_NAN),
        (f64::from_bits(0x7ff0_0000_0000_0001), FP_NAN),
    ];

    const FLOATS: &[(f32, c_int)] = &[
        (0.0, FP_ZERO),
        (-0.0, FP_ZERO),
        (f32::MIN_POSITIVE, FP_NORMAL),
        (f32::MIN_POSITIVE / 2.0, FP_SUBNORMAL),
        (-f32::from_bits(1), FP_SUBNORMAL),
        (1.0, FP_NORMAL),
        (f32::MAX, FP_NORMAL),
        (f32::INFINITY, FP_INFINITE),
        (f32::NEG_INFINITY, FP_INFINITE),
        (f32::NAN, FP_NAN),
        (f32::from_bits(0xff80_0001), FP_NAN),
    ];

    #[test]
    fn every_class_of_double() {
        for &(x, class) in DOUBLES {
            assert_eq!(__fpclassify(x), class, "{x:e}");
            assert_eq!(__isnan(x) != 0, class == FP_NAN, "{x:e}");
            assert_eq!(isnan(x) != 0, class == FP_NAN, "{x:e}");
            assert_eq!(__isinf(x) != 0, class == FP_INFINITE, "{x:e}");
            assert_eq!(isinf(x) != 0, class == FP_INFINITE, "{x:e}");
            assert_eq!(__finite(x) != 0, class > FP_INFINITE, "{x:e}");
            assert_eq!(finite(x) != 0, class > FP_INFINITE, "{x:e}");
            assert_eq!(__signbit(x), (x.to_bits() >> 63) as c_int, "{x:e}");
        }
        assert_eq!(__isinf(f64::NEG_INFINITY), -1);
        assert_eq!(isinf(f64::INFINITY), 1);
    }

    #[test]
    fn every_class_of_float() {
        for &(x, class) in FLOATS {
            assert_eq!(__fpclassifyf(x), class, "{x:e}");
            assert_eq!(__isnanf(x) != 0, class == FP_NAN, "{x:e}");
            assert_eq!(isnanf(x) != 0, class == FP_NAN, "{x:e}");
            assert_eq!(__isinff(x) != 0, class == FP_INFINITE, "{x:e}");
            assert_eq!(isinff(x) != 0, class == FP_INFINITE, "{x:e}");
            assert_eq!(__finitef(x) != 0, class > FP_INFINITE, "{x:e}");
            assert_eq!(finitef(x) != 0, class > FP_INFINITE, "{x:e}");
            assert_eq!(__signbitf(x), (x.to_bits() >> 31) as c_int, "{x:e}");
        }
        assert_eq!(__isinff(f32::NEG_INFINITY), -1);
    }
}
