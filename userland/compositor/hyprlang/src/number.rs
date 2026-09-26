//! The C and C++ library pieces hyprlang leans on, done over again: the C
//! locale's `isspace`, hyprutils' `trim` and `CConstVarList`, `strtof` (as
//! `std::stof` calls it), `strtoll` in base 16, and `std::format("{}", f)`
//! for a `float`.

/// `std::isspace` in the C locale: space, `\t`, `\n`, `\v`, `\f`, `\r`.
/// Rust's `is_ascii_whitespace` leaves out `\v`.
pub(crate) fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// [`is_c_space`] for a `char`; nothing outside ASCII is a space to it.
pub(crate) fn is_c_space_char(c: char) -> bool {
    u8::try_from(c).is_ok_and(is_c_space)
}

/// hyprutils' `trim`: C spaces off both ends.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(is_c_space_char)
}

/// `CConstVarList(text, 0, 's', true)`: the words between runs of C spaces.
pub(crate) fn words(text: &str) -> Vec<&str> {
    text.split(is_c_space_char)
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether `bytes` begins with the lower-case ASCII `prefix`, in any case.
fn starts_with_ignoring_case(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.len() >= prefix.len()
        && bytes
            .iter()
            .zip(prefix)
            .all(|(byte, want)| byte.to_ascii_lowercase() == *want)
}

/// How many of `bytes`, from `from`, satisfy `test`.
fn run(bytes: &[u8], from: usize, test: impl Fn(u8) -> bool) -> usize {
    bytes.get(from..).map_or(0, |rest| {
        rest.iter().take_while(|byte| test(**byte)).count()
    })
}

/// `strtod`'s reading of the longest number `text` begins with: spaces,
/// a sign, then `inf`/`infinity`, `nan`, a `0x` hexadecimal float, or a
/// decimal one. `None` where `strtod` converts nothing.
fn leading_number(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut at = run(bytes, 0, is_c_space);
    let mut negative = false;
    if let Some(&sign) = bytes.get(at)
        && (sign == b'+' || sign == b'-')
    {
        negative = sign == b'-';
        at += 1;
    }
    let rest = bytes.get(at..).unwrap_or_default();
    let magnitude = if starts_with_ignoring_case(rest, b"inf") {
        f64::INFINITY
    } else if starts_with_ignoring_case(rest, b"nan") {
        f64::NAN
    } else if let Some(value) = hexadecimal(rest) {
        value
    } else {
        decimal(rest)?
    };
    Some(if negative { -magnitude } else { magnitude })
}

/// A `0x` float: hex digits, a point, hex digits, then `p` and a decimal
/// power of two. `None` unless a digit follows the `0x`, where `strtod`
/// reads the `0` alone.
fn hexadecimal(bytes: &[u8]) -> Option<f64> {
    if !starts_with_ignoring_case(bytes, b"0x") {
        return None;
    }
    let whole = run(bytes, 2, |byte| byte.is_ascii_hexdigit());
    let point = 2 + whole;
    let fraction = if bytes.get(point) == Some(&b'.') {
        run(bytes, point + 1, |byte| byte.is_ascii_hexdigit())
    } else {
        0
    };
    if whole + fraction == 0 {
        return None;
    }
    let mut mantissa = 0.0_f64;
    for &byte in bytes.get(2..point).unwrap_or_default() {
        mantissa = mantissa * 16.0 + hex_digit(byte);
    }
    let fraction_start = point + 1;
    for &byte in bytes
        .get(fraction_start..fraction_start + fraction)
        .unwrap_or_default()
    {
        mantissa = mantissa * 16.0 + hex_digit(byte);
    }
    let mut exponent = -4 * i64::try_from(fraction).unwrap_or(i64::MAX / 8);
    let after = if fraction > 0 {
        fraction_start + fraction
    } else if bytes.get(point) == Some(&b'.') {
        point + 1
    } else {
        point
    };
    if let Some(&p) = bytes.get(after)
        && (p == b'p' || p == b'P')
    {
        let mut at = after + 1;
        let mut negative = false;
        if let Some(&sign) = bytes.get(at)
            && (sign == b'+' || sign == b'-')
        {
            negative = sign == b'-';
            at += 1;
        }
        let digits = run(bytes, at, |byte| byte.is_ascii_digit());
        if digits > 0 {
            let mut power: i64 = 0;
            for &byte in bytes.get(at..at + digits).unwrap_or_default() {
                power = power
                    .saturating_mul(10)
                    .saturating_add(i64::from(byte - b'0'));
            }
            exponent = exponent.saturating_add(if negative { -power } else { power });
        }
    }
    let exponent = i32::try_from(exponent.clamp(-10_000, 10_000)).unwrap_or(0);
    Some(mantissa * 2.0_f64.powi(exponent))
}

/// A hexadecimal digit's value.
fn hex_digit(byte: u8) -> f64 {
    let value = match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    };
    f64::from(value)
}

/// A decimal float: digits, a point, digits (one at least in all), then an
/// exponent if a digit follows its `e`.
fn decimal(bytes: &[u8]) -> Option<f64> {
    let whole = run(bytes, 0, |byte| byte.is_ascii_digit());
    let fraction = if bytes.get(whole) == Some(&b'.') {
        run(bytes, whole + 1, |byte| byte.is_ascii_digit())
    } else {
        0
    };
    if whole + fraction == 0 {
        return None;
    }
    let mut end = if bytes.get(whole) == Some(&b'.') {
        whole + 1 + fraction
    } else {
        whole
    };
    if let Some(&e) = bytes.get(end)
        && (e == b'e' || e == b'E')
    {
        let mut at = end + 1;
        if let Some(&sign) = bytes.get(at)
            && (sign == b'+' || sign == b'-')
        {
            at += 1;
        }
        let digits = run(bytes, at, |byte| byte.is_ascii_digit());
        if digits > 0 {
            end = at + digits;
        }
    }
    let number = core::str::from_utf8(bytes.get(..end)?).ok()?;
    number.parse::<f64>().ok()
}

/// `std::stof`: the leading number of `text`, which must fit a `float` --
/// `strtof` sets `ERANGE` on overflow and on underflow, and `stof` throws
/// then as it does when nothing converts. The value keeps `f64` precision.
pub(crate) fn stof(text: &str) -> Option<f64> {
    let value = leading_number(text)?;
    // The narrowing is the range check strtof makes.
    let narrow = value as f32;
    if value.is_finite() && narrow.is_infinite() {
        return None;
    }
    if value != 0.0 && narrow.abs() < f32::MIN_POSITIVE {
        return None;
    }
    Some(value)
}

/// `std::stof` as a `float`, for arithmetic hyprlang does in `float`.
pub(crate) fn stof32(text: &str) -> Option<f32> {
    // stof returns a float, and stof() has checked the range.
    stof(text).map(|value| value as f32)
}

/// `std::stoll(text, &position, 16)` that must use all of `text`:
/// hyprlang's `parseHex`. Spaces, a sign and a `0x` may lead.
pub(crate) fn hex(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut at = run(bytes, 0, is_c_space);
    let mut negative = false;
    if let Some(&sign) = bytes.get(at)
        && (sign == b'+' || sign == b'-')
    {
        negative = sign == b'-';
        at += 1;
    }
    if starts_with_ignoring_case(bytes.get(at..).unwrap_or_default(), b"0x")
        && bytes.get(at + 2).is_some_and(u8::is_ascii_hexdigit)
    {
        at += 2;
    }
    let digits = run(bytes, at, |byte| byte.is_ascii_hexdigit());
    if digits == 0 || at + digits != bytes.len() {
        return None;
    }
    let mut magnitude: i128 = 0;
    for &byte in bytes.get(at..).unwrap_or_default() {
        let digit = u32::from(byte);
        let value = char::from_u32(digit)?.to_digit(16)?;
        magnitude = magnitude.checked_mul(16)?.checked_add(i128::from(value))?;
        if magnitude > i128::from(i64::MAX) + 1 {
            return None;
        }
    }
    let signed = if negative { -magnitude } else { magnitude };
    i64::try_from(signed).ok()
}

/// `std::format("{}", value)` for a `float`: the shortest digits that read
/// back, fixed or scientific (`1e+20`, `1e-05`), whichever is shorter,
/// fixed on a tie.
pub(crate) fn format_float(value: f32) -> String {
    if value.is_nan() {
        return if value.is_sign_negative() {
            "-nan".to_owned()
        } else {
            "nan".to_owned()
        };
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-inf".to_owned()
        } else {
            "inf".to_owned()
        };
    }
    let fixed = format!("{value}");
    let exponential = format!("{value:e}");
    let Some((mantissa, exponent)) = exponential.split_once('e') else {
        return fixed;
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return fixed;
    };
    let sign = if exponent < 0 { '-' } else { '+' };
    let scientific = format!("{mantissa}e{sign}{:02}", exponent.unsigned_abs());
    if scientific.len() < fixed.len() {
        scientific
    } else {
        fixed
    }
}
