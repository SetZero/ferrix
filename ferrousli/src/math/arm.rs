//! The VFP instructions the math library uses on ARMv7-A, and what stands in
//! for those it lacks.
//!
//! VFPv3 has square roots and conversions to 32-bit integers, which is what a
//! `long` is here: `vcvtr` rounds in FPSCR's mode and `vcvt` truncates, each
//! raising inexact when it rounds and invalid, with a saturated result, when
//! the value is out of range or NaN. It has no conversion to a 64-bit integer,
//! so those -- for `llrint`, `llround` and the gamma functions -- are done in
//! software here, raising the same exceptions x86-64's instruction does and
//! returning `i64::MIN` where it does.

use core::arch::asm;
use core::ffi::c_long;

use crate::fenv::{FE_INEXACT, FE_INVALID, FE_OVERFLOW, FE_UNDERFLOW, feraiseexcept};

/// The square root of `x`, correctly rounded in the current mode, with
/// `vsqrt`: invalid for a negative `x`, inexact when the root is not exact.
#[inline]
pub(crate) fn sqrt(x: f64) -> f64 {
    let root: f64;
    // SAFETY: `vsqrt` writes one register and FPSCR's flags.
    unsafe {
        asm!(
            "vsqrt.f64 {r}, {x}",
            r = lateout(dreg) root,
            x = in(dreg) x,
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
            "vsqrt.f32 {r}, {x}",
            r = lateout(sreg) root,
            x = in(sreg) x,
            options(nomem, nostack, preserves_flags),
        );
    }
    root
}

/// Defines a conversion to a 32-bit integer with one VFP instruction, through
/// a single-precision register, which is where VFP puts an integer.
macro_rules! convert {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $class:ident, $instruction:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(x: $ty) -> c_long {
            let result: c_long;
            // SAFETY: the conversion writes one scratch register and FPSCR's
            // flags, and `vmov` copies the scratch register out.
            unsafe {
                asm!(
                    concat!($instruction, " {t}, {x}"),
                    "vmov {r}, {t}",
                    r = lateout(reg) result,
                    x = in($class) x,
                    t = out(sreg) _,
                    options(nomem, nostack, preserves_flags),
                );
            }
            result
        }
    };
}

convert!(
    /// `x` rounded to a `long` in the current mode, with `vcvtr`.
    round_to_long, f64, dreg, "vcvtr.s32.f64"
);
convert!(
    /// [`round_to_long`] for `float`.
    round_to_longf, f32, sreg, "vcvtr.s32.f32"
);
convert!(
    /// `x` truncated to a `long`, as C's conversion does, with `vcvt`.
    trunc_to_long, f64, dreg, "vcvt.s32.f64"
);
convert!(
    /// [`trunc_to_long`] for `float`.
    trunc_to_longf, f32, sreg, "vcvt.s32.f32"
);

/// `x` truncated to a 64-bit integer, as C's conversion does: inexact if that
/// discards a fraction, and invalid with `i64::MIN` if the result is out of
/// range or `x` is NaN.
pub(crate) fn trunc_to_i64(x: f64) -> i64 {
    /// 2^63.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    let magnitude = x.to_bits() & !(1 << 63);
    // NaN, or outside [-2^63, 2^63).
    if magnitude > f64::INFINITY.to_bits() || !(-LIMIT..LIMIT).contains(&x) {
        let _ = feraiseexcept(FE_INVALID);
        return i64::MIN;
    }
    // In range, so the cast truncates exactly.
    let result = x as i64;
    // Where |x| is 2^53 or more it is an integer, and `result` converts back
    // exactly; below that the conversion back is exact too.
    if result as f64 != x {
        let _ = feraiseexcept(FE_INEXACT);
    }
    result
}

/// [`trunc_to_i64`] for `float`, whose every value converts to `double`
/// exactly.
pub(crate) fn trunc_to_i64f(x: f32) -> i64 {
    trunc_to_i64(f64::from(x))
}

/// `x` rounded to a 64-bit integer in the current mode: `rint` rounds,
/// raising inexact if that changes it, and the integral result is converted,
/// raising invalid if it is out of range.
pub(crate) fn round_to_i64(x: f64) -> i64 {
    trunc_to_i64(crate::math::rounding::rint(x))
}

/// [`round_to_i64`] for `float`.
pub(crate) fn round_to_i64f(x: f32) -> i64 {
    trunc_to_i64f(crate::math::rounding::rintf(x))
}

/// `i` as a `double`, rounded in the current mode. VFP has no conversion from
/// a 64-bit integer, and the library routine Rust calls always rounds to
/// nearest, so the integer is split into halves that convert exactly and one
/// addition rounds, as FPSCR says.
pub(crate) fn i64_to_f64(i: i64) -> f64 {
    let high = f64::from((i >> 32) as i32) * 4_294_967_296.0;
    let low = f64::from(i as u32);
    // Neither operand is a constant, so the addition is made at run time.
    crate::math::support::barrier(high) + low
}

/// The next `float` after `x` toward `y`, which C declares a `long double`:
/// a `double` here. `nexttoward` and `nexttowardl` are `nextafter` on
/// ARMv7-A, as `arm_names` says; this one is not `nextafterf`, whose `y` is a
/// `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[cfg_attr(
    test,
    expect(dead_code, reason = "exported to C, which unit tests do not link")
)]
pub(crate) extern "C" fn nexttowardf(x: f32, y: f64) -> f32 {
    let wide = f64::from(x);
    if x.is_nan() || y.is_nan() {
        return (wide + y) as f32;
    }
    if wide == y {
        return x;
    }
    let bits = x.to_bits();
    let next = if x == 0.0 {
        f32::from_bits(if y < 0.0 { 1 << 31 | 1 } else { 1 })
    } else if (wide < y) != x.is_sign_negative() {
        f32::from_bits(bits + 1)
    } else {
        f32::from_bits(bits - 1)
    };
    if next.is_infinite() {
        let _ = feraiseexcept(FE_OVERFLOW | FE_INEXACT);
    } else if next.is_subnormal() || next == 0.0 {
        let _ = feraiseexcept(FE_UNDERFLOW | FE_INEXACT);
    }
    next
}
