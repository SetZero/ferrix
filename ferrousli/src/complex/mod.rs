//! `complex.h`: every function for `double complex` and `float complex`,
//! `cabs` to `ctanh` and `cabsf` to `ctanhf`. The `long double complex`
//! functions wait for `long double` math.
//!
//! # Where the code comes from
//!
//! Ported from musl 1.2.5's `src/complex`, which is MIT licensed (see
//! [`crate::math`] for the notice). musl took `ccosh`, `csinh`, `ctanh`,
//! `cexp`, `csqrt` and their kernels from FreeBSD's msun and `catan` from
//! OpenBSD, which had it from Stephen Moshier's Cephes; each module names its
//! files and repeats the notices they carry. The rest musl wrote itself.
//!
//! `cpow` multiplies two complex numbers, which C compiles into a call to
//! libgcc's `__muldc3` whenever the plain product has a NaN in it. That
//! function is C11's Annex G.5.1 example, which [`log`] follows.
//!
//! # musl's bits
//!
//! Each function gives the result musl 1.2.5 gives on x86-64, bit for bit, and
//! raises the exceptions it raises, in every rounding mode; [`crate::math`]
//! says how that is kept, and these functions call its functions exactly
//! where musl calls them. musl is built with `-ffreestanding`, so GCC calls
//! `fabs` and `copysign` rather than inlining them, and compiles each
//! comparison musl writes as one instruction. The ones whose flags differ
//! from the instruction LLVM would choose are written as that instruction:
//! GCC's `>=` is `comisd`, which raises invalid for a quiet NaN where LLVM's
//! `ucomisd` does not, and every comparison of a subnormal raises denormal.
//! musl's `y - y`, a NaN raising invalid for an infinite `y`, is written
//! `barrier(y) - y`; the barrier only keeps clippy from calling it a mistake.
//!
//! Where an operation meets two NaNs, SSE returns its destination's, quieted,
//! and GCC and LLVM each pick the destination of `+` and `*` as they please.
//! So where musl's result can be either of two NaNs, the operation is written
//! as the instruction GCC chose, with [`add`], [`sub`], [`mul`] or [`div`].
//! A function that another calls with a negated part is never inlined, as
//! LLVM would fold the negation into the arithmetic after it, which changes
//! the sign of a NaN, and rounds the other way when rounding up or down.
//! And [`ComplexF::new`] is never inlined either; it says why.
//!
//! The same C program was built against musl 1.2.5 and against this
//! library, and the bits and exceptions of all 44 functions compared for
//! 100,000 arguments each in each of the four rounding modes.
//!
//! # Calling convention
//!
//! C passes and returns a `double complex` as the structure of its two
//! parts, in two SSE registers, and a `float complex` as two `float`s in one;
//! [`Complex`] and [`ComplexF`] are those structures.

use core::arch::asm;

pub mod catan;
pub mod cexp;
pub mod csqrt;
pub mod hyperbolic;
pub mod inverse;
pub mod log;
pub mod parts;
#[cfg(test)]
pub(crate) mod tests;

/// C's `double complex`: the real part, then the imaginary part.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Complex {
    /// The real part.
    pub re: f64,
    /// The imaginary part.
    pub im: f64,
}

/// C's `float complex`: the real part, then the imaginary part.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ComplexF {
    /// The real part.
    pub re: f32,
    /// The imaginary part.
    pub im: f32,
}

impl Complex {
    /// The complex number `re` + `im`i. musl's `CMPLX`.
    #[inline]
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }
}

impl ComplexF {
    /// The complex number `re` + `im`i. musl's `CMPLXF`.
    ///
    /// Never inlined: where two `float` parts are computed side by side for
    /// one of these, LLVM's vectoriser computes them in two lanes of one
    /// `mulps` whose other lanes hold stale register contents, and a stale
    /// zero times an infinity raises invalid, or a stale large number
    /// overflow. A call takes the parts one register at a time.
    #[inline(never)]
    pub const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }
}

/// Whether `x` is an infinity, tested on its bits, as musl's `isinf` is.
#[inline]
pub(crate) const fn is_inf(x: f64) -> bool {
    x.to_bits() << 1 == 0x7ff << 53
}

/// [`is_inf`] for `float`.
#[inline]
pub(crate) const fn is_inff(x: f32) -> bool {
    x.to_bits() << 1 == 0xff << 24
}

/// Whether `x` has its sign bit set, as musl's `signbit` says.
#[inline]
pub(crate) const fn sign_bit(x: f64) -> bool {
    x.to_bits() >> 63 != 0
}

/// [`sign_bit`] for `float`.
#[inline]
pub(crate) const fn sign_bitf(x: f32) -> bool {
    x.to_bits() >> 31 != 0
}

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
