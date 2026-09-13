//! `sqrt` and `sqrtf`, as musl 1.2.5 has them on x86-64
//! (`x86_64/sqrt.c`, `x86_64/sqrtf.c`; MIT, see [`crate::math`] for the
//! notice): one SSE2 instruction, which is correctly rounded in every rounding
//! mode, raises inexact when the root is not exact, and raises invalid for a
//! negative argument.
//!
//! musl's portable `sqrt.c`, for architectures without such an instruction,
//! is not ported yet; the architecture module supplies the root.

use crate::math::arch;

/// The square root of `x`, correctly rounded.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sqrt(x: f64) -> f64 {
    arch::sqrt(x)
}

/// [`sqrt`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sqrtf(x: f32) -> f32 {
    arch::sqrtf(x)
}
