//! `time.h`'s calendar: `gmtime`, `gmtime_r`, `localtime`, `localtime_r`,
//! `mktime`, `timegm`, `difftime`, `asctime`, `asctime_r`, `ctime` and
//! `ctime_r`, and the `struct tm` arithmetic `strftime` and `strptime` share.
//!
//! The clocks themselves, `time` and `clock_gettime`, are in `time`; time
//! zones are in [`crate::tz`].
//!
//! # Range
//!
//! `time_t` is 64 bits, and `struct tm` counts years in an `int`. A time whose
//! year does not fit is refused with `EOVERFLOW`, as in musl, whose bound this
//! keeps: a year is at most 31622400 seconds, so any `t` beyond
//! `INT_MAX * 31622400` in either direction is refused before the arithmetic
//! starts. Within it, every intermediate fits in 64 bits.
//!
//! Days and dates convert with Howard Hinnant's `days_from_civil` and
//! `civil_from_days`, from "chrono-Compatible Low-Level Date Algorithms",
//! which he placed in the public domain. They count in the proleptic
//! Gregorian calendar with a year 0, which is what `struct tm` means.
//!
//! # The static results
//!
//! `gmtime` and `localtime` each return one static `struct tm`, and `asctime`
//! and `ctime` share one static buffer, as in musl. C documents all four as
//! overwritten by the next call and as unsafe to share between threads.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_double, c_int, c_long};
use core::mem::{offset_of, size_of};
use core::ptr::{null, null_mut};

use crate::{errno, tz};

/// C's `struct tm`, as `time.h` lays it out, `tm_gmtoff` and `tm_zone`
/// included. glibc's has the same layout on x86-64.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Tm {
    /// Seconds after the minute, 0 to 60.
    pub tm_sec: c_int,
    /// Minutes after the hour, 0 to 59.
    pub tm_min: c_int,
    /// Hours since midnight, 0 to 23.
    pub tm_hour: c_int,
    /// The day of the month, 1 to 31.
    pub tm_mday: c_int,
    /// Months since January, 0 to 11.
    pub tm_mon: c_int,
    /// Years since 1900.
    pub tm_year: c_int,
    /// Days since Sunday, 0 to 6.
    pub tm_wday: c_int,
    /// Days since the 1st of January, 0 to 365.
    pub tm_yday: c_int,
    /// Positive when daylight saving time is in effect, zero when not, and
    /// negative when unknown.
    pub tm_isdst: c_int,
    /// Seconds east of UTC.
    pub tm_gmtoff: c_long,
    /// The zone's abbreviation.
    pub tm_zone: *const c_char,
}

const _: () = assert!(size_of::<Tm>() == 56);
const _: () = assert!(offset_of!(Tm, tm_isdst) == 32);
const _: () = assert!(offset_of!(Tm, tm_gmtoff) == 40);
const _: () = assert!(offset_of!(Tm, tm_zone) == 48);

impl Tm {
    /// A `struct tm` of zeros, with a null zone.
    pub const ZERO: Self = Self {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: null(),
    };
}

/// Seconds in a day.
pub const DAY: i64 = 86_400;

/// The most seconds a year can hold, a leap year's.
const LONGEST_YEAR: i64 = 366 * DAY;
/// The earliest time `struct tm` is asked to hold, as in musl.
const MIN_SECS: i64 = i32::MIN as i64 * LONGEST_YEAR;
/// The latest time `struct tm` is asked to hold, as in musl.
const MAX_SECS: i64 = i32::MAX as i64 * LONGEST_YEAR;

/// The C locale's abbreviated day names, from Sunday.
pub const DAY_ABBR: [&[u8]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
/// The C locale's day names, from Sunday.
pub const DAY_NAME: [&[u8]; 7] = [
    b"Sunday",
    b"Monday",
    b"Tuesday",
    b"Wednesday",
    b"Thursday",
    b"Friday",
    b"Saturday",
];
/// The C locale's abbreviated month names.
pub const MONTH_ABBR: [&[u8]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];
/// The C locale's month names.
pub const MONTH_NAME: [&[u8]; 12] = [
    b"January",
    b"February",
    b"March",
    b"April",
    b"May",
    b"June",
    b"July",
    b"August",
    b"September",
    b"October",
    b"November",
    b"December",
];

/// Whether `year`, counted from year 0, is a leap year.
pub const fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The number of days in `month` (1 to 12) of a year that is or is not leap.
pub const fn days_in_month(month: i64, leap: bool) -> i64 {
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The days from 1970-01-01 to `year`-`month`-`day`, where `month` is 1 to 12
/// and `day` may run past the month's end.
///
/// Hinnant's `days_from_civil`. It holds for any year within a few hundred
/// million times `int`'s range.
pub const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The year, month (1 to 12) and day of the date `days` after 1970-01-01.
///
/// Hinnant's `civil_from_days`.
pub const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The year `t` seconds after the epoch falls in.
pub const fn year_of(t: i64) -> i64 {
    civil_from_days(t.div_euclid(DAY)).0
}

/// The calendar time `t` seconds after the epoch, in UTC, or `None` when its
/// year does not fit an `int`. The zone fields are zero and null.
pub fn secs_to_tm(t: i64) -> Option<Tm> {
    if !(MIN_SECS..=MAX_SECS).contains(&t) {
        return None;
    }
    let days = t.div_euclid(DAY);
    let secs = t.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    let tm_year = c_int::try_from(year - 1900).ok()?;
    let yday = days - days_from_civil(year, 1, 1);
    // The range check bounds every other field well within `int`.
    Some(Tm {
        tm_sec: (secs % 60) as c_int,
        tm_min: (secs / 60 % 60) as c_int,
        tm_hour: (secs / 3600) as c_int,
        tm_mday: day as c_int,
        tm_mon: (month - 1) as c_int,
        tm_year,
        // 1970-01-01 was a Thursday.
        tm_wday: (days + 4).rem_euclid(7) as c_int,
        tm_yday: yday as c_int,
        ..Tm::ZERO
    })
}

/// The seconds from the epoch to the calendar time `tm` describes, reading it
/// as UTC and letting every field run out of its range: month 13 is January
/// of the next year, day 0 the last day of the month before, and so on.
///
/// Every `int` field together stays far inside 64 bits, so this cannot
/// overflow.
pub fn tm_to_secs(tm: &Tm) -> i64 {
    let month = i64::from(tm.tm_mon);
    let year = i64::from(tm.tm_year) + 1900 + month.div_euclid(12);
    let days = days_from_civil(year, month.rem_euclid(12) + 1, 1) + i64::from(tm.tm_mday) - 1;
    days * DAY + 3600 * i64::from(tm.tm_hour) + 60 * i64::from(tm.tm_min) + i64::from(tm.tm_sec)
}

/// `t` as local calendar time, or `None` when its year does not fit.
pub fn local(t: i64) -> Option<Tm> {
    // musl refuses these before the zone lookup, which works in years.
    if !(MIN_SECS..=MAX_SECS).contains(&t) {
        return None;
    }
    let zone = tz::at(t, false);
    let mut tm = secs_to_tm(t.checked_add(zone.utoff)?)?;
    tm.tm_isdst = zone.isdst;
    tm.tm_gmtoff = zone.utoff;
    tm.tm_zone = zone.name;
    Some(tm)
}

/// `t` as UTC calendar time, or `None` when its year does not fit.
fn utc(t: i64) -> Option<Tm> {
    let mut tm = secs_to_tm(t)?;
    tm.tm_zone = tz::UTC.as_ptr();
    Some(tm)
}

/// `mktime`'s work: the time `tm` names in the local zone, and `tm`
/// normalised, or `None` when the result's year does not fit.
///
/// This is musl's algorithm. The wall-clock seconds are looked up as local
/// time, which picks the offset in effect. A skipped wall-clock time reads as
/// the offset after the change, and a repeated one as the offset before it.
/// When `tm_isdst` asks for the other kind of time than the one found, the
/// difference between the two offsets is applied, which moves a time into the
/// hour on the side the program asked for.
pub fn make_local(tm: &Tm) -> Option<(i64, Tm)> {
    let mut t = tm_to_secs(tm);
    let zone = tz::at(t, true);
    if tm.tm_isdst >= 0 && zone.isdst != tm.tm_isdst {
        t -= zone.opp - zone.utoff;
    }
    t -= zone.utoff;
    let new = local(t)?;
    Some((t, new))
}

/// The static `struct tm` or text buffer a non-reentrant function returns.
#[derive(Debug)]
struct Shared<T>(UnsafeCell<T>);

// SAFETY: C documents `gmtime`, `localtime`, `asctime` and `ctime` as
// returning static storage that the next call overwrites, and as unsafe to
// call from two threads at once. The library writes each buffer only from
// those functions, as musl and glibc do.
unsafe impl<T> Sync for Shared<T> {}

/// What `gmtime` returns.
static GMTIME: Shared<Tm> = Shared(UnsafeCell::new(Tm::ZERO));
/// What `localtime` returns.
static LOCALTIME: Shared<Tm> = Shared(UnsafeCell::new(Tm::ZERO));
/// What `asctime` and `ctime` return.
static ASCTIME: Shared<[c_char; 26]> = Shared(UnsafeCell::new([0; 26]));

/// Stores `tm` at `out`, or sets `errno` to `EOVERFLOW` and returns null when
/// there is none.
///
/// # Safety
///
/// `out` must be valid for writing a `struct tm`.
unsafe fn deliver(tm: Option<Tm>, out: *mut Tm) -> *mut Tm {
    match tm {
        Some(tm) => {
            // SAFETY: the caller vouches for `out`.
            unsafe { out.write(tm) };
            out
        }
        None => {
            errno::set(errno::EOVERFLOW);
            null_mut()
        }
    }
}

/// Breaks `*t` down into UTC calendar time in `*out`.
///
/// # Safety
///
/// `t` must be readable and `out` writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gmtime_r(t: *const i64, out: *mut Tm) -> *mut Tm {
    // SAFETY: the caller passes a readable `time_t`.
    let t = unsafe { t.read() };
    // SAFETY: the caller passes a writable `struct tm`.
    unsafe { deliver(utc(t), out) }
}

/// Breaks `*t` down into UTC calendar time in a static `struct tm`.
///
/// # Safety
///
/// `t` must be readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gmtime(t: *const i64) -> *mut Tm {
    // SAFETY: the static is valid, and see `Shared`.
    unsafe { gmtime_r(t, GMTIME.0.get()) }
}

/// Breaks `*t` down into local calendar time in `*out`.
///
/// # Safety
///
/// `t` must be readable and `out` writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm {
    // SAFETY: the caller passes a readable `time_t`.
    let t = unsafe { t.read() };
    // SAFETY: the caller passes a writable `struct tm`.
    unsafe { deliver(local(t), out) }
}

/// Breaks `*t` down into local calendar time in a static `struct tm`.
///
/// # Safety
///
/// `t` must be readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn localtime(t: *const i64) -> *mut Tm {
    // SAFETY: the static is valid, and see `Shared`.
    unsafe { localtime_r(t, LOCALTIME.0.get()) }
}

/// The time `*tm` names in the local zone. `*tm` is normalised, and its day
/// of the week and of the year filled in. On overflow it returns -1 with
/// `errno` set to `EOVERFLOW` and leaves `*tm` alone.
///
/// # Safety
///
/// `tm` must be valid for reading and writing a `struct tm`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mktime(tm: *mut Tm) -> i64 {
    // SAFETY: the caller passes a readable `struct tm`.
    let given = unsafe { tm.read() };
    let Some((t, new)) = make_local(&given) else {
        errno::set(errno::EOVERFLOW);
        return -1;
    };
    // SAFETY: the caller passes a writable `struct tm`.
    unsafe { tm.write(new) };
    t
}

/// The time `*tm` names in UTC, normalising `*tm` as `mktime` does.
///
/// # Safety
///
/// `tm` must be valid for reading and writing a `struct tm`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn timegm(tm: *mut Tm) -> i64 {
    // SAFETY: the caller passes a readable `struct tm`.
    let t = tm_to_secs(&unsafe { tm.read() });
    let Some(new) = utc(t) else {
        errno::set(errno::EOVERFLOW);
        return -1;
    };
    // SAFETY: the caller passes a writable `struct tm`.
    unsafe { tm.write(new) };
    t
}

/// `t1 - t0` in seconds.
///
/// The difference is taken in 128 bits, so it cannot overflow, and then
/// rounded to a `double` once.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn difftime(t1: i64, t0: i64) -> c_double {
    (i128::from(t1) - i128::from(t0)) as c_double
}

/// Text written into a byte buffer, which remembers whether any of it did not
/// fit.
#[derive(Debug)]
pub struct Cursor<'a> {
    buf: &'a mut [u8],
    len: usize,
    lost: bool,
}

/// How [`Cursor::number`] lays a number out, as `printf`'s `%d` would.
#[derive(Debug, Clone, Copy, Default)]
pub struct Number {
    /// The minimum width.
    pub width: usize,
    /// Pad to the width with zeros after the sign, rather than spaces before
    /// it.
    pub zero: bool,
    /// The minimum number of digits.
    pub precision: usize,
    /// Write `+` before a number that is not negative.
    pub plus: bool,
}

impl<'a> Cursor<'a> {
    /// An empty cursor at the start of `buf`.
    pub const fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            lost: false,
        }
    }

    /// Appends `byte`, or records that it did not fit.
    pub fn push(&mut self, byte: u8) {
        match self.buf.get_mut(self.len) {
            Some(slot) => {
                *slot = byte;
                self.len += 1;
            }
            None => self.lost = true,
        }
    }

    /// Appends `bytes`.
    pub fn text(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push(byte);
        }
    }

    /// Appends `value` laid out as `format` says.
    pub fn number(&mut self, value: i64, format: Number) {
        let mut digits = [0u8; 20];
        let mut count = 0;
        let mut rest = value.unsigned_abs();
        loop {
            if let Some(slot) = digits.get_mut(count) {
                *slot = b'0' + (rest % 10) as u8;
            }
            count += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        let sign = if value < 0 {
            Some(b'-')
        } else if format.plus {
            Some(b'+')
        } else {
            None
        };
        let body = count.max(format.precision);
        let used = body + usize::from(sign.is_some());
        let fill = format.width.saturating_sub(used);
        if !format.zero {
            for _ in 0..fill {
                self.push(b' ');
            }
        }
        if let Some(sign) = sign {
            self.push(sign);
        }
        if format.zero {
            for _ in 0..fill {
                self.push(b'0');
            }
        }
        for _ in count..body {
            self.push(b'0');
        }
        for i in (0..count).rev() {
            self.push(digits.get(i).copied().unwrap_or(b'0'));
        }
    }

    /// How many bytes have been written.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing has been written.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether anything did not fit.
    pub const fn lost(&self) -> bool {
        self.lost
    }

    /// The bytes written so far.
    pub fn written(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or_default()
    }
}

/// `names[index]`, or `???` for an index out of range, as glibc writes one.
pub fn name_or_unknown(names: &[&'static [u8]], index: c_int) -> &'static [u8] {
    usize::try_from(index)
        .ok()
        .and_then(|i| names.get(i))
        .copied()
        .unwrap_or(b"???")
}

/// Writes `tm` as `asctime` does, with its NUL, into `out`. `false` when the
/// text and its NUL would take more than the 26 bytes C allows.
///
/// C defines the text by a `printf` format,
/// `"%.3s %.3s%3d %.2d:%.2d:%.2d %d\n"`. Fields out of their range still
/// print, until the text outgrows the buffer; musl then crashes on purpose,
/// and this reports `EOVERFLOW` as glibc's `asctime_r` does instead. A day or
/// month name out of range prints as `???`, as in glibc.
fn asctime_into(tm: &Tm, out: &mut [c_char; 26]) -> bool {
    let mut scratch = [0u8; 80];
    let mut text = Cursor::new(&mut scratch);
    let two = Number {
        precision: 2,
        ..Number::default()
    };
    text.text(name_or_unknown(&DAY_ABBR, tm.tm_wday));
    text.push(b' ');
    text.text(name_or_unknown(&MONTH_ABBR, tm.tm_mon));
    text.number(
        i64::from(tm.tm_mday),
        Number {
            width: 3,
            ..Number::default()
        },
    );
    text.push(b' ');
    text.number(i64::from(tm.tm_hour), two);
    text.push(b':');
    text.number(i64::from(tm.tm_min), two);
    text.push(b':');
    text.number(i64::from(tm.tm_sec), two);
    text.push(b' ');
    text.number(i64::from(tm.tm_year) + 1900, Number::default());
    text.push(b'\n');
    let written = text.written();
    if written.len() >= out.len() {
        return false;
    }
    for (slot, &byte) in out.iter_mut().zip(written.iter().chain(&[0])) {
        *slot = byte as c_char;
    }
    true
}

/// Writes `*tm` as text, such as `"Sun Sep 16 01:03:52 1973\n"`, into `buf`,
/// which must hold 26 bytes. Returns `buf`, or null with `errno` set to
/// `EOVERFLOW` when the fields are too large for 26 bytes.
///
/// # Safety
///
/// `tm` must be readable and `buf` valid for writing 26 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn asctime_r(tm: *const Tm, buf: *mut c_char) -> *mut c_char {
    // SAFETY: the caller passes a readable `struct tm`.
    let tm = unsafe { tm.read() };
    let mut text = [0; 26];
    if !asctime_into(&tm, &mut text) {
        errno::set(errno::EOVERFLOW);
        return null_mut();
    }
    // SAFETY: the caller passes 26 writable bytes, which is `[c_char; 26]`'s
    // size and alignment.
    unsafe { buf.cast::<[c_char; 26]>().write(text) };
    buf
}

/// `asctime_r` into a static buffer.
///
/// # Safety
///
/// `tm` must be readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn asctime(tm: *const Tm) -> *mut c_char {
    // SAFETY: the static holds 26 bytes, and see `Shared`.
    unsafe { asctime_r(tm, ASCTIME.0.get().cast()) }
}

/// `*t` as local time, written as `asctime_r` writes it into `buf`.
///
/// # Safety
///
/// `t` must be readable and `buf` valid for writing 26 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ctime_r(t: *const i64, buf: *mut c_char) -> *mut c_char {
    let mut tm = Tm::ZERO;
    // SAFETY: the caller passes a readable `time_t`, and `tm` is local.
    if unsafe { localtime_r(t, &raw mut tm) }.is_null() {
        return null_mut();
    }
    // SAFETY: `tm` is filled in, and the caller vouches for `buf`.
    unsafe { asctime_r(&raw const tm, buf) }
}

/// `ctime_r` into `asctime`'s static buffer.
///
/// # Safety
///
/// `t` must be readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ctime(t: *const i64) -> *mut c_char {
    // SAFETY: the static holds 26 bytes, and see `Shared`.
    unsafe { ctime_r(t, ASCTIME.0.get().cast()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tm_year`, `tm_mon`, `tm_mday`, `tm_hour`, `tm_min`, `tm_sec`,
    /// `tm_wday` and `tm_yday`.
    fn fields(tm: &Tm) -> [c_int; 8] {
        [
            tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec, tm.tm_wday,
            tm.tm_yday,
        ]
    }

    #[test]
    fn days_and_dates_agree_across_eras() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(0, 1, 1), -719_528);
        let mut days = -800_000;
        while days < 800_000 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
            assert!((1..=12).contains(&m) && d >= 1 && d <= days_in_month(m, is_leap(y)));
            days += 37;
        }
    }

    #[test]
    fn secs_to_tm_breaks_down_known_times() {
        // Values from Python's `datetime`, proleptic Gregorian.
        let epoch = secs_to_tm(0).unwrap_or(Tm::ZERO);
        assert_eq!(fields(&epoch), [70, 0, 1, 0, 0, 0, 4, 0]);
        let before = secs_to_tm(-1).unwrap_or(Tm::ZERO);
        assert_eq!(fields(&before), [69, 11, 31, 23, 59, 59, 3, 364]);
        let y2038 = secs_to_tm(2_147_483_648).unwrap_or(Tm::ZERO);
        assert_eq!(fields(&y2038), [138, 0, 19, 3, 14, 8, 2, 18]);
        // 0000-02-29 00:00:00, a Tuesday in the leap year 0.
        let year0 = secs_to_tm(-62_162_121_600).unwrap_or(Tm::ZERO);
        assert_eq!(fields(&year0), [-1900, 1, 29, 0, 0, 0, 2, 59]);
    }

    #[test]
    fn the_int_year_bounds_the_range() {
        // musl's bound is loose: a year near it still overflows `int`, which
        // the year check then catches.
        assert!(secs_to_tm(MAX_SECS).is_none());
        assert!(secs_to_tm(MAX_SECS + 1).is_none());
        assert!(secs_to_tm(MIN_SECS).is_none());
        assert!(secs_to_tm(MIN_SECS - 1).is_none());
        assert!(secs_to_tm(i64::MAX).is_none());
        assert!(secs_to_tm(i64::MIN).is_none());
        // The last second of year INT_MAX + 1900 fits; tm_year is INT_MAX.
        let top = Tm {
            tm_year: i32::MAX,
            tm_mon: 11,
            tm_mday: 31,
            tm_hour: 23,
            tm_min: 59,
            tm_sec: 59,
            ..Tm::ZERO
        };
        let t = tm_to_secs(&top);
        assert_eq!(secs_to_tm(t).map(|tm| tm.tm_year), Some(i32::MAX));
        assert!(secs_to_tm(t + 1).is_none());
    }

    #[test]
    fn tm_to_secs_normalises_out_of_range_fields() {
        let tm = Tm {
            tm_year: 124,
            tm_mon: 12,
            tm_mday: 0,
            tm_hour: -1,
            tm_min: 60,
            tm_sec: -3600,
            ..Tm::ZERO
        };
        // 2024-13-00 -01:60:-3600 is 2024-12-31 00:00 - 1 h = 2024-12-30 23:00.
        assert_eq!(tm_to_secs(&tm), 1_735_599_600);
        let back = secs_to_tm(1_735_599_600).unwrap_or(Tm::ZERO);
        assert_eq!(fields(&back), [124, 11, 30, 23, 0, 0, 1, 364]);
        let extreme = Tm {
            tm_year: i32::MIN,
            tm_mon: i32::MIN,
            tm_mday: i32::MIN,
            tm_hour: i32::MIN,
            tm_min: i32::MIN,
            tm_sec: i32::MIN,
            ..Tm::ZERO
        };
        assert!(tm_to_secs(&extreme) < 0);
    }

    #[test]
    fn numbers_lay_out_as_printf_would() {
        let mut buf = [0u8; 64];
        let mut text = Cursor::new(&mut buf);
        text.number(
            -5,
            Number {
                precision: 2,
                ..Number::default()
            },
        );
        text.push(b'|');
        text.number(
            7,
            Number {
                width: 3,
                ..Number::default()
            },
        );
        text.push(b'|');
        text.number(
            -7,
            Number {
                width: 4,
                zero: true,
                ..Number::default()
            },
        );
        text.push(b'|');
        text.number(
            330,
            Number {
                precision: 4,
                plus: true,
                ..Number::default()
            },
        );
        text.push(b'|');
        text.number(i64::MIN, Number::default());
        assert_eq!(text.written(), b"-05|  7|-007|+0330|-9223372036854775808");
    }

    #[test]
    fn asctime_refuses_text_longer_than_26_bytes() {
        let tm = secs_to_tm(0).unwrap_or(Tm::ZERO);
        let mut out = [0; 26];
        assert!(asctime_into(&tm, &mut out));
        let text: Vec<u8> = out.iter().map(|&c| c as u8).collect();
        assert_eq!(&text, b"Thu Jan  1 00:00:00 1970\n\0");
        let big = Tm {
            tm_year: 100_000,
            ..tm
        };
        assert!(!asctime_into(&big, &mut out));
    }
}
