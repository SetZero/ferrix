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

#[cfg(target_arch = "aarch64")]
#[path = "aarch64.rs"]
mod arch;
#[cfg(target_arch = "arm")]
#[path = "arm.rs"]
mod arch;
#[cfg(target_arch = "x86_64")]
#[path = "x86_64.rs"]
mod arch;

pub(crate) use arch::*;
