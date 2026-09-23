//! `wchar.h` and `inttypes.h`: parsing numbers from wide strings, with
//! `wcstol` and its relatives, `wcstoimax`, `wcstoumax`, `wcstof`, `wcstod` and
//! `wcstold`.
//!
//! These are the narrow parsers in [`crate::strtol`] and [`crate::strtod`] read
//! through [`WText`], as musl 1.2.5's `stdlib/wcstol.c` and `wcstod.c` (MIT)
//! read its narrow scanners through a `FILE` over the wide string. As there:
//!
//! * White space by `iswspace`, which includes the wide spaces, is skipped
//!   first, then the parser reads from the first character that is not.
//! * A character above ASCII reads as `@`, which no number contains, so it ends
//!   the subject without the parser knowing wide characters at all.
//! * `*endptr` is the start of the string when nothing converted, and just past
//!   the subject otherwise.
//!
//! glibc 2.38's headers redirect the integer functions to `__isoc23_wcstol` and
//! the rest under C23 or `_GNU_SOURCE`, as they do the narrow ones. Those names
//! accept C23's `0b` prefix and are defined here too.

use core::cell::Cell;
use core::ffi::{c_double, c_float, c_int, c_long, c_longlong, c_ulong, c_ulonglong};

use crate::errno;
use crate::float::{BINARY32, BINARY64, Format};
use crate::multibyte::{WChar, WInt};
use crate::scan::Input;
use crate::strtod::parse;
use crate::strtol::{scan, to_signed, to_unsigned};
use crate::wctype::iswspace;

/// `intmax_t`: 64 bits on every target, a `long` on the 64-bit ones and a
/// `long long` on ARMv7-A, as `bits/alltypes.h` defines it.
type IntMax = i64;
/// `uintmax_t`.
type UintMax = u64;

/// A NUL-terminated wide string read as the narrow parsers need it: each
/// character as its byte when it is ASCII, as `@` when it is not, and NUL at
/// and after the end. Like [`crate::scan::CText`], it reads no further than a
/// parser asks.
#[derive(Debug)]
pub(crate) struct WText {
    start: *const WChar,
    /// How many characters from `start` are known not to be NUL.
    known: Cell<usize>,
    /// Whether the character at `known` has been read and is the NUL.
    ended: Cell<bool>,
}

impl WText {
    /// Wraps the wide string at `start`.
    ///
    /// # Safety
    ///
    /// `start` must point at a NUL-terminated wide string that outlives the
    /// wrapper.
    pub(crate) unsafe fn new(start: *const WChar) -> Self {
        Self {
            start,
            known: Cell::new(0),
            ended: Cell::new(false),
        }
    }
}

impl Input for WText {
    fn at(&self, i: usize) -> u8 {
        while self.known.get() <= i {
            if self.ended.get() {
                return 0;
            }
            let next = self.known.get();
            // SAFETY: every character before `next` is not NUL, so `next` is
            // at or before the terminator and inside the string.
            let wc = unsafe { self.start.wrapping_add(next).read() };
            if wc == 0 {
                self.ended.set(true);
                return 0;
            }
            self.known.set(next + 1);
        }
        // SAFETY: `i` is below `known`, so it is inside the string.
        let wc = unsafe { self.start.wrapping_add(i).read() };
        u8::try_from(wc).ok().filter(u8::is_ascii).unwrap_or(b'@')
    }
}

/// The first character of `s` that `iswspace` does not call white space.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string.
unsafe fn skip_wide_space(s: *const WChar) -> *const WChar {
    let mut at = s;
    loop {
        // SAFETY: the string has not ended before `at`: NUL is not space.
        let wc = unsafe { at.read() };
        if iswspace(wc as WInt) == 0 {
            return at;
        }
        at = at.wrapping_add(1);
    }
}

/// Writes `at` to `*endptr`, unless `endptr` is null.
///
/// # Safety
///
/// `endptr` must be null or valid to write a pointer to.
unsafe fn set_wide_end(endptr: *mut *mut WChar, at: *const WChar) {
    if !endptr.is_null() {
        // SAFETY: the caller vouches for `endptr`.
        unsafe { endptr.write(at.cast_mut()) };
    }
}

/// Which way an integer conversion fits its result.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// A signed type with this maximum.
    Signed(u64),
    /// An unsigned type with this maximum.
    Unsigned(u64),
}

/// The body every `wcsto*` integer function shares.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string, and `endptr` null or valid to
/// write.
unsafe fn convert_integer(
    s: *const WChar,
    endptr: *mut *mut WChar,
    base: c_int,
    binary_prefix: bool,
    kind: Kind,
) -> u64 {
    // SAFETY: the caller passes a NUL-terminated wide string.
    let t = unsafe { skip_wide_space(s) };
    // SAFETY: `t` is inside the same string.
    let text = unsafe { WText::new(t) };
    let Some(scanned) = scan(&text, base, binary_prefix) else {
        errno::set(errno::EINVAL);
        // SAFETY: the caller vouches for `endptr`.
        unsafe { set_wide_end(endptr, s) };
        return 0;
    };
    // SAFETY: the caller vouches for `endptr`, and the scan stopped at or
    // before the NUL.
    unsafe { set_wide_end(endptr, t.wrapping_add(scanned.end)) };
    let (value, saturated) = match kind {
        Kind::Signed(max) => to_signed(scanned, max),
        Kind::Unsigned(max) => to_unsigned(scanned, max),
    };
    if saturated {
        errno::set(errno::ERANGE);
    }
    value
}

/// Defines one exported integer conversion.
macro_rules! wcsto {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $kind:ident, $binary:expr) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// `s` must be a NUL-terminated wide string, and `endptr` null or valid
        /// to write a pointer to.
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name(s: *const WChar, endptr: *mut *mut WChar, base: c_int) -> $ty {
            let kind = Kind::$kind(<$ty>::MAX as u64);
            // SAFETY: the caller's contract is `convert_integer`'s.
            let value = unsafe { convert_integer(s, endptr, base, $binary, kind) };
            // The value fits: it was saturated to the type's range, and a
            // negative value is its two's complement bits.
            value as $ty
        }
    };
}

wcsto!(
    /// Parses a `long` in `base` from a wide string, as C17 specifies.
    wcstol, c_long, Signed, false
);
wcsto!(
    /// Parses an `unsigned long` in `base` from a wide string.
    wcstoul, c_ulong, Unsigned, false
);
wcsto!(
    /// Parses a `long long` in `base` from a wide string.
    wcstoll, c_longlong, Signed, false
);
wcsto!(
    /// Parses an `unsigned long long` in `base` from a wide string.
    wcstoull, c_ulonglong, Unsigned, false
);
wcsto!(
    /// Parses an `intmax_t` in `base` from a wide string.
    wcstoimax, IntMax, Signed, false
);
wcsto!(
    /// Parses a `uintmax_t` in `base` from a wide string.
    wcstoumax, UintMax, Unsigned, false
);
wcsto!(
    /// `wcstol` with C23's grammar, which accepts `0b`.
    __isoc23_wcstol, c_long, Signed, true
);
wcsto!(
    /// `wcstoul` with C23's grammar.
    __isoc23_wcstoul, c_ulong, Unsigned, true
);
wcsto!(
    /// `wcstoll` with C23's grammar.
    __isoc23_wcstoll, c_longlong, Signed, true
);
wcsto!(
    /// `wcstoull` with C23's grammar.
    __isoc23_wcstoull, c_ulonglong, Unsigned, true
);
wcsto!(
    /// `wcstoimax` with C23's grammar.
    __isoc23_wcstoimax, IntMax, Signed, true
);
wcsto!(
    /// `wcstoumax` with C23's grammar.
    __isoc23_wcstoumax, UintMax, Unsigned, true
);

/// Parses the wide string `s` in `format`, sets `*endptr` and `errno`, and
/// returns the bit pattern.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string, and `endptr` null or valid to
/// write.
unsafe fn convert_float(s: *const WChar, endptr: *mut *mut WChar, format: &Format) -> u128 {
    // SAFETY: the caller passes a NUL-terminated wide string.
    let t = unsafe { skip_wide_space(s) };
    // SAFETY: `t` is inside the same string.
    let text = unsafe { WText::new(t) };
    let parsed = parse(&text, format);
    let end = if parsed.end == 0 {
        s
    } else {
        t.wrapping_add(parsed.end)
    };
    // SAFETY: the caller vouches for `endptr`.
    unsafe { set_wide_end(endptr, end) };
    if parsed.error != 0 {
        errno::set(parsed.error);
    }
    parsed.bits
}

/// Parses a `double` from a wide string.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string, and `endptr` null or valid to
/// write a pointer to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcstod(s: *const WChar, endptr: *mut *mut WChar) -> c_double {
    // SAFETY: the caller's contract is `convert_float`'s.
    let bits = unsafe { convert_float(s, endptr, &BINARY64) };
    f64::from_bits(bits as u64)
}

/// Parses a `float` from a wide string.
///
/// # Safety
///
/// As [`wcstod`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcstof(s: *const WChar, endptr: *mut *mut WChar) -> c_float {
    // SAFETY: the caller's contract is `convert_float`'s.
    let bits = unsafe { convert_float(s, endptr, &BINARY32) };
    f32::from_bits(bits as u32)
}

/// Parses an x87 `long double` from a wide string and writes its ten bytes,
/// little-endian, to `out`. [`wcstold`] calls it and loads the result.
///
/// # Safety
///
/// As [`wcstod`], and `out` must be valid to write ten bytes.
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn wcstold_x87(s: *const WChar, endptr: *mut *mut WChar, out: *mut u8) {
    // SAFETY: the caller's contract is `convert_float`'s.
    let bits = unsafe { convert_float(s, endptr, &crate::float::X87_EXTENDED) };
    let bytes = bits.to_le_bytes();
    let mut k = 0;
    while let Some(&byte) = bytes.get(k).filter(|_| k < 10) {
        // SAFETY: the caller vouches for ten bytes at `out`.
        unsafe { out.wrapping_add(k).write(byte) };
        k += 1;
    }
}

/// Parses a `long double`, the x87 80-bit type, from a wide string. It returns
/// in `st(0)`, which Rust cannot, so this is the same shim as
/// [`crate::strtod::strtold`]: C declares it `long double wcstold(const
/// wchar_t *, wchar_t **)`.
///
/// # Safety
///
/// As [`wcstod`].
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcstold(s: *const WChar, endptr: *mut *mut WChar) {
    // On entry the stack is 8 below a multiple of 16. Taking 24 aligns it for
    // the call and leaves 16 bytes for the result.
    core::arch::naked_asm!(
        "sub rsp, 24",
        "mov rdx, rsp",
        "call {convert}",
        "fld tbyte ptr [rsp]",
        "add rsp, 24",
        "ret",
        convert = sym wcstold_x87,
    )
}

/// The bits of a binary128 `long double` parsed from a wide string:
/// [`wcstold`]'s work on AArch64.
///
/// # Safety
///
/// As [`wcstod`].
#[cfg(target_arch = "aarch64")]
unsafe extern "C" fn wcstold_binary128(s: *const WChar, endptr: *mut *mut WChar) -> u128 {
    // SAFETY: the caller's contract is `convert_float`'s.
    unsafe { convert_float(s, endptr, &crate::float::BINARY128) }
}

#[cfg(target_arch = "aarch64")]
crate::math::ld128::returns_long_double!(
    /// Parses a `long double` from a wide string: on AArch64, IEEE binary128,
    /// returned in q0.
    fn wcstold(s: *const WChar, endptr: *mut *mut WChar) via wcstold_binary128
);

/// Parses a `long double`, which on ARMv7-A is a `double`, from a wide
/// string.
///
/// # Safety
///
/// As [`wcstod`].
#[cfg(target_arch = "arm")]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcstold(s: *const WChar, endptr: *mut *mut WChar) -> f64 {
    // SAFETY: the caller's contract is `wcstod`'s.
    unsafe { wcstod(s, endptr) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr::null_mut;

    /// A NUL-terminated wide string from `text`.
    fn wide(text: &str) -> Vec<WChar> {
        text.chars().map(|c| c as WChar).chain([0]).collect()
    }

    #[test]
    fn integers_parse_through_the_narrow_scanner() {
        let s = wide("\u{3000} -0x1fz");
        let mut end = null_mut();
        // SAFETY: `s` is NUL-terminated and `end` a live local.
        let value = unsafe { wcstol(s.as_ptr(), &raw mut end, 0) };
        assert_eq!(value, -31);
        assert_eq!(end.cast_const(), s.as_ptr().wrapping_add(7));
    }

    #[test]
    fn a_wide_character_ends_the_subject_and_nothing_converts_to_the_start() {
        let s = wide("12\u{e9}3");
        let mut end = null_mut();
        // SAFETY: as above.
        assert_eq!(unsafe { wcstoul(s.as_ptr(), &raw mut end, 10) }, 12);
        assert_eq!(end.cast_const(), s.as_ptr().wrapping_add(2));
        let none = wide("  \u{e9}");
        // SAFETY: as above.
        assert_eq!(unsafe { wcstoll(none.as_ptr(), &raw mut end, 10) }, 0);
        assert_eq!(end.cast_const(), none.as_ptr());
    }

    #[test]
    fn floats_parse_and_report_their_end() {
        let s = wide(" 2.5e1x");
        let mut end = null_mut();
        // SAFETY: as above.
        assert_eq!(unsafe { wcstod(s.as_ptr(), &raw mut end) }, 25.0);
        assert_eq!(end.cast_const(), s.as_ptr().wrapping_add(6));
        // SAFETY: as above.
        assert_eq!(unsafe { wcstof(s.as_ptr(), null_mut()) }, 25.0);
        let none = wide("x");
        // SAFETY: as above.
        assert_eq!(unsafe { wcstod(none.as_ptr(), &raw mut end) }, 0.0);
        assert_eq!(end.cast_const(), none.as_ptr());
    }
}
