//! The AArch64 instructions the complex functions compare and combine with,
//! chosen as on x86-64 for the exceptions each raises and the NaN it returns.
//!
//! `fcmp` is the quiet comparison, raising invalid only for a signalling NaN,
//! and `fcmpe` the signalling one, raising it for any NaN; an unordered result
//! sets NZCV to 0011, which fails every ordered condition. With FPCR.DN clear,
//! as Linux leaves it, an arithmetic instruction given two NaNs returns the
//! first, quieted. AArch64 has no denormal exception.

use core::arch::asm;

/// Defines a function doing one arithmetic instruction with its first operand
/// as the first source.
macro_rules! ordered {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $instruction:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(a: $ty, b: $ty) -> $ty {
            let result: $ty;
            // SAFETY: the instruction writes only its destination register
            // and FPSR's exception flags.
            unsafe {
                asm!(
                    $instruction,
                    r = lateout(vreg) result,
                    a = in(vreg) a,
                    b = in(vreg) b,
                    options(nomem, nostack, preserves_flags),
                );
            }
            result
        }
    };
}

ordered!(
    /// `a + b` with `a` as the first operand of `fadd`, so that where both are
    /// NaNs the result is `a`'s, quieted, as in GCC's code. LLVM takes `+`
    /// and `*` to commute and may pick either NaN.
    add, f64, "fadd {r:d}, {a:d}, {b:d}"
);
ordered!(
    /// `a - b` with `a` as the first operand of `fsub`; see [`add`].
    sub, f64, "fsub {r:d}, {a:d}, {b:d}"
);
ordered!(
    /// `a × b` with `a` as the first operand of `fmul`; see [`add`].
    mul, f64, "fmul {r:d}, {a:d}, {b:d}"
);
ordered!(
    /// `a / b` with `a` as the first operand of `fdiv`; see [`add`].
    div, f64, "fdiv {r:d}, {a:d}, {b:d}"
);
ordered!(
    /// [`add`] for `float`.
    addf, f32, "fadd {r:s}, {a:s}, {b:s}"
);
ordered!(
    /// [`sub`] for `float`.
    subf, f32, "fsub {r:s}, {a:s}, {b:s}"
);
ordered!(
    /// [`mul`] for `float`.
    mulf, f32, "fmul {r:s}, {a:s}, {b:s}"
);

/// Defines a function comparing two values with one instruction and testing
/// one condition.
macro_rules! compare {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $compare:literal, $condition:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(x: $ty, y: $ty) -> bool {
            let result: u32;
            // SAFETY: the comparison reads two registers and sets NZCV, which
            // `cset` reads into a third.
            unsafe {
                asm!(
                    $compare,
                    concat!("cset {r:w}, ", $condition),
                    x = in(vreg) x,
                    y = in(vreg) y,
                    r = lateout(reg) result,
                    options(nomem, nostack),
                );
            }
            result != 0
        }
    };
}

compare!(
    /// Whether `x` or `y` is a NaN, with `fcmp`, as GCC tests a complex
    /// product: invalid only for a signalling NaN.
    unordered, f64, "fcmp {x:d}, {y:d}", "vs"
);
compare!(
    /// [`unordered`] for `float`.
    unorderedf, f32, "fcmp {x:s}, {y:s}", "vs"
);
compare!(
    /// C's `x >= y` as GCC compiles it, with `fcmpe`: invalid if either is a
    /// NaN, quiet or not.
    ordered_ge, f64, "fcmpe {x:d}, {y:d}", "ge"
);
compare!(
    /// [`ordered_ge`] for `float`.
    ordered_gef, f32, "fcmpe {x:s}, {y:s}", "ge"
);
compare!(
    /// `x > y` with `fcmp`: quiet, so invalid only for a signalling NaN.
    quiet_gt, f64, "fcmp {x:d}, {y:d}", "gt"
);
compare!(
    /// [`quiet_gt`] for `float`.
    quiet_gtf, f32, "fcmp {x:s}, {y:s}", "gt"
);
compare!(
    /// `x == y` with `fcmp`: quiet, so invalid only for a signalling NaN.
    quiet_eq, f64, "fcmp {x:d}, {y:d}", "eq"
);
compare!(
    /// [`quiet_eq`] for `float`.
    quiet_eqf, f32, "fcmp {x:s}, {y:s}", "eq"
);

/// C's `isnan(x)` as GCC compiles it where it cannot use the bits: `fcmp`
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
/// compared above `DBL_MAX` with `fcmp`, which raises invalid for a
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

/// C's `x == 0`, with `fcmp`: invalid only for a signalling NaN.
#[inline]
pub(crate) fn equals_zero(x: f64) -> bool {
    quiet_eq(x, 0.0)
}

/// [`equals_zero`] for `float`.
#[inline]
pub(crate) fn equals_zerof(x: f32) -> bool {
    quiet_eqf(x, 0.0)
}
