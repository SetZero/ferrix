//! `regex.h`: POSIX regular expressions, basic and extended.
//!
//! # How it works
//!
//! `regcomp` parses the pattern into a syntax tree ([`parse`]) and compiles
//! the tree into a Thompson automaton ([`program`]), each node a contiguous
//! run of instructions. `regexec` runs the automaton over the string once,
//! keeping every live state at once, to find the leftmost-longest match; that
//! takes time proportional to the string's length times the pattern's size,
//! whatever the pattern, so `(a*)*b` against a long run of `a`s is linear.
//! When the caller wants subexpressions, [`solve`] splits the match among
//! them top down, running parts of the automaton forwards and backwards over
//! parts of the match, which gives POSIX's answer exactly: each subexpression,
//! from left to right, as long as it can be, and a repeated group's last
//! iteration.
//!
//! A back-reference compiles to "any bytes, as many as its group can match",
//! so the automaton over-approximates, and the solver checks each candidate
//! match and backtracks among its choices. Only a pattern with
//! back-references can take more than polynomial time.
//!
//! # Why not TRE
//!
//! musl 1.2.5 uses TRE (MIT): a tagged automaton whose tags record where
//! subexpressions start and end, with a parallel matcher and a backtracking
//! one for back-references. Porting it faithfully is four thousand lines of
//! pointer-heavy C with its own allocator and stack, all to be rewritten
//! without indexing or panics, and TRE's tag priorities do not always give
//! POSIX's submatches. The automaton-and-solver design splits the problem so
//! that each part is small and checked on its own: a parser, a compiler with
//! simulations that are exact without back-references, and a solver whose
//! choices follow POSIX's rules directly. It keeps musl's grammar and error
//! codes, which [`parse`] documents.
//!
//! # Differences from musl and glibc
//!
//! * The layouts are musl's, from `include/regex.h`: `regex_t` is 64 bytes
//!   with `re_nsub` first, and `regoff_t` is a `long`, so `regmatch_t` is 16
//!   bytes. glibc's `regex_t` is also 64 bytes but keeps `re_nsub` at offset
//!   48, and its `regoff_t` is an `int`, making `regmatch_t` 8 bytes. A
//!   program built against glibc's header will need glibc's layout here.
//! * Matching is by bytes, as in the C locale, whatever the locale: a
//!   multibyte character is its bytes, and a byte that is not valid UTF-8 is
//!   not an error, where musl's `regcomp` returns `REG_BADPAT` in a UTF-8
//!   locale.
//! * A back-reference with `REG_ICASE` ignores case, as glibc's does; musl's
//!   compares bytes exactly.
//! * A group inside a repetition keeps its match from an earlier iteration
//!   when a later one does not reach it: `((a)|b)*` against `ab` reports
//!   group 2 at 0,1.
//! * `regerror`'s messages are musl's. glibc's words differ.
//! * `REG_STARTEND`, glibc's extension, is not in musl's header and not here.

mod parse;
mod program;
mod solve;

use core::ffi::{CStr, c_char, c_int, c_long, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::malloc::{free, malloc};
use program::{Input, Machine, Mem, Program};
use solve::{Solver, UNSET};

/// `REG_EXTENDED`: the pattern is an extended expression.
pub(crate) const REG_EXTENDED: c_int = 1;
/// `REG_ICASE`: letters match either case.
pub(crate) const REG_ICASE: c_int = 2;
/// `REG_NEWLINE`: newlines separate lines for `^`, `$`, `.` and `[^...]`.
pub(crate) const REG_NEWLINE: c_int = 4;
/// `REG_NOSUB`: `regexec` reports only whether there is a match.
pub(crate) const REG_NOSUB: c_int = 8;

/// `REG_NOTBOL`: the string's start is not the start of a line.
pub(crate) const REG_NOTBOL: c_int = 1;
/// `REG_NOTEOL`: the string's end is not the end of a line.
pub(crate) const REG_NOTEOL: c_int = 2;

/// Success.
pub(crate) const REG_OK: c_int = 0;
/// `regexec` found no match.
pub(crate) const REG_NOMATCH: c_int = 1;
/// The pattern is invalid.
pub(crate) const REG_BADPAT: c_int = 2;
/// An unknown collating element.
pub(crate) const REG_ECOLLATE: c_int = 3;
/// An unknown character class.
pub(crate) const REG_ECTYPE: c_int = 4;
/// A trailing backslash.
pub(crate) const REG_EESCAPE: c_int = 5;
/// A back-reference to a group that does not exist.
pub(crate) const REG_ESUBREG: c_int = 6;
/// An unclosed bracket expression.
pub(crate) const REG_EBRACK: c_int = 7;
/// Unbalanced parentheses.
pub(crate) const REG_EPAREN: c_int = 8;
/// An unclosed brace.
pub(crate) const REG_EBRACE: c_int = 9;
/// Invalid contents of a brace.
pub(crate) const REG_BADBR: c_int = 10;
/// An invalid range end.
pub(crate) const REG_ERANGE: c_int = 11;
/// Out of memory.
pub(crate) const REG_ESPACE: c_int = 12;
/// A repetition with nothing to repeat.
pub(crate) const REG_BADRPT: c_int = 13;

/// `regex_t`, in musl's layout; glibc's differs, as the module says.
#[repr(C)]
#[derive(Debug)]
pub struct Regex {
    /// `re_nsub`: how many parenthesised subexpressions the pattern has.
    pub re_nsub: usize,
    /// The compiled pattern, a [`Compiled`] from `malloc`, or null.
    opaque: *mut c_void,
    /// Unused, as in musl.
    _padding: [*mut c_void; 4],
    /// Unused, as in musl.
    _nsub2: usize,
    /// Unused, as in musl.
    _padding2: c_char,
}

const _: () = assert!(size_of::<Regex>() == 64);
const _: () = assert!(offset_of!(Regex, re_nsub) == 0);
const _: () = assert!(offset_of!(Regex, opaque) == 8);

/// `regmatch_t`, in musl's layout: `regoff_t` is a `long`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RegMatch {
    /// Where the match starts, or -1.
    pub rm_so: c_long,
    /// Where it ends, or -1.
    pub rm_eo: c_long,
}

const _: () = assert!(size_of::<RegMatch>() == 16);
const _: () = assert!(offset_of!(RegMatch, rm_eo) == 8);

/// What `regcomp` leaves behind `regex_t`.
#[derive(Debug)]
struct Compiled {
    /// The parsed pattern.
    ast: parse::Ast,
    /// Its automaton.
    program: Program,
    /// The flags it was compiled with.
    cflags: c_int,
}

/// Compiles `pattern` under `cflags` into `*preg`. Returns 0, or an error
/// code for `regerror`.
///
/// # Safety
///
/// `preg` must be writable as a `regex_t`, and `pattern` a NUL-terminated
/// string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn regcomp(preg: *mut Regex, pattern: *const c_char, cflags: c_int) -> c_int {
    // SAFETY: the caller passes a string.
    let pat = unsafe { CStr::from_ptr(pattern) }.to_bytes();
    let ast = match parse::parse(pat, cflags) {
        Ok(ast) => ast,
        Err(code) => return code,
    };
    let Ok(program) = Program::compile(&ast) else {
        return REG_ESPACE;
    };
    let groups = ast.groups as usize;
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "malloc aligns to 16, more than the structure needs"
    )]
    let compiled = malloc(size_of::<Compiled>()).cast::<Compiled>();
    if compiled.is_null() {
        return REG_ESPACE;
    }
    // SAFETY: the allocation is new, large enough and aligned.
    unsafe {
        compiled.write(Compiled {
            ast,
            program,
            cflags,
        })
    };
    // SAFETY: the caller passes a writable `regex_t`.
    unsafe { (*preg).re_nsub = groups };
    // SAFETY: as above.
    unsafe { (*preg).opaque = compiled.cast() };
    REG_OK
}

/// Reports the match `s..e` and, for the entries after it, nothing.
fn report_plain(s: usize, e: usize, want: usize, report: &mut dyn FnMut(usize, usize, usize)) {
    for i in 0..want {
        let (so, eo) = if i == 0 { (s, e) } else { (UNSET, UNSET) };
        report(i, so, eo);
    }
}

/// Reports the match `s..e` and the groups the solver put inside it.
fn report_solved(
    solver: &Solver<'_, '_>,
    s: usize,
    e: usize,
    want: usize,
    report: &mut dyn FnMut(usize, usize, usize),
) {
    for i in 0..want {
        let (so, eo) = if i == 0 { (s, e) } else { solver.capture(i) };
        report(i, so, eo);
    }
}

/// Where the match of `compiled` in `bytes` is, reporting it and the first
/// `want - 1` groups through `report` as index, start and end, with
/// [`UNSET`] for a group that did not match. Returns whether there is one.
fn execute(
    compiled: &Compiled,
    bytes: &[u8],
    eflags: c_int,
    want: usize,
    report: &mut dyn FnMut(usize, usize, usize),
) -> Mem<bool> {
    let ast = &compiled.ast;
    let input = Input {
        bytes,
        notbol: eflags & REG_NOTBOL != 0,
        noteol: eflags & REG_NOTEOL != 0,
        newline: compiled.cflags & REG_NEWLINE != 0,
        icase: compiled.cflags & REG_ICASE != 0,
    };
    let mut machine = Machine::new(&compiled.program, ast.sets.as_slice(), input)?;
    if !ast.has_backrefs {
        let Some((s, e)) = machine.search(0, want == 0)? else {
            return Ok(false);
        };
        if want > 1 && ast.groups > 0 {
            let mut solver = Solver::new(ast, &mut machine)?;
            if solver.solve(s, e)? {
                report_solved(&solver, s, e, want, report);
            } else {
                report_plain(s, e, want, report);
            }
        } else {
            report_plain(s, e, want, report);
        }
        return Ok(true);
    }
    // The search over-approximates back-references, so its start is only
    // the first place a match can start. Each start, then each end from the
    // longest, is checked until one holds.
    let Some((first, _)) = machine.search(0, false)? else {
        return Ok(false);
    };
    let mut solver = Solver::new(ast, &mut machine)?;
    for s in first..=bytes.len() {
        let ends = solver.ends_from(s)?;
        let mut next = ends.highest_at_most(bytes.len());
        while let Some(e) = next {
            if solver.solve(s, e)? {
                report_solved(&solver, s, e, want, report);
                return Ok(true);
            }
            next = e.checked_sub(1).and_then(|p| ends.highest_at_most(p));
        }
    }
    Ok(false)
}

/// Matches `string` against the pattern `preg` holds. Returns 0 and fills
/// the first `nmatch` entries of `pmatch` with the match and its groups, or
/// returns `REG_NOMATCH`.
///
/// # Safety
///
/// `preg` must hold a pattern `regcomp` compiled and `regfree` has not freed,
/// `string` must be a NUL-terminated string, and `pmatch` must have room for
/// `nmatch` entries unless the pattern was compiled with `REG_NOSUB`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn regexec(
    preg: *const Regex,
    string: *const c_char,
    nmatch: usize,
    pmatch: *mut RegMatch,
    eflags: c_int,
) -> c_int {
    // SAFETY: the caller passes a `regex_t` that `regcomp` filled.
    let opaque = unsafe { (*preg).opaque };
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "regcomp put a Compiled from malloc there, aligned for it"
    )]
    let compiled = opaque.cast::<Compiled>().cast_const();
    // SAFETY: `regcomp` left a `Compiled` there, or null.
    let Some(compiled) = (unsafe { compiled.as_ref() }) else {
        return REG_BADPAT;
    };
    // SAFETY: the caller passes a string.
    let bytes = unsafe { CStr::from_ptr(string) }.to_bytes();
    let want = if compiled.cflags & REG_NOSUB != 0 || pmatch.is_null() {
        0
    } else {
        nmatch
    };
    let offset = |x: usize| {
        if x == UNSET {
            -1
        } else {
            c_long::try_from(x).unwrap_or(-1)
        }
    };
    let mut report = |i: usize, so: usize, eo: usize| {
        let entry = RegMatch {
            rm_so: offset(so),
            rm_eo: offset(eo),
        };
        // SAFETY: the caller gives room for `nmatch` entries, and `i` is
        // below `nmatch`.
        unsafe { pmatch.wrapping_add(i).write(entry) };
    };
    match execute(compiled, bytes, eflags, want, &mut report) {
        Ok(true) => REG_OK,
        Ok(false) => REG_NOMATCH,
        Err(_) => REG_ESPACE,
    }
}

/// Frees what `regcomp` left in `*preg`.
///
/// # Safety
///
/// `preg` must hold a pattern `regcomp` compiled, not yet freed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn regfree(preg: *mut Regex) {
    // SAFETY: the caller passes a `regex_t` that `regcomp` filled.
    let compiled = unsafe { (*preg).opaque }.cast::<Compiled>();
    if compiled.is_null() {
        return;
    }
    // SAFETY: `regcomp` wrote a `Compiled` there, which nothing uses again.
    unsafe { compiled.drop_in_place() };
    // SAFETY: the memory came from `malloc`.
    unsafe { free(compiled.cast()) };
    // SAFETY: as in the first read.
    unsafe { (*preg).opaque = null_mut() };
}

/// musl's message for the error code `code`.
fn message(code: c_int) -> &'static [u8] {
    match code {
        REG_OK => b"No error",
        REG_NOMATCH => b"No match",
        REG_BADPAT => b"Invalid regexp",
        REG_ECOLLATE => b"Unknown collating element",
        REG_ECTYPE => b"Unknown character class name",
        REG_EESCAPE => b"Trailing backslash",
        REG_ESUBREG => b"Invalid back reference",
        REG_EBRACK => b"Missing ']'",
        REG_EPAREN => b"Missing ')'",
        REG_EBRACE => b"Missing '}'",
        REG_BADBR => b"Invalid contents of {}",
        REG_ERANGE => b"Invalid character range",
        REG_ESPACE => b"Out of memory",
        REG_BADRPT => b"Repetition not preceded by valid expression",
        _ => b"Unknown error",
    }
}

/// Writes the message for `errcode` into `buf`, cut to `size` bytes with its
/// NUL. Returns the size the whole message needs, NUL included.
///
/// # Safety
///
/// `buf` must have room for `size` bytes, or `size` must be 0.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn regerror(
    errcode: c_int,
    _preg: *const Regex,
    buf: *mut c_char,
    size: usize,
) -> usize {
    let text = message(errcode);
    if let Some(room) = size.checked_sub(1) {
        let n = text.len().min(room);
        for (i, &b) in text.iter().take(n).enumerate() {
            // SAFETY: `i` is below `size`, which the caller gives room for.
            unsafe { buf.wrapping_add(i).write(b.cast_signed()) };
        }
        // SAFETY: `n` is below `size`.
        unsafe { buf.wrapping_add(n).write(0) };
    }
    text.len() + 1
}

#[cfg(test)]
mod tests;
