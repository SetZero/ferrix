//! The SSE instructions the complex functions compare and combine with, each
//! chosen for the exceptions it raises and the NaN it returns, as GCC's code
//! for the same C does.

use core::arch::asm;

/// Defines a function doing one SSE arithmetic instruction with its first
/// operand as the destination.
macro_rules! ordered {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $reg:ident, $instruction:literal) => {
        $(#[$doc])*
        #[inline]
        pub(crate) fn $name(a: $ty, b: $ty) -> $ty {
            let mut result = a;
            // SAFETY: the instruction changes only its destination register
            // and MXCSR's exception flags.
            unsafe {
                asm!(
                    concat!($instruction, " {a}, {b}"),
                    a = inout($reg) result,
                    b = in($reg) b,
                    options(nomem, nostack, preserves_flags),
                );
            }
            result
        }
    };
}

ordered!(
    /// `a + b` with `a` as the destination of `addsd`, so that where both are
    /// NaNs the result is `a`'s, quieted, as in GCC's code. LLVM takes `+`
    /// and `*` to commute and may pick either NaN.
    add, f64, xmm_reg, "addsd"
);
ordered!(
    /// `a - b` with `a` as the destination of `subsd`; see [`add`].
    sub, f64, xmm_reg, "subsd"
);
ordered!(
    /// `a × b` with `a` as the destination of `mulsd`; see [`add`].
    mul, f64, xmm_reg, "mulsd"
);
ordered!(
    /// `a / b` with `a` as the destination of `divsd`; see [`add`].
    div, f64, xmm_reg, "divsd"
);
ordered!(
    /// [`add`] for `float`, with `addss`.
    addf, f32, xmm_reg, "addss"
);
ordered!(
    /// [`sub`] for `float`, with `subss`.
    subf, f32, xmm_reg, "subss"
);
ordered!(
    /// [`mul`] for `float`, with `mulss`.
    mulf, f32, xmm_reg, "mulss"
);

/// C's `isnan(x)` as GCC compiles it where it cannot use the bits:
/// `ucomisd x, x`, which raises denormal for a subnormal `x` and invalid for a
/// signalling NaN.
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
/// compared above `DBL_MAX` with `ucomisd`, which raises denormal for a
/// subnormal `x` and invalid for a signalling NaN.
#[inline]
pub(crate) fn compares_inf(x: f64) -> bool {
    let above: u8;
    // SAFETY: `ucomisd` reads two registers and sets the flags, which `seta`
    // reads into a third.
    unsafe {
        asm!(
            "ucomisd {x}, {max}",
            "seta {above}",
            x = in(xmm_reg) f64::from_bits(x.to_bits() & !(1 << 63)),
            max = in(xmm_reg) f64::MAX,
            above = out(reg_byte) above,
            options(nomem, nostack),
        );
    }
    above != 0
}

/// [`compares_inf`] for `float`, against `FLT_MAX` with `ucomiss`.
#[inline]
pub(crate) fn compares_inff(x: f32) -> bool {
    let above: u8;
    // SAFETY: as in `compares_inf`.
    unsafe {
        asm!(
            "ucomiss {x}, {max}",
            "seta {above}",
            x = in(xmm_reg) f32::from_bits(x.to_bits() & !(1 << 31)),
            max = in(xmm_reg) f32::MAX,
            above = out(reg_byte) above,
            options(nomem, nostack),
        );
    }
    above != 0
}

/// C's `x >= y` as GCC compiles it, with `comisd`: invalid if either is a
/// NaN, quiet or not, and denormal if either is subnormal.
#[inline]
pub(crate) fn ordered_ge(x: f64, y: f64) -> bool {
    let ge: u8;
    // SAFETY: `comisd` reads two registers and sets the flags, which `setae`
    // reads into a third.
    unsafe {
        asm!(
            "comisd {x}, {y}",
            "setae {ge}",
            x = in(xmm_reg) x,
            y = in(xmm_reg) y,
            ge = out(reg_byte) ge,
            options(nomem, nostack),
        );
    }
    ge != 0
}

/// [`ordered_ge`] for `float`, with `comiss`.
#[inline]
pub(crate) fn ordered_gef(x: f32, y: f32) -> bool {
    let ge: u8;
    // SAFETY: as in `ordered_ge`.
    unsafe {
        asm!(
            "comiss {x}, {y}",
            "setae {ge}",
            x = in(xmm_reg) x,
            y = in(xmm_reg) y,
            ge = out(reg_byte) ge,
            options(nomem, nostack),
        );
    }
    ge != 0
}

/// C's `x == 0`, with `ucomisd` against zero: invalid only for a signalling
/// NaN, and denormal if `x` is subnormal.
#[inline]
pub(crate) fn equals_zero(x: f64) -> bool {
    let equal: u8;
    let ordered: u8;
    // SAFETY: `xorpd` clears a register the block owns, `ucomisd` compares it
    // with `x` and sets the flags, and the `set`s read them.
    unsafe {
        asm!(
            "xorpd {zero}, {zero}",
            "ucomisd {x}, {zero}",
            "sete {equal}",
            "setnp {ordered}",
            x = in(xmm_reg) x,
            zero = out(xmm_reg) _,
            equal = out(reg_byte) equal,
            ordered = out(reg_byte) ordered,
            options(nomem, nostack),
        );
    }
    equal & ordered != 0
}

/// [`equals_zero`] for `float`, with `ucomiss`.
#[inline]
pub(crate) fn equals_zerof(x: f32) -> bool {
    let equal: u8;
    let ordered: u8;
    // SAFETY: as in `equals_zero`.
    unsafe {
        asm!(
            "xorps {zero}, {zero}",
            "ucomiss {x}, {zero}",
            "sete {equal}",
            "setnp {ordered}",
            x = in(xmm_reg) x,
            zero = out(xmm_reg) _,
            equal = out(reg_byte) equal,
            ordered = out(reg_byte) ordered,
            options(nomem, nostack),
        );
    }
    equal & ordered != 0
}

/// Whether `x` or `y` is a NaN, with `ucomisd`, as GCC tests a complex
/// product: invalid only for a signalling NaN, and denormal if either is
/// subnormal.
#[inline]
pub(crate) fn unordered(x: f64, y: f64) -> bool {
    let parity: u8;
    // SAFETY: `ucomisd` reads two registers and sets the flags, which `setp`
    // reads into a third.
    unsafe {
        asm!(
            "ucomisd {x}, {y}",
            "setp {parity}",
            x = in(xmm_reg) x,
            y = in(xmm_reg) y,
            parity = out(reg_byte) parity,
            options(nomem, nostack),
        );
    }
    parity != 0
}

/// [`unordered`] for `float`, with `ucomiss`.
#[inline]
pub(crate) fn unorderedf(x: f32, y: f32) -> bool {
    let parity: u8;
    // SAFETY: as in `unordered`.
    unsafe {
        asm!(
            "ucomiss {x}, {y}",
            "setp {parity}",
            x = in(xmm_reg) x,
            y = in(xmm_reg) y,
            parity = out(reg_byte) parity,
            options(nomem, nostack),
        );
    }
    parity != 0
}
