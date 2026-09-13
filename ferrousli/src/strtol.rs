//! `stdlib.h` and `inttypes.h`: parsing integers, with `strtol` and its
//! relatives, and `atoi`, `atol` and `atoll`.
//!
//! One scanner reads the digits for every width, and each function then fits
//! the magnitude to its type. The rules, including the ones C leaves to the
//! implementation, are musl's (`src/internal/intscan.c`, MIT):
//!
//! * A base other than 0 or 2 to 36 sets `EINVAL`, returns 0 and converts
//!   nothing.
//! * A subject with no digits also sets `EINVAL`. POSIX allows this.
//! * `"0x"` not followed by a hexadecimal digit converts the `0` alone, and
//!   leaves `endptr` at the `x`.
//! * An out-of-range value saturates and sets `ERANGE`. For the unsigned
//!   functions a minus sign negates the magnitude modulo the type's range,
//!   but a magnitude beyond the range still saturates to the maximum.
//!
//! glibc 2.38's headers redirect these functions to `__isoc23_strtol` and the
//! rest when compiling C23 or with `_GNU_SOURCE`. C23 adds the `0b` prefix in
//! bases 0 and 2, and those names accept it. The plain names keep C17's
//! grammar, which musl's headers promise.

use core::ffi::{c_char, c_int, c_long, c_longlong, c_ulong, c_ulonglong};

use crate::errno;
use crate::scan::{CText, Input, digit, set_end, skip_space};

/// `intmax_t`: `long` on x86-64, as `bits/alltypes.h` defines it.
type IntMax = c_long;
/// `uintmax_t`.
type UintMax = c_ulong;

/// The digits of a subject sequence, before they are fitted to a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Scanned {
    /// The magnitude, or the low bits of it when it overflowed.
    pub(crate) magnitude: u64,
    /// Whether a minus sign came before the digits.
    pub(crate) negative: bool,
    /// Whether the magnitude does not fit in 64 bits.
    pub(crate) overflow: bool,
    /// How many bytes the subject sequence and the space before it take.
    pub(crate) end: usize,
}

/// Reads an integer in `base` from the start of `text`. `None` means nothing
/// converts, either because the base is invalid or because there are no
/// digits. `binary_prefix` accepts C23's `0b`.
pub(crate) fn scan(
    text: &(impl Input + ?Sized),
    base: c_int,
    binary_prefix: bool,
) -> Option<Scanned> {
    let Ok(mut base) = u32::try_from(base) else {
        return None;
    };
    if base == 1 || base > 36 {
        return None;
    }
    let mut i = skip_space(text, 0);
    let sign = text.at(i);
    let negative = sign == b'-';
    if sign == b'-' || sign == b'+' {
        i += 1;
    }

    let zero_then = |letter: u8| text.at(i) == b'0' && text.at(i + 1) | 0x20 == letter;
    let hex = (base == 0 || base == 16) && zero_then(b'x');
    let binary = binary_prefix && (base == 0 || base == 2) && zero_then(b'b');
    if hex || binary {
        let radix = if hex { 16 } else { 2 };
        if digit(text.at(i + 2)).is_some_and(|d| d < radix) {
            base = radix;
            i += 2;
        } else {
            // The `0` is the whole subject sequence.
            return Some(Scanned {
                magnitude: 0,
                negative,
                overflow: false,
                end: i + 1,
            });
        }
    } else if base == 0 {
        base = if text.at(i) == b'0' { 8 } else { 10 };
    }

    if !digit(text.at(i)).is_some_and(|d| d < base) {
        return None;
    }
    let mut magnitude = 0_u64;
    let mut overflow = false;
    while let Some(d) = digit(text.at(i)).filter(|&d| d < base) {
        match magnitude
            .checked_mul(u64::from(base))
            .and_then(|m| m.checked_add(u64::from(d)))
        {
            Some(m) => magnitude = m,
            None => overflow = true,
        }
        i += 1;
    }
    Some(Scanned {
        magnitude,
        negative,
        overflow,
        end: i,
    })
}

/// Fits a scan to a signed type whose maximum is `max`: the value as two's
/// complement bits, and whether it saturated.
pub(crate) const fn to_signed(scanned: Scanned, max: u64) -> (u64, bool) {
    let limit = if scanned.negative { max + 1 } else { max };
    if scanned.overflow || scanned.magnitude > limit {
        (
            if scanned.negative {
                limit.wrapping_neg()
            } else {
                max
            },
            true,
        )
    } else if scanned.negative {
        (scanned.magnitude.wrapping_neg(), false)
    } else {
        (scanned.magnitude, false)
    }
}

/// Fits a scan to an unsigned type whose maximum is `max`.
pub(crate) const fn to_unsigned(scanned: Scanned, max: u64) -> (u64, bool) {
    if scanned.overflow || scanned.magnitude > max {
        (max, true)
    } else if scanned.negative {
        (scanned.magnitude.wrapping_neg() & max, false)
    } else {
        (scanned.magnitude, false)
    }
}

/// Which way a conversion fits its result.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// A signed type with this maximum.
    Signed(u64),
    /// An unsigned type with this maximum.
    Unsigned(u64),
}

/// The body every `strto*` integer function shares.
///
/// # Safety
///
/// `s` must be a NUL-terminated string, and `endptr` null or valid to write.
unsafe fn convert(
    s: *const c_char,
    endptr: *mut *mut c_char,
    base: c_int,
    binary_prefix: bool,
    kind: Kind,
) -> u64 {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { CText::new(s) };
    let Some(scanned) = scan(&text, base, binary_prefix) else {
        errno::set(errno::EINVAL);
        // SAFETY: the caller vouches for `endptr`; nothing is consumed.
        unsafe { set_end(endptr, s, 0) };
        return 0;
    };
    // SAFETY: the caller vouches for `endptr`, and the scan stopped at or
    // before the NUL.
    unsafe { set_end(endptr, s, scanned.end) };
    let (value, saturated) = match kind {
        Kind::Signed(max) => to_signed(scanned, max),
        Kind::Unsigned(max) => to_unsigned(scanned, max),
    };
    if saturated {
        errno::set(errno::ERANGE);
    }
    value
}

/// Defines one exported conversion.
macro_rules! strto {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $kind:ident, $binary:expr) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// `s` must be a NUL-terminated string, and `endptr` null or valid to
        /// write a pointer to.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name(s: *const c_char, endptr: *mut *mut c_char, base: c_int) -> $ty {
            // SAFETY: the caller's contract is `convert`'s.
            let value = unsafe { convert(s, endptr, base, $binary, Kind::$kind(<$ty>::MAX as u64)) };
            // The value fits: it was saturated to the type's range, and a
            // negative value is its two's complement bits.
            value as $ty
        }
    };
}

strto!(
    /// Parses a `long` in `base`, as C17 specifies.
    strtol, c_long, Signed, false
);
strto!(
    /// Parses an `unsigned long` in `base`, as C17 specifies.
    strtoul, c_ulong, Unsigned, false
);
strto!(
    /// Parses a `long long` in `base`, as C17 specifies.
    strtoll, c_longlong, Signed, false
);
strto!(
    /// Parses an `unsigned long long` in `base`, as C17 specifies.
    strtoull, c_ulonglong, Unsigned, false
);
strto!(
    /// Parses an `intmax_t` in `base`, as C17 specifies.
    strtoimax, IntMax, Signed, false
);
strto!(
    /// Parses a `uintmax_t` in `base`, as C17 specifies.
    strtoumax, UintMax, Unsigned, false
);
strto!(
    /// `strtol` with C23's grammar, which accepts `0b`. glibc's headers call it
    /// in `strtol`'s place.
    __isoc23_strtol, c_long, Signed, true
);
strto!(
    /// `strtoul` with C23's grammar.
    __isoc23_strtoul, c_ulong, Unsigned, true
);
strto!(
    /// `strtoll` with C23's grammar.
    __isoc23_strtoll, c_longlong, Signed, true
);
strto!(
    /// `strtoull` with C23's grammar.
    __isoc23_strtoull, c_ulonglong, Unsigned, true
);
strto!(
    /// `strtoimax` with C23's grammar.
    __isoc23_strtoimax, IntMax, Signed, true
);
strto!(
    /// `strtoumax` with C23's grammar.
    __isoc23_strtoumax, UintMax, Unsigned, true
);

/// The decimal integer at the start of `text`, wrapping on overflow, for the
/// `ato*` functions. C leaves their overflow undefined and they never set
/// `errno`.
fn ato(text: &(impl Input + ?Sized)) -> u64 {
    let mut i = skip_space(text, 0);
    let negative = text.at(i) == b'-';
    if negative || text.at(i) == b'+' {
        i += 1;
    }
    let mut n = 0_u64;
    while text.at(i).is_ascii_digit() {
        n = n
            .wrapping_mul(10)
            .wrapping_add(u64::from(text.at(i) - b'0'));
        i += 1;
    }
    if negative { n.wrapping_neg() } else { n }
}

/// Parses a decimal `int`. Overflow is undefined; this wraps.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn atoi(s: *const c_char) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { CText::new(s) };
    // Keeping the low bits is the wrap.
    ato(&text) as c_int
}

/// Parses a decimal `long`. Overflow is undefined; this wraps.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn atol(s: *const c_char) -> c_long {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { CText::new(s) };
    ato(&text) as c_long
}

/// Parses a decimal `long long`. Overflow is undefined; this wraps.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn atoll(s: *const c_char) -> c_longlong {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { CText::new(s) };
    ato(&text) as c_longlong
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr::null_mut;
    use std::ffi::CStr;

    /// Runs `f` on `s` with `errno` cleared, and returns the value, how far
    /// `endptr` moved, and `errno`.
    fn run<T>(
        f: unsafe extern "C" fn(*const c_char, *mut *mut c_char, c_int) -> T,
        s: &CStr,
        base: c_int,
    ) -> (T, usize, c_int) {
        errno::set(0);
        let mut end = null_mut();
        // SAFETY: `s` is a C string and `end` a valid place to write.
        let value = unsafe { f(s.as_ptr(), &raw mut end, base) };
        // SAFETY: `endptr` points into `s`.
        let moved = unsafe { end.cast_const().offset_from(s.as_ptr()) };
        // SAFETY: this thread's `errno`.
        let error = unsafe { errno::__errno_location().read() };
        (value, moved.unsigned_abs(), error)
    }

    #[test]
    fn plain_decimal_octal_and_hex() {
        assert_eq!(run(strtol, c"  -42x", 10), (-42, 5, 0));
        assert_eq!(run(strtol, c"\t\n\x0b\x0c\r +17", 0), (17, 9, 0));
        assert_eq!(run(strtol, c"0755", 0), (0o755, 4, 0));
        assert_eq!(run(strtol, c"0x1F", 0), (0x1f, 4, 0));
        assert_eq!(run(strtol, c"0X1f", 16), (0x1f, 4, 0));
        assert_eq!(run(strtol, c"1f", 16), (0x1f, 2, 0));
        assert_eq!(run(strtol, c"zZ", 36), (35 * 36 + 35, 2, 0));
        assert_eq!(run(strtol, c"08", 0), (0, 1, 0));
        assert_eq!(run(strtol, c"0", 0), (0, 1, 0));
    }

    #[test]
    fn a_prefix_with_no_digits_after_it_converts_the_zero() {
        assert_eq!(run(strtol, c"0x", 0), (0, 1, 0));
        assert_eq!(run(strtol, c"-0xg", 16), (0, 2, 0));
        assert_eq!(run(strtol, c"0x", 10), (0, 1, 0));
        assert_eq!(run(strtoul, c"0b1", 0), (0, 1, 0));
        assert_eq!(run(__isoc23_strtoul, c"0b", 2), (0, 1, 0));
        assert_eq!(run(__isoc23_strtoul, c"0b2", 0), (0, 1, 0));
    }

    #[test]
    fn c23_accepts_0b_in_bases_0_and_2_only() {
        assert_eq!(run(__isoc23_strtol, c"0b101", 0), (5, 5, 0));
        assert_eq!(run(__isoc23_strtol, c"-0B11", 2), (-3, 5, 0));
        assert_eq!(run(__isoc23_strtol, c"0b101", 16), (0xb101, 5, 0));
        assert_eq!(run(__isoc23_strtol, c"0b101", 10), (0, 1, 0));
        assert_eq!(run(__isoc23_strtoimax, c"0x10", 0), (16, 4, 0));
        assert_eq!(run(strtol, c"0b101", 2), (0, 1, 0));
    }

    #[test]
    fn no_digits_or_a_bad_base_is_einval_and_converts_nothing() {
        assert_eq!(run(strtol, c"", 10), (0, 0, errno::EINVAL));
        assert_eq!(run(strtol, c"  +", 10), (0, 0, errno::EINVAL));
        assert_eq!(run(strtol, c"  -z", 10), (0, 0, errno::EINVAL));
        assert_eq!(run(strtol, c"12", 1), (0, 0, errno::EINVAL));
        assert_eq!(run(strtol, c"12", 37), (0, 0, errno::EINVAL));
        assert_eq!(run(strtol, c"12", -1), (0, 0, errno::EINVAL));
        assert_eq!(run(strtoull, c"9", 8), (0, 0, errno::EINVAL));
    }

    #[test]
    fn signed_limits_saturate_with_erange() {
        assert_eq!(run(strtol, c"9223372036854775807", 10), (i64::MAX, 19, 0));
        assert_eq!(run(strtol, c"-9223372036854775808", 10), (i64::MIN, 20, 0));
        assert_eq!(
            run(strtol, c"9223372036854775808", 10),
            (i64::MAX, 19, errno::ERANGE)
        );
        assert_eq!(
            run(strtoll, c"-9223372036854775809", 10),
            (i64::MIN, 20, errno::ERANGE)
        );
        assert_eq!(
            run(strtoimax, c"-99999999999999999999999999z", 10),
            (i64::MIN, 27, errno::ERANGE)
        );
        assert_eq!(
            run(strtol, c"0x8000000000000000", 0),
            (i64::MAX, 18, errno::ERANGE)
        );
    }

    #[test]
    fn unsigned_negation_wraps_but_overflow_saturates() {
        assert_eq!(run(strtoul, c"-1", 10), (u64::MAX, 2, 0));
        assert_eq!(run(strtoul, c"-18446744073709551615", 10), (1, 21, 0));
        assert_eq!(
            run(strtoull, c"-18446744073709551616", 10),
            (u64::MAX, 21, errno::ERANGE)
        );
        assert_eq!(
            run(strtoumax, c"18446744073709551616", 0),
            (u64::MAX, 20, errno::ERANGE)
        );
        assert_eq!(run(strtoul, c"ffffffffffffffff", 16), (u64::MAX, 16, 0));
    }

    #[test]
    fn fitting_to_narrower_types() {
        let scanned = |magnitude, negative| Scanned {
            magnitude,
            negative,
            overflow: false,
            end: 0,
        };
        let max32 = i32::MAX as u64;
        assert_eq!(to_signed(scanned(1 << 31, true), max32).0 as i32, i32::MIN);
        assert_eq!(to_signed(scanned(1 << 31, false), max32), (max32, true));
        let (value, saturated) = to_signed(scanned((1 << 31) + 1, true), max32);
        assert!(saturated);
        assert_eq!(value as i32, i32::MIN);
        let umax32 = u64::from(u32::MAX);
        assert_eq!(to_unsigned(scanned(1, true), umax32), (umax32, false));
        assert_eq!(to_unsigned(scanned(1 << 32, true), umax32), (umax32, true));
    }

    #[test]
    fn a_null_endptr_is_allowed() {
        errno::set(0);
        // SAFETY: a C string, and a null `endptr`.
        assert_eq!(unsafe { strtol(c"12".as_ptr(), null_mut(), 0) }, 12);
    }

    #[test]
    fn ato_functions_wrap_and_ignore_trailing_text() {
        // SAFETY: C string literals.
        unsafe {
            assert_eq!(atoi(c"  -123abc".as_ptr()), -123);
        }
        // SAFETY: as above.
        assert_eq!(unsafe { atoi(c"+7".as_ptr()) }, 7);
        // SAFETY: as above.
        assert_eq!(unsafe { atoi(c"4294967297".as_ptr()) }, 1);
        // SAFETY: as above.
        assert_eq!(unsafe { atol(c"-9223372036854775808".as_ptr()) }, i64::MIN);
        // SAFETY: as above.
        assert_eq!(unsafe { atoll(c"x1".as_ptr()) }, 0);
    }
}
