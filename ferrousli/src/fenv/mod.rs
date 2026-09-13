//! `fenv.h`: the floating-point exception flags, the rounding mode, and the
//! environment that holds both.
//!
//! The functions that only combine others are here, and are the same on every
//! architecture. Those that touch the hardware, and the layout of `fenv_t`, are
//! in the architecture's module, and re-exported from here.
//!
//! Ported from musl 1.2.5's `src/fenv` (MIT; see [`crate::math`] for the
//! notice).
//!
//! # x86-64
//!
//! Every x86-64 thread has two floating-point units. `float` and `double`
//! arithmetic runs on SSE, controlled by MXCSR. `long double` arithmetic runs
//! on the x87, which has its own control and status words. So, as in musl:
//!
//! * the flags a function tests are the union of both units' flags;
//! * clearing clears both, and raising sets MXCSR's;
//! * the rounding mode is set in both, and read from MXCSR;
//! * `fenv_t` holds the whole x87 environment followed by MXCSR.

use core::ffi::c_int;

#[cfg(target_arch = "x86_64")]
#[path = "x86_64.rs"]
mod arch;

pub use arch::{
    FE_ALL_EXCEPT, FE_DFL_ENV, FE_DIVBYZERO, FE_DOWNWARD, FE_INEXACT, FE_INVALID, FE_OVERFLOW,
    FE_TONEAREST, FE_TOWARDZERO, FE_UNDERFLOW, FE_UPWARD, feclearexcept, fegetenv, fegetround,
    fenv_t, feraiseexcept, fesetenv, fetestexcept, fexcept_t,
};

/// Stores which of the exceptions in `excepts` are raised in `*flagp`.
///
/// # Safety
///
/// `flagp` must be valid for writing a `fexcept_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fegetexceptflag(flagp: *mut fexcept_t, excepts: c_int) -> c_int {
    // The flags all fit in the low six bits.
    let flags = fetestexcept(excepts) as fexcept_t;
    // SAFETY: the caller vouches for `flagp`.
    unsafe { flagp.write(flags) };
    0
}

/// Sets the exceptions in `excepts` to the states saved in `*flagp` by
/// [`fegetexceptflag`], raising or clearing each.
///
/// # Safety
///
/// `flagp` must be valid for reading a `fexcept_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fesetexceptflag(flagp: *const fexcept_t, excepts: c_int) -> c_int {
    // SAFETY: the caller vouches for `flagp`.
    let flags = c_int::from(unsafe { flagp.read() });
    let _ = feclearexcept(!flags & excepts);
    let _ = feraiseexcept(flags & excepts);
    0
}

/// Sets the rounding mode, one of `FE_TONEAREST`, `FE_DOWNWARD`, `FE_UPWARD`
/// and `FE_TOWARDZERO`. Returns 0, or -1 for anything else, which changes
/// nothing.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fesetround(round: c_int) -> c_int {
    match round {
        FE_TONEAREST | FE_DOWNWARD | FE_UPWARD | FE_TOWARDZERO => {
            arch::set_round(round);
            0
        }
        _ => -1,
    }
}

/// Saves the environment in `*envp`, then clears every exception flag.
///
/// Every exception is already masked unless a program unmasked it, and this
/// C library offers no way to, so there is no non-stop mode to install.
///
/// # Safety
///
/// `envp` must be valid for writing a `fenv_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn feholdexcept(envp: *mut fenv_t) -> c_int {
    // SAFETY: the caller's contract is `fegetenv`'s.
    let _ = unsafe { fegetenv(envp) };
    let _ = feclearexcept(FE_ALL_EXCEPT);
    0
}

/// Installs the environment at `envp`, then raises again the exceptions that
/// were raised before it.
///
/// # Safety
///
/// As [`fesetenv`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn feupdateenv(envp: *const fenv_t) -> c_int {
    let raised = fetestexcept(FE_ALL_EXCEPT);
    // SAFETY: the caller's contract is `fesetenv`'s.
    let _ = unsafe { fesetenv(envp) };
    let _ = feraiseexcept(raised);
    0
}

/// The rounding mode as `<float.h>`'s `FLT_ROUNDS` numbers it: 0 toward zero,
/// 1 to nearest, 2 upward, 3 downward.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __flt_rounds() -> c_int {
    match fegetround() {
        FE_TOWARDZERO => 0,
        FE_TONEAREST => 1,
        FE_UPWARD => 2,
        FE_DOWNWARD => 3,
        _ => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::hint::black_box;

    /// Starts and ends each test in the default environment.
    struct Default;

    impl Default {
        fn new() -> Self {
            // SAFETY: `FE_DFL_ENV` is the default environment's name.
            let _ = unsafe { fesetenv(FE_DFL_ENV) };
            Self
        }
    }

    impl Drop for Default {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            let _ = unsafe { fesetenv(FE_DFL_ENV) };
        }
    }

    #[test]
    fn each_flag_is_raised_tested_and_cleared_alone() {
        let _default = Default::new();
        for flag in [
            FE_INVALID,
            FE_DIVBYZERO,
            FE_OVERFLOW,
            FE_UNDERFLOW,
            FE_INEXACT,
        ] {
            assert_eq!(fetestexcept(FE_ALL_EXCEPT), 0);
            assert_eq!(feraiseexcept(flag), 0);
            assert_eq!(fetestexcept(FE_ALL_EXCEPT), flag);
            assert_eq!(fetestexcept(FE_ALL_EXCEPT & !flag), 0);
            assert_eq!(feclearexcept(flag), 0);
            assert_eq!(fetestexcept(FE_ALL_EXCEPT), 0);
        }
    }

    #[test]
    fn arithmetic_raises_the_flags_it_should() {
        let _default = Default::new();
        let _ = black_box(black_box(1.0f64) / 0.0);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_DIVBYZERO);
        let _ = feclearexcept(FE_ALL_EXCEPT);
        let _ = black_box(black_box(1e300f64) * 1e300);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_OVERFLOW | FE_INEXACT);
        let _ = feclearexcept(FE_OVERFLOW);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_INEXACT);
    }

    #[test]
    fn the_rounding_mode_reaches_sse_and_the_x87() {
        let _default = Default::new();
        for (mode, third) in [
            (FE_TONEAREST, 0x3fd5_5555_5555_5555),
            (FE_DOWNWARD, 0x3fd5_5555_5555_5555),
            (FE_UPWARD, 0x3fd5_5555_5555_5556),
            (FE_TOWARDZERO, 0x3fd5_5555_5555_5555),
        ] {
            assert_eq!(fesetround(mode), 0);
            assert_eq!(fegetround(), mode);
            assert_eq!((black_box(1.0f64) / 3.0).to_bits(), third);
            assert_eq!(c_int::from(arch::x87_control()) & 0xc00, mode);
            let _ = fesetround(FE_TONEAREST);
        }
        assert_eq!(fesetround(FE_DOWNWARD), 0);
        assert_eq!((black_box(-1.0f64) / 3.0).to_bits(), 0xbfd5_5555_5555_5556);
        assert_eq!(fesetround(1), -1);
        assert_eq!(fesetround(FE_UPWARD | 0x1000), -1);
        assert_eq!(fegetround(), FE_DOWNWARD);
    }

    #[test]
    fn flt_rounds_numbers_the_modes_as_float_h_does() {
        let _default = Default::new();
        assert_eq!(__flt_rounds(), 1);
        let _ = fesetround(FE_TOWARDZERO);
        assert_eq!(__flt_rounds(), 0);
        let _ = fesetround(FE_UPWARD);
        assert_eq!(__flt_rounds(), 2);
        let _ = fesetround(FE_DOWNWARD);
        assert_eq!(__flt_rounds(), 3);
    }

    #[test]
    fn x87_flags_are_tested_cleared_and_kept_apart_from_the_mask() {
        let _default = Default::new();
        arch::x87_divide_by_zero();
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_DIVBYZERO);
        let _ = feraiseexcept(FE_INEXACT);
        // Clearing another flag keeps the x87's.
        let _ = feclearexcept(FE_INEXACT);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_DIVBYZERO);
        let _ = feclearexcept(FE_DIVBYZERO);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), 0);
        // Clearing one of two x87 flags keeps the other.
        arch::x87_divide_by_zero();
        arch::x87_invalid();
        let _ = feclearexcept(FE_INVALID);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_DIVBYZERO);
    }

    #[test]
    fn exception_flags_are_saved_and_restored() {
        let _default = Default::new();
        let _ = feraiseexcept(FE_OVERFLOW | FE_INEXACT);
        let mut saved: fexcept_t = 0;
        // SAFETY: `saved` is a local `fexcept_t`.
        assert_eq!(unsafe { fegetexceptflag(&raw mut saved, FE_ALL_EXCEPT) }, 0);
        let _ = feclearexcept(FE_ALL_EXCEPT);
        let _ = feraiseexcept(FE_INVALID);
        // SAFETY: as above.
        let _ = unsafe { fesetexceptflag(&raw const saved, FE_OVERFLOW | FE_INVALID) };
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_OVERFLOW);
    }

    #[test]
    fn environments_are_saved_restored_held_and_updated() {
        let _default = Default::new();
        let _ = fesetround(FE_UPWARD);
        let _ = feraiseexcept(FE_UNDERFLOW);
        let mut env = arch::zeroed_env();
        // SAFETY: `env` is a local `fenv_t`.
        assert_eq!(unsafe { fegetenv(&raw mut env) }, 0);
        assert_eq!(env.__mxcsr & 0x3f, 0x10);
        assert_eq!(env.__control_word & 0xc00, 0x800);

        // SAFETY: the default environment's name.
        assert_eq!(unsafe { fesetenv(FE_DFL_ENV) }, 0);
        assert_eq!(fegetround(), FE_TONEAREST);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), 0);
        assert_eq!(arch::x87_control(), 0x37f);

        // SAFETY: `env` was filled in by `fegetenv`.
        assert_eq!(unsafe { fesetenv(&raw const env) }, 0);
        assert_eq!(fegetround(), FE_UPWARD);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_UNDERFLOW);
        assert_eq!(arch::x87_control() & 0xc00, 0x800);

        let mut held = arch::zeroed_env();
        // SAFETY: `held` is a local `fenv_t`.
        assert_eq!(unsafe { feholdexcept(&raw mut held) }, 0);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), 0);
        let _ = feraiseexcept(FE_INEXACT);
        // SAFETY: `held` was filled in by `feholdexcept`.
        assert_eq!(unsafe { feupdateenv(&raw const held) }, 0);
        assert_eq!(fetestexcept(FE_ALL_EXCEPT), FE_UNDERFLOW | FE_INEXACT);
        assert_eq!(fegetround(), FE_UPWARD);
    }
}
