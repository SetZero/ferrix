//! `fnmatch.h`: matching a string against a shell wildcard pattern.
//!
//! The algorithm is musl 1.2.5's (MIT), `src/regex/fnmatch.c`, Rich Felker's
//! "sea of stars". A pattern is split into a head, the tokens before its first
//! `*`; a tail, the tokens after its last `*`; and the components between. The
//! head is matched from the string's start and the tail against its end, and
//! each component is then found at its first occurrence after the previous
//! one. That first occurrence is always the right choice, so the time is at
//! most the product of the pattern's and the string's lengths, however many
//! stars there are.
//!
//! This library has only the C locale, so a character is a byte: bytes from
//! 128 to 255 are ordinary characters, and no sequence is invalid. Case folding
//! and the classes are ASCII's.
//!
//! Brackets take ranges, `!` or `^` negation, and the classes `[:alpha:]` and
//! the rest. An unknown class matches nothing. musl ignores collating symbols
//! and equivalence classes; here `[.c.]` and `[=c=]` naming one byte match that
//! byte, which is what they mean in the C locale, as in glibc.
//!
//! With `FNM_PATHNAME` the pattern and the string are matched a `/`-separated
//! component at a time, so no wildcard or bracket matches a `/`. With
//! `FNM_PERIOD` a leading `.`, of the string or with `FNM_PATHNAME` of any
//! component, matches only a `.` in the pattern, escaped or not. With
//! `FNM_LEADING_DIR` the pattern may match a leading part of the string that
//! ends before a `/`.

use core::ffi::{CStr, c_char, c_int};

use crate::ctype;

/// `FNM_PATHNAME`: wildcards do not match `/`.
pub(crate) const FNM_PATHNAME: c_int = 0x1;
/// `FNM_NOESCAPE`: a backslash is an ordinary character.
pub(crate) const FNM_NOESCAPE: c_int = 0x2;
/// `FNM_PERIOD`: a leading `.` must be matched by a `.`.
pub(crate) const FNM_PERIOD: c_int = 0x4;
/// `FNM_LEADING_DIR`: the pattern may match up to a `/`.
const FNM_LEADING_DIR: c_int = 0x8;
/// `FNM_CASEFOLD`: letters match either case.
const FNM_CASEFOLD: c_int = 0x10;
/// `FNM_NOMATCH`, the result when the string does not match.
pub(crate) const FNM_NOMATCH: c_int = 1;

/// One element of a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// The pattern's end.
    End,
    /// A complete bracket expression.
    Bracket,
    /// `?`.
    Question,
    /// `*`.
    Star,
    /// A byte that matches itself.
    Byte(u8),
}

/// The byte at `i`, or 0 past the end, standing for C's NUL.
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// `s` from `from` on, empty if `from` is past the end.
fn tail(s: &[u8], from: usize) -> &[u8] {
    s.get(from..).unwrap_or(&[])
}

/// `s[from..to]`, empty if that is not within `s`.
fn span(s: &[u8], from: usize, to: usize) -> &[u8] {
    s.get(from..to).unwrap_or(&[])
}

/// The first token of `pat`, and how many bytes it takes.
fn next_token(pat: &[u8], flags: c_int) -> (Token, usize) {
    let Some(&first) = pat.first() else {
        return (Token::End, 0);
    };
    if first == b'\\' && flags & FNM_NOESCAPE == 0 && at(pat, 1) != 0 {
        return (Token::Byte(at(pat, 1)), 2);
    }
    match first {
        b'[' => bracket_token(pat),
        b'*' => (Token::Star, 1),
        b'?' => (Token::Question, 1),
        _ => (Token::Byte(first), 1),
    }
}

/// The token at a `[`: a bracket expression if it is closed, and otherwise a
/// literal `[`.
fn bracket_token(pat: &[u8]) -> (Token, usize) {
    let len = pat.len();
    let mut k = 1;
    if matches!(at(pat, k), b'^' | b'!') {
        k += 1;
    }
    // A `]` first in the list is a member, not the end.
    if at(pat, k) == b']' {
        k += 1;
    }
    while k < len && at(pat, k) != b']' {
        if at(pat, k) == b'[' && matches!(at(pat, k + 1), b':' | b'.' | b'=') {
            let delimiter = at(pat, k + 1);
            k += 2;
            if k < len {
                k += 1;
            }
            while k < len && !(at(pat, k - 1) == delimiter && at(pat, k) == b']') {
                k += 1;
            }
            if k >= len {
                break;
            }
        }
        k += 1;
    }
    if k >= len {
        (Token::Byte(b'['), 1)
    } else {
        (Token::Bracket, k + 1)
    }
}

/// Whether `c` is in the character class `name`, in the C locale. An unknown
/// class holds nothing.
pub(crate) fn in_class(name: &[u8], c: u8) -> bool {
    let test: extern "C" fn(c_int) -> c_int = match name {
        b"alnum" => ctype::isalnum,
        b"alpha" => ctype::isalpha,
        b"blank" => ctype::isblank,
        b"cntrl" => ctype::iscntrl,
        b"digit" => ctype::isdigit,
        b"graph" => ctype::isgraph,
        b"lower" => ctype::islower,
        b"print" => ctype::isprint,
        b"punct" => ctype::ispunct,
        b"space" => ctype::isspace,
        b"upper" => ctype::isupper,
        b"xdigit" => ctype::isxdigit,
        _ => return false,
    };
    test(c_int::from(c)) != 0
}

/// `c` in the other case, if it is an ASCII letter.
const fn other_case(c: u8) -> u8 {
    if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c.to_ascii_uppercase()
    }
}

/// Whether the bracket expression `p`, from its `[` to its `]`, matches `k`,
/// or `kfold`, the same byte in the other case when folding.
fn match_bracket(p: &[u8], k: u8, kfold: u8) -> bool {
    let len = p.len();
    let mut i = 1;
    let mut negated = false;
    if matches!(at(p, i), b'^' | b'!') {
        negated = true;
        i += 1;
    }
    // A leading `]` or `-` is a member.
    if matches!(at(p, i), b']' | b'-') {
        if k == at(p, i) {
            return !negated;
        }
        i += 1;
    }
    // The byte before the current one, the start of a range.
    let mut previous = at(p, i.wrapping_sub(1));
    while i < len && at(p, i) != b']' {
        let c = at(p, i);
        if c == b'-' && at(p, i + 1) != b']' {
            let high = at(p, i + 1);
            let range = previous..=high;
            if previous <= high && (range.contains(&k) || range.contains(&kfold)) {
                return !negated;
            }
            i += 2;
            continue;
        }
        if c == b'[' && matches!(at(p, i + 1), b':' | b'.' | b'=') {
            let delimiter = at(p, i + 1);
            let start = i + 2;
            i += 3;
            while i < len && !(at(p, i - 1) == delimiter && at(p, i) == b']') {
                i += 1;
            }
            let name = span(p, start, i.saturating_sub(1).max(start));
            if element_matches(delimiter, name, k, kfold) {
                return !negated;
            }
            i += 1;
            continue;
        }
        if c == k || c == kfold {
            return !negated;
        }
        previous = c;
        i += 1;
    }
    negated
}

/// Whether the bracket element `[` `delimiter` `name` `delimiter` `]` matches
/// `k` or `kfold`: a class, or a collating symbol or equivalence class of one
/// byte.
fn element_matches(delimiter: u8, name: &[u8], k: u8, kfold: u8) -> bool {
    if delimiter == b':' {
        return in_class(name, k) || in_class(name, kfold);
    }
    matches!(name, [only] if *only == k || *only == kfold)
}

/// Whether `token`, whose bytes start `bytes`, matches the string byte `k`.
fn token_matches(token: Token, bytes: &[u8], k: u8, fold: bool) -> bool {
    let kfold = if fold { other_case(k) } else { k };
    match token {
        Token::Bracket => match_bracket(bytes, k, kfold),
        Token::Question => true,
        Token::Byte(c) => k == c || kfold == c,
        Token::End | Token::Star => false,
    }
}

/// What matching one component between stars found.
enum Component {
    /// It matched; the pattern and string positions after it.
    Matched(usize, usize),
    /// It does not match here.
    Mismatch,
    /// The string ran out.
    Exhausted,
}

/// Matches the component of `pat` at `p`, which ends with a star, at `s[t..]`.
fn component(pat: &[u8], mut p: usize, s: &[u8], mut t: usize, flags: c_int) -> Component {
    let fold = flags & FNM_CASEFOLD != 0;
    loop {
        let rest = tail(pat, p);
        let (token, inc) = next_token(rest, flags);
        if token == Token::Star {
            return Component::Matched(p + inc, t);
        }
        if token == Token::End {
            return Component::Mismatch;
        }
        let Some(&k) = s.get(t) else {
            return Component::Exhausted;
        };
        if !token_matches(token, span(rest, 0, inc), k, fold) {
            return Component::Mismatch;
        }
        p += inc;
        t += 1;
    }
}

/// Whether `pat` matches all of `s`, with no `/` handling.
fn match_whole(pat: &[u8], s: &[u8], flags: c_int) -> bool {
    let fold = flags & FNM_CASEFOLD != 0;
    if flags & FNM_PERIOD != 0
        && s.first() == Some(&b'.')
        && next_token(pat, flags).0 != Token::Byte(b'.')
    {
        return false;
    }

    // The head, up to the first star.
    let mut p = 0;
    let mut t = 0;
    loop {
        let rest = tail(pat, p);
        let (token, inc) = next_token(rest, flags);
        if token == Token::Star {
            p += inc;
            break;
        }
        let Some(&k) = s.get(t) else {
            return token == Token::End;
        };
        if !token_matches(token, span(rest, 0, inc), k, fold) {
            return false;
        }
        p += inc;
        t += 1;
    }
    let pat = tail(pat, p);
    let s = tail(s, t);

    // The tail, after the last star, and how many bytes it takes.
    let mut q = 0;
    let mut tail_start = 0;
    let mut tail_len = 0;
    while q < pat.len() {
        let (token, inc) = next_token(tail(pat, q), flags);
        if token == Token::Star {
            tail_len = 0;
            tail_start = q + inc;
        } else {
            tail_len += 1;
        }
        q += inc.max(1);
    }
    let Some(string_tail) = s.len().checked_sub(tail_len) else {
        return false;
    };
    let mut p = tail_start;
    let mut t = string_tail;
    loop {
        let rest = tail(pat, p);
        let (token, inc) = next_token(rest, flags);
        let Some(&k) = s.get(t) else {
            if token != Token::End {
                return false;
            }
            break;
        };
        if !token_matches(token, span(rest, 0, inc), k, fold) {
            return false;
        }
        p += inc;
        t += 1;
    }

    // The components between, each at its first occurrence.
    let pat = span(pat, 0, tail_start);
    let s = span(s, 0, string_tail);
    let mut p = 0;
    let mut t = 0;
    while p < pat.len() {
        match component(pat, p, s, t, flags) {
            Component::Matched(next_p, next_t) => {
                p = next_p;
                t = next_t;
            }
            Component::Mismatch => t += 1,
            Component::Exhausted => return false,
        }
    }
    true
}

/// Whether the pattern `pat` matches `s` under `flags`.
pub(crate) fn matches(pat: &[u8], s: &[u8], flags: c_int) -> bool {
    if flags & FNM_PATHNAME != 0 {
        return match_path(pat, s, flags);
    }
    if flags & FNM_LEADING_DIR != 0 {
        let prefix_matches = s
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'/' && match_whole(pat, span(s, 0, i), flags));
        if prefix_matches {
            return true;
        }
    }
    match_whole(pat, s, flags)
}

/// [`matches`] with `FNM_PATHNAME`: a component at a time.
fn match_path(mut pat: &[u8], mut s: &[u8], flags: c_int) -> bool {
    loop {
        let s_end = s.iter().position(|&b| b == b'/').unwrap_or(s.len());
        let mut p = 0;
        let (mut token, mut inc) = next_token(pat, flags);
        while token != Token::End && token != Token::Byte(b'/') {
            p += inc;
            (token, inc) = next_token(tail(pat, p), flags);
        }
        let pattern_slash = token != Token::End;
        let string_slash = s_end < s.len();
        if pattern_slash != string_slash && (!string_slash || flags & FNM_LEADING_DIR == 0) {
            return false;
        }
        if !match_whole(span(pat, 0, p), span(s, 0, s_end), flags) {
            return false;
        }
        if !pattern_slash {
            return true;
        }
        s = tail(s, s_end + 1);
        pat = tail(pat, p + inc);
    }
}

/// Matches `string` against the shell wildcard `pattern`. Returns 0 for a
/// match and `FNM_NOMATCH` otherwise.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fnmatch(
    pattern: *const c_char,
    string: *const c_char,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller passes a string.
    let pat = unsafe { CStr::from_ptr(pattern) }.to_bytes();
    // SAFETY: as above.
    let s = unsafe { CStr::from_ptr(string) }.to_bytes();
    if matches(pat, s, flags) {
        0
    } else {
        FNM_NOMATCH
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_questions_and_brackets() {
        assert!(matches(b"*.c", b"foo.c", 0));
        assert!(!matches(b"*.c", b"foo.h", 0));
        assert!(matches(b"a?c", b"abc", 0));
        assert!(matches(b"[!a-c]x", b"dx", 0));
        assert!(matches(b"[]]", b"]", 0));
        assert!(matches(b"[[:digit:]]*", b"7up", 0));
        assert!(matches(b"*a*b*c*", b"xxaxxbxxcxx", 0));
        assert!(!matches(b"*a*b*c*", b"xxaxxbxx", 0));
    }

    #[test]
    fn many_stars_take_linear_time() {
        let s = [b'a'; 5000];
        let pat = b"*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b";
        assert!(!matches(pat, &s, 0));
    }

    #[test]
    fn path_names_and_leading_dots() {
        assert!(!matches(b"a*", b"a/b", FNM_PATHNAME));
        assert!(matches(b"a/*", b"a/b", FNM_PATHNAME));
        assert!(!matches(b"*", b".x", FNM_PERIOD));
        assert!(matches(b"\\.x", b".x", FNM_PERIOD));
        assert!(matches(b"a", b"a/b/c", FNM_PATHNAME | FNM_LEADING_DIR));
        assert!(matches(b"a", b"A", FNM_CASEFOLD));
    }
}
