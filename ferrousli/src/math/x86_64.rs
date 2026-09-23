//! The SSE2 instructions the math library uses on x86-64: square roots, and
//! conversions to integers. Each follows the rounding mode in MXCSR and raises
//! its exceptions exactly as C asks, and each is written as an instruction so
//! the compiler cannot turn it into a call to the function being defined.
//!
//! Another architecture supplies a module with the same functions.

use core::arch::asm;

/// The square root of `x`, correctly rounded in the current mode.
///
/// `sqrtsd` raises invalid for a negative `x` and inexact when the root is not
/// exact.
#[inline]
pub(crate) fn sqrt(mut x: f64) -> f64 {
    // SAFETY: `sqrtsd` changes only the register it is given and the flags.
    unsafe {
        asm!(
            "sqrtsd {0}, {0}",
            inout(xmm_reg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    x
}

/// [`sqrt`] for `float`, with `sqrtss`.
#[inline]
pub(crate) fn sqrtf(mut x: f32) -> f32 {
    // SAFETY: as in `sqrt`.
    unsafe {
        asm!(
            "sqrtss {0}, {0}",
            inout(xmm_reg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    x
}

/// `x` rounded to an integer in the current mode, with `cvtsd2si`: inexact if
/// that rounds, and invalid with `i64::MIN` if the result is out of range or
/// `x` is NaN.
#[inline]
pub(crate) fn round_to_i64(x: f64) -> i64 {
    let result: i64;
    // SAFETY: `cvtsd2si` reads one register and writes another.
    unsafe {
        asm!(
            "cvtsd2si {0}, {1}",
            out(reg) result,
            in(xmm_reg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    result
}

/// [`round_to_i64`] for `float`, with `cvtss2si`.
#[inline]
pub(crate) fn round_to_i64f(x: f32) -> i64 {
    let result: i64;
    // SAFETY: as in `round_to_i64`.
    unsafe {
        asm!(
            "cvtss2si {0}, {1}",
            out(reg) result,
            in(xmm_reg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    result
}

/// `x` truncated to an integer, as C's conversion does, with `cvttsd2si`:
/// inexact if that discards a fraction, and invalid with `i64::MIN` if the
/// result is out of range or `x` is NaN. Rust's `as` saturates instead.
#[inline]
pub(crate) fn trunc_to_i64(x: f64) -> i64 {
    let result: i64;
    // SAFETY: as in `round_to_i64`.
    unsafe {
        asm!(
            "cvttsd2si {0}, {1}",
            out(reg) result,
            in(xmm_reg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    result
}

/// [`trunc_to_i64`] for `float`, with `cvttss2si`.
#[inline]
pub(crate) fn trunc_to_i64f(x: f32) -> i64 {
    let result: i64;
    // SAFETY: as in `round_to_i64`.
    unsafe {
        asm!(
            "cvttss2si {0}, {1}",
            out(reg) result,
            in(xmm_reg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    result
}

// A `long` is 64 bits here.
pub(crate) use round_to_i64 as round_to_long;
pub(crate) use round_to_i64f as round_to_longf;
pub(crate) use trunc_to_i64 as trunc_to_long;
pub(crate) use trunc_to_i64f as trunc_to_longf;

/// `i` as a `double`, rounded in the current mode: `cvtsi2sd` follows MXCSR.
#[inline]
pub(crate) fn i64_to_f64(i: i64) -> f64 {
    i as f64
}
