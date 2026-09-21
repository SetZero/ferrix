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
//! * The layouts are glibc's, as `include/regex.h` gives them: `regex_t` is
//!   64 bytes with `re_nsub` at offset 48, and `regoff_t` is an `int`, so
//!   `regmatch_t` is 8 bytes. They were musl's -- `re_nsub` first and a
//!   `long` `regoff_t` -- until 2026-09-21, when a program built against
//!   glibc began loading this library in glibc's place: it allocates glibc's
//!   `regmatch_t` array, and musl's would overrun it. Every C program here is
//!   built from source against this library's header, so both change
//!   together.
//! * GNU's own interface, `re_compile_pattern` and `re_search` over
//!   `re_syntax_options`, is here too, on the same engine: see
//!   [`re_compile_pattern`] for what of the syntax bits it honours.
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
//! * `REG_STARTEND`, glibc's extension, is here: the string is the bytes
//!   between `pmatch[0]`'s two offsets, and needs no NUL.

mod parse;
mod program;
mod solve;

use core::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

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
/// `REG_STARTEND`, glibc's: the string is `pmatch[0]`'s span.
pub(crate) const REG_STARTEND: c_int = 4;

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

/// `regex_t`, which is glibc's `struct re_pattern_buffer`, in glibc's layout.
///
/// Only `re_nsub` is POSIX's; the rest is GNU's, and a program written for
/// GNU's `re_compile_pattern` fills some of it in before compiling. The
/// compiled pattern goes where glibc keeps its own, in `buffer`.
#[repr(C)]
#[derive(Debug)]
pub struct Regex {
    /// glibc's `buffer`: the compiled pattern, a [`Compiled`] from `malloc`,
    /// or null.
    opaque: *mut c_void,
    /// glibc's `allocated`: the bytes behind `buffer`, which nothing here
    /// reads.
    allocated: c_ulong,
    /// glibc's `used`, likewise.
    used: c_ulong,
    /// The GNU syntax bits the pattern was compiled with.
    syntax: c_ulong,
    /// GNU's `fastmap`: a table of first bytes a caller may supply for
    /// speed. It is never filled here, and `regfree` frees it as glibc's does.
    fastmap: *mut c_char,
    /// GNU's `translate`: a byte map applied before matching, which a caller
    /// may supply. A map is taken to mean case folding, which every known
    /// caller uses it for; `regfree` frees it as glibc's does.
    translate: *mut u8,
    /// `re_nsub`: how many parenthesised subexpressions the pattern has.
    pub re_nsub: usize,
    /// GNU's bit-fields, from the lowest bit: `can_be_null`, the two bits of
    /// `regs_allocated`, `fastmap_accurate`, `no_sub`, `not_bol`, `not_eol`
    /// and `newline_anchor`.
    bits: c_uint,
}

const _: () = assert!(size_of::<Regex>() == 64);
const _: () = assert!(offset_of!(Regex, opaque) == 0);
const _: () = assert!(offset_of!(Regex, re_nsub) == 48);
const _: () = assert!(offset_of!(Regex, bits) == 56);

/// `regmatch_t`, in glibc's layout: `regoff_t` is an `int`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RegMatch {
    /// Where the match starts, or -1.
    pub rm_so: c_int,
    /// Where it ends, or -1.
    pub rm_eo: c_int,
}

const _: () = assert!(size_of::<RegMatch>() == 8);
const _: () = assert!(offset_of!(RegMatch, rm_eo) == 4);

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
    let (compiled, groups) = match compile(pat, cflags) {
        Ok(done) => done,
        Err(code) => return code,
    };
    let mut bits = 0;
    if cflags & REG_NOSUB != 0 {
        bits |= NO_SUB;
    }
    if cflags & REG_NEWLINE != 0 {
        bits |= NEWLINE_ANCHOR;
    }
    // SAFETY: the caller passes a writable `regex_t`. Every field is written,
    // as glibc's `regcomp` writes them.
    unsafe {
        preg.write(Regex {
            opaque: compiled.cast(),
            allocated: 0,
            used: 0,
            syntax: 0,
            fastmap: null_mut(),
            translate: null_mut(),
            re_nsub: groups,
            bits,
        });
    }
    REG_OK
}

/// `no_sub` in [`Regex::bits`].
const NO_SUB: c_uint = 1 << 4;
/// `not_bol` in [`Regex::bits`].
const NOT_BOL: c_uint = 1 << 5;
/// `not_eol` in [`Regex::bits`].
const NOT_EOL: c_uint = 1 << 6;
/// `newline_anchor` in [`Regex::bits`].
const NEWLINE_ANCHOR: c_uint = 1 << 7;
/// Where `regs_allocated` is in [`Regex::bits`], and its width.
const REGS_SHIFT: u32 = 1;
/// `regs_allocated`'s mask, before the shift.
const REGS_MASK: c_uint = 3;

/// Compiles `pat` under `cflags`: the compiled pattern, from `malloc`, and
/// how many groups it has, or an error code for `regerror`.
fn compile(pat: &[u8], cflags: c_int) -> Result<(*mut Compiled, usize), c_int> {
    let ast = parse::parse(pat, cflags)?;
    let program = Program::compile(&ast).map_err(|_| REG_ESPACE)?;
    let groups = ast.groups as usize;
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "malloc aligns to 16, more than the structure needs"
    )]
    let compiled = malloc(size_of::<Compiled>()).cast::<Compiled>();
    if compiled.is_null() {
        return Err(REG_ESPACE);
    }
    // SAFETY: the allocation is new, large enough and aligned.
    unsafe {
        compiled.write(Compiled {
            ast,
            program,
            cflags,
        });
    }
    Ok((compiled, groups))
}

/// The pattern `regcomp` or `re_compile_pattern` left in `*preg`, or `None`.
///
/// # Safety
///
/// `preg` must be a `regex_t` one of them filled, not yet freed.
unsafe fn compiled<'a>(preg: *const Regex) -> Option<&'a Compiled> {
    // SAFETY: the caller passes a filled `regex_t`.
    let opaque = unsafe { (*preg).opaque };
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "the compiler put a Compiled from malloc there, aligned for it"
    )]
    let compiled = opaque.cast::<Compiled>().cast_const();
    // SAFETY: a `Compiled` is there, or null.
    unsafe { compiled.as_ref() }
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
    let Some(compiled) = (unsafe { compiled(preg) }) else {
        return REG_BADPAT;
    };
    let want = if compiled.cflags & REG_NOSUB != 0 || pmatch.is_null() {
        0
    } else {
        nmatch
    };
    // `REG_STARTEND`: the string is `pmatch[0]`'s span, and every offset
    // reported is from `string`, not from the span's start.
    let (bytes, base) = if eflags & REG_STARTEND != 0 && !pmatch.is_null() {
        // SAFETY: with `REG_STARTEND` the caller passes `pmatch[0]` filled in.
        let span = unsafe { pmatch.read() };
        let (Ok(start), Ok(end)) = (usize::try_from(span.rm_so), usize::try_from(span.rm_eo))
        else {
            return REG_BADPAT;
        };
        if end < start {
            return REG_BADPAT;
        }
        let at = string.cast::<u8>().wrapping_add(start);
        // SAFETY: the caller vouches for the span's bytes.
        let bytes = unsafe { core::slice::from_raw_parts(at, end - start) };
        (bytes, start)
    } else {
        // SAFETY: the caller passes a string.
        (unsafe { CStr::from_ptr(string) }.to_bytes(), 0)
    };
    let offset = |x: usize| {
        if x == UNSET {
            -1
        } else {
            x.checked_add(base)
                .and_then(|x| c_int::try_from(x).ok())
                .unwrap_or(-1)
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

/// Frees what `regcomp` left in `*preg`, and, as glibc's does, the `fastmap`
/// and `translate` tables a GNU caller may have allocated for it.
///
/// # Safety
///
/// `preg` must hold a pattern `regcomp` compiled, not yet freed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn regfree(preg: *mut Regex) {
    // SAFETY: the caller passes a `regex_t` that `regcomp` filled, which
    // nothing else uses while it is freed.
    let re = unsafe { &mut *preg };
    let compiled = re.opaque.cast::<Compiled>();
    if !compiled.is_null() {
        // SAFETY: a `Compiled` is there, which nothing uses again.
        unsafe { compiled.drop_in_place() };
        // SAFETY: the memory came from `malloc`.
        unsafe { free(compiled.cast()) };
    }
    // SAFETY: null or from `malloc`, as glibc requires of a caller that sets
    // it.
    unsafe { free(re.fastmap.cast()) };
    // SAFETY: as above.
    unsafe { free(re.translate.cast()) };
    re.opaque = null_mut();
    re.fastmap = null_mut();
    re.translate = null_mut();
}

/// GNU's syntax bits, `reg_syntax_t`, which `re_compile_pattern` compiles
/// under: 0 unless a program sets it, which is glibc's default too.
///
/// A program linked against glibc may take its own copy (`COPY`), which is
/// why it is a variable this library reads through its GOT rather than a
/// value it keeps: see [`crate::stdlib::environ`].
///
/// # Weak, all four GNU names
///
/// `re_syntax_options`, `re_set_syntax`, `re_compile_pattern` and
/// `re_search` are weak, so that a program bringing its own GNU regex -- git
/// links `compat/regex` into itself -- links against this library at all,
/// and keeps its own. The variable is weak `.bss` in assembly, as
/// `__environ` is; each function is a weak tail jump to the Rust one, as
/// `termios.rs`'s versioned names are, since stable Rust cannot mark a
/// function weak.
fn syntax_options() -> &'static AtomicUsize {
    #[allow(unused_unsafe, reason = "the variable is a Rust static in unit tests")]
    // SAFETY: the variable lives as long as the process, and every access to
    // it is atomic.
    unsafe {
        &RE_SYNTAX_OPTIONS
    }
}

/// The variable, in unit tests, where the host's C library has the C name.
#[cfg(test)]
static RE_SYNTAX_OPTIONS: AtomicUsize = AtomicUsize::new(0);

#[cfg(not(test))]
unsafe extern "C" {
    /// The variable, defined below.
    #[link_name = "re_syntax_options"]
    static RE_SYNTAX_OPTIONS: AtomicUsize;
}

// The weak variable, and the weak names of the three functions. The Rust
// functions' own names are hidden, so neither a static program nor
// `libc.so.6` exports them.
#[cfg(all(not(test), target_arch = "x86_64"))]
core::arch::global_asm!(
    ".pushsection .bss.re_syntax_options,\"aw\",@nobits",
    ".p2align 3",
    ".weak re_syntax_options",
    ".type re_syntax_options, @object",
    ".size re_syntax_options, 8",
    "re_syntax_options:",
    ".zero 8",
    ".popsection",
    ".pushsection .text.ferrousli_gnu_regex,\"ax\",@progbits",
    ".hidden __ferrousli_re_set_syntax",
    ".hidden __ferrousli_re_compile_pattern",
    ".hidden __ferrousli_re_search",
    ".p2align 4",
    ".weak re_set_syntax",
    ".type re_set_syntax, @function",
    "re_set_syntax:",
    "jmp __ferrousli_re_set_syntax",
    ".p2align 4",
    ".weak re_compile_pattern",
    ".type re_compile_pattern, @function",
    "re_compile_pattern:",
    "jmp __ferrousli_re_compile_pattern",
    ".p2align 4",
    ".weak re_search",
    ".type re_search, @function",
    "re_search:",
    "jmp __ferrousli_re_search",
    ".popsection",
);

/// `RE_NO_BK_PARENS`: `(` groups without a backslash, which is what makes an
/// expression extended in every GNU syntax that has it.
const RE_NO_BK_PARENS: usize = 1 << 13;
/// `RE_ICASE`: letters match either case.
const RE_ICASE: usize = 1 << 22;
/// `RE_NO_SUB`: the caller wants no registers.
const RE_NO_SUB: usize = 1 << 25;
/// `RE_NREGS`: how many registers `re_search` allocates at least.
const RE_NREGS: usize = 30;

/// Sets [`re_syntax_options`] and returns what it was.
#[cfg_attr(not(test), unsafe(export_name = "__ferrousli_re_set_syntax"))]
pub extern "C" fn re_set_syntax(syntax: usize) -> usize {
    syntax_options().swap(syntax, Ordering::Relaxed)
}

/// GNU's compiler: `pattern`'s `length` bytes, under [`re_syntax_options`],
/// into `*buffer`. Returns null, or a message saying what is wrong.
///
/// Of the syntax bits it reads the three that decide which language the
/// pattern is in: `RE_NO_BK_PARENS` for an extended expression rather than a
/// basic one, `RE_ICASE`, and `RE_NO_SUB`. The others refine GNU's grammar
/// in ways POSIX's two grammars already settle, and are taken as they do. A
/// `translate` table the caller set is taken as case folding. Anchors match
/// only at the ends of the text, not after each newline as glibc's
/// `newline_anchor` has them; every caller known feeds one line at a time.
///
/// # Safety
///
/// `pattern` must be valid for `length` bytes and `buffer` writable as a
/// `regex_t` whose `fastmap` and `translate` are null or from `malloc`.
#[cfg_attr(not(test), unsafe(export_name = "__ferrousli_re_compile_pattern"))]
pub unsafe extern "C" fn re_compile_pattern(
    pattern: *const c_char,
    length: usize,
    buffer: *mut Regex,
) -> *const c_char {
    let syntax = syntax_options().load(Ordering::Relaxed);
    let (translate, fastmap) = {
        // SAFETY: the caller passes a `regex_t`, whose GNU fields it may have
        // set.
        let re = unsafe { &*buffer };
        (re.translate, re.fastmap)
    };
    let mut cflags = 0;
    if syntax & RE_NO_BK_PARENS != 0 {
        cflags |= REG_EXTENDED;
    }
    if syntax & RE_ICASE != 0 || !translate.is_null() {
        cflags |= REG_ICASE;
    }
    if syntax & RE_NO_SUB != 0 {
        cflags |= REG_NOSUB;
    }
    // SAFETY: the caller vouches for the pattern's bytes.
    let pat = unsafe { core::slice::from_raw_parts(pattern.cast::<u8>(), length) };
    let (compiled, groups) = match compile(pat, cflags) {
        Ok(done) => done,
        Err(code) => return message(code).as_ptr(),
    };
    let bits = if cflags & REG_NOSUB != 0 { NO_SUB } else { 0 } | NEWLINE_ANCHOR;
    // SAFETY: as above. glibc's leaves `fastmap` and `translate` to the
    // caller, and starts `regs_allocated` at `REGS_UNALLOCATED`.
    unsafe {
        buffer.write(Regex {
            opaque: compiled.cast(),
            allocated: 0,
            used: 0,
            syntax: syntax as c_ulong,
            fastmap,
            translate,
            re_nsub: groups,
            bits,
        });
    }
    core::ptr::null()
}

/// GNU's `struct re_registers`: where `re_search` reports each group.
#[repr(C)]
#[derive(Debug)]
pub struct Registers {
    /// How many entries `start` and `end` have.
    num_regs: c_uint,
    /// Where each group starts, or -1.
    start: *mut c_int,
    /// Where each group ends, or -1.
    end: *mut c_int,
}

/// GNU's search: the first position from `start` on, stepping towards
/// `start + range` (backwards when `range` is negative), at which the pattern
/// in `*buffer` matches `string`'s `length` bytes. Returns it, -1 if there is
/// none, or -2 if the search ran out of memory; fills `*regs`, when given,
/// with the match and its groups, allocating them as `regs_allocated` says.
///
/// # Safety
///
/// `buffer` must hold a pattern `re_compile_pattern` or `regcomp` compiled,
/// `string` be valid for `length` bytes, and `regs` be null or a
/// `struct re_registers` in the state `buffer`'s `regs_allocated` says.
#[cfg_attr(not(test), unsafe(export_name = "__ferrousli_re_search"))]
pub unsafe extern "C" fn re_search(
    buffer: *mut Regex,
    string: *const c_char,
    length: c_int,
    start: c_int,
    range: c_int,
    regs: *mut Registers,
) -> c_int {
    // SAFETY: the caller passes a compiled pattern.
    let Some(compiled) = (unsafe { compiled(buffer) }) else {
        return -2;
    };
    let (Ok(length), Ok(first)) = (usize::try_from(length), usize::try_from(start)) else {
        return -1;
    };
    if first > length {
        return -1;
    }
    // SAFETY: the caller vouches for `length` bytes.
    let text = unsafe { core::slice::from_raw_parts(string.cast::<u8>(), length) };
    // SAFETY: as above.
    let bits = unsafe { (*buffer).bits };
    let last = if range < 0 {
        first.saturating_sub(range.unsigned_abs() as usize)
    } else {
        first.saturating_add(range as usize).min(length)
    };
    // The flags for matching `text` from `at` on: a start that is not the
    // text's is not the start of a line.
    let flags = |at: usize| {
        (if at > 0 || bits & NOT_BOL != 0 {
            REG_NOTBOL
        } else {
            0
        }) | (if bits & NOT_EOL != 0 { REG_NOTEOL } else { 0 })
    };
    // Where the leftmost match in `text` from `at` on starts, if anywhere.
    let leftmost = |at: usize| -> Mem<Option<usize>> {
        let mut position = None;
        let rest = text.get(at..).unwrap_or_default();
        let _ = execute(compiled, rest, flags(at), 1, &mut |_, so, _| {
            position = Some(so + at);
        })?;
        Ok(position)
    };
    // Forward, one search from `first` finds the first position that
    // matches; backward, each position is tried and must match where it
    // stands.
    let mut found = None;
    let mut at = first;
    loop {
        let position = match leftmost(at) {
            Ok(position) => position,
            Err(_) => return -2,
        };
        if range >= 0 {
            found = position.filter(|&position| position <= last);
            break;
        }
        if position == Some(at) {
            found = Some(at);
            break;
        }
        if at == last {
            break;
        }
        at -= 1;
    }
    let Some(position) = found else {
        return -1;
    };
    let Ok(answer) = c_int::try_from(position) else {
        return -2;
    };
    if regs.is_null() || bits & NO_SUB != 0 {
        return answer;
    }
    let groups = compiled.ast.groups as usize + 1;
    // SAFETY: the caller passes registers, which nothing else uses meanwhile.
    let registers = unsafe { &mut *regs };
    // SAFETY: the caller passes a compiled pattern, likewise.
    let pattern = unsafe { &mut *buffer };
    let Some(count) = make_room(pattern, registers, groups) else {
        return -2;
    };
    let (starts, ends) = (registers.start, registers.end);
    // Writes entry `i` of both arrays, which `make_room` gave `count`
    // entries each.
    let write = |i: usize, so: c_int, eo: c_int| {
        if i < count {
            // SAFETY: `i` is below `count`.
            unsafe { starts.wrapping_add(i).write(so) };
            // SAFETY: as above.
            unsafe { ends.wrapping_add(i).write(eo) };
        }
    };
    for i in 0..count {
        write(i, -1, -1);
    }
    let rest = text.get(position..).unwrap_or_default();
    let mut record = |i: usize, so: usize, eo: usize| {
        if i >= count || so == UNSET {
            return;
        }
        let (Ok(so), Ok(eo)) = (
            c_int::try_from(so + position),
            c_int::try_from(eo + position),
        ) else {
            return;
        };
        write(i, so, eo);
    };
    match execute(
        compiled,
        rest,
        flags(position),
        groups.min(count),
        &mut record,
    ) {
        Ok(true) => answer,
        Ok(false) | Err(_) => -2,
    }
}

/// Make `*regs` hold at least `groups` entries, as `buffer`'s
/// `regs_allocated` says: allocate them when unallocated (and say so),
/// grow them when this library allocated them, and use them as they are when
/// the caller fixed them. Returns how many entries to fill, or `None` when
/// memory ran out.
///
/// `regs` must be in the state `buffer`'s `regs_allocated` says, which is
/// the caller's promise to `re_search`.
fn make_room(buffer: &mut Regex, regs: &mut Registers, groups: usize) -> Option<usize> {
    const UNALLOCATED: c_uint = 0;
    const REALLOCATE: c_uint = 1;
    let (bits, have) = (buffer.bits, regs.num_regs as usize);
    let state = (bits >> REGS_SHIFT) & REGS_MASK;
    let entries = match state {
        UNALLOCATED => groups.max(RE_NREGS),
        REALLOCATE if have < groups => groups,
        // Enough already, or `REGS_FIXED`: the caller's arrays, as they are.
        _ => return Some(have),
    };
    let bytes = entries.checked_mul(size_of::<c_int>())?;
    // An unallocated set's pointers are whatever the caller left, so they are
    // replaced rather than grown.
    let (old_start, old_end) = if state == UNALLOCATED {
        (null_mut(), null_mut())
    } else {
        (regs.start, regs.end)
    };
    // SAFETY: the old array is null or this library's, from `malloc`.
    let start = unsafe { crate::malloc::realloc(old_start.cast(), bytes) }.cast::<c_int>();
    // SAFETY: as above.
    let end = unsafe { crate::malloc::realloc(old_end.cast(), bytes) }.cast::<c_int>();
    if start.is_null() || end.is_null() {
        return None;
    }
    regs.start = start;
    regs.end = end;
    regs.num_regs = entries as c_uint;
    buffer.bits = (bits & !(REGS_MASK << REGS_SHIFT)) | (REALLOCATE << REGS_SHIFT);
    Some(entries)
}

/// musl's message for the error code `code`.
fn message(code: c_int) -> &'static CStr {
    match code {
        REG_OK => c"No error",
        REG_NOMATCH => c"No match",
        REG_BADPAT => c"Invalid regexp",
        REG_ECOLLATE => c"Unknown collating element",
        REG_ECTYPE => c"Unknown character class name",
        REG_EESCAPE => c"Trailing backslash",
        REG_ESUBREG => c"Invalid back reference",
        REG_EBRACK => c"Missing ']'",
        REG_EPAREN => c"Missing ')'",
        REG_EBRACE => c"Missing '}'",
        REG_BADBR => c"Invalid contents of {}",
        REG_ERANGE => c"Invalid character range",
        REG_ESPACE => c"Out of memory",
        REG_BADRPT => c"Repetition not preceded by valid expression",
        _ => c"Unknown error",
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
    let text = message(errcode).to_bytes();
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
