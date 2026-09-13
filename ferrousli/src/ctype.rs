//! `ctype.h`: character classes and case in the C locale, and the tables
//! glibc's headers read.
//!
//! In the C locale the bytes 0 to 127 have their ASCII classes, 128 to 255
//! belong to no class and are their own case, and `EOF` (-1) is accepted.
//!
//! # glibc's tables
//!
//! glibc's `<ctype.h>` expands `isalpha(c)` into
//! `(*__ctype_b_loc())[c] & _ISalpha`, and `tolower` into a lookup through
//! `__ctype_tolower_loc()`. A program compiled against those headers calls no
//! `isalpha` at all, so the library exports the three accessors, and each
//! returns a pointer to a pointer into a table that can be indexed from -128 to
//! 255. The class bits and the values below 0 and above 127 are glibc's, and a
//! unit test compares every entry with the host glibc's. musl exports the same
//! three functions.

use core::ffi::c_int;

/// Where index 0 sits in a table that starts at -128.
const OFFSET: usize = 128;

/// A table's length: the indices -128 to 255.
const TABLE_LEN: usize = 384;

/// The mask glibc's `<ctype.h>` gives class `bit`.
///
/// glibc numbers the classes as bits of a big-endian 16-bit value, and its
/// `_ISbit` swaps the two bytes on a little-endian machine. This is that
/// macro's little-endian branch, from `/usr/include/ctype.h`.
#[cfg(target_endian = "little")]
const fn class_bit(bit: u32) -> u16 {
    if bit < 8 {
        (1 << bit) << 8
    } else {
        (1 << bit) >> 8
    }
}

/// The mask glibc's `<ctype.h>` gives class `bit`: its `_ISbit` on a
/// big-endian machine.
#[cfg(target_endian = "big")]
const fn class_bit(bit: u32) -> u16 {
    1 << bit
}

/// `_ISupper`.
const UPPER: u16 = class_bit(0);
/// `_ISlower`.
const LOWER: u16 = class_bit(1);
/// `_ISalpha`.
const ALPHA: u16 = class_bit(2);
/// `_ISdigit`.
const DIGIT: u16 = class_bit(3);
/// `_ISxdigit`.
const XDIGIT: u16 = class_bit(4);
/// `_ISspace`.
const SPACE: u16 = class_bit(5);
/// `_ISprint`.
const PRINT: u16 = class_bit(6);
/// `_ISgraph`.
const GRAPH: u16 = class_bit(7);
/// `_ISblank`.
const BLANK: u16 = class_bit(8);
/// `_IScntrl`.
const CNTRL: u16 = class_bit(9);
/// `_ISpunct`.
const PUNCT: u16 = class_bit(10);
/// `_ISalnum`.
const ALNUM: u16 = class_bit(11);

/// The classes of `c` in the C locale, as glibc's mask bits.
const fn classes(c: u8) -> u16 {
    let mut bits = 0;
    if c.is_ascii_uppercase() {
        bits |= UPPER | ALPHA | ALNUM;
    }
    if c.is_ascii_lowercase() {
        bits |= LOWER | ALPHA | ALNUM;
    }
    if c.is_ascii_digit() {
        bits |= DIGIT | ALNUM;
    }
    if c.is_ascii_hexdigit() {
        bits |= XDIGIT;
    }
    // C's white space includes the vertical tab, which Rust's does not.
    if c == b' ' || (c >= b'\t' && c <= b'\r') {
        bits |= SPACE;
    }
    if c == b' ' || c == b'\t' {
        bits |= BLANK;
    }
    if c < 0x20 || c == 0x7f {
        bits |= CNTRL;
    }
    if c >= 0x20 && c < 0x7f {
        bits |= PRINT;
    }
    if c > 0x20 && c < 0x7f {
        bits |= GRAPH;
        if !c.is_ascii_alphanumeric() {
            bits |= PUNCT;
        }
    }
    bits
}

/// The class table, for the indices -128 to 255. Only 0 to 127 have a class.
#[allow(
    clippy::indexing_slicing,
    reason = "evaluated at compile time, where an index out of range fails the build"
)]
static CLASSES: [u16; TABLE_LEN] = {
    let mut table = [0; TABLE_LEN];
    let mut c = 0;
    while c < 128 {
        table[OFFSET + c] = classes(c as u8);
        c += 1;
    }
    table
};

/// A case table for the indices -128 to 255, mapping ASCII letters to upper
/// case if `upper`, and to lower case otherwise.
///
/// glibc's C locale maps the other bytes to themselves: 128 to 255 as they
/// are, and -128 to -2 to the unsigned byte with the same bits, so that a
/// negative `char` passed straight in is treated as that byte. `EOF` maps to
/// itself.
#[allow(
    clippy::indexing_slicing,
    reason = "evaluated at compile time, where an index out of range fails the build"
)]
const fn case_table(upper: bool) -> [i32; TABLE_LEN] {
    let mut table = [0; TABLE_LEN];
    let mut index = 0;
    while index < TABLE_LEN {
        // The index as the value of `c`, from -128 to 255.
        let c = index as i32 - OFFSET as i32;
        table[index] = if c == -1 {
            -1
        } else if c < 0 {
            c + 256
        } else if c < 128 {
            let byte = c as u8;
            (if upper {
                byte.to_ascii_uppercase()
            } else {
                byte.to_ascii_lowercase()
            }) as i32
        } else {
            c
        };
        index += 1;
    }
    table
}

/// The lower-case table, for the indices -128 to 255.
static LOWER_CASE: [i32; TABLE_LEN] = case_table(false);

/// The upper-case table, for the indices -128 to 255.
static UPPER_CASE: [i32; TABLE_LEN] = case_table(true);

/// A pointer to index 0 of a table, which is what glibc's accessors point at.
///
/// A raw pointer is not `Sync`, so a `static` cannot hold one bare.
#[derive(Debug)]
#[repr(transparent)]
struct TableStart<T>(*const T);

// SAFETY: the pointer is to an immutable `static` and is never written, so
// threads may share it.
unsafe impl<T> Sync for TableStart<T> {}

/// Index 0 of [`CLASSES`].
static CLASSES_START: TableStart<u16> = TableStart(CLASSES.as_ptr().wrapping_add(OFFSET));

/// Index 0 of [`LOWER_CASE`].
static LOWER_CASE_START: TableStart<i32> = TableStart(LOWER_CASE.as_ptr().wrapping_add(OFFSET));

/// Index 0 of [`UPPER_CASE`].
static UPPER_CASE_START: TableStart<i32> = TableStart(UPPER_CASE.as_ptr().wrapping_add(OFFSET));

/// glibc's class table: a pointer to a pointer to index 0 of a table of
/// masks that can be indexed from -128 to 255.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __ctype_b_loc() -> *const *const u16 {
    (&raw const CLASSES_START).cast()
}

/// glibc's lower-case table, laid out as [`__ctype_b_loc`]'s.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __ctype_tolower_loc() -> *const *const i32 {
    (&raw const LOWER_CASE_START).cast()
}

/// glibc's upper-case table, laid out as [`__ctype_b_loc`]'s.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __ctype_toupper_loc() -> *const *const i32 {
    (&raw const UPPER_CASE_START).cast()
}

/// Whether `c` has any of the classes in `mask`. A value that is not a byte
/// from 0 to 255 has none.
fn is(c: c_int, mask: u16) -> c_int {
    let class = usize::try_from(c)
        .ok()
        .and_then(|index| CLASSES.get(OFFSET + index))
        .copied()
        .unwrap_or(0);
    c_int::from(class & mask != 0)
}

/// Whether `c` is a letter or a digit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isalnum(c: c_int) -> c_int {
    is(c, ALNUM)
}

/// Whether `c` is a letter.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isalpha(c: c_int) -> c_int {
    is(c, ALPHA)
}

/// Whether `c` is a space or a tab.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isblank(c: c_int) -> c_int {
    is(c, BLANK)
}

/// Whether `c` is a control character: 0 to 31, or 127.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iscntrl(c: c_int) -> c_int {
    is(c, CNTRL)
}

/// Whether `c` is a decimal digit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isdigit(c: c_int) -> c_int {
    is(c, DIGIT)
}

/// Whether `c` is printable and not a space.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isgraph(c: c_int) -> c_int {
    is(c, GRAPH)
}

/// Whether `c` is a small letter.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn islower(c: c_int) -> c_int {
    is(c, LOWER)
}

/// Whether `c` is printable, space included.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isprint(c: c_int) -> c_int {
    is(c, PRINT)
}

/// Whether `c` is printable and neither a space nor a letter or digit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ispunct(c: c_int) -> c_int {
    is(c, PUNCT)
}

/// Whether `c` is white space: space, `\t`, `\n`, `\v`, `\f` or `\r`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isspace(c: c_int) -> c_int {
    is(c, SPACE)
}

/// Whether `c` is a capital letter.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isupper(c: c_int) -> c_int {
    is(c, UPPER)
}

/// Whether `c` is a hexadecimal digit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isxdigit(c: c_int) -> c_int {
    is(c, XDIGIT)
}

/// `c` in lower case if it is a capital letter, and otherwise `c`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tolower(c: c_int) -> c_int {
    match u8::try_from(c) {
        Ok(byte) => c_int::from(byte.to_ascii_lowercase()),
        Err(_) => c,
    }
}

/// `c` in upper case if it is a small letter, and otherwise `c`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn toupper(c: c_int) -> c_int {
    match u8::try_from(c) {
        Ok(byte) => c_int::from(byte.to_ascii_uppercase()),
        Err(_) => c,
    }
}

/// Whether `c` is a 7-bit value.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isascii(c: c_int) -> c_int {
    c_int::from(c.cast_unsigned() < 128)
}

/// `c` with all but its low seven bits cleared.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn toascii(c: c_int) -> c_int {
    c & 0x7f
}

#[cfg(test)]
mod tests {
    use super::*;

    // The host glibc's own functions, under other names so they do not
    // collide with this module's.
    unsafe extern "C" {
        #[link_name = "__ctype_b_loc"]
        safe fn glibc_ctype_b_loc() -> *const *const u16;
        #[link_name = "__ctype_tolower_loc"]
        safe fn glibc_ctype_tolower_loc() -> *const *const i32;
        #[link_name = "__ctype_toupper_loc"]
        safe fn glibc_ctype_toupper_loc() -> *const *const i32;
        #[link_name = "isalnum"]
        safe fn glibc_isalnum(c: c_int) -> c_int;
        #[link_name = "isalpha"]
        safe fn glibc_isalpha(c: c_int) -> c_int;
        #[link_name = "isblank"]
        safe fn glibc_isblank(c: c_int) -> c_int;
        #[link_name = "iscntrl"]
        safe fn glibc_iscntrl(c: c_int) -> c_int;
        #[link_name = "isdigit"]
        safe fn glibc_isdigit(c: c_int) -> c_int;
        #[link_name = "isgraph"]
        safe fn glibc_isgraph(c: c_int) -> c_int;
        #[link_name = "islower"]
        safe fn glibc_islower(c: c_int) -> c_int;
        #[link_name = "isprint"]
        safe fn glibc_isprint(c: c_int) -> c_int;
        #[link_name = "ispunct"]
        safe fn glibc_ispunct(c: c_int) -> c_int;
        #[link_name = "isspace"]
        safe fn glibc_isspace(c: c_int) -> c_int;
        #[link_name = "isupper"]
        safe fn glibc_isupper(c: c_int) -> c_int;
        #[link_name = "isxdigit"]
        safe fn glibc_isxdigit(c: c_int) -> c_int;
        #[link_name = "tolower"]
        safe fn glibc_tolower(c: c_int) -> c_int;
        #[link_name = "toupper"]
        safe fn glibc_toupper(c: c_int) -> c_int;
        #[link_name = "isascii"]
        safe fn glibc_isascii(c: c_int) -> c_int;
        #[link_name = "toascii"]
        safe fn glibc_toascii(c: c_int) -> c_int;
    }

    /// Reads index `c` of the table `table` points at.
    ///
    /// # Safety
    ///
    /// `table` must point at a pointer to index 0 of a table indexable from
    /// -128 to 255, and `c` must be in that range.
    unsafe fn entry<T: Copy>(table: *const *const T, c: i32) -> T {
        // SAFETY: the caller passes a pointer to the table's start pointer.
        let start = unsafe { table.read() };
        // SAFETY: `c` is within the table.
        unsafe { start.wrapping_offset(c as isize).read() }
    }

    #[test]
    fn the_tables_match_glibcs_c_locale_at_every_index() {
        // The test binary never calls `setlocale`, so glibc is in the C locale.
        for c in -128..256 {
            // SAFETY: both functions return glibc's table layout, and `c` is
            // in range.
            let ours = unsafe { entry(__ctype_b_loc(), c) };
            // SAFETY: as above.
            let theirs = unsafe { entry(glibc_ctype_b_loc(), c) };
            assert_eq!(ours, theirs, "__ctype_b_loc()[{c}]");
        }
        for c in -128..256 {
            // SAFETY: as above.
            let ours = unsafe { entry(__ctype_tolower_loc(), c) };
            // SAFETY: as above.
            let theirs = unsafe { entry(glibc_ctype_tolower_loc(), c) };
            assert_eq!(ours, theirs, "__ctype_tolower_loc()[{c}]");
        }
        for c in -128..256 {
            // SAFETY: as above.
            let ours = unsafe { entry(__ctype_toupper_loc(), c) };
            // SAFETY: as above.
            let theirs = unsafe { entry(glibc_ctype_toupper_loc(), c) };
            assert_eq!(ours, theirs, "__ctype_toupper_loc()[{c}]");
        }
    }

    #[test]
    fn the_mask_bits_are_glibcs_little_endian_ones() {
        // `_ISbit` in /usr/include/ctype.h, worked out for x86-64.
        assert_eq!(
            [
                UPPER, LOWER, ALPHA, DIGIT, XDIGIT, SPACE, PRINT, GRAPH, BLANK, CNTRL, PUNCT, ALNUM
            ],
            [
                0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 0x1, 0x2, 0x4, 0x8
            ]
        );
    }

    type Classify = extern "C" fn(c_int) -> c_int;

    #[test]
    fn the_functions_agree_with_glibc_from_eof_to_255() {
        let predicates: [(&str, Classify, Classify); 13] = [
            ("isalnum", isalnum, glibc_isalnum),
            ("isalpha", isalpha, glibc_isalpha),
            ("isblank", isblank, glibc_isblank),
            ("iscntrl", iscntrl, glibc_iscntrl),
            ("isdigit", isdigit, glibc_isdigit),
            ("isgraph", isgraph, glibc_isgraph),
            ("islower", islower, glibc_islower),
            ("isprint", isprint, glibc_isprint),
            ("ispunct", ispunct, glibc_ispunct),
            ("isspace", isspace, glibc_isspace),
            ("isupper", isupper, glibc_isupper),
            ("isxdigit", isxdigit, glibc_isxdigit),
            ("isascii", isascii, glibc_isascii),
        ];
        for (name, ours, theirs) in predicates {
            for c in -1..256 {
                assert_eq!(ours(c) != 0, theirs(c) != 0, "{name}({c})");
            }
        }
        let maps: [(&str, Classify, Classify); 3] = [
            ("tolower", tolower, glibc_tolower),
            ("toupper", toupper, glibc_toupper),
            ("toascii", toascii, glibc_toascii),
        ];
        for (name, ours, theirs) in maps {
            for c in -1..256 {
                assert_eq!(ours(c), theirs(c), "{name}({c})");
            }
        }
    }

    #[test]
    fn values_that_are_not_bytes_have_no_class_and_keep_their_case() {
        for c in [c_int::MIN, -129, -56, -2, 256, 0x141, c_int::MAX] {
            assert_eq!(isalpha(c), 0);
            assert_eq!(isprint(c), 0);
            assert_eq!(iscntrl(c), 0);
            assert_eq!(tolower(c), c);
            assert_eq!(toupper(c), c);
        }
        assert_eq!(isspace(0x0b), 1);
        assert_eq!(isblank(b'\n'.into()), 0);
        assert_eq!(ispunct(b'_'.into()), 1);
        assert_eq!(toascii(0x1c1), 0x41);
        assert_eq!(isascii(-1), 0);
    }
}
