//! The AArch64 instructions the math library uses: square roots, and
//! conversions to integers. Each follows the rounding mode in FPCR and raises
//! its exceptions in FPSR exactly as C asks, and each is written as an
//! instruction so the compiler cannot turn it into a call to the function being
//! defined.
//!
//! An out-of-range conversion saturates here, where x86-64's returns
//! `i64::MIN`; both raise invalid, and C leaves the value unspecified.

use core::arch::asm;

/// The square root of `x`, correctly rounded in the current mode, with
/// `fsqrt`: invalid for a negative `x`, inexact when the root is not exact.
#[inline]
pub(crate) fn sqrt(x: f64) -> f64 {
    let root: f64;
    // SAFETY: `fsqrt` writes one register and FPSR's flags.
    unsafe {
        asm!(
            "fsqrt {r:d}, {x:d}",
            r = lateout(vreg) root,
            x = in(vreg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    root
}

/// [`sqrt`] for `float`.
#[inline]
pub(crate) fn sqrtf(x: f32) -> f32 {
    let root: f32;
    // SAFETY: as in `sqrt`.
    unsafe {
        asm!(
            "fsqrt {r:s}, {x:s}",
            r = lateout(vreg) root,
            x = in(vreg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    root
}

/// `x` rounded to an integer in the current mode: `frintx` rounds, raising
/// inexact if that changes it, and `fcvtzs` converts the integral result,
/// raising invalid if it is out of range or NaN.
#[inline]
pub(crate) fn round_to_i64(x: f64) -> i64 {
    let result: i64;
    // SAFETY: the two instructions write one scratch register, the result,
    // and FPSR's flags.
    unsafe {
        asm!(
            "frintx {t:d}, {x:d}",
            "fcvtzs {r}, {t:d}",
            r = lateout(reg) result,
            x = in(vreg) x,
            t = out(vreg) _,
            options(nomem, nostack, preserves_flags),
        );
    }
    result
}

/// [`round_to_i64`] for `float`.
#[inline]
pub(crate) fn round_to_i64f(x: f32) -> i64 {
    let result: i64;
    // SAFETY: as in `round_to_i64`.
    unsafe {
        asm!(
            "frintx {t:s}, {x:s}",
            "fcvtzs {r}, {t:s}",
            r = lateout(reg) result,
            x = in(vreg) x,
            t = out(vreg) _,
            options(nomem, nostack, preserves_flags),
        );
    }
    result
}

/// `x` truncated to an integer, as C's conversion does, with `fcvtzs`:
/// inexact if that discards a fraction, and invalid if the result is out of
/// range or `x` is NaN.
#[inline]
pub(crate) fn trunc_to_i64(x: f64) -> i64 {
    let result: i64;
    // SAFETY: `fcvtzs` writes one register and FPSR's flags.
    unsafe {
        asm!(
            "fcvtzs {r}, {x:d}",
            r = lateout(reg) result,
            x = in(vreg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    result
}

/// [`trunc_to_i64`] for `float`.
#[inline]
pub(crate) fn trunc_to_i64f(x: f32) -> i64 {
    let result: i64;
    // SAFETY: as in `trunc_to_i64`.
    unsafe {
        asm!(
            "fcvtzs {r}, {x:s}",
            r = lateout(reg) result,
            x = in(vreg) x,
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

/// `i` as a `double`, rounded in the current mode: `scvtf` follows FPCR.
#[inline]
pub(crate) fn i64_to_f64(i: i64) -> f64 {
    i as f64
}
