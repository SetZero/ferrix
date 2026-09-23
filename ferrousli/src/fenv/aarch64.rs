//! AArch64's floating-point environment: FPSR holds the exception flags in
//! its low five bits, and FPCR the rounding mode in bits 22 and 23. `fenv_t`
//! holds both registers, FPCR first, as glibc's and musl's do.
//!
//! `float`, `double` and `long double` all run on the one unit -- a
//! `long double` in software, which reads the same two registers -- so there
//! is only one set of flags, unlike x86-64.

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

/// FPCR's rounding mode field.
const ROUNDING: u64 = 0xc0_0000;

/// The flags `fegetexceptflag` saves.
#[allow(non_camel_case_types, reason = "C names it")]
pub type fexcept_t = u32;

/// The whole floating-point environment.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(non_camel_case_types, reason = "C names it")]
pub struct fenv_t {
    /// FPCR: the rounding mode and the trap enables.
    pub __fpcr: u32,
    /// FPSR: the exception flags.
    pub __fpsr: u32,
}

const _: () = assert!(size_of::<fenv_t>() == 8);

/// `FE_DFL_ENV`: not an address, but a value `fesetenv` recognises.
pub const FE_DFL_ENV: *const fenv_t = core::ptr::without_provenance(usize::MAX);

/// FPSR.
fn fpsr() -> u64 {
    let value: u64;
    // SAFETY: reading FPSR changes nothing.
    unsafe { asm!("mrs {}, fpsr", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Sets FPSR.
fn set_fpsr(value: u64) {
    // SAFETY: FPSR holds only the cumulative flags and the saturation bit.
    unsafe { asm!("msr fpsr, {}", in(reg) value, options(nomem, nostack, preserves_flags)) };
}

/// FPCR.
pub(super) fn fpcr() -> u64 {
    let value: u64;
    // SAFETY: reading FPCR changes nothing.
    unsafe { asm!("mrs {}, fpcr", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Sets FPCR.
fn set_fpcr(value: u64) {
    // SAFETY: the callers change only the rounding mode, or install a value
    // `fegetenv` read.
    unsafe { asm!("msr fpcr, {}", in(reg) value, options(nomem, nostack, preserves_flags)) };
}

/// The flag bits of `excepts`.
fn flag_bits(excepts: c_int) -> u64 {
    u64::from((excepts & FE_ALL_EXCEPT).cast_unsigned())
}

/// Clears the exceptions in `excepts`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn feclearexcept(excepts: c_int) -> c_int {
    set_fpsr(fpsr() & !flag_bits(excepts));
    0
}

/// Raises the exceptions in `excepts`, by setting their flags. Every trap is
/// disabled, so nothing more happens.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn feraiseexcept(excepts: c_int) -> c_int {
    set_fpsr(fpsr() | flag_bits(excepts));
    0
}

/// Which of the exceptions in `excepts` are raised.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fetestexcept(excepts: c_int) -> c_int {
    (fpsr() & flag_bits(excepts)) as c_int
}

/// The rounding mode, as FPCR holds it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fegetround() -> c_int {
    (fpcr() & ROUNDING) as c_int
}

/// Sets the rounding mode. `round` has been checked to be one of the four.
pub(super) fn set_round(round: c_int) {
    let bits = u64::from(round.cast_unsigned()) & ROUNDING;
    set_fpcr((fpcr() & !ROUNDING) | bits);
}

/// Saves the environment in `*envp`.
///
/// # Safety
///
/// `envp` must be valid for writing a `fenv_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fegetenv(envp: *mut fenv_t) -> c_int {
    let env = fenv_t {
        // Both registers are 32 bits wide in their defined fields.
        __fpcr: fpcr() as u32,
        __fpsr: fpsr() as u32,
    };
    // SAFETY: the caller vouches for `envp`.
    unsafe { envp.write(env) };
    0
}

/// Installs the environment at `envp`, or the default one -- round to
/// nearest, no flags -- if it is `FE_DFL_ENV`.
///
/// # Safety
///
/// `envp` must be `FE_DFL_ENV` or point to an environment saved by
/// `fegetenv` or `feholdexcept`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fesetenv(envp: *const fenv_t) -> c_int {
    let env = if envp.addr() == FE_DFL_ENV.addr() {
        fenv_t {
            __fpcr: 0,
            __fpsr: 0,
        }
    } else {
        // SAFETY: the caller vouches for `envp`.
        unsafe { envp.read() }
    };
    set_fpcr(u64::from(env.__fpcr));
    set_fpsr(u64::from(env.__fpsr));
    0
}

/// An environment of zeros, for tests to save into.
#[cfg(test)]
pub(super) const fn zeroed_env() -> fenv_t {
    fenv_t {
        __fpcr: 0,
        __fpsr: 0,
    }
}
