//! `getopt`, `getopt_long` and `getopt_long_only`: parsing command-line
//! options, with glibc's behaviour.
//!
//! # Ordering
//!
//! By default the arguments are permuted as they are scanned, so that when
//! scanning ends every option comes first, in order, and `optind` names the
//! first of the non-options that followed. An option string starting with `+`,
//! or a `POSIXLY_CORRECT` environment variable, stops scanning at the first
//! non-option instead, as POSIX specifies. One starting with `-` returns each
//! non-option in place, as the option character 1 with the argument in
//! `optarg`. `--` ends the options in every mode. The mode is fixed when
//! scanning starts, which is at the first call or after `optind` is set to 0.
//!
//! The permutation is observable, since it moves the program's `argv`
//! entries, and it is glibc's: each run of options found after a run of
//! non-options is exchanged with that run as the next run of non-options is
//! reached, and at the end. The two runs are rotated in place.
//!
//! # Errors
//!
//! Unless `opterr` is 0 or the option string (after any `+` or `-`) starts
//! with `:`, a mistake is reported on standard error in glibc's words, with
//! the program's `argv[0]`, and `?` is returned with the option in `optopt`. A
//! leading `:` makes a missing argument return `:` instead. The message is
//! built in a buffer and written with `write`, since the library has no stdio
//! yet.
//!
//! glibc's `-W foo`, which a `W;` in the option string turns into `--foo`, is
//! supported.
//!
//! # `optreset`
//!
//! musl's header declares BSD's `optreset`. Setting it to nonzero restarts
//! scanning at `argv[1]` on the next call, as setting `optind` to 0 does.
//!
//! None of this is thread-safe, and POSIX does not ask it to be.

use core::ffi::{CStr, c_char, c_int};
use core::ptr::{null, null_mut};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU8, Ordering};

use crate::stdlib::getenv;
use crate::syscall::{self, nr};

/// `optarg`: the current option's argument, or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static optarg: AtomicPtr<c_char> = AtomicPtr::new(null_mut());

/// `optind`: the index of the next element of `argv` to scan.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static optind: AtomicI32 = AtomicI32::new(1);

/// `opterr`: whether errors are reported on standard error.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static opterr: AtomicI32 = AtomicI32::new(1);

/// `optopt`: the option character of the last error. It starts at `?`, as
/// glibc's does, and every call overwrites it with [`OPTOPT`].
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static optopt: AtomicI32 = AtomicI32::new(b'?' as i32);

/// The parser's own `optopt`, copied out to [`optopt`] after every call. It
/// starts at 0, so `optopt` reads 0 after calls with no error, as glibc's does.
static OPTOPT: AtomicI32 = AtomicI32::new(0);

/// `optreset`: set nonzero to restart scanning.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static optreset: AtomicI32 = AtomicI32::new(0);

/// Where scanning is inside a group of short options such as `-abc`, or null.
static NEXT_CHAR: AtomicPtr<c_char> = AtomicPtr::new(null_mut());
/// The first of the non-options skipped and not yet moved past options.
static FIRST_NONOPT: AtomicI32 = AtomicI32::new(1);
/// One past the last of those non-options.
static LAST_NONOPT: AtomicI32 = AtomicI32::new(1);
/// Whether scanning has started.
static INITIALIZED: AtomicBool = AtomicBool::new(false);
/// The ordering mode, one of the three constants below.
static ORDERING: AtomicU8 = AtomicU8::new(PERMUTE);

/// Arguments are permuted.
const PERMUTE: u8 = 0;
/// Scanning stops at the first non-option.
const REQUIRE_ORDER: u8 = 1;
/// Non-options are returned as option 1.
const RETURN_IN_ORDER: u8 = 2;

/// `no_argument`, `required_argument` and `optional_argument`.
const REQUIRED_ARGUMENT: c_int = 1;

/// `struct option` from `getopt.h`.
#[repr(C)]
#[derive(Debug)]
pub struct LongOption {
    /// The name, without the dashes. A null name ends the array.
    pub name: *const c_char,
    /// `no_argument`, `required_argument` or `optional_argument`.
    pub has_arg: c_int,
    /// Where to store `val`, in which case 0 is returned, or null.
    pub flag: *mut c_int,
    /// The value to return or store.
    pub val: c_int,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<LongOption>() == 32);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<LongOption>() == 16);

/// A message for standard error, collected and written in one piece where it
/// fits.
struct Message {
    /// The bytes so far.
    buf: [u8; 512],
    /// How many of them there are.
    len: usize,
}

impl Message {
    /// An empty message.
    const fn new() -> Self {
        Self {
            buf: [0; 512],
            len: 0,
        }
    }

    /// Writes what is collected to standard error.
    fn flush(&mut self) {
        let mut done = 0;
        while done < self.len {
            let rest = self.buf.get(done..self.len).unwrap_or(&[]);
            // SAFETY: the kernel only reads the buffer.
            let ret = unsafe { syscall::syscall3(nr::WRITE, 2, rest.as_ptr().addr(), rest.len()) };
            match usize::try_from(ret) {
                Ok(n) if n > 0 => done += n,
                _ => break,
            }
        }
        self.len = 0;
    }

    /// Appends `bytes`, writing out what is collected whenever it fills up.
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.len == self.buf.len() {
                self.flush();
            }
            if let Some(slot) = self.buf.get_mut(self.len) {
                *slot = byte;
                self.len += 1;
            }
        }
    }

    /// Appends the string at `s`, if it is not null.
    ///
    /// # Safety
    ///
    /// `s` must be null or a NUL-terminated string.
    unsafe fn push_c(&mut self, s: *const c_char) {
        if !s.is_null() {
            // SAFETY: the caller passes a string.
            self.push(unsafe { CStr::from_ptr(s) }.to_bytes());
        }
    }
}

/// The parser's view of one call.
struct Scan {
    /// The number of arguments, stopped at the first null entry.
    argc: c_int,
    /// The program's `argv`.
    argv: *const *mut c_char,
    /// The option string, past any leading `+` or `-`.
    optstring: *const c_char,
    /// Whether errors are printed.
    print_errors: bool,
}

impl Scan {
    /// `argv[i]`.
    ///
    /// # Safety
    ///
    /// `i` must be below `argc`.
    unsafe fn arg(&self, i: c_int) -> *mut c_char {
        // SAFETY: the caller keeps `i` within the array.
        unsafe { self.argv.wrapping_add(i as usize).read() }
    }

    /// Exchanges the run of non-options before `LAST_NONOPT` with the run of
    /// options from there to `optind`, keeping each run's order.
    ///
    /// # Safety
    ///
    /// The indices must be within `argv`, which must be writable.
    unsafe fn exchange(&self) {
        let first = FIRST_NONOPT.load(Ordering::Relaxed);
        let last = LAST_NONOPT.load(Ordering::Relaxed);
        let end = optind.load(Ordering::Relaxed);
        // Rotating [first, end) left by `last - first` puts the options first.
        // SAFETY: the caller vouches for the indices.
        unsafe { self.reverse(first, last) };
        // SAFETY: as above.
        unsafe { self.reverse(last, end) };
        // SAFETY: as above.
        unsafe { self.reverse(first, end) };
        FIRST_NONOPT.store(first + (end - last), Ordering::Relaxed);
        LAST_NONOPT.store(end, Ordering::Relaxed);
    }

    /// Reverses `argv[from..to]`.
    ///
    /// # Safety
    ///
    /// As [`Scan::exchange`].
    unsafe fn reverse(&self, from: c_int, to: c_int) {
        let slots = self.argv.cast_mut();
        let (mut i, mut j) = (from, to - 1);
        while i < j {
            let a = slots.wrapping_add(i as usize);
            let b = slots.wrapping_add(j as usize);
            // SAFETY: both are within the program's writable `argv`.
            unsafe { core::ptr::swap(a, b) };
            i += 1;
            j -= 1;
        }
    }

    /// Whether the option string lists `c`, and if so where.
    ///
    /// # Safety
    ///
    /// The option string must be a NUL-terminated string.
    unsafe fn find_short(&self, c: u8) -> Option<*const c_char> {
        let mut p = self.optstring;
        loop {
            // SAFETY: the loop stops at the NUL.
            let byte = unsafe { p.read() } as u8;
            if byte == 0 {
                return None;
            }
            if byte == c {
                return Some(p);
            }
            p = p.wrapping_add(1);
        }
    }

    /// The error return for a missing argument.
    ///
    /// # Safety
    ///
    /// The option string must be a NUL-terminated string.
    unsafe fn missing(&self) -> c_int {
        // SAFETY: the string has at least its NUL.
        if unsafe { self.optstring.read() } == b':' as c_char {
            c_int::from(b':')
        } else {
            c_int::from(b'?')
        }
    }

    /// Starts an error message with `argv[0]` and `": "`.
    ///
    /// # Safety
    ///
    /// `argc` must be at least 1.
    unsafe fn message(&self) -> Message {
        let mut m = Message::new();
        // SAFETY: `argv[0]` exists.
        let name = unsafe { self.arg(0) };
        // SAFETY: it is a string.
        unsafe { m.push_c(name) };
        m.push(b": ");
        m
    }
}

/// Resets the scan: `optind` to 1 if it was 0, the ordering mode from the
/// option string and the environment. Returns the option string past its mode
/// character.
///
/// # Safety
///
/// `optstring` must be a NUL-terminated string.
unsafe fn initialize(optstring: *const c_char) -> *const c_char {
    if optind.load(Ordering::Relaxed) == 0 || optreset.load(Ordering::Relaxed) != 0 {
        optind.store(1, Ordering::Relaxed);
        optreset.store(0, Ordering::Relaxed);
    }
    let start = optind.load(Ordering::Relaxed);
    FIRST_NONOPT.store(start, Ordering::Relaxed);
    LAST_NONOPT.store(start, Ordering::Relaxed);
    NEXT_CHAR.store(null_mut(), Ordering::Relaxed);
    INITIALIZED.store(true, Ordering::Relaxed);
    // SAFETY: the string has at least its NUL.
    let first = unsafe { optstring.read() } as u8;
    let (mode, rest) = match first {
        b'-' => (RETURN_IN_ORDER, optstring.wrapping_add(1)),
        b'+' => (REQUIRE_ORDER, optstring.wrapping_add(1)),
        // SAFETY: the name is a string literal.
        _ if !unsafe { getenv(c"POSIXLY_CORRECT".as_ptr()) }.is_null() => {
            (REQUIRE_ORDER, optstring)
        }
        _ => (PERMUTE, optstring),
    };
    ORDERING.store(mode, Ordering::Relaxed);
    rest
}

/// Whether `arg` is a non-option: it does not start with `-`, or is `-`.
///
/// # Safety
///
/// `arg` must be a NUL-terminated string.
unsafe fn is_nonoption(arg: *const c_char) -> bool {
    // SAFETY: the string has at least its NUL.
    let first = unsafe { arg.read() };
    // SAFETY: the second byte exists if the first is not the NUL.
    first != b'-' as c_char || unsafe { arg.wrapping_add(1).read() } == 0
}

/// Parses the next option. `longopts` is null for plain `getopt`.
///
/// # Safety
///
/// `argv` must hold `argc` writable entries, each a string or null, and
/// `optstring` must be a string. `longopts`, if not null, must be an array of
/// options ending in one with a null name.
unsafe fn getopt_internal(
    argc: c_int,
    argv: *const *mut c_char,
    optstring: *const c_char,
    longopts: *const LongOption,
    longind: *mut c_int,
    long_only: bool,
) -> c_int {
    optarg.store(null_mut(), Ordering::Relaxed);
    if argc < 1 || argv.is_null() || optstring.is_null() {
        return -1;
    }
    // An `argc` past a null entry would have scanning read beyond `argv`.
    let mut real_argc = 0;
    // SAFETY: the loop stays below `argc`.
    while real_argc < argc && !unsafe { argv.wrapping_add(real_argc as usize).read() }.is_null() {
        real_argc += 1;
    }
    let argc = real_argc;
    if argc < 1 {
        return -1;
    }

    let restart = optind.load(Ordering::Relaxed) == 0
        || optreset.load(Ordering::Relaxed) != 0
        || !INITIALIZED.load(Ordering::Relaxed);
    let mut optstring = if restart {
        // SAFETY: the caller passes a string.
        unsafe { initialize(optstring) }
    } else {
        optstring
    };
    if !restart {
        // SAFETY: the string has at least its NUL.
        let first = unsafe { optstring.read() };
        if first == b'-' as c_char || first == b'+' as c_char {
            optstring = optstring.wrapping_add(1);
        }
    }
    let scan = Scan {
        argc,
        argv,
        optstring,
        // SAFETY: as above.
        print_errors: opterr.load(Ordering::Relaxed) != 0
            && unsafe { optstring.read() } != b':' as c_char,
    };

    let next = NEXT_CHAR.load(Ordering::Relaxed);
    // SAFETY: a stored position is within an argument string.
    if next.is_null() || unsafe { next.read() } == 0 {
        // SAFETY: the caller's contract covers `argv`.
        if let Some(result) = unsafe { advance(&scan, longopts, longind, long_only) } {
            return result;
        }
    }
    // SAFETY: as above.
    unsafe { short_option(&scan, longopts, longind) }
}

/// Moves to the next element of `argv`. Returns the result of the call if
/// the element settles it, and otherwise leaves `NEXT_CHAR` at its first short
/// option.
///
/// # Safety
///
/// As [`getopt_internal`].
unsafe fn advance(
    scan: &Scan,
    longopts: *const LongOption,
    longind: *mut c_int,
    long_only: bool,
) -> Option<c_int> {
    let argc = scan.argc;
    let mut ind = optind.load(Ordering::Relaxed);
    // The program may have moved `optind` back.
    if LAST_NONOPT.load(Ordering::Relaxed) > ind {
        LAST_NONOPT.store(ind, Ordering::Relaxed);
    }
    if FIRST_NONOPT.load(Ordering::Relaxed) > ind {
        FIRST_NONOPT.store(ind, Ordering::Relaxed);
    }
    let ordering = ORDERING.load(Ordering::Relaxed);

    if ordering == PERMUTE {
        let first = FIRST_NONOPT.load(Ordering::Relaxed);
        let last = LAST_NONOPT.load(Ordering::Relaxed);
        if first != last && last != ind {
            // SAFETY: the indices are within `argv`.
            unsafe { scan.exchange() };
        } else if last != ind {
            FIRST_NONOPT.store(ind, Ordering::Relaxed);
        }
        while ind < argc {
            // SAFETY: `ind` is below `argc`.
            let arg = unsafe { scan.arg(ind) };
            // SAFETY: an argument is a string.
            if !unsafe { is_nonoption(arg) } {
                break;
            }
            ind += 1;
        }
        optind.store(ind, Ordering::Relaxed);
        LAST_NONOPT.store(ind, Ordering::Relaxed);
    }

    let dash_dash = ind != argc && {
        // SAFETY: `ind` is below `argc`.
        let arg = unsafe { scan.arg(ind) };
        // SAFETY: an argument is a string.
        let arg = unsafe { CStr::from_ptr(arg) };
        arg == c"--"
    };
    if dash_dash {
        ind += 1;
        optind.store(ind, Ordering::Relaxed);
        let first = FIRST_NONOPT.load(Ordering::Relaxed);
        let last = LAST_NONOPT.load(Ordering::Relaxed);
        if first != last && last != ind {
            // SAFETY: as above.
            unsafe { scan.exchange() };
        } else if first == last {
            FIRST_NONOPT.store(ind, Ordering::Relaxed);
        }
        LAST_NONOPT.store(argc, Ordering::Relaxed);
        ind = argc;
        optind.store(ind, Ordering::Relaxed);
    }

    if ind == argc {
        let first = FIRST_NONOPT.load(Ordering::Relaxed);
        if first != LAST_NONOPT.load(Ordering::Relaxed) {
            optind.store(first, Ordering::Relaxed);
        }
        return Some(-1);
    }

    // SAFETY: as above.
    let arg = unsafe { scan.arg(ind) };
    // SAFETY: the argument is a string.
    if unsafe { is_nonoption(arg) } {
        if ordering == REQUIRE_ORDER {
            return Some(-1);
        }
        optarg.store(arg, Ordering::Relaxed);
        optind.store(ind + 1, Ordering::Relaxed);
        return Some(1);
    }

    if !longopts.is_null() {
        // SAFETY: the argument starts with `-` and another byte.
        let second = unsafe { arg.wrapping_add(1).read() } as u8;
        if second == b'-' {
            NEXT_CHAR.store(arg.wrapping_add(2), Ordering::Relaxed);
            // SAFETY: as above.
            return Some(unsafe { long_option(scan, longopts, longind, long_only, b"--") });
        }
        // A lone `-f` naming a short option is that option, not an
        // abbreviation of a long one.
        // SAFETY: the argument has a second byte, so a third.
        let third = unsafe { arg.wrapping_add(2).read() };
        // SAFETY: the option string is a string.
        if long_only && (third != 0 || unsafe { scan.find_short(second) }.is_none()) {
            NEXT_CHAR.store(arg.wrapping_add(1), Ordering::Relaxed);
            // SAFETY: as above.
            let code = unsafe { long_option(scan, longopts, longind, long_only, b"-") };
            if code != -1 {
                return Some(code);
            }
        }
    }
    NEXT_CHAR.store(arg.wrapping_add(1), Ordering::Relaxed);
    None
}

/// Parses the short option at `NEXT_CHAR`.
///
/// # Safety
///
/// As [`getopt_internal`], with `NEXT_CHAR` inside an argument.
unsafe fn short_option(scan: &Scan, longopts: *const LongOption, longind: *mut c_int) -> c_int {
    let next = NEXT_CHAR.load(Ordering::Relaxed);
    // SAFETY: `NEXT_CHAR` is at a byte of an argument that is not its NUL.
    let c = unsafe { next.read() } as u8;
    let next = next.wrapping_add(1);
    NEXT_CHAR.store(next, Ordering::Relaxed);
    // SAFETY: `next` is at most the argument's NUL.
    let at_end = unsafe { next.read() } == 0;
    if at_end {
        let _ = optind.fetch_add(1, Ordering::Relaxed);
    }
    let ind = optind.load(Ordering::Relaxed);

    // SAFETY: the option string is a string.
    let found = unsafe { scan.find_short(c) };
    let Some(spec) = found.filter(|_| c != b':' && c != b';') else {
        if scan.print_errors {
            // SAFETY: `argc` is at least 1.
            let mut m = unsafe { scan.message() };
            m.push(b"invalid option -- '");
            m.push(&[c]);
            m.push(b"'\n");
            m.flush();
        }
        OPTOPT.store(c_int::from(c), Ordering::Relaxed);
        return c_int::from(b'?');
    };
    // SAFETY: `spec` is at a byte of the option string that is not its NUL.
    let spec1 = unsafe { spec.wrapping_add(1).read() } as u8;

    if c == b'W' && spec1 == b';' && !longopts.is_null() {
        let word = if !at_end {
            next
        } else if ind == scan.argc {
            // SAFETY: as above.
            return unsafe { missing_short(scan, c) };
        } else {
            // SAFETY: `ind` is below `argc`.
            unsafe { scan.arg(ind) }
        };
        NEXT_CHAR.store(word, Ordering::Relaxed);
        // SAFETY: as above.
        return unsafe { long_option(scan, longopts, longind, false, b"-W ") };
    }

    if spec1 == b':' {
        // SAFETY: `spec + 1` is not the NUL, so `spec + 2` is readable.
        let optional = unsafe { spec.wrapping_add(2).read() } == b':' as c_char;
        if !at_end {
            optarg.store(next, Ordering::Relaxed);
            optind.store(ind + 1, Ordering::Relaxed);
        } else if optional {
            optarg.store(null_mut(), Ordering::Relaxed);
        } else if ind == scan.argc {
            NEXT_CHAR.store(null_mut(), Ordering::Relaxed);
            // SAFETY: as above.
            return unsafe { missing_short(scan, c) };
        } else {
            // SAFETY: `ind` is below `argc`.
            optarg.store(unsafe { scan.arg(ind) }, Ordering::Relaxed);
            optind.store(ind + 1, Ordering::Relaxed);
        }
        NEXT_CHAR.store(null_mut(), Ordering::Relaxed);
    }
    c_int::from(c)
}

/// Reports that short option `c` is missing its argument.
///
/// # Safety
///
/// As [`getopt_internal`].
unsafe fn missing_short(scan: &Scan, c: u8) -> c_int {
    if scan.print_errors {
        // SAFETY: `argc` is at least 1.
        let mut m = unsafe { scan.message() };
        m.push(b"option requires an argument -- '");
        m.push(&[c]);
        m.push(b"'\n");
        m.flush();
    }
    OPTOPT.store(c_int::from(c), Ordering::Relaxed);
    // SAFETY: the option string is a string.
    unsafe { scan.missing() }
}

/// The `i`th long option.
///
/// # Safety
///
/// `longopts` must hold at least `i + 1` entries.
unsafe fn long_at(longopts: *const LongOption, i: usize) -> &'static LongOption {
    // SAFETY: the caller vouches for the entry, which the program keeps for
    // the call.
    unsafe { &*longopts.wrapping_add(i) }
}

/// The name of `option` as bytes.
///
/// # Safety
///
/// The option's name must be a NUL-terminated string.
unsafe fn name_of(option: &LongOption) -> &[u8] {
    // SAFETY: the caller vouches for the name.
    unsafe { CStr::from_ptr(option.name) }.to_bytes()
}

/// Whether two options an abbreviation matches make it ambiguous: always for
/// `getopt_long_only`, and otherwise only if they would do different things.
fn differ(a: &LongOption, b: &LongOption, long_only: bool) -> bool {
    long_only || a.has_arg != b.has_arg || a.flag != b.flag || a.val != b.val
}

/// The first of the `count` options that `name` abbreviates, and whether a
/// later one [`differ`]s from it.
///
/// # Safety
///
/// `longopts` must hold `count` options with string names.
unsafe fn abbreviation(
    longopts: *const LongOption,
    count: usize,
    name: &[u8],
    long_only: bool,
) -> (Option<usize>, bool) {
    let mut first: Option<&LongOption> = None;
    let mut first_index = None;
    let mut ambiguous = false;
    let mut i = 0;
    while i < count {
        // SAFETY: `i` is below the number of options.
        let option = unsafe { long_at(longopts, i) };
        i += 1;
        // SAFETY: as above.
        if !unsafe { name_of(option) }.starts_with(name) {
            continue;
        }
        match first {
            None => {
                first = Some(option);
                first_index = Some(i - 1);
            }
            Some(prior) => ambiguous |= differ(prior, option, long_only),
        }
    }
    (first_index, ambiguous)
}

/// Reports that `text` abbreviates more than one option. The candidates are
/// listed as glibc lists them: the first, and each later one that
/// [`differ`]s from it.
///
/// # Safety
///
/// `longopts` must hold `count` options with string names, `first` below
/// `count`, and `argc` at least 1.
unsafe fn report_ambiguous(
    scan: &Scan,
    longopts: *const LongOption,
    count: usize,
    first: usize,
    text: &[u8],
    prefix: &[u8],
    long_only: bool,
) {
    let name_len = text.iter().position(|&b| b == b'=').unwrap_or(text.len());
    let name = text.get(..name_len).unwrap_or(&[]);
    // SAFETY: the caller vouches for `argc`.
    let mut m = unsafe { scan.message() };
    m.push(b"option '");
    m.push(prefix);
    m.push(text);
    m.push(b"' is ambiguous; possibilities:");
    // SAFETY: `first` is below the number of options.
    let prior = unsafe { long_at(longopts, first) };
    let mut i = 0;
    while i < count {
        // SAFETY: `i` is below the number of options.
        let option = unsafe { long_at(longopts, i) };
        // SAFETY: as above.
        let full = unsafe { name_of(option) };
        if full.starts_with(name) && (i == first || differ(prior, option, long_only)) {
            m.push(b" '");
            m.push(prefix);
            m.push(full);
            m.push(b"'");
        }
        i += 1;
    }
    m.push(b"\n");
    m.flush();
}

/// Parses the long option at `NEXT_CHAR`, written after `prefix`. Returns -1,
/// changing nothing, if `getopt_long_only` should read it as short options
/// instead.
///
/// # Safety
///
/// As [`getopt_internal`], with `longopts` not null.
unsafe fn long_option(
    scan: &Scan,
    longopts: *const LongOption,
    longind: *mut c_int,
    long_only: bool,
    prefix: &[u8],
) -> c_int {
    let next = NEXT_CHAR.load(Ordering::Relaxed);
    // SAFETY: `NEXT_CHAR` is within an argument string.
    let text = unsafe { CStr::from_ptr(next) }.to_bytes();
    let name_len = text.iter().position(|&b| b == b'=').unwrap_or(text.len());
    let name = text.get(..name_len).unwrap_or(&[]);

    // An exact match wins; otherwise a unique abbreviation.
    let mut count = 0;
    let mut found: Option<usize> = None;
    loop {
        // SAFETY: the array ends with a null name, and the loop stops there.
        let option = unsafe { long_at(longopts, count) };
        if option.name.is_null() {
            break;
        }
        // SAFETY: an option's name is a string.
        if unsafe { CStr::from_ptr(option.name) }.to_bytes() == name {
            found = Some(count);
            break;
        }
        count += 1;
    }

    if found.is_none() {
        // SAFETY: `count` is the number of options.
        let (first, ambiguous) = unsafe { abbreviation(longopts, count, name, long_only) };
        if let (Some(first), true) = (first, ambiguous) {
            if scan.print_errors {
                // SAFETY: as above.
                unsafe { report_ambiguous(scan, longopts, count, first, text, prefix, long_only) };
            }
            NEXT_CHAR.store(null_mut(), Ordering::Relaxed);
            let _ = optind.fetch_add(1, Ordering::Relaxed);
            OPTOPT.store(0, Ordering::Relaxed);
            return c_int::from(b'?');
        }
        found = first;
    }

    let Some(index) = found else {
        let ind = optind.load(Ordering::Relaxed);
        let first = text.first().copied().unwrap_or(0);
        // Only `getopt_long_only` reads `-name` as a long option, and then
        // `optind` is the argument being parsed, which has a second byte.
        let unrecognized = if long_only {
            // SAFETY: `ind` is below `argc` while a long option is parsed.
            let arg = unsafe { scan.arg(ind) };
            // SAFETY: the argument has at least two bytes.
            let dashes = unsafe { arg.wrapping_add(1).read() } == b'-' as c_char;
            // SAFETY: the option string is a string.
            dashes || unsafe { scan.find_short(first) }.is_none()
        } else {
            true
        };
        if unrecognized {
            if scan.print_errors {
                // SAFETY: `argc` is at least 1.
                let mut m = unsafe { scan.message() };
                m.push(b"unrecognized option '");
                m.push(prefix);
                m.push(text);
                m.push(b"'\n");
                m.flush();
            }
            NEXT_CHAR.store(null_mut(), Ordering::Relaxed);
            optind.store(ind + 1, Ordering::Relaxed);
            OPTOPT.store(0, Ordering::Relaxed);
            return c_int::from(b'?');
        }
        return -1;
    };

    // SAFETY: `index` is below the number of options.
    let option = unsafe { long_at(longopts, index) };
    // SAFETY: an option's name is a string.
    let full = unsafe { CStr::from_ptr(option.name) }.to_bytes();
    let ind = optind.load(Ordering::Relaxed) + 1;
    optind.store(ind, Ordering::Relaxed);
    NEXT_CHAR.store(null_mut(), Ordering::Relaxed);
    if name_len < text.len() {
        if option.has_arg != 0 {
            optarg.store(next.wrapping_add(name_len + 1), Ordering::Relaxed);
        } else {
            if scan.print_errors {
                // SAFETY: `argc` is at least 1.
                let mut m = unsafe { scan.message() };
                m.push(b"option '");
                m.push(prefix);
                m.push(full);
                m.push(b"' doesn't allow an argument\n");
                m.flush();
            }
            OPTOPT.store(option.val, Ordering::Relaxed);
            return c_int::from(b'?');
        }
    } else if option.has_arg == REQUIRED_ARGUMENT {
        if ind < scan.argc {
            // SAFETY: `ind` is below `argc`.
            optarg.store(unsafe { scan.arg(ind) }, Ordering::Relaxed);
            optind.store(ind + 1, Ordering::Relaxed);
        } else {
            if scan.print_errors {
                // SAFETY: `argc` is at least 1.
                let mut m = unsafe { scan.message() };
                m.push(b"option '");
                m.push(prefix);
                m.push(full);
                m.push(b"' requires an argument\n");
                m.flush();
            }
            OPTOPT.store(option.val, Ordering::Relaxed);
            // SAFETY: the option string is a string.
            return unsafe { scan.missing() };
        }
    }
    if !longind.is_null() {
        // SAFETY: the caller passes a writable index.
        unsafe { longind.write(index as c_int) };
    }
    if !option.flag.is_null() {
        // SAFETY: the program's flag pointer is writable.
        unsafe { option.flag.write(option.val) };
        return 0;
    }
    option.val
}

/// Parses the next short option in `argv`.
///
/// # Safety
///
/// `argv` must hold `argc` writable entries, each a string or null, and
/// `optstring` must be a string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getopt(
    argc: c_int,
    argv: *const *mut c_char,
    optstring: *const c_char,
) -> c_int {
    // SAFETY: the same contract, with no long options.
    let c = unsafe { getopt_internal(argc, argv, optstring, null(), null_mut(), false) };
    optopt.store(OPTOPT.load(Ordering::Relaxed), Ordering::Relaxed);
    c
}

/// Parses the next short option, or long option written with `--`.
///
/// # Safety
///
/// As [`getopt`], and `longopts` must be null or an array ending with an
/// option whose name is null, and `longindex` null or writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getopt_long(
    argc: c_int,
    argv: *const *mut c_char,
    optstring: *const c_char,
    longopts: *const LongOption,
    longindex: *mut c_int,
) -> c_int {
    // SAFETY: the same contract.
    let c = unsafe { getopt_internal(argc, argv, optstring, longopts, longindex, false) };
    optopt.store(OPTOPT.load(Ordering::Relaxed), Ordering::Relaxed);
    c
}

/// As [`getopt_long`], but a long option may also be written with one `-`.
///
/// # Safety
///
/// As [`getopt_long`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getopt_long_only(
    argc: c_int,
    argv: *const *mut c_char,
    optstring: *const c_char,
    longopts: *const LongOption,
    longindex: *mut c_int,
) -> c_int {
    // SAFETY: the same contract.
    let c = unsafe { getopt_internal(argc, argv, optstring, longopts, longindex, true) };
    optopt.store(OPTOPT.load(Ordering::Relaxed), Ordering::Relaxed);
    c
}
