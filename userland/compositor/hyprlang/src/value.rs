//! Values read as hyprlang's types read them.
//!
//! Each answers hyprlang's own error text, so a program can put it in its
//! diagnostic unchanged.

use crate::number::{hex, stof, stof32, trim};

/// hyprlang's `INT` (`configStringToInt`): `0x` hex; `rgba(r, g, b, a)`
/// with `a` a fraction, or `rgba(rrggbbaa)`; `rgb(r, g, b)` or
/// `rgb(rrggbb)`; a word *starting* `true`/`on`/`yes` is 1 and
/// `false`/`off`/`no` 0; else a decimal integer. Colours come back as
/// `0xAARRGGBB`.
///
/// # Errors
///
/// hyprlang's text: `cannot parse "x" as an int.`, `invalid hex …`,
/// `failed parsing …`.
pub fn int(text: &str) -> Result<i64, String> {
    if text.starts_with("0x") {
        return parse_hex(text);
    }
    if let Some(inner) = text
        .strip_prefix("rgba(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return rgba(trim(inner));
    }
    if let Some(inner) = text
        .strip_prefix("rgb(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return rgb(trim(inner));
    }
    if ["true", "on", "yes"]
        .iter()
        .any(|word| text.starts_with(word))
    {
        return Ok(1);
    }
    if ["false", "off", "no"]
        .iter()
        .any(|word| text.starts_with(word))
    {
        return Ok(0);
    }
    if !is_integer(text) {
        return Err(format!("cannot parse \"{text}\" as an int."));
    }
    // `std::stoll` throws `out_of_range`, whose `what()` is "stoll".
    text.parse::<i64>()
        .map_err(|_| "stoll threw: stoll".to_owned())
}

/// hyprlang's `parseHex`: all of `text` in base 16, or `invalid hex text`.
fn parse_hex(text: &str) -> Result<i64, String> {
    hex(text).ok_or_else(|| format!("invalid hex {text}"))
}

/// hyprutils' `isNumber(text, false)`: digits, a `-` allowed first, ending
/// in a digit.
fn is_integer(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// The comma-separated parts of `inner`, each trimmed, as hyprlang cuts
/// them at each `find(',')`.
fn parts(inner: &str) -> Vec<&str> {
    inner.split(',').map(trim).collect()
}

/// `rgba(...)` without its function: four parts, `a` a fraction, or eight
/// hex digits `rrggbbaa`.
fn rgba(inner: &str) -> Result<i64, String> {
    let failed = || format!("failed parsing {inner}");
    if inner.bytes().filter(|byte| *byte == b',').count() == 3 {
        let parts = parts(inner);
        let [r, g, b, a] = parts.as_slice() else {
            return Err(failed());
        };
        let alpha = stof32(a).ok_or_else(failed)?;
        let (Ok(r), Ok(g), Ok(b)) = (int(r), int(g), int(b)) else {
            return Err(failed());
        };
        let alpha = byte_of(alpha * 255.0);
        return Ok(i64::from(alpha)
            .wrapping_mul(0x100_0000)
            .wrapping_add(r.wrapping_mul(0x1_0000))
            .wrapping_add(g.wrapping_mul(0x100))
            .wrapping_add(b));
    }
    if inner.len() == 8 {
        let value = parse_hex(inner)?;
        // RGBA to the ARGB hyprlang holds.
        return Ok((value >> 8).wrapping_add(0x100_0000_i64.wrapping_mul(value & 0xFF)));
    }
    Err("rgba() expects length of 8 characters (4 bytes) or 4 comma separated values".to_owned())
}

/// `rgb(...)` without its function: three parts or six hex digits.
fn rgb(inner: &str) -> Result<i64, String> {
    let failed = || format!("failed parsing {inner}");
    if inner.bytes().filter(|byte| *byte == b',').count() == 2 {
        let parts = parts(inner);
        let [r, g, b] = parts.as_slice() else {
            return Err(failed());
        };
        let (Ok(r), Ok(g), Ok(b)) = (int(r), int(g), int(b)) else {
            return Err(failed());
        };
        return Ok(0xFF00_0000_i64
            .wrapping_add(r.wrapping_mul(0x1_0000))
            .wrapping_add(g.wrapping_mul(0x100))
            .wrapping_add(b));
    }
    if inner.len() == 6 {
        return Ok(parse_hex(inner)?.wrapping_add(0xFF00_0000));
    }
    Err("rgb() expects length of 6 characters (3 bytes) or 3 comma separated values".to_owned())
}

/// `uint8_t a = std::round(value)`: the float made an `int` the way x86's
/// `cvttss2si` does (anything out of range is `INT_MIN`), then its low
/// byte, so `rgba(0, 0, 0, 2)` is alpha 254, as hyprlang gets.
fn byte_of(value: f32) -> u8 {
    let rounded = value.round();
    let whole = if (-2_147_483_648.0..2_147_483_648.0).contains(&rounded) {
        // In range, so the conversion is exact.
        rounded as i32
    } else {
        i32::MIN
    };
    whole.to_le_bytes().first().copied().unwrap_or(0)
}

/// hyprlang's `FLOAT` (`std::stof`: leading number, rest ignored).
///
/// # Errors
///
/// `failed parsing a float: stof`.
pub fn float(text: &str) -> Result<f64, String> {
    stof(text).ok_or_else(|| "failed parsing a float: stof".to_owned())
}

/// hyprlang's `VEC2`: two floats separated by one space (a comma before
/// the space is allowed by `stof`'s leading-number rule: `"520, 190"`).
///
/// # Errors
///
/// `failed parsing a vec2: …`.
pub fn vec2(text: &str) -> Result<(f64, f64), String> {
    let failed = |why: &str| format!("failed parsing a vec2: {why}");
    let Some((left, right)) = text.split_once(' ') else {
        return Err(failed("no space"));
    };
    if left.contains(' ') || right.contains(' ') {
        return Err(failed("too many args"));
    }
    match (stof(left), stof(right)) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(failed("stof")),
    }
}

/// A colour as hyprlang's `INT` reads it, split: (r, g, b, a), each 0..=255.
///
/// # Errors
///
/// As [`int`].
pub fn color(text: &str) -> Result<(u8, u8, u8, u8), String> {
    let argb = int(text)?;
    let byte = |shift: u32| u8::try_from((argb >> shift) & 0xFF).unwrap_or(0);
    Ok((byte(16), byte(8), byte(0), byte(24)))
}
