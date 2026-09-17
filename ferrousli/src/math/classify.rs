//! Classifying a `float`, a `double` or a `long double`.
//!
//! `math.h`'s `fpclassify` and `signbit` macros call `__fpclassify`,
//! `__signbit` and their `f` and `l` forms for the types they do not
//! open-code. For `long double`, `isinf`, `isnan`, `isnormal` and `isfinite`
//! call `__fpclassifyl` too, and through `isnan` so do `isunordered` and the
//! comparison macros `isless` to `isgreaterequal`.
//!
//! Ported from musl 1.2.5's `__fpclassify.c`, `__fpclassifyf.c`,
//! `__fpclassifyl.c`, `__signbit.c`, `__signbitf.c` and `__signbitl.c` (MIT;
//! see [`crate::math`] for the notice).
//!
//! # `long double`
//!
//! On x86-64 a `long double` is the x87's 80-bit extended format: a 64-bit
//! significand with an explicit integer bit, then the sign and a 15-bit
//! exponent, in 16 bytes of which the last six are padding. Rust has no such
//! type, and the SysV ABI passes one in memory, in the caller's stack frame,
//! which no Rust parameter type matches. So `__fpclassifyl` and `__signbitl`
//! are shims, as `strtold` is: each hands the address of the argument's stack
//! slot to a Rust function that reads its ten bytes.
//!
//! The x87 refuses as invalid operands the encodings with a nonzero exponent
//! and the integer bit clear: unnormals, pseudo-infinities and pseudo-NaNs.
//! As in musl, `__fpclassifyl` calls them NaNs. A pseudo-denormal, with a zero
//! exponent and the integer bit set, it calls normal, as musl does.

use core::ffi::c_int;

/// `FP_NAN`: not a number.
pub const FP_NAN: c_int = 0;
/// `FP_INFINITE`: an infinity.
pub const FP_INFINITE: c_int = 1;
/// `FP_ZERO`: a zero of either sign.
pub const FP_ZERO: c_int = 2;
/// `FP_SUBNORMAL`: nonzero, and smaller than the smallest normal number.
pub const FP_SUBNORMAL: c_int = 3;
/// `FP_NORMAL`: any other finite number.
pub const FP_NORMAL: c_int = 4;

/// Which of the five classes `x` is in.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __fpclassify(x: f64) -> c_int {
    let bits = x.to_bits();
    match bits >> 52 & 0x7ff {
        0 if bits << 1 == 0 => FP_ZERO,
        0 => FP_SUBNORMAL,
        0x7ff if bits << 12 == 0 => FP_INFINITE,
        0x7ff => FP_NAN,
        _ => FP_NORMAL,
    }
}

/// [`__fpclassify`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __fpclassifyf(x: f32) -> c_int {
    let bits = x.to_bits();
    match bits >> 23 & 0xff {
        0 if bits << 1 == 0 => FP_ZERO,
        0 => FP_SUBNORMAL,
        0xff if bits << 9 == 0 => FP_INFINITE,
        0xff => FP_NAN,
        _ => FP_NORMAL,
    }
}

/// 1 if `x`'s sign bit is set, which it is for -0 and for some NaNs, and 0
/// otherwise.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __signbit(x: f64) -> c_int {
    (x.to_bits() >> 63) as c_int
}

/// [`__signbit`] for `float`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __signbitf(x: f32) -> c_int {
    (x.to_bits() >> 31) as c_int
}

/// The class of the x87 `long double` with this significand, whose bit 63 is
/// the integer bit, and this sign and biased exponent.
pub(crate) const fn classify_x87(mantissa: u64, sign_exponent: u16) -> c_int {
    let exponent = sign_exponent & 0x7fff;
    let integer_bit = mantissa >> 63 != 0;
    if exponent == 0 && !integer_bit {
        return if mantissa != 0 { FP_SUBNORMAL } else { FP_ZERO };
    }
    if exponent == 0x7fff {
        // The x87 has one encoding of each infinity, with the integer bit
        // set. With it clear, this is invalid, so a NaN.
        if !integer_bit {
            return FP_NAN;
        }
        return if mantissa << 1 != 0 {
            FP_NAN
        } else {
            FP_INFINITE
        };
    }
    // An unnormal.
    if !integer_bit {
        return FP_NAN;
    }
    FP_NORMAL
}

/// The significand and the sign-and-exponent word of the x87 `long double`
/// whose ten bytes are at `x`.
///
/// # Safety
///
/// `x` must be valid to read ten bytes.
#[cfg(target_arch = "x86_64")]
unsafe fn read_x87(x: *const u8) -> (u64, u16) {
    // SAFETY: the caller vouches for ten bytes, and an array of bytes has no
    // alignment.
    let [m0, m1, m2, m3, m4, m5, m6, m7, s0, s1] = unsafe { x.cast::<[u8; 10]>().read() };
    (
        u64::from_le_bytes([m0, m1, m2, m3, m4, m5, m6, m7]),
        u16::from_le_bytes([s0, s1]),
    )
}

/// [`__fpclassifyl`]'s work, on the `long double` at `x`.
///
/// # Safety
///
/// As [`read_x87`].
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn fpclassifyl_x87(x: *const u8) -> c_int {
    // SAFETY: the caller's contract is `read_x87`'s.
    let (mantissa, sign_exponent) = unsafe { read_x87(x) };
    classify_x87(mantissa, sign_exponent)
}

/// [`__signbitl`]'s work, on the `long double` at `x`.
///
/// # Safety
///
/// As [`read_x87`].
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn signbitl_x87(x: *const u8) -> c_int {
    // SAFETY: the caller's contract is `read_x87`'s.
    let (_, sign_exponent) = unsafe { read_x87(x) };
    c_int::from(sign_exponent >> 15)
}

/// Which of the five classes a `long double` is in.
///
/// C declares it `int __fpclassifyl(long double)`. The argument is in the
/// caller's stack frame, where no Rust parameter can name it, so the Rust
/// signature shows none. On entry the return address is at `rsp` and the
/// argument's 16 bytes above it. The shim passes their address and jumps,
/// rather than calls, so the stack is as the caller left it and the Rust
/// function returns straight to the caller.
///
/// # Safety
///
/// The caller must pass one `long double`, as C does.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __fpclassifyl() -> c_int {
    core::arch::naked_asm!(
        "lea rdi, [rsp + 8]",
        "jmp {classify}",
        classify = sym fpclassifyl_x87,
    )
}

/// 1 if a `long double`'s sign bit is set, and 0 otherwise.
///
/// C declares it `int __signbitl(long double)`; see [`__fpclassifyl`] for why
/// the Rust signature shows no argument.
///
/// # Safety
///
/// The caller must pass one `long double`, as C does.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __signbitl() -> c_int {
    core::arch::naked_asm!(
        "lea rdi, [rsp + 8]",
        "jmp {sign}",
        sign = sym signbitl_x87,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doubles in each class, adapted from libc-test's `fpclassify.c` (MIT).
    const DOUBLES: &[(f64, c_int)] = &[
        (0.0, FP_ZERO),
        (-0.0, FP_ZERO),
        (f64::MIN_POSITIVE, FP_NORMAL),
        (f64::MIN_POSITIVE / 2.0, FP_SUBNORMAL),
        (-f64::from_bits(1), FP_SUBNORMAL),
        (f64::from_bits(0x000f_ffff_ffff_ffff), FP_SUBNORMAL),
        (1.0, FP_NORMAL),
        (-f64::MAX, FP_NORMAL),
        (f64::INFINITY, FP_INFINITE),
        (f64::NEG_INFINITY, FP_INFINITE),
        (f64::NAN, FP_NAN),
        (-f64::NAN, FP_NAN),
        (f64::from_bits(0x7ff0_0000_0000_0001), FP_NAN),
    ];

    const FLOATS: &[(f32, c_int)] = &[
        (0.0, FP_ZERO),
        (-0.0, FP_ZERO),
        (f32::MIN_POSITIVE, FP_NORMAL),
        (f32::MIN_POSITIVE / 2.0, FP_SUBNORMAL),
        (-f32::from_bits(1), FP_SUBNORMAL),
        (1.0, FP_NORMAL),
        (f32::MAX, FP_NORMAL),
        (f32::INFINITY, FP_INFINITE),
        (f32::NEG_INFINITY, FP_INFINITE),
        (f32::NAN, FP_NAN),
        (f32::from_bits(0xff80_0001), FP_NAN),
    ];

    /// x87 `long double`s as significand, sign and exponent, and class.
    const LONG_DOUBLES: &[(u64, u16, c_int)] = &[
        (0, 0, FP_ZERO),
        (0, 0x8000, FP_ZERO),
        (1, 0, FP_SUBNORMAL),
        (0x7fff_ffff_ffff_ffff, 0x8000, FP_SUBNORMAL),
        // A pseudo-denormal.
        (0x8000_0000_0000_0000, 0, FP_NORMAL),
        // LDBL_MIN, -1 and LDBL_MAX.
        (0x8000_0000_0000_0000, 1, FP_NORMAL),
        (0x8000_0000_0000_0000, 0xbfff, FP_NORMAL),
        (u64::MAX, 0x7ffe, FP_NORMAL),
        // An unnormal, a pseudo-infinity and a pseudo-NaN.
        (0x7fff_ffff_ffff_ffff, 0x3fff, FP_NAN),
        (0, 0x7fff, FP_NAN),
        (0x4000_0000_0000_0000, 0xffff, FP_NAN),
        (0x8000_0000_0000_0000, 0x7fff, FP_INFINITE),
        (0x8000_0000_0000_0000, 0xffff, FP_INFINITE),
        // A quiet NaN and a signalling one.
        (0xc000_0000_0000_0000, 0x7fff, FP_NAN),
        (0x8000_0000_0000_0001, 0xffff, FP_NAN),
    ];

    /// The 16 bytes of an x87 `long double`.
    fn x87(mantissa: u64, sign_exponent: u16) -> [u8; 16] {
        let [m0, m1, m2, m3, m4, m5, m6, m7] = mantissa.to_le_bytes();
        let [s0, s1] = sign_exponent.to_le_bytes();
        [m0, m1, m2, m3, m4, m5, m6, m7, s0, s1, 0, 0, 0, 0, 0, 0]
    }

    /// Calls `function`, which C declares as taking one `long double`, with the
    /// 16 bytes at `bytes` in the stack slot the psABI passes it in.
    ///
    /// # Safety
    ///
    /// `function` must take one `long double` and return an `int`, and
    /// `bytes` must be valid to read.
    #[unsafe(naked)]
    unsafe extern "C" fn call_long_double(
        function: unsafe extern "C" fn() -> c_int,
        bytes: *const [u8; 16],
    ) -> c_int {
        // On entry the stack is 8 below a multiple of 16. Taking 24 aligns it
        // for the call, with the argument at its bottom.
        core::arch::naked_asm!(
            "sub rsp, 24",
            "mov rax, qword ptr [rsi]",
            "mov qword ptr [rsp], rax",
            "mov rax, qword ptr [rsi + 8]",
            "mov qword ptr [rsp + 8], rax",
            "call rdi",
            "add rsp, 24",
            "ret",
        )
    }

    #[test]
    fn every_class_of_double() {
        for &(x, class) in DOUBLES {
            assert_eq!(__fpclassify(x), class, "{x:e}");
            assert_eq!(__signbit(x), (x.to_bits() >> 63) as c_int, "{x:e}");
        }
    }

    #[test]
    fn every_class_of_float() {
        for &(x, class) in FLOATS {
            assert_eq!(__fpclassifyf(x), class, "{x:e}");
            assert_eq!(__signbitf(x), (x.to_bits() >> 31) as c_int, "{x:e}");
        }
    }

    #[test]
    fn every_class_and_encoding_of_long_double() {
        for &(mantissa, sign_exponent, class) in LONG_DOUBLES {
            let bytes = x87(mantissa, sign_exponent);
            let at = format!("{mantissa:#x} {sign_exponent:#x}");
            assert_eq!(classify_x87(mantissa, sign_exponent), class, "{at}");
            // SAFETY: `__fpclassifyl` takes one `long double`, and `bytes` is a
            // local.
            let got = unsafe { call_long_double(__fpclassifyl, &raw const bytes) };
            assert_eq!(got, class, "{at}");
            // SAFETY: as above, for `__signbitl`.
            let sign = unsafe { call_long_double(__signbitl, &raw const bytes) };
            assert_eq!(sign, c_int::from(sign_exponent >> 15), "{at}");
        }
    }
}
