//! systemd's value syntaxes, as `src/basic/parse-util.c`, `time-util.c` and
//! `extract-word.c` read them.
//!
//! Each function reads one value that a key has already been matched to, and
//! answers with the value or a [`ValueError`]. What a failure means is the
//! key's business: nearly always a warning and the assignment ignored.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;

/// A value that does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueError {
    /// Not of the form the key takes.
    Invalid,
    /// Of the form, but too large, or outside the key's range.
    Range,
    /// A quote that is never closed, or a backslash at the very end.
    Unterminated,
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ValueError::Invalid => "invalid",
            ValueError::Range => "out of range",
            ValueError::Unterminated => "an unterminated quote or escape",
        })
    }
}

/// A boolean, as `parse_boolean` reads one: `1`, `yes`, `y`, `true`, `t`,
/// `on`, and their opposites `0`, `no`, `n`, `false`, `f`, `off`, in any case.
///
/// # Errors
///
/// [`ValueError::Invalid`] for anything else.
pub fn boolean(text: &str) -> Result<bool, ValueError> {
    const YES: [&str; 6] = ["1", "yes", "y", "true", "t", "on"];
    const NO: [&str; 6] = ["0", "no", "n", "false", "f", "off"];
    if YES.iter().any(|word| word.eq_ignore_ascii_case(text)) {
        Ok(true)
    } else if NO.iter().any(|word| word.eq_ignore_ascii_case(text)) {
        Ok(false)
    } else {
        Err(ValueError::Invalid)
    }
}

/// A time span, or none at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Span {
    /// This long.
    Finite(Duration),
    /// `infinity`: no limit.
    Infinity,
}

/// Microseconds in each time unit `parse_sec` knows.
const TIME_UNITS: [(&str, u64); 30] = [
    ("us", 1),
    ("usec", 1),
    ("µs", 1),
    ("μs", 1),
    ("ms", 1_000),
    ("msec", 1_000),
    ("s", 1_000_000),
    ("sec", 1_000_000),
    ("second", 1_000_000),
    ("seconds", 1_000_000),
    ("m", 60_000_000),
    ("min", 60_000_000),
    ("minute", 60_000_000),
    ("minutes", 60_000_000),
    ("h", 3_600_000_000),
    ("hr", 3_600_000_000),
    ("hour", 3_600_000_000),
    ("hours", 3_600_000_000),
    ("d", 86_400_000_000),
    ("day", 86_400_000_000),
    ("days", 86_400_000_000),
    ("w", 604_800_000_000),
    ("week", 604_800_000_000),
    ("weeks", 604_800_000_000),
    ("M", 2_629_800_000_000),
    ("month", 2_629_800_000_000),
    ("months", 2_629_800_000_000),
    ("y", 31_557_600_000_000),
    ("year", 31_557_600_000_000),
    ("years", 31_557_600_000_000),
];

/// The multiplier of a time unit.
fn time_unit(unit: &str) -> Option<u64> {
    TIME_UNITS
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|&(_, micros)| micros)
}

/// A number with an optional fraction, then a unit, as the loop of
/// `parse_time` and `parse_size` reads each piece: the digits, the fraction's
/// digits, and the rest of the text after the unit's letters.
struct Piece<'a> {
    whole: u64,
    fraction: &'a str,
    unit: &'a str,
    rest: &'a str,
}

/// Read one piece from `text`, blanks before it already skipped.
fn piece(text: &str, unit_char: impl Fn(char) -> bool) -> Result<Piece<'_>, ValueError> {
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, rest) = text.split_at(digits_end);
    let (fraction, rest) = match rest.strip_prefix('.') {
        Some(after) => {
            let end = after
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after.len());
            after.split_at(end)
        }
        None => ("", rest),
    };
    if digits.is_empty() && fraction.is_empty() {
        return Err(ValueError::Invalid);
    }
    let whole = if digits.is_empty() {
        0
    } else {
        digits.parse::<u64>().map_err(|_| ValueError::Range)?
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    let unit_end = rest.find(|c: char| !unit_char(c)).unwrap_or(rest.len());
    let (unit, rest) = rest.split_at(unit_end);
    Ok(Piece {
        whole,
        fraction,
        unit,
        rest,
    })
}

/// `whole.fraction` times `multiplier`, the fraction read to at most nine
/// digits, which is below both functions' resolution.
fn scaled(whole: u64, fraction: &str, multiplier: u64) -> Result<u64, ValueError> {
    let mut value = whole.checked_mul(multiplier).ok_or(ValueError::Range)?;
    let mut divisor: u64 = 1;
    let mut part: u64 = 0;
    for digit in fraction.bytes().take(9) {
        divisor *= 10;
        part = part * 10 + u64::from(digit - b'0');
    }
    let extra = u128::from(part) * u128::from(multiplier) / u128::from(divisor);
    value = value
        .checked_add(u64::try_from(extra).map_err(|_| ValueError::Range)?)
        .ok_or(ValueError::Range)?;
    Ok(value)
}

/// A time span, as `parse_sec` reads one when `default_unit` is a second's
/// worth of microseconds: pieces like `1min 30s`, `100ms`, `2.5s` or `5`, the
/// unit of a bare number being `default_unit` microseconds; or `infinity`.
///
/// # Errors
///
/// [`ValueError::Invalid`] for an unknown unit, a negative number, or no
/// number at all, and [`ValueError::Range`] for one that overflows.
pub fn timespan(text: &str, default_unit: u64) -> Result<Span, ValueError> {
    let text = text.trim_matches([' ', '\t']);
    if text == "infinity" {
        return Ok(Span::Infinity);
    }
    let mut rest = text;
    let mut total: u64 = 0;
    loop {
        let piece = piece(rest, char::is_alphabetic)?;
        let multiplier = if piece.unit.is_empty() {
            default_unit
        } else {
            time_unit(piece.unit).ok_or(ValueError::Invalid)?
        };
        total = total
            .checked_add(scaled(piece.whole, piece.fraction, multiplier)?)
            .ok_or(ValueError::Range)?;
        rest = piece.rest.trim_start_matches([' ', '\t']);
        if rest.is_empty() {
            return Ok(Span::Finite(Duration::from_micros(total)));
        }
        if piece.unit.is_empty() {
            // `parse_time` takes a bare number only as the last piece.
            return Err(ValueError::Invalid);
        }
    }
}

/// A time span in seconds by default, as every `…Sec=` key reads one.
///
/// # Errors
///
/// As [`timespan`].
pub fn seconds(text: &str) -> Result<Span, ValueError> {
    timespan(text, 1_000_000)
}

/// A size in bytes, as `parse_size` reads one with base 1024: `64M`, `1.5G`,
/// `512`, or several pieces added, `1G 512M`. The suffixes are `B`, `K`,
/// `M`, `G`, `T`, `P` and `E`.
///
/// # Errors
///
/// [`ValueError::Invalid`] for an unknown suffix or no number, and
/// [`ValueError::Range`] for one that overflows.
pub fn size(text: &str) -> Result<u64, ValueError> {
    let mut rest = text.trim_matches([' ', '\t']);
    let mut total: u64 = 0;
    loop {
        let piece = piece(rest, |c| c.is_ascii_alphabetic())?;
        let shift = match piece.unit {
            "" | "B" => 0,
            "K" => 10,
            "M" => 20,
            "G" => 30,
            "T" => 40,
            "P" => 50,
            "E" => 60,
            _ => return Err(ValueError::Invalid),
        };
        total = total
            .checked_add(scaled(piece.whole, piece.fraction, 1 << shift)?)
            .ok_or(ValueError::Range)?;
        rest = piece.rest.trim_start_matches([' ', '\t']);
        if rest.is_empty() {
            return Ok(total);
        }
    }
}

/// A percentage in hundredths of a percent, as `parse_permyriad` reads one:
/// `50%`, `12.5%`, `0.25%`, `5‰` or `7‱`. `bounded` refuses more than 100%,
/// as every key but `CPUQuota=` does.
///
/// # Errors
///
/// [`ValueError::Invalid`] for text without the sign or with more fraction
/// digits than the unit allows, and [`ValueError::Range`] for too much.
pub fn permyriad(text: &str, bounded: bool) -> Result<u64, ValueError> {
    let (number, per, places) = if let Some(number) = text.strip_suffix('%') {
        (number, 100, 2)
    } else if let Some(number) = text.strip_suffix('‰') {
        (number, 10, 1)
    } else if let Some(number) = text.strip_suffix('‱') {
        (number, 1, 0)
    } else {
        return Err(ValueError::Invalid);
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > places
        || (number.contains('.') && fraction.is_empty())
    {
        return Err(ValueError::Invalid);
    }
    let whole: u64 = whole.parse().map_err(|_| ValueError::Range)?;
    let value = scaled(whole, fraction, per)?;
    if bounded && value > 10_000 {
        return Err(ValueError::Range);
    }
    Ok(value)
}

/// Linux's signal names, by number; the same on every architecture Ferrix
/// runs on.
const SIGNALS: [(&str, u8); 31] = [
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("ILL", 4),
    ("TRAP", 5),
    ("ABRT", 6),
    ("BUS", 7),
    ("FPE", 8),
    ("KILL", 9),
    ("USR1", 10),
    ("SEGV", 11),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("STKFLT", 16),
    ("CHLD", 17),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
    ("TTIN", 21),
    ("TTOU", 22),
    ("URG", 23),
    ("XCPU", 24),
    ("XFSZ", 25),
    ("VTALRM", 26),
    ("PROF", 27),
    ("WINCH", 28),
    ("IO", 29),
    ("PWR", 30),
    ("SYS", 31),
];

/// A signal, by number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Signal(pub u8);

impl Signal {
    /// `SIGHUP`.
    pub const HUP: Signal = Signal(1);
    /// `SIGINT`.
    pub const INT: Signal = Signal(2);
    /// `SIGKILL`.
    pub const KILL: Signal = Signal(9);
    /// `SIGTERM`.
    pub const TERM: Signal = Signal(15);
    /// `SIGCONT`.
    pub const CONT: Signal = Signal(18);

    /// Its name without `SIG`, if it has one.
    pub fn name(self) -> Option<&'static str> {
        SIGNALS
            .iter()
            .find(|&&(_, number)| number == self.0)
            .map(|&(name, _)| name)
    }
}

/// A signal as `signal_from_string` reads one: `SIGTERM`, `TERM` or `15`,
/// from 1 to 64.
///
/// # Errors
///
/// [`ValueError::Invalid`] for an unknown name, [`ValueError::Range`] for a
/// number outside 1 to 64.
pub fn signal(text: &str) -> Result<Signal, ValueError> {
    if let Ok(number) = text.parse::<u8>() {
        return if (1..=64).contains(&number) {
            Ok(Signal(number))
        } else {
            Err(ValueError::Range)
        };
    }
    let name = text.strip_prefix("SIG").unwrap_or(text);
    SIGNALS
        .iter()
        .find(|&&(known, _)| known == name)
        .map(|&(_, number)| Signal(number))
        .ok_or(ValueError::Invalid)
}

/// A word of a value, unquoted and unescaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The text.
    pub text: String,
    /// Whether it was written with no quote and no backslash, so that a bare
    /// `;` can separate commands and a quoted `";"` cannot.
    pub bare: bool,
}

/// One escape after a backslash, as `cunescape_one` reads it: C's letters,
/// `\s` for a space, `\xNN`, `\NNN` in octal, `\uNNNN` and `\UNNNNNNNN`. A
/// backslash before any other character stands for that character.
fn escape(chars: &mut core::str::Chars<'_>) -> Result<char, ValueError> {
    let c = chars.next().ok_or(ValueError::Unterminated)?;
    let code = match c {
        'a' => 0x07,
        'b' => 0x08,
        'f' => 0x0c,
        'n' => 0x0a,
        'r' => 0x0d,
        's' => 0x20,
        't' => 0x09,
        'v' => 0x0b,
        'x' => digits(chars, 2, 16)?,
        'u' => digits(chars, 4, 16)?,
        'U' => digits(chars, 8, 16)?,
        '0'..='7' => c.to_digit(8).unwrap_or(0) * 64 + digits(chars, 2, 8)?,
        other => return Ok(other),
    };
    char::from_u32(code)
        .filter(|&c| c != '\0')
        .ok_or(ValueError::Invalid)
}

/// `count` digits in `radix` from `chars`, as a number.
fn digits(chars: &mut core::str::Chars<'_>, count: usize, radix: u32) -> Result<u32, ValueError> {
    let mut value: u32 = 0;
    for _ in 0..count {
        let digit = chars.next().and_then(|d| d.to_digit(radix));
        value = value
            .checked_mul(radix)
            .zip(digit)
            .and_then(|(value, digit)| value.checked_add(digit))
            .ok_or(ValueError::Invalid)?;
    }
    Ok(value)
}

/// Split `text` into words as `extract_first_word` does with
/// `EXTRACT_UNQUOTE | EXTRACT_CUNESCAPE`: blanks separate words, `'…'` and
/// `"…"` quote parts of one (`a"b c"d` is one word, `ab cd`), and a backslash
/// escapes, inside quotes as well as outside.
///
/// # Errors
///
/// [`ValueError::Unterminated`] for an unclosed quote or a final backslash,
/// and [`ValueError::Invalid`] for an escape that makes no character.
pub fn words(text: &str) -> Result<Vec<Word>, ValueError> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    let mut current: Option<Word> = None;
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, ' ' | '\t' | '\n' | '\r') => {
                if let Some(word) = current.take() {
                    out.push(word);
                }
            }
            (_, '\\') => {
                let escaped = escape(&mut chars)?;
                let word = current.get_or_insert_with(new_word);
                word.text.push(escaped);
                word.bare = false;
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                current.get_or_insert_with(new_word).bare = false;
            }
            (Some(open), _) if open == c => quote = None,
            _ => current.get_or_insert_with(new_word).text.push(c),
        }
    }
    if quote.is_some() {
        return Err(ValueError::Unterminated);
    }
    out.extend(current);
    Ok(out)
}

fn new_word() -> Word {
    Word {
        text: String::new(),
        bare: true,
    }
}

/// Whether `name` may name an environment variable: a letter or `_`, then
/// letters, digits and `_`.
pub fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `path` is absolute and has no `..`, as `path_is_absolute` and
/// `path_is_normalized` together require of most path keys.
pub fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/') && !path.split('/').any(|part| part == "..")
}
