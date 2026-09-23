//! `strptime`.
//!
//! The conversions follow musl's `src/time/strptime.c` (MIT), in the C
//! locale: `%a %A %b %B %h %c %C %d %e %D %H %I %j %m %M %n %t %p %r %R %S %T
//! %U %W %w %x %X %y %Y %%`, each number read to at most as many digits as its
//! largest value has, or to a width given in the format. A space in the format
//! matches any run of white space, including none. `%U` and `%W` are read and
//! discarded, as in musl.
//!
//! Three conversions musl does not have are added, because libc-test checks
//! them and glibc has them: `%F` is `%Y-%m-%d`, `%s` reads seconds since the
//! epoch and fills in every field as `localtime` would, and `%z` reads
//! `+hhmm`, `+hh:mm`, `+hh` or `Z` into `tm_gmtoff`. `E` and `O` are accepted
//! before a conversion and ignored, as in glibc.
//!
//! As in musl, fields are stored as they are read, so a failed parse may have
//! changed some of them.

use core::ffi::{CStr, c_char, c_int, c_long};

use crate::tm::{self, Tm};

/// Whether `byte` is white space in the C locale.
const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The field a number is read into.
#[derive(Debug, Clone, Copy)]
enum Field {
    Sec,
    Min,
    Hour,
    Mday,
    Mon,
    Year,
    Wday,
    Yday,
    Century,
    RelYear,
    Discard,
}

/// The state of one parse: the input, and what `%C` and `%y` have seen.
#[derive(Debug)]
struct Parse<'a> {
    input: &'a [u8],
    at: usize,
    /// Bit 0 when `%y` was read, bit 1 when `%C` was.
    want_century: u8,
    century: c_int,
    rel_year: c_int,
}

impl Parse<'_> {
    /// The next input byte, or 0 at the end.
    fn peek(&self) -> u8 {
        self.input.get(self.at).copied().unwrap_or(0)
    }

    /// Skips white space.
    fn skip_space(&mut self) {
        while is_space(self.peek()) {
            self.at += 1;
        }
    }

    /// Stores `value` into `field`.
    fn store(&mut self, tm: &mut Tm, field: Field, value: c_int) {
        match field {
            Field::Sec => tm.tm_sec = value,
            Field::Min => tm.tm_min = value,
            Field::Hour => tm.tm_hour = value,
            Field::Mday => tm.tm_mday = value,
            Field::Mon => tm.tm_mon = value,
            Field::Year => tm.tm_year = value,
            Field::Wday => tm.tm_wday = value,
            Field::Yday => tm.tm_yday = value,
            Field::Century => self.century = value,
            Field::RelYear => self.rel_year = value,
            Field::Discard => {}
        }
    }

    /// A number from `min` to `min + range - 1`, of at most as many digits as
    /// `min + range` has, stored less `adjust`.
    fn range(
        &mut self,
        tm: &mut Tm,
        field: Field,
        min: c_int,
        range: c_int,
        adjust: c_int,
    ) -> Option<()> {
        if !self.peek().is_ascii_digit() {
            return None;
        }
        let mut value: c_int = 0;
        let mut place = 1;
        while place <= min + range && self.peek().is_ascii_digit() {
            value = value * 10 + c_int::from(self.peek() - b'0');
            self.at += 1;
            place *= 10;
        }
        if value < min || value - min >= range {
            return None;
        }
        self.store(tm, field, value - adjust);
        Some(())
    }

    /// A signed number of at most `width` digits, stored less `adjust`.
    fn digits(&mut self, tm: &mut Tm, field: Field, width: usize, adjust: c_int) -> Option<()> {
        let negative = match self.peek() {
            b'+' => {
                self.at += 1;
                false
            }
            b'-' => {
                self.at += 1;
                true
            }
            _ => false,
        };
        if !self.peek().is_ascii_digit() {
            return None;
        }
        let mut value: c_int = 0;
        let mut count = 0;
        while count < width && self.peek().is_ascii_digit() {
            value = value
                .wrapping_mul(10)
                .wrapping_add(c_int::from(self.peek() - b'0'));
            self.at += 1;
            count += 1;
        }
        if negative {
            value = value.wrapping_neg();
        }
        self.store(tm, field, value.wrapping_sub(adjust));
        Some(())
    }

    /// The index of the name the input starts with, ignoring case: a full
    /// name first, then an abbreviation, each list from its end, as in musl.
    fn name(&mut self, full: &[&[u8]], abbr: &[&[u8]]) -> Option<c_int> {
        for list in [full, abbr] {
            for (i, name) in list.iter().enumerate().rev() {
                let end = self.at + name.len();
                if self
                    .input
                    .get(self.at..end)
                    .is_some_and(|text| text.eq_ignore_ascii_case(name))
                {
                    self.at = end;
                    return c_int::try_from(i).ok();
                }
            }
        }
        None
    }

    /// Seconds since the epoch, for `%s`.
    fn seconds(&mut self, tm: &mut Tm) -> Option<()> {
        let negative = self.peek() == b'-';
        if negative {
            self.at += 1;
        }
        if !self.peek().is_ascii_digit() {
            return None;
        }
        let mut value: i64 = 0;
        while self.peek().is_ascii_digit() {
            let digit = i64::from(self.peek() - b'0');
            value = value.checked_mul(10)?;
            value = if negative {
                value.checked_sub(digit)?
            } else {
                value.checked_add(digit)?
            };
            self.at += 1;
        }
        *tm = tm::local(value)?;
        Some(())
    }

    /// Exactly two digits.
    fn two_digits(&mut self) -> Option<i64> {
        let tens = self.peek();
        let ones = self.input.get(self.at + 1).copied().unwrap_or(0);
        if !tens.is_ascii_digit() || !ones.is_ascii_digit() {
            return None;
        }
        self.at += 2;
        Some(i64::from(tens - b'0') * 10 + i64::from(ones - b'0'))
    }

    /// An offset from UTC, for `%z`.
    fn offset(&mut self, tm: &mut Tm) -> Option<()> {
        let negative = match self.peek() {
            b'Z' => {
                self.at += 1;
                tm.tm_gmtoff = 0;
                return Some(());
            }
            b'+' => false,
            b'-' => true,
            _ => return None,
        };
        self.at += 1;
        let hours = self.two_digits()?;
        let minutes = if self.peek() == b':' {
            self.at += 1;
            self.two_digits()?
        } else if self.peek().is_ascii_digit() {
            self.two_digits()?
        } else {
            0
        };
        if hours > 24 || minutes > 59 {
            return None;
        }
        let secs = hours * 3600 + minutes * 60;
        // At most a day and an hour: it fits a `long`.
        tm.tm_gmtoff = (if negative { -secs } else { secs }) as c_long;
        Some(())
    }

    /// Parses the input from here by `format`, which must be matched whole.
    fn run(&mut self, format: &[u8], tm: &mut Tm) -> Option<()> {
        let mut at = 0;
        while let Some(&byte) = format.get(at) {
            at += 1;
            if byte != b'%' {
                if is_space(byte) {
                    self.skip_space();
                } else if self.peek() != byte || self.at >= self.input.len() {
                    return None;
                } else {
                    self.at += 1;
                }
                continue;
            }
            if format.get(at) == Some(&b'+') {
                at += 1;
            }
            let mut width = None;
            while let Some(&digit) = format.get(at)
                && digit.is_ascii_digit()
            {
                let so_far: usize = width.unwrap_or(0);
                width = Some(
                    so_far
                        .saturating_mul(10)
                        .saturating_add(usize::from(digit - b'0')),
                );
                at += 1;
            }
            if matches!(format.get(at), Some(b'E' | b'O')) {
                at += 1;
            }
            let conv = format.get(at).copied().unwrap_or(0);
            at += 1;
            match conv {
                b'a' | b'A' => tm.tm_wday = self.name(&tm::DAY_NAME, &tm::DAY_ABBR)?,
                b'b' | b'B' | b'h' => tm.tm_mon = self.name(&tm::MONTH_NAME, &tm::MONTH_ABBR)?,
                b'c' => self.nested(b"%a %b %e %H:%M:%S %Y", tm)?,
                b'C' => {
                    self.digits(tm, Field::Century, width.unwrap_or(2), 0)?;
                    self.want_century |= 2;
                }
                b'd' | b'e' => self.range(tm, Field::Mday, 1, 31, 0)?,
                b'D' | b'x' => self.nested(b"%m/%d/%y", tm)?,
                b'F' => self.nested(b"%Y-%m-%d", tm)?,
                b'H' => self.range(tm, Field::Hour, 0, 24, 0)?,
                b'I' => self.range(tm, Field::Hour, 1, 12, 0)?,
                b'j' => self.range(tm, Field::Yday, 1, 366, 1)?,
                b'm' => self.range(tm, Field::Mon, 1, 12, 1)?,
                b'M' => self.range(tm, Field::Min, 0, 60, 0)?,
                b'n' | b't' => self.skip_space(),
                b'p' => {
                    let hour = tm.tm_hour % 12;
                    match self.name(&[b"PM", b"AM"], &[])? {
                        0 => tm.tm_hour = hour + 12,
                        _ => tm.tm_hour = hour,
                    }
                }
                b'r' => self.nested(b"%I:%M:%S %p", tm)?,
                b'R' => self.nested(b"%H:%M", tm)?,
                b's' => self.seconds(tm)?,
                b'S' => self.range(tm, Field::Sec, 0, 61, 0)?,
                b'T' | b'X' => self.nested(b"%H:%M:%S", tm)?,
                b'U' | b'W' => self.range(tm, Field::Discard, 0, 54, 0)?,
                b'w' => self.range(tm, Field::Wday, 0, 7, 0)?,
                b'y' => {
                    self.digits(tm, Field::RelYear, 2, 0)?;
                    self.want_century |= 1;
                }
                b'Y' => {
                    self.digits(tm, Field::Year, width.unwrap_or(4), 1900)?;
                    self.want_century = 0;
                }
                b'z' => self.offset(tm)?,
                b'%' => {
                    if self.peek() != b'%' || self.at >= self.input.len() {
                        return None;
                    }
                    self.at += 1;
                }
                _ => return None,
            }
        }
        if self.want_century != 0 {
            tm.tm_year = self.rel_year;
            if self.want_century & 2 != 0 {
                tm.tm_year = tm
                    .tm_year
                    .wrapping_add(self.century.wrapping_mul(100))
                    .wrapping_sub(1900);
            } else if tm.tm_year <= 68 {
                tm.tm_year += 100;
            }
        }
        Some(())
    }

    /// Parses by `format` from here as a parse of its own, with its own
    /// century state, as musl's recursive call has.
    fn nested(&mut self, format: &[u8], tm: &mut Tm) -> Option<()> {
        let mut inner = Parse {
            input: self.input,
            at: self.at,
            want_century: 0,
            century: 0,
            rel_year: 0,
        };
        inner.run(format, tm)?;
        self.at = inner.at;
        Some(())
    }
}

/// Parses `input` by `format` into `tm`, returning how many bytes were read.
pub fn parse(input: &[u8], format: &[u8], tm: &mut Tm) -> Option<usize> {
    let mut parse = Parse {
        input,
        at: 0,
        want_century: 0,
        century: 0,
        rel_year: 0,
    };
    parse.run(format, tm)?;
    Some(parse.at)
}

/// Parses the string `s` by `format` into `*tm`, and returns a pointer to the
/// first byte not read, or null if the string does not match.
///
/// # Safety
///
/// `s` and `format` must be NUL-terminated strings, and `tm` valid for reading
/// and writing a `struct tm`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strptime(
    s: *const c_char,
    format: *const c_char,
    tm: *mut Tm,
) -> *mut c_char {
    // SAFETY: the caller passes NUL-terminated strings.
    let input = unsafe { CStr::from_ptr(s) }.to_bytes();
    // SAFETY: as above.
    let format = unsafe { CStr::from_ptr(format) }.to_bytes();
    // SAFETY: the caller passes a readable `struct tm`.
    let mut fields = unsafe { tm.read() };
    let read = parse(input, format, &mut fields);
    // SAFETY: the caller passes a writable `struct tm`.
    unsafe { tm.write(fields) };
    match read {
        Some(len) => s.wrapping_add(len).cast_mut(),
        None => core::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(input: &str, format: &str) -> Option<(usize, [c_int; 8])> {
        let mut tm = Tm::ZERO;
        let len = parse(input.as_bytes(), format.as_bytes(), &mut tm)?;
        Some((
            len,
            [
                tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec, tm.tm_wday,
                tm.tm_yday,
            ],
        ))
    }

    #[test]
    fn dates_times_and_names() {
        assert_eq!(
            fields("1991-08-25", "%Y-%m-%d"),
            Some((10, [91, 7, 25, 0, 0, 0, 0, 0]))
        );
        assert_eq!(
            fields("25.08.91", "%d.%m.%y"),
            Some((8, [91, 7, 25, 0, 0, 0, 0, 0]))
        );
        assert_eq!(
            fields("21.10.15", "%d.%m.%y"),
            Some((8, [115, 9, 21, 0, 0, 0, 0, 0]))
        );
        assert_eq!(
            fields("10.7.56 in 18th", "%d.%m.%y in %C th"),
            Some((15, [-44, 6, 10, 0, 0, 0, 0, 0]))
        );
        assert_eq!(
            fields("wednesday MARCH", "%A %b"),
            Some((15, [0, 2, 0, 0, 0, 0, 3, 0]))
        );
        assert_eq!(
            fields("12:30:00 am", "%r"),
            Some((11, [0, 0, 0, 0, 30, 0, 0, 0]))
        );
        assert_eq!(
            fields("20:57:8  rest", "%R:%S "),
            Some((9, [0, 0, 0, 20, 57, 8, 0, 0]))
        );
        assert_eq!(fields("366", "%j"), Some((3, [0, 0, 0, 0, 0, 0, 0, 365])));
    }

    #[test]
    fn out_of_range_and_mismatches_fail() {
        assert_eq!(fields("24", "%H"), None);
        assert_eq!(fields("13", "%m"), None);
        assert_eq!(fields("0", "%d"), None);
        assert_eq!(fields("367", "%j"), None);
        assert_eq!(fields("x", "%Y"), None);
        assert_eq!(fields("20", "%H:%M"), None);
        assert_eq!(fields("5", "%Q"), None);
        assert_eq!(fields("5", "%"), None);
        assert_eq!(fields("Jux", "%b"), None);
    }

    #[test]
    fn offsets() {
        for (text, seconds) in [
            ("+0200", 7200),
            ("-0530", -19_800),
            ("-06", -21_600),
            ("+05:45", 20_700),
            ("Z", 0),
        ] {
            let mut tm = Tm::ZERO;
            assert_eq!(parse(text.as_bytes(), b"%z", &mut tm), Some(text.len()));
            assert_eq!(tm.tm_gmtoff, seconds, "{text}");
        }
        let mut tm = Tm::ZERO;
        assert_eq!(parse(b"+2500", b"%z", &mut tm), None);
    }
}
