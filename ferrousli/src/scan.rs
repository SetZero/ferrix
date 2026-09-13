//! Reading the text the numeric conversions parse, and the C locale's
//! character classes they need.
//!
//! The parsers in [`crate::strtol`] and [`crate::strtod`] are written against
//! [`Input`], which reads a byte by index and reads NUL at and after the end.
//! Unit tests hand them byte strings. The C functions hand them a [`CText`],
//! which reads a C string lazily, so `strtol("12", ...)` on a string with a
//! megabyte after the digits reads three bytes rather than measuring the whole
//! string first.
//!
//! `ctype.h` belongs to another part of the library, and the conversions must
//! behave as in the C locale whatever the program's locale is, so the checks
//! they need live here.

use core::cell::Cell;
use core::ffi::c_char;

/// Text a conversion parses: a byte at each index, and NUL at and after the
/// end.
pub(crate) trait Input {
    /// The byte at `i`, or NUL at or past the end.
    fn at(&self, i: usize) -> u8;
}

impl Input for [u8] {
    fn at(&self, i: usize) -> u8 {
        self.get(i).copied().unwrap_or(0)
    }
}

/// A NUL-terminated C string, read no further than a parser asks.
#[derive(Debug)]
pub(crate) struct CText {
    start: *const c_char,
    /// How many bytes from `start` are known not to be NUL. Reading below
    /// this, or the byte at it, stays inside the string.
    known: Cell<usize>,
    /// Whether the byte at `known` has been read and is the NUL.
    ended: Cell<bool>,
}

impl CText {
    /// Wraps the C string at `start`.
    ///
    /// # Safety
    ///
    /// `start` must point at a NUL-terminated string that outlives the
    /// wrapper.
    pub(crate) unsafe fn new(start: *const c_char) -> Self {
        Self {
            start,
            known: Cell::new(0),
            ended: Cell::new(false),
        }
    }
}

impl Input for CText {
    fn at(&self, i: usize) -> u8 {
        // Walk forward one byte at a time, so no byte past the NUL is read.
        while self.known.get() <= i {
            if self.ended.get() {
                return 0;
            }
            let next = self.known.get();
            // SAFETY: every byte before `next` is not NUL, so `next` is at or
            // before the terminator and inside the string.
            let byte = unsafe { self.start.wrapping_add(next).read() };
            if byte == 0 {
                self.ended.set(true);
                return 0;
            }
            self.known.set(next + 1);
        }
        // SAFETY: `i` is below `known`, so it is inside the string.
        unsafe { self.start.wrapping_add(i).read() }.cast_unsigned()
    }
}

/// Whether `c` is white space in the C locale: space, `\t`, `\n`, `\v`, `\f`
/// or `\r`.
pub(crate) const fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The value of `c` as a digit in bases up to 36, or `None`. Letters of
/// either case stand for 10 to 35.
pub(crate) const fn digit(c: u8) -> Option<u32> {
    match c {
        b'0'..=b'9' => Some((c - b'0') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32 + 10),
        b'A'..=b'Z' => Some((c - b'A') as u32 + 10),
        _ => None,
    }
}

/// The index of the first byte at or after `i` that is not white space.
pub(crate) fn skip_space(text: &(impl Input + ?Sized), mut i: usize) -> usize {
    while is_space(text.at(i)) {
        i += 1;
    }
    i
}

/// Writes `start + consumed` to `*endptr`, unless `endptr` is null.
///
/// # Safety
///
/// `endptr` must be null or valid to write a pointer to, and `consumed` must
/// not reach past the string's NUL.
pub(crate) unsafe fn set_end(endptr: *mut *mut c_char, start: *const c_char, consumed: usize) {
    if !endptr.is_null() {
        // SAFETY: the caller vouches for `endptr`.
        unsafe { endptr.write(start.wrapping_add(consumed).cast_mut()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_text_reads_nul_at_and_after_the_end() {
        // SAFETY: a C string literal is NUL-terminated and static.
        let text = unsafe { CText::new(c"ab".as_ptr()) };
        assert_eq!(text.at(1), b'b');
        assert_eq!(text.at(0), b'a');
        assert_eq!(text.at(2), 0);
        assert_eq!(text.at(100), 0);
    }

    #[test]
    fn a_nul_inside_a_byte_string_ends_nothing_but_reads_as_nul() {
        assert_eq!(b"a\0b"[..].at(1), 0);
        assert_eq!(b"a"[..].at(5), 0);
    }

    #[test]
    fn character_classes_are_the_c_locales() {
        for c in [b' ', b'\t', b'\n', 0x0b, 0x0c, b'\r'] {
            assert!(is_space(c));
        }
        assert!(!is_space(0));
        assert!(!is_space(0xa0));
        assert!(!is_space(0x85));
        assert_eq!(digit(b'7'), Some(7));
        assert_eq!(digit(b'z'), Some(35));
        assert_eq!(digit(b'Z'), Some(35));
        assert_eq!(digit(b'@'), None);
        assert_eq!(digit(b'['), None);
        assert_eq!(digit(0xe1), None);
    }
}
