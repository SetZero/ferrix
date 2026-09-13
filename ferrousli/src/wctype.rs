//! `wctype.h`, and `wcwidth` from `wchar.h`: wide character classes, case and
//! width, for all of Unicode.
//!
//! The functions and their tables are musl 1.2.5's (MIT), from `src/ctype/`.
//! `tools/gen-unicode.py` converts musl's generated tables to Rust unchanged.
//! Those tables are Unicode 12.1.0. The classes do not depend on the locale:
//! in C and C.UTF-8 alike they are Unicode's, and the `_l` forms ignore their
//! locale, as in musl.
//!
//! The classes are musl's definitions, not glibc's:
//!
//! * `iswalpha` is the table, and everything from U+20000 to U+2FFFD.
//! * `iswdigit` is `0` to `9`, and `iswxdigit` adds `a` to `f` in either case.
//! * `iswspace` is Unicode's White_Space less the non-breaking spaces
//!   (U+00A0, U+2007, U+202F) and U+1680 and U+180E, whose glyphs are not
//!   blank. `iswblank` is only space and tab.
//! * `iswcntrl` is the C0 and C1 controls, U+2028, U+2029, and U+FFF9 to
//!   U+FFFB.
//! * `iswprint` is every scalar value, assigned or not, except those controls
//!   and the noncharacters ending in FFFE or FFFF. `iswgraph` is printable and
//!   not a space.
//! * `iswpunct` is the table: punctuation and symbols.
//! * `iswupper` and `iswlower` mean that `towlower` or `towupper` changes the
//!   character.
//!
//! The unit tests compare every code point with the host glibc's C.UTF-8 and
//! record where the two differ, and why.

use core::ffi::{CStr, c_char, c_int, c_ulong};
use core::ptr::{null, without_provenance};

use crate::ctype::isblank;
use crate::locale::Locale;
use crate::multibyte::{WChar, WInt};

include!("generated/unicode.rs");

/// `wctype_t`.
pub type WCtype = c_ulong;

/// `wctrans_t`: a pointer that is only compared.
pub type WCtrans = *const c_int;

/// Whether `wc`'s bit is set in one of musl's two-level bit tables.
///
/// The first 512 bytes map each 256-character block below U+20000 to a
/// 32-byte bitmap after them. `wc` must be below U+20000; any other value
/// reads as unset.
fn in_table(table: &[u8], wc: WInt) -> bool {
    let Some(&block) = table.get((wc >> 8) as usize) else {
        return false;
    };
    let index = usize::from(block) * 32 + ((wc & 0xff) >> 3) as usize;
    table
        .get(index)
        .is_some_and(|&bits| (bits >> (wc & 7)) & 1 != 0)
}

/// Whether `wc` is a letter or another alphabetic character.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswalpha(wc: WInt) -> c_int {
    c_int::from(if wc < 0x20000 {
        in_table(&ALPHA, wc)
    } else {
        wc < 0x2fffe
    })
}

/// Whether `wc` is a decimal digit, `0` to `9`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswdigit(wc: WInt) -> c_int {
    c_int::from(wc.wrapping_sub(u32::from(b'0')) < 10)
}

/// Whether `wc` is alphabetic or a digit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswalnum(wc: WInt) -> c_int {
    c_int::from(iswdigit(wc) != 0 || iswalpha(wc) != 0)
}

/// Whether `wc` is a space or a tab.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswblank(wc: WInt) -> c_int {
    isblank(wc.cast_signed())
}

/// Whether `wc` is a control character.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswcntrl(wc: WInt) -> c_int {
    c_int::from(
        wc < 32
            || wc.wrapping_sub(0x7f) < 33
            || wc.wrapping_sub(0x2028) < 2
            || wc.wrapping_sub(0xfff9) < 3,
    )
}

/// The white space [`iswspace`] accepts.
const SPACES: [WInt; 20] = [
    0x20, 0x09, 0x0a, 0x0d, 0x0b, 0x0c, 0x85, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005,
    0x2006, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x205f,
];

/// Whether `wc` is white space.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswspace(wc: WInt) -> c_int {
    c_int::from(wc == 0x3000 || SPACES.contains(&wc))
}

/// Whether `wc` is printable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswprint(wc: WInt) -> c_int {
    if wc < 0xff {
        return c_int::from(((wc + 1) & 0x7f) >= 0x21);
    }
    if wc < 0x2028
        || wc.wrapping_sub(0x202a) < 0xd800 - 0x202a
        || wc.wrapping_sub(0xe000) < 0xfff9 - 0xe000
    {
        return 1;
    }
    c_int::from(!(wc.wrapping_sub(0xfffc) > 0x10_ffff - 0xfffc || (wc & 0xfffe) == 0xfffe))
}

/// Whether `wc` is printable and not white space.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswgraph(wc: WInt) -> c_int {
    c_int::from(iswspace(wc) == 0 && iswprint(wc) != 0)
}

/// Whether `wc` is punctuation or a symbol.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswpunct(wc: WInt) -> c_int {
    c_int::from(wc < 0x20000 && in_table(&PUNCT, wc))
}

/// Whether `wc` is a hexadecimal digit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswxdigit(wc: WInt) -> c_int {
    c_int::from(
        wc.wrapping_sub(u32::from(b'0')) < 10 || (wc | 32).wrapping_sub(u32::from(b'a')) < 6,
    )
}

/// Whether `wc` has an upper-case mapping, and so is lower case.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswlower(wc: WInt) -> c_int {
    c_int::from(towupper(wc) != wc)
}

/// Whether `wc` has a lower-case mapping, and so is upper case.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswupper(wc: WInt) -> c_int {
    c_int::from(towlower(wc) != wc)
}

/// Applies a case rule `rule` to `c0` in direction `upper`: rules 0 and 1 are
/// a lower- or upper-case letter whose other case is the delta above the low
/// byte away.
fn apply_delta(c0: u32, rule: i32, upper: bool) -> Option<u32> {
    let kind = rule & 0xff;
    if kind >= 2 {
        return None;
    }
    let delta = rule >> 8;
    // The delta applies only when the letter's case is not already the one
    // asked for.
    let applies = kind ^ i32::from(upper);
    Some(c0.wrapping_add_signed(delta & applies.wrapping_neg()))
}

/// musl's `casemap`: `c` in upper case if `upper`, and otherwise in lower
/// case.
///
/// A two-level base-6 table picks one of up to six rules for the character's
/// 256-character block. A rule is a delta for a lower- or upper-case letter,
/// or a range of the block's exceptions to search, each naming a rule of its
/// own. Four title-case letters, such as U+01C5, are the exceptions'
/// exception: one below in upper case and one above in lower.
fn casemap(c: WInt, upper: bool) -> WInt {
    if c >= 0x20000 {
        return c;
    }
    let block = (c >> 8) as usize;
    let low = c & 0xff;
    let (x, y) = ((low / 3) as usize, (low % 3) as usize);

    let Some(&row) = CASE_TAB.get(block) else {
        return c;
    };
    let Some(&packed) = CASE_TAB.get(usize::from(row) * 86 + x) else {
        return c;
    };
    // `packed` holds three base-6 digits; this extracts digit `y`.
    let multiplier = [2048_u32, 342, 57].get(y).copied().unwrap_or(0);
    let digit = ((u32::from(packed) * multiplier) >> 11) % 6;
    let Some(&base) = CASE_RULE_BASES.get(block) else {
        return c;
    };
    let Some(&rule) = CASE_RULES.get(usize::from(base) + digit as usize) else {
        return c;
    };
    if let Some(mapped) = apply_delta(c, rule, upper) {
        return mapped;
    }

    // A binary search of the block's exceptions, whose start and count are
    // the rule's delta.
    let delta = rule >> 8;
    let mut count = (delta & 0xff) as usize;
    let mut start = (delta.cast_unsigned() >> 8) as usize;
    while count > 0 {
        let Some(&[key, exception]) = CASE_EXCEPTIONS.get(start + count / 2) else {
            return c;
        };
        let key = u32::from(key);
        if key == low {
            let Some(&rule) = CASE_RULES.get(usize::from(exception)) else {
                return c;
            };
            return apply_delta(c, rule, upper).unwrap_or(if upper { c - 1 } else { c + 1 });
        } else if key > low {
            count /= 2;
        } else {
            start += count / 2;
            count -= count / 2;
        }
    }
    c
}

/// `wc` in lower case, or `wc` if it has none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn towlower(wc: WInt) -> WInt {
    casemap(wc, false)
}

/// `wc` in upper case, or `wc` if it has none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn towupper(wc: WInt) -> WInt {
    casemap(wc, true)
}

/// The class names [`wctype`] knows, in the order of their numbers from 1.
const CLASS_NAMES: [&CStr; 12] = [
    c"alnum", c"alpha", c"blank", c"cntrl", c"digit", c"graph", c"lower", c"print", c"punct",
    c"space", c"upper", c"xdigit",
];

/// The classifiers, in [`CLASS_NAMES`]' order.
const CLASSIFIERS: [extern "C" fn(WInt) -> c_int; 12] = [
    iswalnum, iswalpha, iswblank, iswcntrl, iswdigit, iswgraph, iswlower, iswprint, iswpunct,
    iswspace, iswupper, iswxdigit,
];

/// The class named `name`, for [`iswctype`], or 0 for none.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wctype(name: *const c_char) -> WCtype {
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name) };
    CLASS_NAMES
        .iter()
        .position(|&class| class == name)
        .map_or(0, |i| i as WCtype + 1)
}

/// Whether `wc` is in the class `class` from [`wctype`]. 0 for class 0 or any
/// number that is not a class.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn iswctype(wc: WInt, class: WCtype) -> c_int {
    class
        .checked_sub(1)
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| CLASSIFIERS.get(i))
        .map_or(0, |classify| classify(wc))
}

/// [`wctrans`]'s value for `toupper`.
const TO_UPPER: WCtrans = without_provenance(1);
/// [`wctrans`]'s value for `tolower`.
const TO_LOWER: WCtrans = without_provenance(2);

/// The mapping named `name`, `toupper` or `tolower`, for [`towctrans`], or
/// null for none.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wctrans(name: *const c_char) -> WCtrans {
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name) };
    if name == c"toupper" {
        TO_UPPER
    } else if name == c"tolower" {
        TO_LOWER
    } else {
        null()
    }
}

/// `wc` mapped by `mapping` from [`wctrans`], or `wc` for any other mapping.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn towctrans(wc: WInt, mapping: WCtrans) -> WInt {
    if mapping == TO_UPPER {
        towupper(wc)
    } else if mapping == TO_LOWER {
        towlower(wc)
    } else {
        wc
    }
}

/// The columns `wc` takes on a terminal: 0 for NUL and combining characters,
/// 2 for wide East Asian characters, -1 for controls and noncharacters, and
/// otherwise 1, unassigned code points included.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn wcwidth(wc: WChar) -> c_int {
    let wc = wc.cast_unsigned();
    if wc < 0xff {
        return if ((wc + 1) & 0x7f) >= 0x21 {
            1
        } else if wc != 0 {
            -1
        } else {
            0
        };
    }
    if (wc & 0xfffe_ffff) < 0xfffe {
        if in_table(&NONSPACING, wc) {
            return 0;
        }
        if in_table(&WIDE, wc) {
            return 2;
        }
        return 1;
    }
    if (wc & 0xfffe) == 0xfffe {
        return -1;
    }
    if wc.wrapping_sub(0x20000) < 0x20000 {
        return 2;
    }
    if wc == 0xe0001 || wc.wrapping_sub(0xe0020) < 0x5f || wc.wrapping_sub(0xe0100) < 0xef {
        return 0;
    }
    1
}

/// The columns the first `n` wide characters of `wcs` take, stopping at a
/// NUL, or -1 if any of them has no width.
///
/// # Safety
///
/// `wcs` must be valid for `n` characters or up to its NUL.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcswidth(wcs: *const WChar, n: usize) -> c_int {
    let mut total: c_int = 0;
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and no NUL came before `i`.
        let wc = unsafe { wcs.wrapping_add(i).read() };
        if wc == 0 {
            break;
        }
        let width = wcwidth(wc);
        if width < 0 {
            return width;
        }
        total = total.wrapping_add(width);
        i += 1;
    }
    total
}

/// Declares `_l` forms that ignore their locale, as in musl.
macro_rules! locale_ignored {
    ($($(#[$doc:meta])* $name:ident => $plain:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(
            $(#[$doc])*
            #[cfg_attr(not(test), unsafe(no_mangle))]
            pub extern "C" fn $name($($arg: $ty,)* locale: *mut Locale) -> $ret {
                let _ = locale;
                $plain($($arg),*)
            }
        )*
    };
}

locale_ignored! {
    /// [`iswalnum`]: the class does not depend on the locale.
    iswalnum_l => iswalnum(wc: WInt) -> c_int;
    /// [`iswalpha`]: the class does not depend on the locale.
    iswalpha_l => iswalpha(wc: WInt) -> c_int;
    /// [`iswblank`]: the class does not depend on the locale.
    iswblank_l => iswblank(wc: WInt) -> c_int;
    /// [`iswcntrl`]: the class does not depend on the locale.
    iswcntrl_l => iswcntrl(wc: WInt) -> c_int;
    /// [`iswdigit`]: the class does not depend on the locale.
    iswdigit_l => iswdigit(wc: WInt) -> c_int;
    /// [`iswgraph`]: the class does not depend on the locale.
    iswgraph_l => iswgraph(wc: WInt) -> c_int;
    /// [`iswlower`]: the class does not depend on the locale.
    iswlower_l => iswlower(wc: WInt) -> c_int;
    /// [`iswprint`]: the class does not depend on the locale.
    iswprint_l => iswprint(wc: WInt) -> c_int;
    /// [`iswpunct`]: the class does not depend on the locale.
    iswpunct_l => iswpunct(wc: WInt) -> c_int;
    /// [`iswspace`]: the class does not depend on the locale.
    iswspace_l => iswspace(wc: WInt) -> c_int;
    /// [`iswupper`]: the class does not depend on the locale.
    iswupper_l => iswupper(wc: WInt) -> c_int;
    /// [`iswxdigit`]: the class does not depend on the locale.
    iswxdigit_l => iswxdigit(wc: WInt) -> c_int;
    /// [`iswctype`]: the classes do not depend on the locale.
    iswctype_l => iswctype(wc: WInt, class: WCtype) -> c_int;
    /// [`towlower`]: case does not depend on the locale.
    towlower_l => towlower(wc: WInt) -> WInt;
    /// [`towupper`]: case does not depend on the locale.
    towupper_l => towupper(wc: WInt) -> WInt;
    /// [`towctrans`]: case does not depend on the locale.
    towctrans_l => towctrans(wc: WInt, mapping: WCtrans) -> WInt;
}

/// [`wctype`]: the class names do not depend on the locale.
///
/// # Safety
///
/// As [`wctype`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wctype_l(name: *const c_char, locale: *mut Locale) -> WCtype {
    let _ = locale;
    // SAFETY: the caller's contract is `wctype`'s.
    unsafe { wctype(name) }
}

/// [`wctrans`]: the mapping names do not depend on the locale.
///
/// # Safety
///
/// As [`wctrans`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wctrans_l(name: *const c_char, locale: *mut Locale) -> WCtrans {
    let _ = locale;
    // SAFETY: the caller's contract is `wctrans`'s.
    unsafe { wctrans(name) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_void;
    use core::ptr::null_mut;
    use std::fmt::Write;

    // The host glibc's functions, under other names so they do not collide
    // with this module's.
    unsafe extern "C" {
        #[link_name = "newlocale"]
        fn glibc_newlocale(mask: c_int, name: *const c_char, base: *mut c_void) -> *mut c_void;
        #[link_name = "uselocale"]
        fn glibc_uselocale(locale: *mut c_void) -> *mut c_void;
        #[link_name = "freelocale"]
        fn glibc_freelocale(locale: *mut c_void);
        #[link_name = "iswalnum"]
        safe fn glibc_iswalnum(wc: WInt) -> c_int;
        #[link_name = "iswalpha"]
        safe fn glibc_iswalpha(wc: WInt) -> c_int;
        #[link_name = "iswblank"]
        safe fn glibc_iswblank(wc: WInt) -> c_int;
        #[link_name = "iswcntrl"]
        safe fn glibc_iswcntrl(wc: WInt) -> c_int;
        #[link_name = "iswdigit"]
        safe fn glibc_iswdigit(wc: WInt) -> c_int;
        #[link_name = "iswgraph"]
        safe fn glibc_iswgraph(wc: WInt) -> c_int;
        #[link_name = "iswlower"]
        safe fn glibc_iswlower(wc: WInt) -> c_int;
        #[link_name = "iswprint"]
        safe fn glibc_iswprint(wc: WInt) -> c_int;
        #[link_name = "iswpunct"]
        safe fn glibc_iswpunct(wc: WInt) -> c_int;
        #[link_name = "iswspace"]
        safe fn glibc_iswspace(wc: WInt) -> c_int;
        #[link_name = "iswupper"]
        safe fn glibc_iswupper(wc: WInt) -> c_int;
        #[link_name = "iswxdigit"]
        safe fn glibc_iswxdigit(wc: WInt) -> c_int;
        #[link_name = "towlower"]
        safe fn glibc_towlower(wc: WInt) -> WInt;
        #[link_name = "towupper"]
        safe fn glibc_towupper(wc: WInt) -> WInt;
        #[link_name = "wcwidth"]
        safe fn glibc_wcwidth(wc: WChar) -> c_int;
    }

    /// glibc's `LC_CTYPE_MASK`.
    const GLIBC_LC_CTYPE_MASK: c_int = 1;

    /// One past the last Unicode code point.
    const UNICODE_END: u32 = 0x11_0000;

    /// Collects runs of consecutive code points that disagree the same way,
    /// as lines of text.
    struct Runs<'a> {
        out: &'a mut String,
        name: &'static str,
        run: Option<(u32, u32, i64, i64)>,
    }

    impl Runs<'_> {
        /// Notes what the two libraries say of `wc`.
        fn note(&mut self, wc: u32, ours: i64, theirs: i64) {
            if ours == theirs {
                self.flush();
                return;
            }
            if let Some((_, last, o, t)) = &mut self.run
                && *last + 1 == wc
                && *o == ours
                && *t == theirs
            {
                *last = wc;
                return;
            }
            self.flush();
            self.run = Some((wc, wc, ours, theirs));
        }

        /// Writes the run in progress, if any.
        fn flush(&mut self) {
            if let Some((first, last, ours, theirs)) = self.run.take() {
                let _ = writeln!(
                    self.out,
                    "{} U+{first:04X}..U+{last:04X} {} musl {ours:+} glibc {theirs:+}",
                    self.name,
                    last - first + 1,
                );
            }
        }
    }

    type Classify = extern "C" fn(WInt) -> c_int;
    type Map = extern "C" fn(WInt) -> WInt;

    /// Every disagreement with glibc's C.UTF-8 over all of Unicode, one run
    /// per line. Classes are 0 or 1, case mappings are the offset from the
    /// character, and widths are as returned.
    fn disagreements() -> String {
        // glibc's `uselocale` changes only this thread. `setlocale` would
        // change the whole test binary, whose other tests compare against
        // glibc's C locale in parallel.
        // SAFETY: the name is a NUL-terminated string.
        let utf8 = unsafe { glibc_newlocale(GLIBC_LC_CTYPE_MASK, c"C.UTF-8".as_ptr(), null_mut()) };
        assert!(!utf8.is_null(), "the host glibc has no C.UTF-8 locale");
        // SAFETY: `utf8` is a locale glibc made.
        let previous = unsafe { glibc_uselocale(utf8) };

        let classes: [(&str, Classify, Classify); 12] = [
            ("iswalnum", iswalnum, glibc_iswalnum),
            ("iswalpha", iswalpha, glibc_iswalpha),
            ("iswblank", iswblank, glibc_iswblank),
            ("iswcntrl", iswcntrl, glibc_iswcntrl),
            ("iswdigit", iswdigit, glibc_iswdigit),
            ("iswgraph", iswgraph, glibc_iswgraph),
            ("iswlower", iswlower, glibc_iswlower),
            ("iswprint", iswprint, glibc_iswprint),
            ("iswpunct", iswpunct, glibc_iswpunct),
            ("iswspace", iswspace, glibc_iswspace),
            ("iswupper", iswupper, glibc_iswupper),
            ("iswxdigit", iswxdigit, glibc_iswxdigit),
        ];
        let mut out = String::new();
        for (name, ours, theirs) in classes {
            let mut runs = Runs {
                out: &mut out,
                name,
                run: None,
            };
            for wc in 0..UNICODE_END {
                runs.note(wc, i64::from(ours(wc) != 0), i64::from(theirs(wc) != 0));
            }
            runs.flush();
        }
        let maps: [(&str, Map, Map); 2] = [
            ("towlower", towlower, glibc_towlower),
            ("towupper", towupper, glibc_towupper),
        ];
        for (name, ours, theirs) in maps {
            let mut runs = Runs {
                out: &mut out,
                name,
                run: None,
            };
            for wc in 0..UNICODE_END {
                let offset = |mapped: WInt| i64::from(mapped) - i64::from(wc);
                runs.note(wc, offset(ours(wc)), offset(theirs(wc)));
            }
            runs.flush();
        }
        let mut runs = Runs {
            out: &mut out,
            name: "wcwidth",
            run: None,
        };
        for wc in 0..UNICODE_END {
            let signed = wc.cast_signed();
            runs.note(
                wc,
                i64::from(wcwidth(signed)),
                i64::from(glibc_wcwidth(signed)),
            );
        }
        runs.flush();

        // SAFETY: `previous` is what glibc returned before.
        let _ = unsafe { glibc_uselocale(previous) };
        // SAFETY: `utf8` is no longer in use.
        unsafe { glibc_freelocale(utf8) };
        out
    }

    #[test]
    fn every_disagreement_with_glibcs_c_utf8_is_the_recorded_one() {
        if !crate::host_glibc::is_recorded() {
            return;
        }
        let actual = disagreements();
        if let Some(path) = std::env::var_os("FERROUSLI_UNICODE_DIFFERENCES_OUT") {
            std::fs::write(path, &actual).unwrap_or_default();
            return;
        }
        let recorded: String = include_str!("wctype/glibc-differences.txt")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| {
                let run = line.split("  #").next().unwrap_or_default();
                format!("{run}\n")
            })
            .collect();
        let changed: Vec<_> = actual
            .lines()
            .filter(|line| !recorded.lines().any(|r| r == *line))
            .chain(
                recorded
                    .lines()
                    .filter(|line| !actual.lines().any(|a| a == *line)),
            )
            .take(20)
            .collect();
        assert!(
            actual == recorded,
            "the differences from glibc changed; regenerate \
             src/wctype/glibc-differences.txt as tools/unicode-differences.py \
             says. The first lines not in both:\n{}",
            changed.join("\n")
        );
    }
}
