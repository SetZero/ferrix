//! ARMv7-A's floating-point environment, under the hard-float ABI: one
//! register, FPSCR, holds the exception flags in its low five bits and the
//! rounding mode in bits 22 and 23. `fenv_t` is that register, as glibc's and
//! musl's are.
//!
//! A `long double` is a `double` here, so there is only one unit and one set
//! of flags.

use core::arch::asm;
use core::ffi::c_int;

/// Invalid operation.
pub const FE_INVALID: c_int = 1;
/// Division by zero.
pub const FE_DIVBYZERO: c_int = 2;
/// A result too large to represent.
pub const FE_OVERFLOW: c_int = 4;
/// A result too small to represent normally.
pub const FE_UNDERFLOW: c_int = 8;
/// A rounded result.
pub const FE_INEXACT: c_int = 16;
/// Every exception.
pub const FE_ALL_EXCEPT: c_int = 31;

/// Round to nearest, ties to even.
pub const FE_TONEAREST: c_int = 0;
/// Round toward positive infinity.
pub const FE_UPWARD: c_int = 0x40_0000;
/// Round toward negative infinity.
pub const FE_DOWNWARD: c_int = 0x80_0000;
/// Round toward zero.
pub const FE_TOWARDZERO: c_int = 0xc0_0000;

/// FPSCR's rounding mode field.
const ROUNDING: u32 = 0xc0_0000;

/// The flags `fegetexceptflag` saves.
#[allow(non_camel_case_types, reason = "C names it")]
pub type fexcept_t = u32;

/// The whole floating-point environment.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(non_camel_case_types, reason = "C names it")]
pub struct fenv_t {
    /// FPSCR.
    pub __cw: u32,
}

const _: () = assert!(size_of::<fenv_t>() == 4);

/// `FE_DFL_ENV`: not an address, but a value `fesetenv` recognises.
pub const FE_DFL_ENV: *const fenv_t = core::ptr::without_provenance(usize::MAX);

/// FPSCR.
pub(super) fn fpscr() -> u32 {
    let value: u32;
    // SAFETY: reading FPSCR changes nothing.
    unsafe { asm!("vmrs {}, fpscr", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Sets FPSCR.
fn set_fpscr(value: u32) {
    // SAFETY: the callers change only the flags or the rounding mode, or
    // install a value `fegetenv` read.
    unsafe { asm!("vmsr fpscr, {}", in(reg) value, options(nomem, nostack, preserves_flags)) };
}

/// The flag bits of `excepts`.
fn flag_bits(excepts: c_int) -> u32 {
    (excepts & FE_ALL_EXCEPT).cast_unsigned()
}

/// Clears the exceptions in `excepts`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn feclearexcept(excepts: c_int) -> c_int {
    set_fpscr(fpscr() & !flag_bits(excepts));
    0
}

/// Raises the exceptions in `excepts`, by setting their flags. Every trap is
/// disabled, so nothing more happens.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn feraiseexcept(excepts: c_int) -> c_int {
    set_fpscr(fpscr() | flag_bits(excepts));
    0
}

/// Which of the exceptions in `excepts` are raised.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fetestexcept(excepts: c_int) -> c_int {
    (fpscr() & flag_bits(excepts)).cast_signed()
}

/// The rounding mode, as FPSCR holds it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fegetround() -> c_int {
    (fpscr() & ROUNDING).cast_signed()
}

/// Sets the rounding mode. `round` has been checked to be one of the four.
pub(super) fn set_round(round: c_int) {
    let bits = round.cast_unsigned() & ROUNDING;
    set_fpscr((fpscr() & !ROUNDING) | bits);
}

/// Saves the environment in `*envp`.
///
/// # Safety
///
/// `envp` must be valid for writing a `fenv_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fegetenv(envp: *mut fenv_t) -> c_int {
    // SAFETY: the caller vouches for `envp`.
    unsafe { envp.write(fenv_t { __cw: fpscr() }) };
    0
}

/// Installs the environment at `envp`, or the default one -- round to
/// nearest, no flags -- if it is `FE_DFL_ENV`. The bits outside the flags and
/// the rounding mode are the thread's own and are kept.
///
/// # Safety
///
/// `envp` must be `FE_DFL_ENV` or point to an environment saved by
/// `fegetenv` or `feholdexcept`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fesetenv(envp: *const fenv_t) -> c_int {
    let env = if envp.addr() == FE_DFL_ENV.addr() {
        0
    } else {
        // SAFETY: the caller vouches for `envp`.
        unsafe { envp.read() }.__cw
    };
    let ours = ROUNDING | flag_bits(FE_ALL_EXCEPT);
    set_fpscr((fpscr() & !ours) | (env & ours));
    0
}

/// An environment of zeros, for tests to save into.
#[cfg(test)]
pub(super) const fn zeroed_env() -> fenv_t {
    fenv_t { __cw: 0 }
}
