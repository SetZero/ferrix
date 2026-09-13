//! Time zones: `tzset`, `tzname`, `timezone` and `daylight`, and the lookup
//! of the offset in effect that [`crate::tm`] and `strftime` use.
//!
//! # Where a zone comes from
//!
//! The rules are musl's (MIT), whose `src/time/__tz.c` this follows:
//!
//! * `TZ` unset means `/etc/localtime`, and `TZ` empty means UTC.
//! * A value not starting with `:` whose name is followed by a sign or a digit,
//!   or which is `UTC` or `GMT`, is a POSIX string such as
//!   `CET-1CEST,M3.5.0,M10.5.0/3`.
//! * Anything else, less a leading `:`, names a TZif file. A name starting
//!   with `/` or `.` is a path. Any other name without a `.` is looked for in
//!   `$TZDIR` when that is set, and otherwise in `/usr/share/zoneinfo`,
//!   `/share/zoneinfo` and `/etc/zoneinfo`, in that order. musl has no
//!   `TZDIR`; glibc reads it, and so does this. In a secure-mode program
//!   (`AT_SECURE`) only the standard directories and `/etc/localtime` are
//!   read, as in musl.
//! * A file that cannot be read or fails validation, and a POSIX string that
//!   does not parse, give UTC under the name `UTC`, as in musl. glibc keeps
//!   the name from `TZ` instead.
//!
//! `tzset` reads the file again only when `TZ` changes. Every conversion calls
//! it, as in musl, so a program that changes `TZ` sees the change at once.
//!
//! # Differences from musl
//!
//! * A TZif file is validated before use: its headers, its counts against its
//!   length, each transition's type, the order of the transitions, each type's
//!   offset, flag and abbreviation, and its footer. musl checks the magic and
//!   the length of the first header only.
//! * A POSIX string with a daylight-saving name and no rules, such as
//!   `EST5EDT`, uses the United States' rules, `M3.2.0,M11.1.0`, as glibc and
//!   tzcode do. musl leaves both rules zero, which puts almost the whole year
//!   in daylight saving time.
//! * Times before a file's first transition use its type 0, as RFC 9636
//!   specifies. musl uses its first type that is not daylight saving time.
//!   tzdata's files make both the same.
//! * Past the last transition, a file with no footer keeps the last
//!   transition's type rather than musl's rules made from its type table.
//! * The rule lookup considers the transitions of the year before and the
//!   year after as well, so a transition time pushed over a year boundary by
//!   POSIX 2024's hours beyond 24 or below 0 is still found. musl looks in the
//!   current year only.
//! * Abbreviations are kept for the life of the process (see [`intern`]), so
//!   `tm_zone` and `tzname` never dangle when the zone changes. musl unmaps
//!   the old file.
//!
//! Leap-second records in a `right/` zone are skipped and not applied, as in
//! musl.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int, c_long};
use core::ptr::null_mut;
use core::slice;
use core::sync::atomic::{AtomicI32, AtomicI64, AtomicPtr, Ordering};

use crate::lock::SpinLock;
use crate::malloc::{free, malloc, realloc};
use crate::syscall::{self, nr};
use crate::tm::{self, DAY};
use crate::{auxv, errno, stdlib};

/// The name of UTC.
pub const UTC: &CStr = c"UTC";
/// The name a zone without daylight saving time gives `tzname[1]`, as in musl.
const NO_NAME: &CStr = c"";

/// `AT_SECURE`, from `linux/auxvec.h`: nonzero in a set-user-ID program.
const AT_SECURE: usize = 23;
/// `AT_FDCWD`, from `linux/fcntl.h`.
const AT_FDCWD: isize = -100;
/// `O_NONBLOCK`, from `asm-generic/fcntl.h`. Not blocking keeps a FIFO named
/// by `TZ` from stopping the program.
const O_NONBLOCK: usize = 0o4000;
/// `O_CLOEXEC`, from `asm-generic/fcntl.h`.
const O_CLOEXEC: usize = 0o2_000_000;
/// The flags a zone file is opened with: `O_RDONLY`, which is 0, and the two
/// above.
const OPEN_FLAGS: usize = O_NONBLOCK | O_CLOEXEC;
/// The furthest from the epoch a rule is evaluated. Callers stay within
/// `int` years, about 2^56 seconds; this keeps the year arithmetic far from
/// overflow for any `t`.
const RULE_LIMIT: i64 = 1 << 60;
/// `PATH_MAX`, from `limits.h`.
const PATH_MAX: usize = 4096;
/// `NAME_MAX`, from `limits.h`.
const NAME_MAX: usize = 255;
/// The largest zone file read. tzdata's largest is a few kilobytes.
const MAX_FILE: usize = 1 << 20;
/// The directories searched for a zone name when `TZDIR` is not set, as in
/// musl.
const ZONE_DIRS: [&[u8]; 3] = [b"/usr/share/zoneinfo", b"/share/zoneinfo", b"/etc/zoneinfo"];

/// The abbreviations of standard and daylight saving time in the current
/// zone: `char *tzname[2]`. `AtomicPtr` has a pointer's layout.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static tzname: [AtomicPtr<c_char>; 2] =
    [AtomicPtr::new(null_mut()), AtomicPtr::new(null_mut())];

/// Seconds west of UTC of the current zone's standard time: `long timezone`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static timezone: AtomicI64 = AtomicI64::new(0);

/// Whether the current zone has daylight saving time: `int daylight`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static daylight: AtomicI32 = AtomicI32::new(0);

/// The zone in effect at one moment.
#[derive(Debug, Clone, Copy)]
pub struct Local {
    /// 1 in daylight saving time, 0 otherwise.
    pub isdst: c_int,
    /// Seconds east of UTC.
    pub utoff: c_long,
    /// Seconds east of UTC of the opposite kind of time: standard time's
    /// during daylight saving time and the other way round. `mktime` applies
    /// it when `tm_isdst` asks for the other kind.
    pub opp: c_long,
    /// The abbreviation, which lives as long as the process.
    pub name: *const c_char,
}

impl Local {
    /// UTC.
    const UTC: Self = Self {
        isdst: 0,
        utoff: 0,
        opp: 0,
        name: UTC.as_ptr(),
    };
}

// ---------------------------------------------------------------------------
// Abbreviations
// ---------------------------------------------------------------------------

/// The head of the list of abbreviations, each in a block from `malloc`: the
/// address of the next block, then the NUL-terminated name. Blocks are only
/// ever added, at the head, and never freed.
static NAMES: AtomicPtr<u8> = AtomicPtr::new(null_mut());

/// The size of a name block's link.
const LINK: usize = size_of::<usize>();

/// The name stored in the block at `block`.
///
/// # Safety
///
/// `block` must be a block on [`NAMES`].
unsafe fn block_name(block: *mut u8) -> *const c_char {
    block.wrapping_add(LINK).cast_const().cast()
}

/// The block after `block` on [`NAMES`].
///
/// # Safety
///
/// `block` must be a block on [`NAMES`].
unsafe fn block_next(block: *mut u8) -> *mut u8 {
    // SAFETY: every block starts with its link, written before it was
    // published.
    let next = unsafe { block.cast::<usize>().read_unaligned() };
    core::ptr::with_exposed_provenance_mut(next)
}

/// The name in the list equal to `bytes`, if there is one.
fn find_name(bytes: &[u8]) -> Option<*const c_char> {
    let mut block = NAMES.load(Ordering::Acquire);
    while !block.is_null() {
        // SAFETY: `block` came from the list.
        let name = unsafe { block_name(block) };
        // SAFETY: every block holds a NUL-terminated name.
        if unsafe { CStr::from_ptr(name) }.to_bytes() == bytes {
            return Some(name);
        }
        // SAFETY: as above.
        block = unsafe { block_next(block) };
    }
    None
}

/// A copy of `bytes`, which holds no NUL, as a C string that lives as long as
/// the process: the same one each time for the same text.
///
/// `tm_zone` and `tzname` point at these. Keeping them for good means a
/// `struct tm` made before `TZ` changed still names its zone. The set of
/// abbreviations a program meets is small, and each is stored once.
///
/// Blocks are pushed with compare-and-swap, so two threads adding the same
/// name at once may store it twice, which is harmless. `None` when `malloc`
/// fails.
pub fn intern(bytes: &[u8]) -> Option<*const c_char> {
    if bytes == UTC.to_bytes() {
        return Some(UTC.as_ptr());
    }
    if bytes.is_empty() {
        return Some(NO_NAME.as_ptr());
    }
    if let Some(name) = find_name(bytes) {
        return Some(name);
    }
    let size = LINK.checked_add(bytes.len())?.checked_add(1)?;
    let block = malloc(size).cast::<u8>();
    if block.is_null() {
        return None;
    }
    for (i, &byte) in bytes.iter().chain(&[0]).enumerate() {
        // SAFETY: the block holds `LINK + bytes.len() + 1` bytes.
        unsafe { block.wrapping_add(LINK + i).write(byte) };
    }
    let mut head = NAMES.load(Ordering::Acquire);
    loop {
        // SAFETY: the block starts with `LINK` writable bytes.
        unsafe {
            block
                .cast::<usize>()
                .write_unaligned(head.expose_provenance())
        };
        match NAMES.compare_exchange_weak(head, block, Ordering::AcqRel, Ordering::Acquire) {
            // SAFETY: the block is on the list now.
            Ok(_) => return Some(unsafe { block_name(block) }),
            Err(now) => head = now,
        }
    }
}

/// Whether `name` is an abbreviation this module handed out, and so safe to
/// read. musl's `strftime` prints `%Z` only for such a `tm_zone`, and prints
/// nothing for any other, which might be garbage.
pub fn is_known_name(name: *const c_char) -> bool {
    if name == UTC.as_ptr() || name == NO_NAME.as_ptr() {
        return true;
    }
    let mut block = NAMES.load(Ordering::Acquire);
    while !block.is_null() {
        // SAFETY: `block` came from the list.
        if unsafe { block_name(block) } == name {
            return true;
        }
        // SAFETY: as above.
        block = unsafe { block_next(block) };
    }
    false
}

// ---------------------------------------------------------------------------
// POSIX TZ strings
// ---------------------------------------------------------------------------

/// The day a POSIX rule names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleDay {
    /// `Jn`: day 1 to 365, never counting the 29th of February.
    Julian(i64),
    /// `n`: day 0 to 365, counting the 29th of February.
    Zero(i64),
    /// `Mm.w.d`: day `d` (0 is Sunday) of week `w` (1 to 5, 5 the last) of
    /// month `m`.
    Month { month: i64, week: i64, wday: i64 },
}

/// One of a POSIX string's two transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rule {
    day: RuleDay,
    /// Seconds after local midnight, in the time in effect before the
    /// transition. POSIX 2024 allows -167 to 167 hours.
    time: i64,
}

impl Rule {
    /// The local time, in seconds as if the epoch were local, of this
    /// transition in `year`.
    fn local_secs(&self, year: i64) -> i64 {
        let leap = tm::is_leap(year);
        let day = match self.day {
            RuleDay::Julian(n) => {
                tm::days_from_civil(year, 1, 1) + n - 1 + i64::from(leap && n >= 60)
            }
            RuleDay::Zero(n) => tm::days_from_civil(year, 1, 1) + n,
            RuleDay::Month { month, week, wday } => {
                let first = tm::days_from_civil(year, month, 1);
                // 1970-01-01 was a Thursday.
                let first_wday = (first + 4).rem_euclid(7);
                let mut offset = (wday - first_wday).rem_euclid(7) + 7 * (week - 1);
                while offset >= tm::days_in_month(month, leap) {
                    offset -= 7;
                }
                first + offset
            }
        };
        day * DAY + self.time
    }
}

/// A zone's daylight saving time, as a POSIX string gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dst {
    name: *const c_char,
    /// Seconds east of UTC.
    utoff: i64,
    start: Rule,
    end: Rule,
}

/// A parsed POSIX `TZ` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posix {
    std: *const c_char,
    /// Seconds east of UTC, the opposite of the string's sign.
    std_utoff: i64,
    dst: Option<Dst>,
}

/// The rules a POSIX string with daylight saving time and none of its own
/// gets: the United States' since 2007, as in glibc and tzcode.
const DEFAULT_RULES: (Rule, Rule) = (
    Rule {
        day: RuleDay::Month {
            month: 3,
            week: 2,
            wday: 0,
        },
        time: 7200,
    },
    Rule {
        day: RuleDay::Month {
            month: 11,
            week: 1,
            wday: 0,
        },
        time: 7200,
    },
);

/// A cursor over a POSIX string.
#[derive(Debug)]
struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    /// The next byte, or 0 at the end.
    fn peek(&self) -> u8 {
        self.text.get(self.at).copied().unwrap_or(0)
    }

    /// Whether the whole string has been read.
    fn done(&self) -> bool {
        self.at >= self.text.len()
    }

    /// Consumes `byte` if it is next.
    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == byte && !self.done();
        if found {
            self.at += 1;
        }
        found
    }

    /// A zone name: letters, or anything but `>` between `<` and `>`. It may
    /// be empty only when unquoted. `None` for an unclosed `<`.
    fn name(&mut self) -> Option<&'a [u8]> {
        let quoted = self.eat(b'<');
        let start = self.at;
        if quoted {
            while !self.done() && self.peek() != b'>' {
                self.at += 1;
            }
            let name = self.text.get(start..self.at)?;
            if !self.eat(b'>') || name.is_empty() {
                return None;
            }
            Some(name)
        } else {
            while self.peek().is_ascii_alphabetic() {
                self.at += 1;
            }
            self.text.get(start..self.at)
        }
    }

    /// A decimal number of one to `digits` digits.
    fn number(&mut self, digits: usize) -> Option<i64> {
        let mut value = 0;
        let mut count = 0;
        while count < digits && self.peek().is_ascii_digit() {
            value = value * 10 + i64::from(self.peek() - b'0');
            self.at += 1;
            count += 1;
        }
        (count > 0).then_some(value)
    }

    /// `[+-]hh[:mm[:ss]]` in seconds, with at most `max_hours` hours.
    fn hms(&mut self, max_hours: i64) -> Option<i64> {
        let negative = if self.eat(b'-') {
            true
        } else {
            let _ = self.eat(b'+');
            false
        };
        let hours = self.number(3)?;
        let mut secs = hours * 3600;
        if hours > max_hours {
            return None;
        }
        if self.eat(b':') {
            let minutes = self.number(2)?;
            if minutes > 59 {
                return None;
            }
            secs += minutes * 60;
            if self.eat(b':') {
                let seconds = self.number(2)?;
                if seconds > 59 {
                    return None;
                }
                secs += seconds;
            }
        }
        Some(if negative { -secs } else { secs })
    }

    /// Whether an offset starts here.
    fn at_offset(&self) -> bool {
        matches!(self.peek(), b'+' | b'-' | b'0'..=b'9')
    }

    /// A rule: `Jn`, `n` or `Mm.w.d`, then `/time`, which defaults to 02:00.
    fn rule(&mut self) -> Option<Rule> {
        let day = if self.eat(b'J') {
            let n = self.number(3)?;
            if !(1..=365).contains(&n) {
                return None;
            }
            RuleDay::Julian(n)
        } else if self.eat(b'M') {
            let month = self.number(2)?;
            if !self.eat(b'.') {
                return None;
            }
            let week = self.number(1)?;
            if !self.eat(b'.') {
                return None;
            }
            let wday = self.number(1)?;
            if !(1..=12).contains(&month) || !(1..=5).contains(&week) || wday > 6 {
                return None;
            }
            RuleDay::Month { month, week, wday }
        } else {
            let n = self.number(3)?;
            if n > 365 {
                return None;
            }
            RuleDay::Zero(n)
        };
        let time = if self.eat(b'/') { self.hms(167)? } else { 7200 };
        Some(Rule { day, time })
    }
}

/// Whether musl reads `tz` as a POSIX string rather than a file name: it does
/// not start with `:`, and its first name is followed by a sign or a digit or
/// is `UTC` or `GMT`.
fn is_posix_form(tz: &[u8]) -> bool {
    if tz.first() == Some(&b':') {
        return false;
    }
    let mut parser = Parser { text: tz, at: 0 };
    match parser.name() {
        Some(name) if !name.is_empty() => parser.at_offset() || name == b"UTC" || name == b"GMT",
        _ => false,
    }
}

/// Parses a whole POSIX `TZ` string, such as `CET-1CEST,M3.5.0,M10.5.0/3` or
/// `<+0330>-3:30`. `None` if it does not parse or a name cannot be stored.
pub fn parse_posix(text: &[u8]) -> Option<Posix> {
    let mut parser = Parser { text, at: 0 };
    let std_name = parser.name()?;
    if std_name.is_empty() {
        return None;
    }
    let std_utoff = if parser.at_offset() {
        -parser.hms(24)?
    } else {
        0
    };
    let std = intern(std_name)?;
    if parser.done() {
        return Some(Posix {
            std,
            std_utoff,
            dst: None,
        });
    }
    let dst_name = parser.name()?;
    if dst_name.is_empty() {
        return None;
    }
    let utoff = if parser.at_offset() {
        -parser.hms(24)?
    } else {
        std_utoff + 3600
    };
    let (start, end) = if parser.done() {
        DEFAULT_RULES
    } else {
        if !parser.eat(b',') {
            return None;
        }
        let start = parser.rule()?;
        if !parser.eat(b',') {
            return None;
        }
        (start, parser.rule()?)
    };
    if !parser.done() {
        return None;
    }
    Some(Posix {
        std,
        std_utoff,
        dst: Some(Dst {
            name: intern(dst_name)?,
            utoff,
            start,
            end,
        }),
    })
}

impl Posix {
    /// UTC, named `UTC`.
    const UTC: Self = Self {
        std: UTC.as_ptr(),
        std_utoff: 0,
        dst: None,
    };

    /// Standard time.
    fn standard(&self) -> Local {
        Local {
            isdst: 0,
            utoff: self.std_utoff,
            opp: self.dst.map_or(self.std_utoff, |dst| dst.utoff),
            name: self.std,
        }
    }

    /// The zone in effect at `t`, which is local wall-clock seconds when
    /// `local` and UTC seconds otherwise.
    ///
    /// The transitions of the year `t` falls in and of the years either side
    /// are placed on `t`'s scale. A local start is read in standard time and
    /// a local end in daylight saving time, as POSIX specifies. The latest
    /// transition at or before `t` says which time is in effect, and a start
    /// wins a tie, so a zone whose daylight saving time ends as it begins,
    /// such as `EST5EDT,0/0,J365/25`, stays in it all year.
    ///
    /// On the local scale, a skipped wall-clock time is therefore in daylight
    /// saving time and a repeated one too, as in musl.
    fn at(&self, t: i64, local: bool) -> Local {
        let Some(dst) = self.dst else {
            return self.standard();
        };
        let t = t.clamp(-RULE_LIMIT, RULE_LIMIT);
        let year = tm::year_of(t);
        let mut latest: Option<(i64, bool)> = None;
        for y in year - 1..=year + 1 {
            let mut start = dst.start.local_secs(y);
            let mut end = dst.end.local_secs(y);
            if !local {
                start -= self.std_utoff;
                end -= dst.utoff;
            }
            for (when, is_start) in [(end, false), (start, true)] {
                let later = latest.is_none_or(|(best, best_is_start)| {
                    when > best || (when == best && is_start && !best_is_start)
                });
                if when <= t && later {
                    latest = Some((when, is_start));
                }
            }
        }
        match latest {
            Some((_, true)) => Local {
                isdst: 1,
                utoff: dst.utoff,
                opp: self.std_utoff,
                name: dst.name,
            },
            _ => self.standard(),
        }
    }
}

// ---------------------------------------------------------------------------
// TZif files
// ---------------------------------------------------------------------------

/// The size of a TZif header.
const HEADER: usize = 44;
/// The size of a local time type record.
const TTINFO: usize = 6;
/// The most local time types a file can have: a transition names one in a
/// byte.
const MAX_TYPES: usize = 256;
/// The smallest offset RFC 9636 allows, in seconds east of UTC.
const MIN_UTOFF: i64 = -89_999;
/// The largest offset RFC 9636 allows.
const MAX_UTOFF: i64 = 93_599;

/// The big-endian `u32` at `at`.
fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// The big-endian `i64` at `at`.
fn be64(bytes: &[u8], at: usize) -> Option<i64> {
    let end = at.checked_add(8)?;
    Some(i64::from_be_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// A TZif header's counts.
#[derive(Debug, Clone, Copy)]
struct Counts {
    isut: usize,
    isstd: usize,
    leap: usize,
    time: usize,
    types: usize,
    chars: usize,
}

impl Counts {
    /// The header at `at`, whose magic must be `TZif`. Also returns the
    /// version byte.
    fn read(bytes: &[u8], at: usize) -> Option<(u8, Self)> {
        if bytes.get(at..at.checked_add(4)?)? != b"TZif" {
            return None;
        }
        let version = *bytes.get(at + 4)?;
        let count =
            |i: usize| -> Option<usize> { usize::try_from(be32(bytes, at + 20 + 4 * i)?).ok() };
        Some((
            version,
            Self {
                isut: count(0)?,
                isstd: count(1)?,
                leap: count(2)?,
                time: count(3)?,
                types: count(4)?,
                chars: count(5)?,
            },
        ))
    }

    /// The length of the data block these counts describe, with times of
    /// `time_size` bytes.
    fn block_len(&self, time_size: usize) -> Option<usize> {
        let parts = [
            self.time.checked_mul(time_size + 1)?,
            self.types.checked_mul(TTINFO)?,
            self.chars,
            self.leap.checked_mul(time_size + 4)?,
            self.isstd,
            self.isut,
        ];
        parts
            .iter()
            .try_fold(0usize, |sum, &part| sum.checked_add(part))
    }
}

/// Where a validated TZif file keeps what the lookup needs, with each type's
/// abbreviation already stored by [`intern`].
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Whether times are 8 bytes, the version 2 and later data, or 4.
    wide: bool,
    /// The number of transitions.
    count: usize,
    /// Where the transition times start.
    times_at: usize,
    /// Where the transition types start.
    kinds_at: usize,
    /// The number of local time types.
    types: usize,
    /// Where the local time types start.
    types_at: usize,
    /// Each type's abbreviation.
    names: [*const c_char; MAX_TYPES],
    /// The footer's rules for times after the last transition.
    footer: Option<Posix>,
}

impl Layout {
    /// Validates a whole TZif file and finds its parts. `None` if anything is
    /// out of place.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let (version, first) = Counts::read(bytes, 0)?;
        let (wide, counts, data_at) = if version == 0 {
            (false, first, HEADER)
        } else if version >= b'2' {
            let second_at = HEADER.checked_add(first.block_len(4)?)?;
            let (_, second) = Counts::read(bytes, second_at)?;
            (true, second, second_at.checked_add(HEADER)?)
        } else {
            return None;
        };
        let time_size = if wide { 8 } else { 4 };
        let data_end = data_at.checked_add(counts.block_len(time_size)?)?;
        if data_end > bytes.len()
            || !(1..=MAX_TYPES).contains(&counts.types)
            || counts.chars == 0
            || (counts.isstd != 0 && counts.isstd != counts.types)
            || (counts.isut != 0 && counts.isut != counts.types)
        {
            return None;
        }
        let kinds_at = data_at + counts.time * time_size;
        let types_at = kinds_at + counts.time;
        let chars_at = types_at + counts.types * TTINFO;
        let chars = bytes.get(chars_at..chars_at + counts.chars)?;

        let mut layout = Self {
            wide,
            count: counts.time,
            times_at: data_at,
            kinds_at,
            types: counts.types,
            types_at,
            names: [UTC.as_ptr(); MAX_TYPES],
            footer: None,
        };

        let mut previous = None;
        for i in 0..layout.count {
            let time = layout.time(bytes, i)?;
            if previous.is_some_and(|previous| time <= previous) {
                return None;
            }
            previous = Some(time);
            if layout.kind(bytes, i)? >= layout.types {
                return None;
            }
        }
        for ty in 0..layout.types {
            let at = types_at + ty * TTINFO;
            let utoff = i64::from(be32(bytes, at)?.cast_signed());
            let isdst = *bytes.get(at + 4)?;
            let name_at = usize::from(*bytes.get(at + 5)?);
            if !(MIN_UTOFF..=MAX_UTOFF).contains(&utoff) || isdst > 1 {
                return None;
            }
            let tail = chars.get(name_at..)?;
            let len = tail.iter().position(|&byte| byte == 0)?;
            let slot = layout.names.get_mut(ty)?;
            *slot = intern(tail.get(..len)?)?;
        }

        if wide {
            // The footer: a newline, a POSIX string, and a newline that ends
            // the file.
            let rest = bytes.get(data_end..)?;
            let (&first, rest) = rest.split_first()?;
            let (&last, footer) = rest.split_last()?;
            if first != b'\n' || last != b'\n' || footer.contains(&b'\n') {
                return None;
            }
            if !footer.is_empty() {
                layout.footer = Some(parse_posix(footer)?);
            }
        } else if data_end != bytes.len() {
            return None;
        }
        Some(layout)
    }

    /// Transition `i`'s time.
    fn time(&self, bytes: &[u8], i: usize) -> Option<i64> {
        if self.wide {
            be64(bytes, self.times_at + i * 8)
        } else {
            Some(i64::from(be32(bytes, self.times_at + i * 4)?.cast_signed()))
        }
    }

    /// Transition `i`'s local time type.
    fn kind(&self, bytes: &[u8], i: usize) -> Option<usize> {
        bytes.get(self.kinds_at + i).map(|&kind| usize::from(kind))
    }

    /// Type `ty`'s offset and whether it is daylight saving time.
    fn ttinfo(&self, bytes: &[u8], ty: usize) -> Option<(i64, bool)> {
        let at = self.types_at + ty * TTINFO;
        Some((
            i64::from(be32(bytes, at)?.cast_signed()),
            *bytes.get(at + 4)? != 0,
        ))
    }

    /// Type `ty` in effect, with `alt` as its opposite.
    fn local(&self, bytes: &[u8], ty: usize, alt: usize) -> Option<Local> {
        let (utoff, isdst) = self.ttinfo(bytes, ty)?;
        Some(Local {
            isdst: c_int::from(isdst),
            utoff,
            opp: self.ttinfo(bytes, alt)?.0,
            name: *self.names.get(ty)?,
        })
    }

    /// Where transition `i` falls on the scale of `t`: its UTC time, or,
    /// when `local`, its wall-clock time in the type in effect before it.
    fn key(&self, bytes: &[u8], i: usize, local: bool) -> Option<i64> {
        let time = self.time(bytes, i)?;
        if !local {
            return Some(time);
        }
        let before = match i.checked_sub(1) {
            Some(previous) => self.kind(bytes, previous)?,
            None => 0,
        };
        Some(time.saturating_add(self.ttinfo(bytes, before)?.0))
    }

    /// The zone in effect at `t`, as [`Posix::at`] reads `t` and `local`.
    ///
    /// Before the first transition, type 0 is in effect, and at or after the
    /// last the footer's rules are, if there are any. In between, the latest
    /// transition at or before `t` gives the type, and a neighbouring
    /// transition to the opposite kind of time gives `opp`, as in musl.
    fn at(&self, bytes: &[u8], t: i64, local: bool) -> Option<Local> {
        if self.count == 0 {
            return match self.footer {
                Some(footer) => Some(footer.at(t, local)),
                None => self.local(bytes, 0, 0),
            };
        }
        // How many transitions are at or before `t`.
        let (mut low, mut high) = (0, self.count);
        while low < high {
            let middle = low + (high - low) / 2;
            if self.key(bytes, middle, local)? <= t {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let Some(index) = low.checked_sub(1) else {
            return self.local(bytes, 0, self.kind(bytes, 0)?);
        };
        if index == self.count - 1
            && let Some(footer) = self.footer
        {
            return Some(footer.at(t, local));
        }
        let ty = self.kind(bytes, index)?;
        let isdst = self.ttinfo(bytes, ty)?.1;
        let mut alt = ty;
        if let Some(previous) = index.checked_sub(1) {
            let kind = self.kind(bytes, previous)?;
            if self.ttinfo(bytes, kind)?.1 != isdst {
                alt = kind;
            }
        }
        if alt == ty && index + 1 < self.count {
            let kind = self.kind(bytes, index + 1)?;
            if self.ttinfo(bytes, kind)?.1 != isdst {
                alt = kind;
            }
        }
        self.local(bytes, ty, alt)
    }

    /// `tzname`, `timezone` and `daylight` for this zone: the footer's when
    /// there is one, and otherwise those of the latest transitions to each
    /// kind of time.
    fn globals(&self, bytes: &[u8]) -> Globals {
        if let Some(footer) = self.footer {
            return footer.globals();
        }
        let mut std = None;
        let mut dst = None;
        for i in (0..self.count).rev() {
            let Some(ty) = self.kind(bytes, i) else {
                break;
            };
            let Some((utoff, isdst)) = self.ttinfo(bytes, ty) else {
                break;
            };
            let slot = if isdst { &mut dst } else { &mut std };
            if slot.is_none() {
                *slot = Some((utoff, self.names.get(ty).copied().unwrap_or(UTC.as_ptr())));
            }
        }
        let (std_utoff, std_name) = std
            .or_else(|| {
                let (utoff, _) = self.ttinfo(bytes, 0)?;
                Some((utoff, *self.names.first()?))
            })
            .unwrap_or((0, UTC.as_ptr()));
        Globals {
            names: [std_name, dst.map_or(NO_NAME.as_ptr(), |(_, name)| name)],
            timezone: -std_utoff,
            daylight: dst.is_some(),
        }
    }
}

/// The values of `tzname`, `timezone` and `daylight`.
#[derive(Debug, Clone, Copy)]
struct Globals {
    names: [*const c_char; 2],
    timezone: i64,
    daylight: bool,
}

impl Posix {
    /// `tzname`, `timezone` and `daylight` for this string.
    fn globals(&self) -> Globals {
        Globals {
            names: [self.std, self.dst.map_or(NO_NAME.as_ptr(), |dst| dst.name)],
            timezone: -self.std_utoff,
            daylight: self.dst.is_some(),
        }
    }
}

// ---------------------------------------------------------------------------
// The current zone
// ---------------------------------------------------------------------------

/// Where the current zone's rules come from.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "one lives in a static, and there is no allocator to box a layout with"
)]
enum Config {
    /// A POSIX string, or UTC.
    Posix(Posix),
    /// A TZif file, held whole in memory from `malloc`.
    File {
        data: *mut u8,
        len: usize,
        layout: Layout,
    },
}

impl Config {
    /// The zone in effect at `t`.
    fn at(&self, t: i64, local: bool) -> Local {
        match self {
            Self::Posix(posix) => posix.at(t, local),
            Self::File { data, len, layout } => {
                // SAFETY: `data` holds `len` bytes from `malloc` that nothing
                // writes while the configuration is installed.
                let bytes = unsafe { slice::from_raw_parts(*data, *len) };
                layout.at(bytes, t, local).unwrap_or(Local::UTC)
            }
        }
    }

    /// `tzname`, `timezone` and `daylight` for this zone.
    fn globals(&self) -> Globals {
        match self {
            Self::Posix(posix) => posix.globals(),
            Self::File { data, len, layout } => {
                // SAFETY: as in `at`.
                let bytes = unsafe { slice::from_raw_parts(*data, *len) };
                layout.globals(bytes)
            }
        }
    }

    /// Frees what the configuration holds.
    fn release(self) {
        if let Self::File { data, .. } = self {
            // SAFETY: the file came from `malloc`, and the configuration that
            // held it is gone.
            unsafe { free(data.cast()) };
        }
    }
}

/// The size of the copy of `TZ` kept to notice a change: musl's bound.
const KEY_CAP: usize = PATH_MAX + 2;

/// The current zone and the `TZ` it was made from.
#[derive(Debug)]
struct State {
    loaded: bool,
    key: [u8; KEY_CAP],
    key_len: usize,
    config: Config,
}

/// [`State`] behind a lock.
#[derive(Debug)]
struct Guarded {
    lock: SpinLock,
    state: UnsafeCell<State>,
}

// SAFETY: the state is only reached through `Guarded::with`, which holds the
// lock for as long as the reference lives.
unsafe impl Sync for Guarded {}

impl Guarded {
    /// Runs `f` on the state with the lock held. `f` must not make a system
    /// call or call back into this.
    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let _held = self.lock.lock();
        // SAFETY: the lock is held, so this is the only reference.
        f(unsafe { &mut *self.state.get() })
    }
}

/// The current zone.
static ZONE: Guarded = Guarded {
    lock: SpinLock::new(),
    state: UnsafeCell::new(State {
        loaded: false,
        key: [0; KEY_CAP],
        key_len: 0,
        config: Config::Posix(Posix::UTC),
    }),
};

/// The value of the environment variable `name`, if set.
fn env(name: &CStr) -> Option<&'static [u8]> {
    // SAFETY: `name` is a C string. The environment is the program's to keep
    // consistent, as for every caller of `getenv`.
    let value = unsafe { stdlib::getenv(name.as_ptr()) };
    if value.is_null() {
        return None;
    }
    // SAFETY: `getenv` returns a NUL-terminated string in the environment.
    Some(unsafe { CStr::from_ptr(value) }.to_bytes())
}

/// The `TZ` to act on: `/etc/localtime` when unset, and `UTC` when empty or
/// longer than a path, as in musl.
fn effective_tz() -> &'static [u8] {
    match env(c"TZ") {
        None => b"/etc/localtime",
        Some(b"") => b"UTC",
        Some(tz) if tz.len() > PATH_MAX + 1 => b"UTC",
        Some(tz) => tz,
    }
}

/// Reads the whole file at `path`, which holds no NUL, into memory from
/// `malloc`. `None` if it cannot be opened or read, or is larger than
/// [`MAX_FILE`].
fn read_file(path: &[u8]) -> Option<(*mut u8, usize)> {
    let mut name = [0u8; PATH_MAX];
    if path.len() >= name.len() || path.contains(&0) {
        return None;
    }
    for (slot, &byte) in name.iter_mut().zip(path) {
        *slot = byte;
    }
    // SAFETY: `name` is NUL-terminated, and `openat` only reads it.
    let fd = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD.cast_unsigned(),
            name.as_ptr().addr(),
            OPEN_FLAGS,
            0,
        )
    };
    let fd = errno::decode(fd).ok()?;
    let file = read_all(fd);
    // SAFETY: `fd` is the descriptor just opened, and nothing else uses it.
    let _ = unsafe { syscall::syscall2(nr::CLOSE, fd, 0) };
    file
}

/// Reads `fd` to its end into memory from `malloc`.
fn read_all(fd: usize) -> Option<(*mut u8, usize)> {
    let mut data: *mut u8 = null_mut();
    let mut cap = 0;
    let mut len = 0;
    loop {
        if len == cap {
            if cap == MAX_FILE {
                // SAFETY: `data` is null or came from `malloc`.
                unsafe { free(data.cast()) };
                return None;
            }
            let grown_cap = if cap == 0 { 4096 } else { cap * 2 };
            // SAFETY: `data` is null or came from `malloc`, and nothing else
            // holds it.
            let grown = unsafe { realloc(data.cast(), grown_cap) }.cast::<u8>();
            if grown.is_null() {
                // SAFETY: as above.
                unsafe { free(data.cast()) };
                return None;
            }
            data = grown;
            cap = grown_cap;
        }
        // SAFETY: `data` holds `cap` bytes, the last `cap - len` unused.
        let ret =
            unsafe { syscall::syscall3(nr::READ, fd, data.wrapping_add(len).addr(), cap - len) };
        match errno::decode(ret) {
            Ok(0) => return Some((data, len)),
            Ok(count) => len += count,
            Err(errno::EINTR) => {}
            Err(_) => {
                // SAFETY: as above.
                unsafe { free(data.cast()) };
                return None;
            }
        }
    }
}

/// Reads the zone file `name` in `dir`.
fn read_in(dir: &[u8], name: &[u8]) -> Option<(*mut u8, usize)> {
    let mut path = [0u8; PATH_MAX];
    let mut cursor = tm::Cursor::new(&mut path);
    cursor.text(dir);
    if dir.last() != Some(&b'/') {
        cursor.push(b'/');
    }
    cursor.text(name);
    if cursor.lost() {
        return None;
    }
    read_file(cursor.written())
}

/// Finds and reads the zone file `TZ` names, less any leading `:`.
fn find_file(name: &[u8]) -> Option<(*mut u8, usize)> {
    let secure = auxv::get(AT_SECURE).is_some_and(|value| value != 0);
    if let Some(b'/' | b'.') = name.first() {
        if secure && name != b"/etc/localtime" {
            return None;
        }
        return read_file(name);
    }
    if name.is_empty() || name.len() > NAME_MAX || name.contains(&b'.') {
        return None;
    }
    if !secure && let Some(dir) = env(c"TZDIR").filter(|dir| !dir.is_empty()) {
        return read_in(dir, name);
    }
    ZONE_DIRS.iter().find_map(|dir| read_in(dir, name))
}

/// The zone `tz` describes.
fn load(tz: &[u8]) -> Config {
    if is_posix_form(tz) {
        return Config::Posix(parse_posix(tz).unwrap_or(Posix::UTC));
    }
    let name = tz.strip_prefix(b":").unwrap_or(tz);
    let Some((data, len)) = find_file(name) else {
        return Config::Posix(Posix::UTC);
    };
    // SAFETY: `read_all` filled `len` bytes of `data`.
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    match Layout::parse(bytes) {
        Some(layout) => Config::File { data, len, layout },
        None => {
            // SAFETY: `data` came from `malloc`, and nothing refers to it.
            unsafe { free(data.cast()) };
            Config::Posix(Posix::UTC)
        }
    }
}

/// Brings the current zone up to date with `TZ`.
fn refresh() {
    let tz = effective_tz();
    let current = ZONE.with(|state| state.loaded && state.key.get(..state.key_len) == Some(tz));
    if current {
        return;
    }
    // Reading a file makes system calls, which the lock must not be held
    // across.
    let config = load(tz);
    let globals = config.globals();
    let old = ZONE.with(|state| {
        for (slot, &byte) in state.key.iter_mut().zip(tz) {
            *slot = byte;
        }
        state.key_len = tz.len().min(KEY_CAP);
        state.loaded = true;
        let [std, dst] = &tzname;
        std.store(globals.names[0].cast_mut(), Ordering::Relaxed);
        dst.store(globals.names[1].cast_mut(), Ordering::Relaxed);
        timezone.store(globals.timezone, Ordering::Relaxed);
        daylight.store(c_int::from(globals.daylight), Ordering::Relaxed);
        core::mem::replace(&mut state.config, config)
    });
    old.release();
}

/// The zone in effect at `t`, after bringing the zone up to date with `TZ`.
/// `t` is local wall-clock seconds when `local`, and UTC seconds otherwise.
pub fn at(t: i64, local: bool) -> Local {
    refresh();
    ZONE.with(|state| state.config.at(t, local))
}

/// Sets `tzname`, `timezone` and `daylight` from `TZ`, reading the zone file
/// it names if it has changed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tzset() {
    refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(p: *const c_char) -> &'static [u8] {
        // SAFETY: every name here was interned.
        unsafe { CStr::from_ptr(p) }.to_bytes()
    }

    #[test]
    fn interned_names_are_shared_and_known() {
        let a = intern(b"CEST").unwrap_or(UTC.as_ptr());
        let b = intern(b"CEST").unwrap_or(UTC.as_ptr());
        assert_eq!(a, b);
        assert_eq!(name(a), b"CEST");
        assert!(is_known_name(a));
        assert!(!is_known_name(c"CEST".as_ptr()));
        assert_eq!(intern(b"UTC"), Some(UTC.as_ptr()));
    }

    #[test]
    fn posix_form_is_told_from_a_file_name() {
        assert!(is_posix_form(b"EST5EDT"));
        assert!(is_posix_form(b"UTC"));
        assert!(is_posix_form(b"GMT"));
        assert!(is_posix_form(b"<+0330>-3:30"));
        assert!(!is_posix_form(b"Europe/Berlin"));
        assert!(!is_posix_form(b":UTC"));
        assert!(!is_posix_form(b"EST"));
        assert!(!is_posix_form(b"/etc/localtime"));
    }

    #[test]
    fn posix_strings_parse() {
        let cet = parse_posix(b"CET-1CEST,M3.5.0,M10.5.0/3");
        let Some(Posix {
            std,
            std_utoff,
            dst: Some(dst),
        }) = cet
        else {
            panic!("CET did not parse");
        };
        assert_eq!((name(std), std_utoff), (&b"CET"[..], 3600));
        assert_eq!((name(dst.name), dst.utoff), (&b"CEST"[..], 7200));
        assert_eq!(
            dst.end,
            Rule {
                day: RuleDay::Month {
                    month: 10,
                    week: 5,
                    wday: 0
                },
                time: 3 * 3600
            }
        );
        let quoted = parse_posix(b"<+0330>-3:30").map(|p| (name(p.std), p.std_utoff, p.dst));
        assert_eq!(quoted, Some((&b"+0330"[..], 12_600, None)));
        let edt = parse_posix(b"EST5EDT").and_then(|p| p.dst);
        assert_eq!(
            edt.map(|d| (d.utoff, d.start, d.end)),
            Some((-14_400, DEFAULT_RULES.0, DEFAULT_RULES.1))
        );
        let julian = parse_posix(b"XST3XDT,J60/-1,300/167:30:15").and_then(|p| p.dst);
        assert_eq!(
            julian.map(|d| (d.start, d.end)),
            Some((
                Rule {
                    day: RuleDay::Julian(60),
                    time: -3600
                },
                Rule {
                    day: RuleDay::Zero(300),
                    time: 167 * 3600 + 30 * 60 + 15
                }
            ))
        );
    }

    #[test]
    fn malformed_posix_strings_are_refused() {
        for bad in [
            &b""[..],
            b"5",
            b"<+03",
            b"<>3",
            b"EST5EDT,M3.2.0",
            b"EST5EDT,M3.2.0,",
            b"EST5EDT,M13.1.0,M11.1.0",
            b"EST5EDT,M3.6.0,M11.1.0",
            b"EST5EDT,M3.2.7,M11.1.0",
            b"EST5EDT,J0,J365",
            b"EST5EDT,366,1",
            b"EST5EDT,M3.2.0/168,M11.1.0",
            b"EST25",
            b"EST5:60",
            b"EST5EDT4,M3.2.0,M11.1.0junk",
            b"EST5 ",
        ] {
            assert!(
                parse_posix(bad).is_none(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn rules_find_their_days() {
        // Days from Python's `datetime`.
        let last_sunday_march = Rule {
            day: RuleDay::Month {
                month: 3,
                week: 5,
                wday: 0,
            },
            time: 0,
        };
        // 2024-03-31 and 2026-03-29.
        assert_eq!(last_sunday_march.local_secs(2024), 1_711_843_200);
        assert_eq!(last_sunday_march.local_secs(2026), 1_774_742_400);
        // J60 is the 1st of March in any year; 59 is the 29th of February in
        // a leap year.
        let j60 = Rule {
            day: RuleDay::Julian(60),
            time: 0,
        };
        assert_eq!(j60.local_secs(2024), 1_709_251_200);
        let zero59 = Rule {
            day: RuleDay::Zero(59),
            time: 0,
        };
        assert_eq!(zero59.local_secs(2024), 1_709_164_800);
    }

    #[test]
    fn a_posix_rule_switches_at_both_edges() {
        let Some(cet) = parse_posix(b"CET-1CEST,M3.5.0,M10.5.0/3") else {
            panic!("CET did not parse");
        };
        // 2024-03-31 01:00:00 UTC starts CEST; 2024-10-27 01:00:00 UTC ends it.
        assert_eq!(cet.at(1_711_846_799, false).isdst, 0);
        assert_eq!(cet.at(1_711_846_800, false).isdst, 1);
        assert_eq!(cet.at(1_729_990_799, false).isdst, 1);
        assert_eq!(cet.at(1_729_990_800, false).isdst, 0);
        // Local 02:30 on the 31st of March does not exist, and reads as CEST.
        let skipped = cet.at(1_711_852_200, true);
        assert_eq!((skipped.isdst, skipped.utoff, skipped.opp), (1, 7200, 3600));
        // Local 02:30 on the 27th of October happens twice, and reads as CEST.
        assert_eq!(cet.at(1_729_996_200, true).isdst, 1);
        assert_eq!(cet.at(1_729_999_800, true).isdst, 0);
        let southern = parse_posix(b"<+1030>-10:30<+11>-11,M10.1.0,M4.1.0");
        let Some(howe) = southern else {
            panic!("Lord Howe did not parse");
        };
        // January is summer: +11.
        assert_eq!(howe.at(1_704_067_200, false).utoff, 39_600);
        assert_eq!(howe.at(1_719_792_000, false).utoff, 37_800);
    }

    #[test]
    fn a_rule_ending_as_it_starts_is_daylight_saving_all_year() {
        let Some(always) = parse_posix(b"EST5EDT,0/0,J365/25") else {
            panic!("did not parse");
        };
        for t in [1_704_067_200, 1_704_085_200, 1_719_792_000, 1_735_689_599] {
            assert_eq!(always.at(t, false).isdst, 1, "{t}");
        }
    }

    #[test]
    fn host_zone_files_validate_and_resolve() {
        let Ok(berlin) = std::fs::read("/usr/share/zoneinfo/Europe/Berlin") else {
            return;
        };
        let Some(layout) = Layout::parse(&berlin) else {
            panic!("Berlin did not validate");
        };
        assert!(layout.footer.is_some());
        let summer = layout.at(&berlin, 1_720_000_000, false);
        assert!(summer.is_some_and(|l| l.utoff == 7200 && l.isdst == 1));
        let winter = layout.at(&berlin, 1_704_067_200, false);
        assert!(winter.is_some_and(|l| l.utoff == 3600 && l.isdst == 0));
        // Every truncation and every single-byte corruption parses or not,
        // without a panic.
        for len in 0..berlin.len() {
            let _ = berlin.get(..len).and_then(Layout::parse);
        }
        let mut copy = berlin.clone();
        for i in 0..copy.len() {
            if let Some(byte) = copy.get_mut(i) {
                *byte ^= 0xa5;
            }
            if let Some(layout) = Layout::parse(&copy) {
                let _ = layout.at(&copy, 0, false);
                let _ = layout.at(&copy, i64::MAX, true);
                let _ = layout.at(&copy, i64::MIN, true);
            }
            if let Some(byte) = copy.get_mut(i) {
                *byte ^= 0xa5;
            }
        }
    }
}
