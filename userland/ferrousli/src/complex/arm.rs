//! The VFP instructions the complex functions compare and combine with on
//! ARMv7-A, chosen as on x86-64 for the exceptions each raises and the NaN it
//! returns.
//!
//! `vcmp` is the quiet comparison, raising invalid only for a signalling NaN,
//! and `vcmpe` the signalling one, raising it for any NaN. The result lands in
//! FPSCR, and `vmrs` copies it to the condition flags; an unordered result is
//! NZCV 0011, which fails every ordered condition. With FPSCR.DN clear, as
//! Linux leaves it, an arithmetic instruction given two NaNs returns the
//! first, quieted. There is no denormal exception.

use core::arch::asm;

/// Defines a function doing one arithmetic instruction with its first operand
/// as the first source.
macro_rules! ordered {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $class:ident, $instruction:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(a: $ty, b: $ty) -> $ty {
            let result: $ty;
            // SAFETY: the instruction writes only its destination register
            // and FPSCR's exception flags.
            unsafe {
                asm!(
                    $instruction,
                    r = lateout($class) result,
                    a = in($class) a,
                    b = in($class) b,
                    options(nomem, nostack, preserves_flags),
                );
            }
            result
        }
    };
}

ordered!(
    /// `a + b` with `a` as the first operand of `vadd`, so that where both are
    /// NaNs the result is `a`'s, quieted, as in GCC's code. LLVM takes `+`
    /// and `*` to commute and may pick either NaN.
    add, f64, dreg, "vadd.f64 {r}, {a}, {b}"
);
ordered!(
    /// `a - b` with `a` as the first operand of `vsub`; see [`add`].
    sub, f64, dreg, "vsub.f64 {r}, {a}, {b}"
);
ordered!(
    /// `a × b` with `a` as the first operand of `vmul`; see [`add`].
    mul, f64, dreg, "vmul.f64 {r}, {a}, {b}"
);
ordered!(
    /// `a / b` with `a` as the first operand of `vdiv`; see [`add`].
    div, f64, dreg, "vdiv.f64 {r}, {a}, {b}"
);
ordered!(
    /// [`add`] for `float`.
    addf, f32, sreg, "vadd.f32 {r}, {a}, {b}"
);
ordered!(
    /// [`sub`] for `float`.
    subf, f32, sreg, "vsub.f32 {r}, {a}, {b}"
);
ordered!(
    /// [`mul`] for `float`.
    mulf, f32, sreg, "vmul.f32 {r}, {a}, {b}"
);

/// Defines a function comparing two values with one instruction and testing
/// one condition.
macro_rules! compare {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $class:ident, $compare:literal, $condition:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(x: $ty, y: $ty) -> bool {
            let result: u32;
            // SAFETY: the comparison reads two registers and sets FPSCR's
            // flags, `vmrs` copies them to the condition flags, and the
            // conditional move reads those.
            unsafe {
                asm!(
                    "mov {r}, #0",
                    $compare,
                    "vmrs APSR_nzcv, fpscr",
                    concat!("mov", $condition, " {r}, #1"),
                    x = in($class) x,
                    y = in($class) y,
                    r = out(reg) result,
                    options(nomem, nostack),
                );
            }
            result != 0
        }
    };
}

compare!(
    /// Whether `x` or `y` is a NaN, with `vcmp`, as GCC tests a complex
    /// product: invalid only for a signalling NaN.
    unordered, f64, dreg, "vcmp.f64 {x}, {y}", "vs"
);
compare!(
    /// [`unordered`] for `float`.
    unorderedf, f32, sreg, "vcmp.f32 {x}, {y}", "vs"
);
compare!(
    /// C's `x >= y` as GCC compiles it, with `vcmpe`: invalid if either is a
    /// NaN, quiet or not.
    ordered_ge, f64, dreg, "vcmpe.f64 {x}, {y}", "ge"
);
compare!(
    /// [`ordered_ge`] for `float`.
    ordered_gef, f32, sreg, "vcmpe.f32 {x}, {y}", "ge"
);
compare!(
    /// `x > y` with `vcmp`: quiet, so invalid only for a signalling NaN.
    quiet_gt, f64, dreg, "vcmp.f64 {x}, {y}", "gt"
);
compare!(
    /// [`quiet_gt`] for `float`.
    quiet_gtf, f32, sreg, "vcmp.f32 {x}, {y}", "gt"
);
compare!(
    /// `x == y` with `vcmp`: quiet, so invalid only for a signalling NaN.
    quiet_eq, f64, dreg, "vcmp.f64 {x}, {y}", "eq"
);
compare!(
    /// [`quiet_eq`] for `float`.
    quiet_eqf, f32, sreg, "vcmp.f32 {x}, {y}", "eq"
);

/// C's `isnan(x)` as GCC compiles it where it cannot use the bits: `vcmp`
/// of `x` with itself, which raises invalid for a signalling NaN.
#[inline]
pub(crate) fn compares_nan(x: f64) -> bool {
    unordered(x, x)
}

/// [`compares_nan`] for `float`.
#[inline]
pub(crate) fn compares_nanf(x: f32) -> bool {
    unorderedf(x, x)
}

/// C's `isinf(x)` as GCC compiles it where it cannot use the bits: |x|
/// compared above `DBL_MAX` with `vcmp`, which raises invalid for a
/// signalling NaN.
#[inline]
pub(crate) fn compares_inf(x: f64) -> bool {
    quiet_gt(f64::from_bits(x.to_bits() & !(1 << 63)), f64::MAX)
}

/// [`compares_inf`] for `float`, against `FLT_MAX`.
#[inline]
pub(crate) fn compares_inff(x: f32) -> bool {
    quiet_gtf(f32::from_bits(x.to_bits() & !(1 << 31)), f32::MAX)
}

/// C's `x == 0`, with `vcmp`: invalid only for a signalling NaN.
#[inline]
pub(crate) fn equals_zero(x: f64) -> bool {
    quiet_eq(x, 0.0)
}

/// [`equals_zero`] for `float`.
#[inline]
pub(crate) fn equals_zerof(x: f32) -> bool {
    quiet_eqf(x, 0.0)
}
