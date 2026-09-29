//! `regex.h` through its C interface.
//!
//! [`CASES`] is a table of pattern, string and result, in the notation
//! Glenn Fowler's test suite uses: `(0,4)(0,2)` for a match and its groups,
//! `(?,?)` for a group that did not match, `NOMATCH`, or `E` and the code a
//! failed `regcomp` returned. Several cases are POSIX's classic ones, and
//! several are adapted from libc-test's regex regressions (MIT), which say
//! which musl commit they came from.
//!
//! The same table is run against the host glibc, and every difference has to
//! be listed in [`GLIBC_DIFFERS`] with a reason, so a change in what this
//! library answers cannot pass unnoticed. That comparison runs on any glibc,
//! unlike the ones in `ctype` and `wctype`: glibc's answers here come from
//! its matcher, not from Unicode tables that change between releases. The
//! list was recorded on glibc 2.39.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test reports failure by panicking"
)]

use core::mem::MaybeUninit;
use std::ffi::CString;
use std::time::{Duration, Instant};

use super::*;

/// `REG_EXTENDED`, for the table.
const E: c_int = REG_EXTENDED;
/// `REG_ICASE`, for the table.
const I: c_int = REG_ICASE;
/// `REG_NEWLINE`, for the table.
const N: c_int = REG_NEWLINE;

/// One case: the `regcomp` flags, the pattern, the string, the `regexec`
/// flags, and what the match must be.
type Case = (c_int, &'static str, &'static str, c_int, &'static str);

/// The result of compiling `pat` and matching `s`, in the notation above.
fn ours(cflags: c_int, pat: &str, s: &str, eflags: c_int) -> String {
    let pattern = CString::new(pat).unwrap();
    let string = CString::new(s).unwrap();
    let mut re = MaybeUninit::<Regex>::zeroed();
    // SAFETY: a `regex_t` to fill and a string to compile.
    let code = unsafe { regcomp(re.as_mut_ptr(), pattern.as_ptr(), cflags) };
    if code != REG_OK {
        return format!("E{code}");
    }
    // SAFETY: `regcomp` returned success, so it filled the structure.
    let nsub = unsafe { re.assume_init_ref() }.re_nsub;
    let mut found = vec![
        RegMatch {
            rm_so: -2,
            rm_eo: -2
        };
        nsub + 1
    ];
    // SAFETY: the pattern is compiled, and `found` has `nsub + 1` entries.
    let code = unsafe {
        regexec(
            re.as_ptr(),
            string.as_ptr(),
            found.len(),
            found.as_mut_ptr(),
            eflags,
        )
    };
    // SAFETY: the pattern is compiled and not used again.
    unsafe { regfree(re.as_mut_ptr()) };
    report(
        code,
        found
            .iter()
            .map(|m| (i64::from(m.rm_so), i64::from(m.rm_eo))),
    )
}

/// How many groups `pat` has, as this library counts them, and 0 if it does
/// not compile.
fn groups(cflags: c_int, pat: &str) -> usize {
    let pattern = CString::new(pat).unwrap();
    let mut re = MaybeUninit::<Regex>::zeroed();
    // SAFETY: a `regex_t` to fill and a string to compile.
    if unsafe { regcomp(re.as_mut_ptr(), pattern.as_ptr(), cflags) } != REG_OK {
        return 0;
    }
    // SAFETY: `regcomp` returned success, so it filled the structure.
    let nsub = unsafe { re.assume_init_ref() }.re_nsub;
    // SAFETY: the pattern is compiled and not used again.
    unsafe { regfree(re.as_mut_ptr()) };
    nsub
}

/// A `regexec` result in the table's notation.
fn report(code: c_int, found: impl Iterator<Item = (i64, i64)>) -> String {
    if code == REG_NOMATCH {
        return "NOMATCH".to_owned();
    }
    if code != REG_OK {
        return format!("X{code}");
    }
    let mut text = String::new();
    for (so, eo) in found {
        if so < 0 || eo < 0 {
            text.push_str("(?,?)");
        } else {
            text.push_str(&format!("({so},{eo})"));
        }
    }
    text
}

/// What each pattern must match, and where its groups must land.
const CASES: &[Case] = &[
    // Extended expressions: the plain forms.
    (E, "abc", "xabcy", 0, "(1,4)"),
    (E, "a|b|c|d|e", "e", 0, "(0,1)"),
    (E, "(a|b|c|d|e)f", "ef", 0, "(0,2)(0,1)"),
    (E, "cat|dog", "hotdog", 0, "(3,6)"),
    (E, "a+", "baaac", 0, "(1,4)"),
    (E, "a?b", "ab", 0, "(0,2)"),
    (E, "a{2,3}", "aaaa", 0, "(0,3)"),
    (E, "a{2}", "a", 0, "NOMATCH"),
    (E, "(a{2})*", "aaaaa", 0, "(0,4)(2,4)"),
    (E, "a{0}b", "ab", 0, "(1,2)"),
    (E, "a.c", "abc", 0, "(0,3)"),
    (E, "a**", "aa", 0, "(0,2)"),
    (E, "()", "x", 0, "(0,0)(0,0)"),
    (E, "(a|)", "b", 0, "(0,0)(0,0)"),
    (E, "a)", "a)", 0, "(0,2)"),
    (E, "(a)\\1", "a1", 0, "(0,2)(0,1)"),
    (E, "^$", "", 0, "(0,0)"),
    (E, "^a", "ba", 0, "NOMATCH"),
    (E, "a$", "ab", 0, "NOMATCH"),
    // Bracket expressions.
    (E, "[[:digit:]]+", "ab123c", 0, "(2,5)"),
    (E, "[]a]+", "x]a]", 0, "(1,4)"),
    (E, "[^]a]", "]ab", 0, "(2,3)"),
    (E, "[a-]+", "x-a", 0, "(1,3)"),
    (E, "[[.-.]a]+", "b-a", 0, "(1,3)"),
    (E, "[[=a=]]", "ba", 0, "(1,2)"),
    // musl's extension: `[a-z--@]` is `[a-z]` or the range `-` to `@`, which
    // takes in the digits.
    (E, "[a-z--@]+", "9-q@", 0, "(0,4)"),
    // Basic expressions, where the operators differ.
    (0, "a\\{2\\}", "aaa", 0, "(0,2)"),
    (0, "a\\|b", "b", 0, "(0,1)"),
    (0, "a\\+", "caa", 0, "(1,3)"),
    (0, "a\\?b", "b", 0, "(0,1)"),
    (0, "*a", "*a", 0, "(0,2)"),
    (0, "^*", "*", 0, "(0,1)"),
    (0, "a^b$c", "a^b$c", 0, "(0,5)"),
    (0, "\\(^a\\)", "a", 0, "(0,1)(0,1)"),
    (0, "\\(a$\\)", "a", 0, "(0,1)(0,1)"),
    (0, "a+", "a+", 0, "(0,2)"),
    // libc-test's regex-backref-0: \0 is not a back-reference.
    (0, "a\\0", "a0", 0, "(0,2)"),
    // Escapes, as musl expands them.
    (E, "\\<foo\\>", "a foo b", 0, "(2,5)"),
    (E, "\\bfoo\\b", "foobar", 0, "NOMATCH"),
    (E, "o\\B", "foo", 0, "(1,2)"),
    (E, "\\w+", " ab_1 ", 0, "(1,5)"),
    (E, "\\d\\D", "x5y", 0, "(1,3)"),
    (E, "a\\tb", "a\tb", 0, "(0,3)"),
    (E, "\\x41+", "zAA", 0, "(1,3)"),
    // POSIX's leftmost-longest rule, subexpression by subexpression. These
    // are Fowler's cases, which glibc and musl are also measured against.
    (E, "(a|ab)(c|bcd)(d*)", "abcd", 0, "(0,4)(0,2)(2,3)(3,4)"),
    (E, "(a|ab)(bcd|c)(d*)", "abcd", 0, "(0,4)(0,2)(2,3)(3,4)"),
    (E, "(ab|a)(c|bcd)(d*)", "abcd", 0, "(0,4)(0,2)(2,3)(3,4)"),
    (E, "(ab|a)(bcd|c)(d*)", "abcd", 0, "(0,4)(0,2)(2,3)(3,4)"),
    (E, "(a*)(b|abc)(c*)", "abc", 0, "(0,3)(0,1)(1,2)(2,3)"),
    (E, "(a*)(ab)*(b*)", "abc", 0, "(0,2)(0,1)(?,?)(1,2)"),
    (
        E,
        "(wee|week)(knights|night)",
        "weeknights",
        0,
        "(0,10)(0,3)(3,10)",
    ),
    (E, "(ab|a)(bc|c)", "abc", 0, "(0,3)(0,2)(2,3)"),
    (E, "([abc])*d", "abbbcd", 0, "(0,6)(4,5)"),
    (E, "([abc])*bcd", "abcd", 0, "(0,4)(0,1)"),
    (E, "(a|b)*c|(a|ab)*c", "abc", 0, "(0,3)(1,2)(?,?)"),
    (E, "(.a|.b).*|.*(.a|.b)", "xa", 0, "(0,2)(0,2)(?,?)"),
    (E, "(a)|(b)", "b", 0, "(0,1)(?,?)(0,1)"),
    (E, "x(a)?(b)?", "xb", 0, "(0,2)(?,?)(1,2)"),
    (E, "(.*)(.*)", "ab", 0, "(0,2)(0,2)(2,2)"),
    // A repetition that can match nothing still reports one iteration, and
    // a group keeps the iteration that reached it.
    (E, "(a*)*", "b", 0, "(0,0)(0,0)"),
    (E, "(a*)+", "b", 0, "(0,0)(0,0)"),
    (E, "((a*|b))*", "-", 0, "(0,0)(0,0)(0,0)"),
    (E, "(a*|b)*", "b", 0, "(0,1)(0,1)"),
    (E, "(a+|b)*", "ab", 0, "(0,2)(1,2)"),
    (E, "(a+|b){0,}", "ab", 0, "(0,2)(1,2)"),
    (E, "(a+|b)+", "ab", 0, "(0,2)(1,2)"),
    (E, "(a+|b)?", "ab", 0, "(0,1)(0,1)"),
    (E, "(a*)*(x)", "x", 0, "(0,1)(0,0)(0,1)"),
    (E, "(a*)*(x)", "ax", 0, "(0,2)(0,1)(1,2)"),
    (E, "(a*)+(x)", "x", 0, "(0,1)(0,0)(0,1)"),
    (E, "(a*){2}(x)", "x", 0, "(0,1)(0,0)(0,1)"),
    (E, "((a)|b)*", "ab", 0, "(0,2)(1,2)(0,1)"),
    (E, "(a|b)*", "", 0, "(0,0)(?,?)"),
    // Back-references, which only a BRE has.
    (0, "\\(a*\\)\\1", "aaaa", 0, "(0,4)(0,2)"),
    (0, "\\(a*\\)\\1", "aaa", 0, "(0,2)(0,1)"),
    (0, "\\(.\\)\\1", "abccd", 0, "(2,4)(2,3)"),
    (0, "\\(a\\|b\\)*\\1", "abb", 0, "(0,3)(1,2)"),
    (0, "\\(a\\)\\(b\\)\\2\\1", "xabba", 0, "(1,5)(1,2)(2,3)"),
    (0, "\\(a\\)*\\1", "aa", 0, "(0,2)(0,1)"),
    (0, "^\\(.*\\)\n\\1$", "abc\nabc", 0, "(0,7)(0,3)"),
    (0, "\\(.*\\)x\\1", "abxab", 0, "(0,5)(0,2)"),
    (0, "\\(a\\)\\1", "aA", 0, "NOMATCH"),
    (I, "\\(a\\)\\1", "aA", 0, "(0,2)(0,1)"),
    // libc-test's regex-ere-backref: an ERE has none, so \1 is a 1.
    (E, "(a)\\1", "aa", 0, "NOMATCH"),
    // REG_ICASE.
    (I, "abc", "xABC", 0, "(1,4)"),
    (I | E, "[[:upper:]]+", "abC", 0, "(0,3)"),
    (I, "[[:upper:]]", "ab", 0, "(0,1)"),
    // libc-test's regex-bracket-icase, from Austin Group bug 872.
    (I, "[^aBcC]", "b", 0, "NOMATCH"),
    (I, "[^aBcC]", "C", 0, "NOMATCH"),
    (I, "[^aBcC]", "D", 0, "(0,1)"),
    // libc-test's regex-negated-range.
    (0, "[^aa-z]", "k", 0, "NOMATCH"),
    // REG_NEWLINE, and the flags regexec takes.
    (N, "^b", "a\nb", 0, "(2,3)"),
    (0, "^b", "a\nb", 0, "NOMATCH"),
    (N, "a$", "a\nb", 0, "(0,1)"),
    (N, ".", "\n", 0, "NOMATCH"),
    (0, ".", "\n", 0, "(0,1)"),
    (N, "[^a]", "\n", 0, "NOMATCH"),
    (0, "[^a]", "\n", 0, "(0,1)"),
    (N | E, "a.*", "ab\nac", 0, "(0,2)"),
    (0, "^a", "a", REG_NOTBOL, "NOMATCH"),
    (N, "^a", "b\na", REG_NOTBOL, "(2,3)"),
    (0, "a$", "a", REG_NOTEOL, "NOMATCH"),
    (N, "a$", "a\n", REG_NOTEOL, "(0,1)"),
    (0, "a", "a", REG_NOTBOL | REG_NOTEOL, "(0,1)"),
    // Patterns that do not compile.
    (E, "(a", "", 0, "E8"),
    (0, "\\(a", "", 0, "E8"),
    (0, "a\\)", "", 0, "E8"),
    (0, "\\1", "", 0, "E6"),
    (0, "a\\", "", 0, "E5"),
    (E, "*a", "", 0, "E13"),
    (E, "a|*", "", 0, "E13"),
    (E, "a{1", "", 0, "E9"),
    (E, "a{,2}", "", 0, "E10"),
    (E, "a{3,2}", "", 0, "E10"),
    (E, "a{256}", "", 0, "E10"),
    (0, "[a", "", 0, "E7"),
    (0, "[z-a]", "", 0, "E11"),
    (0, "[[:foo:]]", "", 0, "E4"),
    (0, "[[.ab.]]", "", 0, "E3"),
];

#[test]
fn every_case_matches_where_posix_says() {
    let mut wrong = Vec::new();
    for &(cflags, pat, s, eflags, want) in CASES {
        let got = ours(cflags, pat, s, eflags);
        if got != want {
            wrong.push(format!("/{pat}/ ~ {s:?} [{cflags}]: {got}, wanted {want}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// glibc's `regex.h`, which the test binary links: its `regex_t` is 64 bytes
/// with `re_nsub` at offset 48, and its `regoff_t` is an `int`.
mod host {
    use core::ffi::{c_char, c_int};

    /// Room for glibc's `regex_t`, with more than enough space.
    #[repr(C, align(8))]
    pub(super) struct Regex(pub(super) [u8; 128]);

    /// glibc's `regmatch_t`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct Match {
        /// Where the match starts, or -1.
        pub(super) rm_so: c_int,
        /// Where it ends, or -1.
        pub(super) rm_eo: c_int,
    }

    unsafe extern "C" {
        #[link_name = "regcomp"]
        pub(super) fn host_regcomp(preg: *mut Regex, pat: *const c_char, cflags: c_int) -> c_int;
        #[link_name = "regexec"]
        pub(super) fn host_regexec(
            preg: *const Regex,
            s: *const c_char,
            nmatch: usize,
            pmatch: *mut Match,
            eflags: c_int,
        ) -> c_int;
        #[link_name = "regfree"]
        pub(super) fn host_regfree(preg: *mut Regex);
    }
}

/// What the host glibc answers, in the same notation. How many groups the
/// pattern has is a property of the pattern, so it is taken from this
/// library rather than read out of glibc's `regex_t`, whose layout differs.
fn theirs(cflags: c_int, pat: &str, s: &str, eflags: c_int, nsub: usize) -> String {
    let pattern = CString::new(pat).unwrap();
    let string = CString::new(s).unwrap();
    let mut re = host::Regex([0; 128]);
    // SAFETY: glibc fills the structure, which is larger than its `regex_t`.
    let code = unsafe { host::host_regcomp(&raw mut re, pattern.as_ptr(), cflags) };
    if code != 0 {
        return format!("E{code}");
    }
    let mut found = vec![
        host::Match {
            rm_so: -2,
            rm_eo: -2
        };
        nsub + 1
    ];
    // SAFETY: the pattern is compiled, and `found` has `nsub + 1` entries.
    let code = unsafe {
        host::host_regexec(
            &raw const re,
            string.as_ptr(),
            found.len(),
            found.as_mut_ptr(),
            eflags,
        )
    };
    // SAFETY: the pattern is compiled and not used again.
    unsafe { host::host_regfree(&raw mut re) };
    report(
        code,
        found
            .iter()
            .map(|m| (i64::from(m.rm_so), i64::from(m.rm_eo))),
    )
}

/// The cases where this library and glibc answer differently, by pattern and
/// string, with why.
const GLIBC_DIFFERS: &[(&str, &str, &str)] = &[
    (
        "(a)\\1",
        "a1",
        "POSIX gives an ERE no back-references, so `\\1` is the character 1, \
         as musl has it. glibc takes it as a back-reference.",
    ),
    (
        "(a)\\1",
        "aa",
        "The same difference, from libc-test's regex-ere-backref.",
    ),
    (
        "[a-z--@]+",
        "9-q@",
        "musl's `--` range extension, which glibc rejects with REG_ERANGE.",
    ),
    (
        "\\d\\D",
        "x5y",
        "musl expands `\\d` and `\\D` to the digit class and its complement; \
         to glibc they are the letters.",
    ),
    (
        "a\\tb",
        "a\tb",
        "musl expands `\\t` to a tab; to glibc it is the letter t.",
    ),
    (
        "\\x41+",
        "zAA",
        "musl's `\\xHH` names a character by its value; to glibc it is the \
         letter x.",
    ),
    (
        "(a|ab)(c|bcd)(d*)",
        "abcd",
        "POSIX wants each subexpression, left to right, as long as it can be, \
         so group 1 is `ab`. glibc answers (0,1)(1,4)(4,4), which is a match \
         but not the longest-subexpression one; this is the classic case \
         glibc has never got right.",
    ),
    (
        "(a|ab)(bcd|c)(d*)",
        "abcd",
        "The same case with the alternatives the other way round.",
    ),
    (
        "a{,2}",
        "",
        "`{,2}` is REG_BADBR in musl, which POSIX allows, since `{,` does not \
         begin a valid interval. glibc reads it as `{0,2}`.",
    ),
    (
        "a{256}",
        "",
        "A count above RE_DUP_MAX (255) is REG_BADBR in musl; glibc compiles it.",
    ),
];

#[test]
fn the_host_glibc_answers_the_same_or_is_listed() {
    let mut unlisted = Vec::new();
    for &(cflags, pat, s, eflags, _) in CASES {
        let ours = ours(cflags, pat, s, eflags);
        let theirs = theirs(cflags, pat, s, eflags, groups(cflags, pat));
        if ours == theirs {
            continue;
        }
        let listed = GLIBC_DIFFERS
            .iter()
            .any(|&(p, string, _)| p == pat && string == s);
        if !listed {
            unlisted.push(format!(
                "/{pat}/ ~ {s:?} [{cflags}]: ours {ours}, glibc {theirs}"
            ));
        }
    }
    assert!(unlisted.is_empty(), "{}", unlisted.join("\n"));
}

#[test]
fn nosub_and_short_match_arrays_are_honoured() {
    let pat = CString::new("(a)(b)").unwrap();
    let string = CString::new("zab").unwrap();
    let mut re = MaybeUninit::<Regex>::zeroed();
    // SAFETY: a `regex_t` to fill and a pattern to compile.
    let code = unsafe { regcomp(re.as_mut_ptr(), pat.as_ptr(), REG_EXTENDED) };
    assert_eq!(code, REG_OK);
    // SAFETY: `regcomp` filled the structure.
    assert_eq!(unsafe { re.assume_init_ref() }.re_nsub, 2);
    // More entries than the pattern has groups: the rest are -1.
    let mut found = [RegMatch {
        rm_so: -2,
        rm_eo: -2,
    }; 5];
    // SAFETY: the pattern is compiled and there is room for five entries.
    let code = unsafe { regexec(re.as_ptr(), string.as_ptr(), 5, found.as_mut_ptr(), 0) };
    assert_eq!(code, REG_OK);
    let seen: Vec<_> = found.iter().map(|m| (m.rm_so, m.rm_eo)).collect();
    assert_eq!(seen, [(1, 3), (1, 2), (2, 3), (-1, -1), (-1, -1)]);
    // Fewer: only those are written.
    let mut two = [RegMatch {
        rm_so: -2,
        rm_eo: -2,
    }; 2];
    // SAFETY: as above, with room for two.
    let code = unsafe { regexec(re.as_ptr(), string.as_ptr(), 2, two.as_mut_ptr(), 0) };
    assert_eq!(code, REG_OK);
    assert_eq!((two[0].rm_so, two[0].rm_eo, two[1].rm_so), (1, 3, 1));
    // SAFETY: the pattern is compiled and not used again.
    unsafe { regfree(re.as_mut_ptr()) };

    // libc-test's regexec-nosub: a non-zero nmatch with REG_NOSUB and no
    // array at all must not be written through.
    let mut re = MaybeUninit::<Regex>::zeroed();
    let pat = CString::new("abc").unwrap();
    let string = CString::new("zyx abc").unwrap();
    // SAFETY: a `regex_t` to fill and a pattern to compile.
    let code = unsafe { regcomp(re.as_mut_ptr(), pat.as_ptr(), REG_NOSUB) };
    assert_eq!(code, REG_OK);
    // SAFETY: the pattern is compiled; REG_NOSUB means pmatch is not read.
    let code = unsafe { regexec(re.as_ptr(), string.as_ptr(), 1, null_mut(), 0) };
    assert_eq!(code, REG_OK);
    // SAFETY: the pattern is compiled and not used again.
    unsafe { regfree(re.as_mut_ptr()) };
}

#[test]
fn regerror_gives_musls_messages_and_the_room_they_need() {
    let mut buf = [0 as c_char; 64];
    // SAFETY: the buffer holds 64 bytes.
    let n = unsafe { regerror(REG_BADBR, core::ptr::null(), buf.as_mut_ptr(), buf.len()) };
    // SAFETY: `regerror` NUL-terminated it.
    let text = unsafe { CStr::from_ptr(buf.as_ptr()) };
    assert_eq!(text.to_bytes(), b"Invalid contents of {}");
    assert_eq!(n, text.to_bytes().len() + 1);

    // A short buffer is cut, and the whole size is still returned.
    let mut small = [0 as c_char; 5];
    // SAFETY: the buffer holds five bytes.
    let n = unsafe { regerror(REG_NOMATCH, core::ptr::null(), small.as_mut_ptr(), 5) };
    // SAFETY: `regerror` NUL-terminated it.
    let cut = unsafe { CStr::from_ptr(small.as_ptr()) };
    assert_eq!(cut.to_bytes(), b"No m");
    assert_eq!(n, b"No match".len() + 1);

    // No buffer at all: only the size.
    // SAFETY: a size of zero writes nothing.
    let n = unsafe { regerror(REG_ESPACE, core::ptr::null(), null_mut(), 0) };
    assert_eq!(n, b"Out of memory".len() + 1);

    for (code, want) in [
        (REG_OK, "No error"),
        (REG_EESCAPE, "Trailing backslash"),
        (REG_ESUBREG, "Invalid back reference"),
        (REG_EBRACK, "Missing ']'"),
        (REG_EPAREN, "Missing ')'"),
        (REG_EBRACE, "Missing '}'"),
        (REG_BADRPT, "Repetition not preceded by valid expression"),
        (999, "Unknown error"),
        (-1, "Unknown error"),
    ] {
        let mut buf = [0 as c_char; 64];
        // SAFETY: the buffer holds 64 bytes.
        let _ = unsafe { regerror(code, core::ptr::null(), buf.as_mut_ptr(), buf.len()) };
        // SAFETY: `regerror` NUL-terminated it.
        let text = unsafe { CStr::from_ptr(buf.as_ptr()) };
        assert_eq!(text.to_str().unwrap_or_default(), want, "code {code}");
    }
}

/// How long a pattern that would make a backtracking matcher explode may
/// take. Each of these finishes in milliseconds; the limit is loose so that
/// an unoptimised build on a busy machine does not fail, while an exponential
/// matcher would never reach it.
const PATIENCE: Duration = Duration::from_secs(20);

#[test]
fn nothing_blows_up_on_a_long_run_of_the_same_byte() {
    let many = "a".repeat(100_000);
    let started = Instant::now();
    // The classic cases: without a `b` anywhere, a backtracking matcher
    // tries every way of splitting the run.
    assert_eq!(ours(E, "(a*)*b", &many, 0), "NOMATCH");
    assert_eq!(ours(E, "(a|aa)+c", &many, 0), "NOMATCH");
    assert_eq!(ours(E, "(a*)(a*)(a*)d", &many, 0), "NOMATCH");
    assert_eq!(ours(0, "\\(a*\\)*b", &many, 0), "NOMATCH");
    let xs = "x".repeat(20_000);
    assert_eq!(ours(E, "(x+x+)+y", &xs, 0), "NOMATCH");
    assert!(started.elapsed() < PATIENCE, "{:?}", started.elapsed());
}

#[test]
fn a_long_match_is_split_among_its_groups_quickly() {
    let n = 50_000;
    let many = format!("{}b", "a".repeat(n));
    let started = Instant::now();
    assert_eq!(ours(E, "(a*)*b", &many, 0), format!("(0,{})(0,{n})", n + 1));
    assert_eq!(
        ours(E, "(a)*b", &many, 0),
        format!("(0,{})({},{n})", n + 1, n - 1)
    );
    let pairs = "ab".repeat(n / 2);
    assert_eq!(
        ours(E, "(ab|a)*", &pairs, 0),
        format!("(0,{})({},{})", n, n - 2, n)
    );
    assert!(started.elapsed() < PATIENCE, "{:?}", started.elapsed());
}

#[test]
fn a_pattern_that_is_freed_can_be_compiled_again() {
    let mut re = MaybeUninit::<Regex>::zeroed();
    let pat = CString::new("a(b)c").unwrap();
    for _ in 0..3 {
        // SAFETY: a `regex_t` to fill and a pattern to compile.
        assert_eq!(unsafe { regcomp(re.as_mut_ptr(), pat.as_ptr(), 0) }, REG_OK);
        // SAFETY: the pattern is compiled and not used again.
        unsafe { regfree(re.as_mut_ptr()) };
        // A second free does nothing, because the first cleared the pointer.
        // SAFETY: as above.
        unsafe { regfree(re.as_mut_ptr()) };
    }
}

#[test]
fn the_layouts_are_glibcs() {
    // What a program compiled against glibc's `regex.h` reads and allocates.
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(size_of::<Regex>(), 64);
        assert_eq!(offset_of!(Regex, re_nsub), 48);
    }
    #[cfg(target_pointer_width = "32")]
    {
        assert_eq!(size_of::<Regex>(), 32);
        assert_eq!(offset_of!(Regex, re_nsub), 24);
    }
    assert_eq!(size_of::<RegMatch>(), 8);
}

#[test]
fn startend_matches_inside_a_span_and_reports_from_the_string() {
    let mut re = MaybeUninit::<Regex>::zeroed();
    let pat = CString::new("b+").unwrap();
    // SAFETY: a `regex_t` to fill and a pattern to compile.
    let code = unsafe { regcomp(re.as_mut_ptr(), pat.as_ptr(), REG_EXTENDED) };
    assert_eq!(code, REG_OK);
    // No NUL after the span, and a `b` just past it that must not count.
    let text = b"xbbxbbb";
    let mut found = [RegMatch { rm_so: 2, rm_eo: 5 }];
    // SAFETY: the span is inside `text`, and `found` has one entry.
    let code = unsafe {
        regexec(
            re.as_ptr(),
            text.as_ptr().cast(),
            1,
            found.as_mut_ptr(),
            REG_STARTEND,
        )
    };
    assert_eq!(code, REG_OK);
    assert_eq!((found[0].rm_so, found[0].rm_eo), (2, 3));
    // SAFETY: the pattern is compiled and not used again.
    unsafe { regfree(re.as_mut_ptr()) };
}

#[test]
fn re_search_finds_forwards_backwards_and_fills_its_registers() {
    const RE_SYNTAX_POSIX_EXTENDED_ISH: usize = RE_NO_BK_PARENS;
    let old = re_set_syntax(RE_SYNTAX_POSIX_EXTENDED_ISH);
    let mut re = MaybeUninit::<Regex>::zeroed();
    let pattern = b"(b+)c";
    // SAFETY: the pattern's bytes and a zeroed `regex_t`.
    let error =
        unsafe { re_compile_pattern(pattern.as_ptr().cast(), pattern.len(), re.as_mut_ptr()) };
    assert!(error.is_null());
    let _ = re_set_syntax(old);

    let text = b"abbc abbbc";
    let mut regs = Registers {
        num_regs: 0,
        start: null_mut(),
        end: null_mut(),
    };
    // Forwards from 0: the first match starts at 1.
    // SAFETY: the pattern is compiled, the text is `len` bytes, and the
    // registers are unallocated, as a zeroed `regex_t` says.
    let at = unsafe {
        re_search(
            re.as_mut_ptr(),
            text.as_ptr().cast(),
            10,
            0,
            10,
            &raw mut regs,
        )
    };
    assert_eq!(at, 1);
    assert!(regs.num_regs >= 2, "at least the match and its group");
    // The first `n` registers, as start and end pairs.
    let spans = |regs: &Registers, n: usize| -> Vec<(c_int, c_int)> {
        // SAFETY: `re_search` allocated `num_regs` entries, at least `n`.
        let starts = unsafe { core::slice::from_raw_parts(regs.start, n) };
        // SAFETY: as above.
        let ends = unsafe { core::slice::from_raw_parts(regs.end, n) };
        starts.iter().copied().zip(ends.iter().copied()).collect()
    };
    assert_eq!(spans(&regs, 2), [(1, 4), (1, 3)]);
    // Forwards from 4 with a range that stops before the second match, at 6.
    // SAFETY: as above; the registers are now this library's to grow.
    let at = unsafe {
        re_search(
            re.as_mut_ptr(),
            text.as_ptr().cast(),
            10,
            4,
            1,
            &raw mut regs,
        )
    };
    assert_eq!(at, -1);
    // Backwards from 9: a match must start where it is tried, and the
    // first place going back where one does is 8, the `bc` inside `bbbc`.
    // SAFETY: as above.
    let at = unsafe {
        re_search(
            re.as_mut_ptr(),
            text.as_ptr().cast(),
            10,
            9,
            -9,
            &raw mut regs,
        )
    };
    assert_eq!(at, 8);
    assert_eq!(spans(&regs, 1), [(8, 10)]);
    // SAFETY: the array came from `malloc`.
    unsafe { free(regs.start.cast()) };
    // SAFETY: as above.
    unsafe { free(regs.end.cast()) };
    // SAFETY: the pattern is compiled and not used again.
    unsafe { regfree(re.as_mut_ptr()) };
}
