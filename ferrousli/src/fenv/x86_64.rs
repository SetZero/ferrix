//! The x86-64 half of `fenv.h`: the constants and `fenv_t` from
//! `include/bits/fenv.h`, and the functions that reach MXCSR and the x87.
//!
//! Ported from musl 1.2.5's `src/fenv/x86_64/fenv.s` (MIT; see
//! [`crate::math`] for the notice).
//!
//! Inline assembly may change MXCSR and the x87 control word; that is this
//! header's purpose. The optimiser still assumes the default environment
//! elsewhere, which is why the math library hides its operands from it; see
//! [`crate::math`].
//!
//! The x87 status word's exception flags and MXCSR's are the same six bits,
//! in the same order, so one mask serves both. The rounding control is bits 10
//! and 11 of the x87 control word, and bits 13 and 14 of MXCSR.

use core::arch::asm;
use core::ffi::c_int;

/// The invalid operation exception.
pub const FE_INVALID: c_int = 1;
/// The divide-by-zero exception.
pub const FE_DIVBYZERO: c_int = 4;
/// The overflow exception.
pub const FE_OVERFLOW: c_int = 8;
/// The underflow exception.
pub const FE_UNDERFLOW: c_int = 16;
/// The inexact exception.
pub const FE_INEXACT: c_int = 32;
/// Every exception, and the x87's denormal-operand flag with them.
pub const FE_ALL_EXCEPT: c_int = 63;

/// Rounding to nearest, ties to even.
pub const FE_TONEAREST: c_int = 0;
/// Rounding toward negative infinity.
pub const FE_DOWNWARD: c_int = 0x400;
/// Rounding toward positive infinity.
pub const FE_UPWARD: c_int = 0x800;
/// Rounding toward zero.
pub const FE_TOWARDZERO: c_int = 0xc00;

/// How far MXCSR's rounding control sits above the x87's.
const MXCSR_ROUNDING_SHIFT: u32 = 3;

/// The saved exception flags, as `fegetexceptflag` stores them.
#[allow(non_camel_case_types, reason = "C names it")]
pub type fexcept_t = u16;

/// The whole floating-point environment: the 28 bytes `fnstenv` saves for the
/// x87, then MXCSR. glibc's x86-64 `fenv_t` has the same layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(non_camel_case_types, reason = "C names it")]
pub struct fenv_t {
    /// The x87 control word: exception masks, precision and rounding.
    pub __control_word: u16,
    /// Padding.
    pub __unused1: u16,
    /// The x87 status word, whose low six bits are its exception flags.
    pub __status_word: u16,
    /// Padding.
    pub __unused2: u16,
    /// The x87 tag word.
    pub __tags: u16,
    /// Padding.
    pub __unused3: u16,
    /// The last x87 instruction's address.
    pub __eip: u32,
    /// The last x87 instruction's code segment.
    pub __cs_selector: u16,
    /// C's `__opcode:11` and `__unused4:5` bit-fields, which share these 16
    /// bits.
    pub __opcode: u16,
    /// The last x87 operand's address.
    pub __data_offset: u32,
    /// The last x87 operand's segment.
    pub __data_selector: u16,
    /// Padding.
    pub __unused5: u16,
    /// MXCSR.
    pub __mxcsr: u32,
}

const _: () = assert!(size_of::<fenv_t>() == 32);
const _: () = assert!(core::mem::offset_of!(fenv_t, __status_word) == 4);
const _: () = assert!(core::mem::offset_of!(fenv_t, __eip) == 12);
const _: () = assert!(core::mem::offset_of!(fenv_t, __opcode) == 18);
const _: () = assert!(core::mem::offset_of!(fenv_t, __data_offset) == 20);
const _: () = assert!(core::mem::offset_of!(fenv_t, __mxcsr) == 28);

/// `FE_DFL_ENV`: C's `(const fenv_t *) -1`, which names the default
/// environment rather than pointing at one.
pub const FE_DFL_ENV: *const fenv_t = core::ptr::without_provenance(usize::MAX);

/// The environment a program starts in: every exception masked and clear,
/// rounding to nearest, the x87 at 64-bit precision with its stack empty.
const DEFAULT_ENV: fenv_t = fenv_t {
    __control_word: 0x37f,
    __unused1: 0,
    __status_word: 0,
    __unused2: 0,
    __tags: 0xffff,
    __unused3: 0,
    __eip: 0,
    __cs_selector: 0,
    __opcode: 0,
    __data_offset: 0,
    __data_selector: 0,
    __unused5: 0,
    __mxcsr: 0x1f80,
};

/// [`DEFAULT_ENV`] at an address, for `fldenv` to read.
static DEFAULT_ENV_STORAGE: fenv_t = DEFAULT_ENV;

/// MXCSR.
fn mxcsr() -> u32 {
    let mut value = 0u32;
    // SAFETY: `stmxcsr` writes four bytes to the local it is given.
    unsafe {
        asm!(
            "stmxcsr dword ptr [{}]",
            in(reg) &raw mut value,
            options(nostack, preserves_flags),
        );
    }
    value
}

/// Loads MXCSR.
fn set_mxcsr(value: u32) {
    // SAFETY: `ldmxcsr` reads four bytes from the local it is given. Every
    // value loaded here came from `stmxcsr` with only flag, mask or rounding
    // bits changed, so no reserved bit is set.
    unsafe {
        asm!(
            "ldmxcsr dword ptr [{}]",
            in(reg) &raw const value,
            options(nostack, preserves_flags),
        );
    }
}

/// The x87 status word.
fn x87_status() -> u16 {
    let mut value = 0u16;
    // SAFETY: `fnstsw` writes two bytes to the local it is given.
    unsafe {
        asm!(
            "fnstsw word ptr [{}]",
            in(reg) &raw mut value,
            options(nostack, preserves_flags),
        );
    }
    value
}

/// Clears the x87's exception flags.
fn x87_clear() {
    // SAFETY: `fnclex` changes only the x87 status word.
    unsafe { asm!("fnclex", options(nomem, nostack, preserves_flags)) };
}

/// The x87 control word.
pub(super) fn x87_control() -> u16 {
    let mut value = 0u16;
    // SAFETY: `fnstcw` writes two bytes to the local it is given.
    unsafe {
        asm!(
            "fnstcw word ptr [{}]",
            in(reg) &raw mut value,
            options(nostack, preserves_flags),
        );
    }
    value
}

/// Loads the x87 control word.
fn set_x87_control(value: u16) {
    // SAFETY: `fldcw` reads two bytes from the local it is given.
    unsafe {
        asm!(
            "fldcw word ptr [{}]",
            in(reg) &raw const value,
            options(nostack, preserves_flags),
        );
    }
}

/// `excepts` reduced to the flag bits.
fn flag_bits(excepts: c_int) -> u32 {
    (excepts & FE_ALL_EXCEPT).cast_unsigned()
}

/// Clears the exceptions in `excepts`.
///
/// The x87 can only clear all of its flags at once. The ones not being
/// cleared are moved into MXCSR first, as musl does, so they stay raised.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn feclearexcept(excepts: c_int) -> c_int {
    let mask = flag_bits(excepts);
    let x87 = u32::from(x87_status()) & flag_bits(FE_ALL_EXCEPT);
    if x87 & mask != 0 {
        x87_clear();
    }
    let csr = mxcsr() | x87;
    if csr & mask != 0 {
        set_mxcsr(csr & !mask);
    }
    0
}

/// Raises the exceptions in `excepts` by setting their flags in MXCSR.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn feraiseexcept(excepts: c_int) -> c_int {
    set_mxcsr(mxcsr() | flag_bits(excepts));
    0
}

/// Which of the exceptions in `excepts` are raised in either unit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fetestexcept(excepts: c_int) -> c_int {
    ((mxcsr() | u32::from(x87_status())) & flag_bits(excepts)).cast_signed()
}

/// The rounding mode, as MXCSR holds it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fegetround() -> c_int {
    ((mxcsr() >> MXCSR_ROUNDING_SHIFT) & 0xc00).cast_signed()
}

/// Sets the rounding mode in both units. `round` has been checked to be one
/// of the four modes.
pub(super) fn set_round(round: c_int) {
    let bits = round.cast_unsigned() & 0xc00;
    // The mode's bits fit in the control word's.
    set_x87_control((x87_control() & !0xc00) | bits as u16);
    let rounding = 0xc00 << MXCSR_ROUNDING_SHIFT;
    set_mxcsr((mxcsr() & !rounding) | bits << MXCSR_ROUNDING_SHIFT);
}

/// Saves the environment in `*envp`.
///
/// `fnstenv` masks every x87 exception once it has saved the environment; the
/// saved control word is loaded again straight after, so nothing changes.
///
/// # Safety
///
/// `envp` must be valid for writing a `fenv_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fegetenv(envp: *mut fenv_t) -> c_int {
    // SAFETY: the caller vouches for the 32 bytes at `envp`, of which
    // `fnstenv` writes the first 28 and `fldcw` reads the first two.
    unsafe {
        asm!(
            "fnstenv [{0}]",
            "fldcw word ptr [{0}]",
            in(reg) envp,
            options(nostack, preserves_flags),
        );
    }
    let csr = mxcsr();
    // SAFETY: as above.
    unsafe { (*envp).__mxcsr = csr };
    0
}

/// Installs the environment at `envp`, or the default one if it is
/// `FE_DFL_ENV`.
///
/// # Safety
///
/// `envp` must be `FE_DFL_ENV` or point to an environment saved by
/// `fegetenv` or `feholdexcept`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fesetenv(envp: *const fenv_t) -> c_int {
    let envp = if envp.addr() == FE_DFL_ENV.addr() {
        &raw const DEFAULT_ENV_STORAGE
    } else {
        envp
    };
    // SAFETY: `envp` is the default environment or, by the caller's word, a
    // saved one, so every field is one the processor accepts.
    unsafe {
        asm!(
            "fldenv [{0}]",
            "ldmxcsr dword ptr [{0} + 28]",
            in(reg) envp,
            options(nostack, preserves_flags),
        );
    }
    0
}

/// An environment of zeros, for tests to save into.
#[cfg(test)]
pub(super) const fn zeroed_env() -> fenv_t {
    fenv_t {
        __control_word: 0,
        __tags: 0,
        __mxcsr: 0,
        ..DEFAULT_ENV
    }
}

/// Divides one by zero on the x87, raising its divide-by-zero flag.
#[cfg(test)]
pub(super) fn x87_divide_by_zero() {
    // SAFETY: pushes two values on the x87 stack and pops both.
    unsafe {
        asm!(
            "fld1",
            "fldz",
            "fdivp st(1), st",
            "fstp st(0)",
            options(nomem, nostack),
        );
    }
}

/// Takes the square root of -1 on the x87, raising its invalid flag.
#[cfg(test)]
pub(super) fn x87_invalid() {
    // SAFETY: pushes one value on the x87 stack and pops it.
    unsafe {
        asm!(
            "fld1",
            "fchs",
            "fsqrt",
            "fstp st(0)",
            options(nomem, nostack),
        );
    }
}
