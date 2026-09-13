//! `stdlib.h`: parsing floating-point numbers, with `strtof`, `strtod`,
//! `strtold` and `atof`.
//!
//! This module reads C's grammar and decides `errno`; [`crate::float`] does
//! the correctly rounded conversion. The grammar, after optional white space
//! and a sign, is one of:
//!
//! * `inf` or `infinity`, in any case;
//! * `nan`, optionally followed by `(` letters, digits and `_` `)`;
//! * `0x` or `0X`, hexadecimal digits with an optional point, and an optional
//!   binary exponent `p` or `P`;
//! * decimal digits with an optional point, and an optional exponent `e` or
//!   `E`.
//!
//! The longest prefix of that form is the subject, and `endptr` is set just
//! past it: `"1e"` stops after the `1`, `"0x"` and `"0x.p1"` after the `0`,
//! `"0x1p"` after the `1`, and `"nan("` with no closing parenthesis after the
//! `nan`.
//!
//! The choices C leaves open are musl's (`src/internal/floatscan.c`, MIT),
//! with one exception:
//!
//! * No subject at all sets `EINVAL`, returns zero and consumes nothing.
//! * Overflow returns an infinity and sets `ERANGE`.
//! * A decimal input sets `ERANGE` on underflow when the rounded result is
//!   zero or subnormal and not exact. A hexadecimal input sets it only when
//!   the result is zero, as musl's `hexfloat` does.
//! * A NaN's `n-char-sequence` is read and ignored; the result is the default
//!   quiet NaN.
//! * Unlike musl, `-nan` has its sign bit set, as C's "the result is
//!   negated" asks and as glibc does.

use core::ffi::{c_char, c_double, c_float, c_int};
use core::ptr::null_mut;

use crate::errno;
use crate::float::{self, BINARY32, BINARY64, Class, Format, Rounded};
use crate::scan::{CText, Input, set_end, skip_space};

/// A parsed number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// The bit pattern in the requested format.
    pub(crate) bits: u128,
    /// How many bytes the subject and the space before it take; zero when
    /// nothing converted.
    pub(crate) end: usize,
    /// The `errno` to set, or zero.
    pub(crate) error: c_int,
}

/// Whether `word`, in lowercase, is at `i` in any case.
fn word_at(text: &(impl Input + ?Sized), i: usize, word: &[u8]) -> bool {
    word.iter()
        .enumerate()
        .all(|(k, &letter)| text.at(i + k) | 0x20 == letter)
}

/// Parses a number in `format` from the start of `text`.
pub(crate) fn parse(text: &(impl Input + ?Sized), format: &Format) -> Parsed {
    let mut i = skip_space(text, 0);
    let negative = text.at(i) == b'-';
    if negative || text.at(i) == b'+' {
        i += 1;
    }

    if word_at(text, i, b"inf") {
        let end = if word_at(text, i, b"infinity") {
            i + 8
        } else {
            i + 3
        };
        return Parsed {
            bits: format.infinity(negative),
            end,
            error: 0,
        };
    }
    if word_at(text, i, b"nan") {
        return Parsed {
            bits: format.nan(negative),
            end: nan_end(text, i + 3),
            error: 0,
        };
    }
    if text.at(i) == b'0' && text.at(i + 1) | 0x20 == b'x' {
        return hexadecimal(text, format, negative, i);
    }
    decimal(text, format, negative, i)
}

/// Where a NaN whose `nan` ends at `i` ends, with its optional parenthesised
/// sequence.
fn nan_end(text: &(impl Input + ?Sized), i: usize) -> usize {
    if text.at(i) != b'(' {
        return i;
    }
    let mut k = i + 1;
    while text.at(k).is_ascii_alphanumeric() || text.at(k) == b'_' {
        k += 1;
    }
    if text.at(k) == b')' { k + 1 } else { i }
}

/// Reads an exponent's optional sign and digits at `i`: its value, saturated
/// far beyond any format's range, and where it ends. `None` when there are no
/// digits, so the exponent is not part of the subject.
fn exponent_at(text: &(impl Input + ?Sized), mut i: usize) -> Option<(i64, usize)> {
    /// Beyond every format's range, with room to add digit counts.
    const CAP: i64 = 1 << 50;
    let negative = text.at(i) == b'-';
    if negative || text.at(i) == b'+' {
        i += 1;
    }
    if !text.at(i).is_ascii_digit() {
        return None;
    }
    let mut value = 0_i64;
    while text.at(i).is_ascii_digit() {
        value = (value * 10 + i64::from(text.at(i) - b'0')).min(CAP);
        i += 1;
    }
    Some((if negative { -value } else { value }, i))
}

/// The decimal digits between two indices of a text, skipping the point.
#[derive(Debug)]
struct Digits<'a, T: Input + ?Sized> {
    text: &'a T,
    at: usize,
    end: usize,
}

impl<T: Input + ?Sized> Iterator for Digits<'_, T> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        while self.at < self.end {
            let c = self.text.at(self.at);
            self.at += 1;
            if c.is_ascii_digit() {
                return Some(c - b'0');
            }
        }
        None
    }
}

/// Parses a decimal number whose digits or point start at `start`.
fn decimal(text: &(impl Input + ?Sized), format: &Format, negative: bool, start: usize) -> Parsed {
    let mut i = start;
    // Digits are numbered from zero, leading zeros included.
    let mut digits = 0_u64;
    let mut before_point = 0_u64;
    let mut first_nonzero = None;
    let mut last_nonzero = 0_u64;
    let mut seen_point = false;
    loop {
        let c = text.at(i);
        if c.is_ascii_digit() {
            if c != b'0' {
                let _ = first_nonzero.get_or_insert(digits);
                last_nonzero = digits;
            }
            digits += 1;
            if !seen_point {
                before_point += 1;
            }
        } else if c == b'.' && !seen_point {
            seen_point = true;
        } else {
            break;
        }
        i += 1;
    }
    if digits == 0 {
        return Parsed {
            bits: format.zero(false),
            end: 0,
            error: errno::EINVAL,
        };
    }
    let digits_end = i;
    let mut exponent = 0;
    if text.at(i) | 0x20 == b'e'
        && let Some((value, after)) = exponent_at(text, i + 1)
    {
        exponent = value;
        i = after;
    }

    let Some(first) = first_nonzero else {
        return Parsed {
            bits: format.zero(negative),
            end: i,
            error: 0,
        };
    };
    // Digit `k` stands for `10^(before_point - 1 - k)`, so the last nonzero
    // digit fixes the power of ten of the integer they make. Counts of bytes
    // in memory fit an `i64`.
    let exponent = exponent.saturating_add(before_point as i64 - 1 - last_nonzero as i64);
    let significant = Digits {
        text,
        at: start,
        end: digits_end,
    }
    .skip(first as usize);
    let rounded = float::decimal(
        format,
        negative,
        significant,
        last_nonzero - first + 1,
        exponent,
    );
    let error = match rounded {
        Rounded {
            class: Class::Infinite,
            ..
        }
        | Rounded {
            class: Class::Zero | Class::Subnormal,
            inexact: true,
            ..
        } => errno::ERANGE,
        _ => 0,
    };
    Parsed {
        bits: rounded.bits,
        end: i,
        error,
    }
}

/// Parses a hexadecimal number whose `0x` is at `start`.
fn hexadecimal(
    text: &(impl Input + ?Sized),
    format: &Format,
    negative: bool,
    start: usize,
) -> Parsed {
    let mut i = start + 2;
    let mut significand = 0_u128;
    // The value is `significand * 2^exponent`, plus a little when `sticky`.
    let mut exponent = 0_i64;
    let mut sticky = false;
    let mut any = false;
    let mut seen_point = false;
    loop {
        let c = text.at(i);
        if c == b'.' && !seen_point {
            seen_point = true;
        } else if let Some(d) = crate::scan::digit(c).filter(|&d| d < 16) {
            any = true;
            // Keep digits while they fit, which is far more bits than any
            // format has, and fold the rest into `sticky`.
            if significand >> 124 == 0 {
                significand = significand << 4 | u128::from(d);
                if seen_point {
                    exponent -= 4;
                }
            } else {
                sticky |= d != 0;
                if !seen_point {
                    exponent += 4;
                }
            }
        } else {
            break;
        }
        i += 1;
    }
    if !any {
        // The `0` alone is the subject.
        return Parsed {
            bits: format.zero(negative),
            end: start + 1,
            error: 0,
        };
    }
    if text.at(i) | 0x20 == b'p'
        && let Some((value, after)) = exponent_at(text, i + 1)
    {
        exponent = exponent.saturating_add(value);
        i = after;
    }
    if significand == 0 {
        return Parsed {
            bits: format.zero(negative),
            end: i,
            error: 0,
        };
    }
    let rounded = float::round(format, negative, significand, exponent, sticky);
    let error = match rounded.class {
        Class::Infinite | Class::Zero => errno::ERANGE,
        Class::Subnormal | Class::Normal => 0,
    };
    Parsed {
        bits: rounded.bits,
        end: i,
        error,
    }
}

/// Parses the C string `s` in `format`, sets `*endptr` and `errno`, and
/// returns the bit pattern.
///
/// # Safety
///
/// `s` must be a NUL-terminated string, and `endptr` null or valid to write.
unsafe fn convert(s: *const c_char, endptr: *mut *mut c_char, format: &Format) -> u128 {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { CText::new(s) };
    let parsed = parse(&text, format);
    // SAFETY: the caller vouches for `endptr`, and the parse stopped at or
    // before the NUL.
    unsafe { set_end(endptr, s, parsed.end) };
    if parsed.error != 0 {
        errno::set(parsed.error);
    }
    parsed.bits
}

/// Parses a `double`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string, and `endptr` null or valid to write a
/// pointer to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strtod(s: *const c_char, endptr: *mut *mut c_char) -> c_double {
    // SAFETY: the caller's contract is `convert`'s.
    let bits = unsafe { convert(s, endptr, &BINARY64) };
    // A `double`'s pattern is in the low 64 bits.
    f64::from_bits(bits as u64)
}

/// Parses a `float`.
///
/// # Safety
///
/// As [`strtod`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strtof(s: *const c_char, endptr: *mut *mut c_char) -> c_float {
    // SAFETY: the caller's contract is `convert`'s.
    let bits = unsafe { convert(s, endptr, &BINARY32) };
    // A `float`'s pattern is in the low 32 bits.
    f32::from_bits(bits as u32)
}

/// Parses a `double`, as `strtod(s, NULL)`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn atof(s: *const c_char) -> c_double {
    // SAFETY: a null `endptr` is allowed.
    unsafe { strtod(s, null_mut()) }
}

/// Parses an x87 `long double` and writes its ten bytes, little-endian, to
/// `out`. [`strtold`] calls it and loads the result.
///
/// # Safety
///
/// As [`strtod`], and `out` must be valid to write ten bytes.
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn strtold_x87(s: *const c_char, endptr: *mut *mut c_char, out: *mut u8) {
    // SAFETY: the caller's contract is `convert`'s.
    let bits = unsafe { convert(s, endptr, &float::X87_EXTENDED) };
    let bytes = bits.to_le_bytes();
    let mut k = 0;
    while let Some(&byte) = bytes.get(k).filter(|_| k < 10) {
        // SAFETY: the caller vouches for ten bytes at `out`.
        unsafe { out.wrapping_add(k).write(byte) };
        k += 1;
    }
}

/// Parses a `long double`: on x86-64, the x87 80-bit extended type.
///
/// Rust has no such type, and the SysV ABI returns it on the x87 stack in
/// `st(0)`, which a Rust function cannot. So this is a shim: it makes room
/// on the stack for the ten bytes, has [`strtold_x87`] compute them, and loads
/// them with `fld`. C declares it `long double strtold(const char *, char
/// **)`; the Rust signature shows no return value because the value is not
/// in any register Rust knows.
///
/// # Safety
///
/// As [`strtod`].
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strtold(s: *const c_char, endptr: *mut *mut c_char) {
    // On entry the stack is 8 below a multiple of 16. Taking 24 aligns it for
    // the call and leaves 16 bytes for the result. `rdi` and `rsi` pass
    // through unchanged.
    core::arch::naked_asm!(
        "sub rsp, 24",
        "mov rdx, rsp",
        "call {convert}",
        "fld tbyte ptr [rsp]",
        "add rsp, 24",
        "ret",
        convert = sym strtold_x87,
    )
}

#[cfg(test)]
mod tests;
