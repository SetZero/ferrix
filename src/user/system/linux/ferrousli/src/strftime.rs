//! `strftime` and `strftime_l`, and `wcsftime` and `wcsftime_l` beside them.
//!
//! The conversions and their numbers follow musl's `src/time/strftime.c`
//! (MIT), in the C locale, which is the only one this library has:
//!
//! * A conversion musl does not know makes `strftime` return 0, as does a
//!   result that does not fit with its NUL. The buffer then holds as much as
//!   fit, terminated.
//! * The flags `-` (no padding), `_` (spaces) and `0` (zeros) choose a number's
//!   padding, as in musl and glibc.
//! * `+` and a width apply to `%C`, `%F`, `%G` and `%Y` only, as POSIX 2008
//!   and musl have it: the number is zero-padded to the width, and a `+` is
//!   written before a year of more than four digits or a century of more
//!   than two.
//! * `E` and `O` are accepted before any conversion and change nothing, as in
//!   musl.
//!
//! Beyond musl, and as glibc has them:
//!
//! * `^` writes the result in upper case, and `#` swaps the case of the
//!   conversions whose case it makes sense to swap: upper case for `%a`,
//!   `%A`, `%b`, `%B` and `%h`, lower case for `%p` and `%Z`.
//! * `%k` and `%l` are the 24-hour and 12-hour hour padded with a space, and
//!   `%P` is `am` or `pm`.
//!
//! `%Z` prints `tm_zone` only when it is an abbreviation the library itself
//! handed out, and nothing otherwise, as in musl, which guards this way
//! against a `struct tm` the program filled in by hand. It prints nothing
//! when `tm_isdst` is negative, and so does `%z`.

use core::ffi::{CStr, c_char, c_void};
use core::slice;

use crate::multibyte::{WChar, mbstowcs};
use crate::tm::{self, Cursor, Number, Tm};
use crate::tz;

/// The room each conversion is written into before it is copied out, as in
/// musl.
const PIECE: usize = 100;

/// The flags before a conversion.
#[derive(Debug, Clone, Copy, Default)]
struct Flags {
    /// `-`, `_` or `0`, or 0 for none.
    pad: u8,
    /// `+`.
    plus: bool,
    /// `^`.
    upper: bool,
    /// `#`.
    swap: bool,
}

/// Whether the year `year` years after 1900 is a leap year.
fn is_leap_since_1900(year: i64) -> bool {
    tm::is_leap(year + 1900)
}

/// The ISO 8601 week number of `tm`, 1 to 53, from its day of the week and of
/// the year. musl's `week_num`, whose unsigned arithmetic is kept so that
/// fields out of range give musl's result.
fn iso_week(tm: &Tm) -> i64 {
    let yday = tm.tm_yday.cast_unsigned();
    let wday = tm.tm_wday.cast_unsigned();
    let base = yday.wrapping_add(7).wrapping_sub(wday.wrapping_add(6) % 7) / 7;
    let mut week = i64::from(base.cast_signed());
    // When the 1st of January is one to three days after a Monday, the week
    // before is also in this year.
    if wday.wrapping_add(371).wrapping_sub(yday).wrapping_sub(2) % 7 <= 2 {
        week += 1;
    }
    if week == 0 {
        week = 52;
        // The year before has 53 weeks if its 31st of December is a Thursday,
        // or a Friday in a leap year.
        let dec31 = wday.wrapping_add(7).wrapping_sub(yday).wrapping_sub(1) % 7;
        if dec31 == 4 || (dec31 == 5 && is_leap_since_1900(i64::from(tm.tm_year % 400 - 1))) {
            week += 1;
        }
    } else if week == 53 {
        // This year has only 52 unless its 1st of January is a Thursday, or a
        // Wednesday in a leap year.
        let jan1 = wday.wrapping_add(371).wrapping_sub(yday) % 7;
        if jan1 != 4 && (jan1 != 3 || !is_leap_since_1900(i64::from(tm.tm_year))) {
            week = 1;
        }
    }
    week
}

/// `names[index]`, or `-` for an index out of range, as musl writes one.
fn name_or_dash(names: &[&'static [u8]], index: i32) -> &'static [u8] {
    usize::try_from(index)
        .ok()
        .and_then(|i| names.get(i))
        .copied()
        .unwrap_or(b"-")
}

/// The text of conversion `conv` for `tm`, written into `buf` or static.
/// `None` for a conversion that does not exist, or a composite one that does
/// not fit.
fn conversion<'a>(conv: u8, tm: &Tm, pad: u8, buf: &'a mut [u8; PIECE]) -> Option<&'a [u8]> {
    let year = i64::from(tm.tm_year) + 1900;
    let mut width = 2;
    let mut default_pad = b'0';
    let value = match conv {
        b'a' => return Some(name_or_dash(&tm::DAY_ABBR, tm.tm_wday)),
        b'A' => return Some(name_or_dash(&tm::DAY_NAME, tm.tm_wday)),
        b'b' | b'h' => return Some(name_or_dash(&tm::MONTH_ABBR, tm.tm_mon)),
        b'B' => return Some(name_or_dash(&tm::MONTH_NAME, tm.tm_mon)),
        b'c' => return composite(b"%a %b %e %T %Y", tm, buf),
        b'C' => year / 100,
        b'd' => i64::from(tm.tm_mday),
        b'e' => {
            default_pad = b'_';
            i64::from(tm.tm_mday)
        }
        b'D' | b'x' => return composite(b"%m/%d/%y", tm, buf),
        b'F' => return composite(b"%Y-%m-%d", tm, buf),
        b'g' | b'G' => {
            let mut iso_year = year;
            if tm.tm_yday < 3 && iso_week(tm) != 1 {
                iso_year -= 1;
            } else if tm.tm_yday > 360 && iso_week(tm) == 1 {
                iso_year += 1;
            }
            if conv == b'g' {
                iso_year % 100
            } else {
                width = 4;
                iso_year
            }
        }
        b'H' => i64::from(tm.tm_hour),
        b'k' => {
            default_pad = b'_';
            i64::from(tm.tm_hour)
        }
        b'I' | b'l' => {
            if conv == b'l' {
                default_pad = b'_';
            }
            match tm.tm_hour {
                0 => 12,
                hour if hour > 12 => i64::from(hour) - 12,
                hour => i64::from(hour),
            }
        }
        b'j' => {
            width = 3;
            i64::from(tm.tm_yday) + 1
        }
        b'm' => i64::from(tm.tm_mon) + 1,
        b'M' => i64::from(tm.tm_min),
        b'n' => return Some(b"\n"),
        b'p' => return Some(if tm.tm_hour >= 12 { b"PM" } else { b"AM" }),
        b'P' => return Some(if tm.tm_hour >= 12 { b"pm" } else { b"am" }),
        b'r' => return composite(b"%I:%M:%S %p", tm, buf),
        b'R' => return composite(b"%H:%M", tm, buf),
        b's' => {
            width = 1;
            tm::tm_to_secs(tm).wrapping_sub(i64::from(tm.tm_gmtoff))
        }
        b'S' => i64::from(tm.tm_sec),
        b't' => return Some(b"\t"),
        b'T' | b'X' => return composite(b"%H:%M:%S", tm, buf),
        b'u' => {
            width = 1;
            if tm.tm_wday == 0 {
                7
            } else {
                i64::from(tm.tm_wday)
            }
        }
        b'U' => {
            let (yday, wday) = (tm.tm_yday.cast_unsigned(), tm.tm_wday.cast_unsigned());
            i64::from(yday.wrapping_add(7).wrapping_sub(wday) / 7)
        }
        b'W' => {
            let (yday, wday) = (tm.tm_yday.cast_unsigned(), tm.tm_wday.cast_unsigned());
            i64::from(yday.wrapping_add(7).wrapping_sub(wday.wrapping_add(6) % 7) / 7)
        }
        b'V' => iso_week(tm),
        b'w' => {
            width = 1;
            i64::from(tm.tm_wday)
        }
        b'y' => (year % 100).abs(),
        b'Y' => {
            if year >= 10_000 {
                let len = {
                    let mut text = Cursor::new(buf);
                    text.number(
                        year,
                        Number {
                            plus: true,
                            ..Number::default()
                        },
                    );
                    text.len()
                };
                return buf.get(..len);
            }
            width = 4;
            year
        }
        b'z' => {
            if tm.tm_isdst < 0 {
                return Some(b"");
            }
            let offset = i64::from(tm.tm_gmtoff);
            let hhmm = offset / 3600 * 100 + offset % 3600 / 60;
            let len = {
                let mut text = Cursor::new(buf);
                text.number(
                    hhmm,
                    Number {
                        precision: 4,
                        plus: true,
                        ..Number::default()
                    },
                );
                text.len()
            };
            return buf.get(..len);
        }
        b'Z' => {
            if tm.tm_isdst < 0 || !tz::is_known_name(tm.tm_zone) {
                return Some(b"");
            }
            // SAFETY: the library's abbreviations are NUL-terminated and live
            // as long as the process.
            return Some(unsafe { CStr::from_ptr(tm.tm_zone) }.to_bytes());
        }
        b'%' => return Some(b"%"),
        _ => return None,
    };
    let format = match if pad == 0 { default_pad } else { pad } {
        b'-' => Number::default(),
        b'_' => Number {
            width,
            ..Number::default()
        },
        _ => Number {
            width,
            zero: true,
            ..Number::default()
        },
    };
    let len = {
        let mut text = Cursor::new(buf);
        text.number(value, format);
        text.len()
    };
    buf.get(..len)
}

/// A conversion defined as another format, written into `buf`.
fn composite<'a>(format_text: &[u8], tm: &Tm, buf: &'a mut [u8; PIECE]) -> Option<&'a [u8]> {
    let len = format(buf, format_text, tm);
    if len == 0 {
        return None;
    }
    buf.get(..len)
}

/// Whether the case of conversion `conv` changes under `flags`: to upper case
/// with `Some(true)` and to lower case with `Some(false)`.
fn case_change(conv: u8, flags: Flags) -> Option<bool> {
    if flags.swap && matches!(conv, b'p' | b'Z') {
        Some(false)
    } else if flags.upper || (flags.swap && matches!(conv, b'a' | b'A' | b'b' | b'B' | b'h')) {
        Some(true)
    } else {
        None
    }
}

/// Appends `byte` at `*len` if there is room.
fn put(out: &mut [u8], len: &mut usize, byte: u8) {
    if let Some(slot) = out.get_mut(*len) {
        *slot = byte;
        *len += 1;
    }
}

/// Formats `tm` by `format_text` into `out`, as `strftime` does, returning the
/// length written before the NUL, or 0 when it did not fit.
pub fn format(out: &mut [u8], format_text: &[u8], tm: &Tm) -> usize {
    let n = out.len();
    let mut len = 0;
    let mut at = 0;
    while len < n {
        let Some(&byte) = format_text.get(at) else {
            put(out, &mut len, 0);
            return len - 1;
        };
        at += 1;
        if byte != b'%' {
            put(out, &mut len, byte);
            continue;
        }

        let mut flags = Flags::default();
        loop {
            match format_text.get(at) {
                Some(&pad @ (b'-' | b'_' | b'0')) => flags.pad = pad,
                Some(b'+') => flags.plus = true,
                Some(b'^') => flags.upper = true,
                Some(b'#') => flags.swap = true,
                _ => break,
            }
            at += 1;
        }
        let digits_at = at;
        let mut width: usize = 0;
        while let Some(&digit) = format_text.get(at)
            && digit.is_ascii_digit()
        {
            width = width
                .saturating_mul(10)
                .saturating_add(usize::from(digit - b'0'));
            at += 1;
        }
        // The width applies only to these, and a width of 0 means 1.
        let spec = format_text.get(at).copied().unwrap_or(0);
        if matches!(spec, b'C' | b'F' | b'G' | b'Y') {
            if width == 0 && at != digits_at {
                width = 1;
            }
        } else {
            width = 0;
        }
        if matches!(spec, b'E' | b'O') {
            at += 1;
        }
        let conv = format_text.get(at).copied().unwrap_or(0);
        at += 1;

        let mut buf = [0u8; PIECE];
        let Some(mut piece) = conversion(conv, tm, flags.pad, &mut buf) else {
            break;
        };
        if width > 0 {
            // musl's handling: drop any sign and leading zeros, then pad the
            // digits back out to the width, with a `-` for a year before 0
            // or a `+` for a long one.
            if let Some((b'+' | b'-', rest)) = piece.split_first() {
                piece = rest;
            }
            while piece.first() == Some(&b'0') && piece.get(1).is_some_and(u8::is_ascii_digit) {
                piece = piece.get(1..).unwrap_or_default();
            }
            let k = piece.len();
            width = width.max(k);
            let digits = piece
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            let long = if spec == b'C' { 3 } else { 5 };
            if tm.tm_year < -1900 {
                put(out, &mut len, b'-');
                width -= 1;
            } else if flags.plus && digits + (width - k) >= long {
                put(out, &mut len, b'+');
                width -= 1;
            }
            while width > k && len < n {
                put(out, &mut len, b'0');
                width -= 1;
            }
        }
        let case = case_change(conv, flags);
        for &byte in piece {
            if len >= n {
                break;
            }
            let byte = match case {
                Some(true) => byte.to_ascii_uppercase(),
                Some(false) => byte.to_ascii_lowercase(),
                None => byte,
            };
            put(out, &mut len, byte);
        }
    }
    if n > 0 {
        let last = len.min(n - 1);
        if let Some(slot) = out.get_mut(last) {
            *slot = 0;
        }
    }
    0
}

/// Formats `*tm` by `format` into the `max` bytes at `s`, and returns the
/// length of the result, not counting its NUL. Returns 0 when the result and
/// its NUL do not fit, or a conversion is unknown.
///
/// # Safety
///
/// `s` must be valid for writing `max` bytes, `format` a NUL-terminated
/// string, and `tm` readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strftime(
    s: *mut c_char,
    max: usize,
    format_text: *const c_char,
    tm: *const Tm,
) -> usize {
    if max == 0 {
        return 0;
    }
    // SAFETY: the caller passes a NUL-terminated format.
    let format_text = unsafe { CStr::from_ptr(format_text) }.to_bytes();
    // SAFETY: the caller passes a readable `struct tm`.
    let tm = unsafe { tm.read() };
    // SAFETY: the caller passes `max` writable bytes, which nothing else
    // refers to during the call.
    let out = unsafe { slice::from_raw_parts_mut(s.cast::<u8>(), max) };
    format(out, format_text, &tm)
}

/// `strftime` in the locale `locale`. There is only the C locale, so the
/// locale is not read.
///
/// # Safety
///
/// As [`strftime`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strftime_l(
    s: *mut c_char,
    max: usize,
    format_text: *const c_char,
    tm: *const Tm,
    _locale: *mut c_void,
) -> usize {
    // SAFETY: the caller's contract is `strftime`'s.
    unsafe { strftime(s, max, format_text, tm) }
}

/// The ASCII byte of a wide character, or `None` for NUL or anything wider.
fn ascii(wc: WChar) -> Option<u8> {
    u8::try_from(wc).ok().filter(|&b| b != 0 && b.is_ascii())
}

/// `strftime` for wide strings: formats `*tm` by `format` into the `max` wide
/// characters at `s`, and returns the length of the result, not counting its
/// NUL, or 0 when it does not fit or a conversion is unknown.
///
/// As in musl 1.2.5's `time/wcsftime.c` (MIT), a character outside a
/// conversion is copied as it is, and each conversion is formatted by
/// [`format`] and widened with `mbstowcs`. A conversion is `%`, then any of
/// `-`, `_`, `0`, `+`, `^` and `#`, a width, `E` or `O`, then its letter.
///
/// # Safety
///
/// `s` must be valid for writing `max` wide characters, `format` a
/// NUL-terminated wide string, and `tm` readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsftime(
    s: *mut WChar,
    max: usize,
    format_text: *const WChar,
    tm: *const Tm,
) -> usize {
    if max == 0 {
        return 0;
    }
    // SAFETY: the caller passes a readable `struct tm`.
    let tm = unsafe { tm.read() };
    let mut len = 0;
    let mut at = 0;
    let push = |len: &mut usize, wc: WChar| {
        if *len + 1 >= max {
            return false;
        }
        // SAFETY: `*len` is below `max - 1`, inside the caller's buffer.
        unsafe { s.wrapping_add(*len).write(wc) };
        *len += 1;
        true
    };
    loop {
        // SAFETY: the format has not ended before `at`.
        let wc = unsafe { format_text.wrapping_add(at).read() };
        if wc == 0 {
            break;
        }
        at += 1;
        if wc != WChar::from(b'%') {
            if !push(&mut len, wc) {
                return 0;
            }
            continue;
        }
        // The conversion, narrowed, with an `x` after it so that a conversion
        // whose text is empty still gives a nonzero length.
        let mut spec = [0u8; 40];
        let mut n = 1;
        if let Some(first) = spec.get_mut(0) {
            *first = b'%';
        }
        loop {
            // SAFETY: the format has not ended before `at`.
            let Some(byte) = ascii(unsafe { format_text.wrapping_add(at).read() }) else {
                return 0;
            };
            let Some(slot) = spec.get_mut(n).filter(|_| n + 1 < 40) else {
                return 0;
            };
            *slot = byte;
            n += 1;
            at += 1;
            if !matches!(
                byte,
                b'-' | b'_' | b'+' | b'^' | b'#' | b'0'..=b'9' | b'E' | b'O'
            ) {
                break;
            }
        }
        if let Some(slot) = spec.get_mut(n) {
            *slot = b'x';
        }
        let mut piece = [0u8; PIECE];
        let written = format(&mut piece, spec.get(..=n).unwrap_or(&[]), &tm);
        // Drop the `x`: the text ends in a NUL where it was.
        let Some(sentinel) = written.checked_sub(1).and_then(|x| piece.get_mut(x)) else {
            return 0;
        };
        *sentinel = 0;
        let mut wide = [0 as WChar; PIECE];
        // SAFETY: `piece` is NUL-terminated and `wide` has room for `PIECE`
        // characters.
        let count = unsafe { mbstowcs(wide.as_mut_ptr(), piece.as_ptr().cast(), PIECE) };
        let Some(text) = wide.get(..count) else {
            return 0;
        };
        for &wc in text {
            if !push(&mut len, wc) {
                return 0;
            }
        }
    }
    // SAFETY: `len` is below `max`.
    unsafe { s.wrapping_add(len).write(0) };
    len
}

/// [`wcsftime`] in the locale `locale`, which is not read.
///
/// # Safety
///
/// As [`wcsftime`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsftime_l(
    s: *mut WChar,
    max: usize,
    format_text: *const WChar,
    tm: *const Tm,
    _locale: *mut c_void,
) -> usize {
    // SAFETY: the caller's contract is `wcsftime`'s.
    unsafe { wcsftime(s, max, format_text, tm) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(format_text: &str, tm: &Tm) -> String {
        let mut out = [0u8; 100];
        let len = format(&mut out, format_text.as_bytes(), tm);
        String::from_utf8_lossy(out.get(..len).unwrap_or_default()).into_owned()
    }

    #[test]
    fn iso_weeks_at_year_ends() {
        // From Python's `date.isocalendar()`.
        for (t, week) in [
            (1_451_779_200_i64, 53), // 2016-01-03
            (1_451_865_600, 1),      // 2016-01-04
            (1_230_595_200, 1),      // 2008-12-30
            (1_262_217_600, 53),     // 2009-12-31
            (1_735_603_200, 1),      // 2024-12-31
        ] {
            let tm = tm::secs_to_tm(t).unwrap_or(Tm::ZERO);
            assert_eq!(iso_week(&tm), week, "{t}");
        }
    }

    #[test]
    fn numbers_flags_and_case() {
        let tm = tm::secs_to_tm(1_451_827_425).unwrap_or(Tm::ZERO);
        assert_eq!(show("%Y-%m-%d %H:%M:%S", &tm), "2016-01-03 13:23:45");
        assert_eq!(show("%-m/%_d/%e|%k|%l|%-I", &tm), "1/ 3/ 3|13| 1|1");
        assert_eq!(show("%^a %#b %#p %^B %P", &tm), "SUN JAN pm JANUARY pm");
        assert_eq!(
            show("%EY %Od %+4Y %08F %012F", &tm),
            "2016 03 2016 2016-01-03 002016-01-03"
        );
    }

    #[test]
    fn a_result_that_does_not_fit_returns_zero() {
        let tm = tm::secs_to_tm(0).unwrap_or(Tm::ZERO);
        let mut out = [0xffu8; 5];
        assert_eq!(format(&mut out, b"%Y-%m", &tm), 0);
        assert_eq!(out, *b"1970\0");
        let mut exact = [0u8; 5];
        assert_eq!(format(&mut exact, b"%Y", &tm), 4);
        assert_eq!(format(&mut exact, b"%Q", &tm), 0);
        assert_eq!(format(&mut exact, b"", &tm), 0);
    }
}
