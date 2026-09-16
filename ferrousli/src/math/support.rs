//! What every part of the math library shares: constants written in C's
//! hexadecimal notation, the words of a `double`, keeping the optimiser away
//! from operations done for their exceptions, and raising those exceptions.
//!
//! Ported from musl 1.2.5's `src/internal/libm.h` and `src/math/__math_*.c`
//! (MIT; see [`crate::math`] for the notice).

#![allow(dead_code, reason = "shared by modules still being written")]

use core::hint::black_box;

/// A hexadecimal floating constant taken apart: its value is
/// `±mantissa × 2^exponent`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Hex {
    /// Whether it had a minus sign.
    pub(crate) negative: bool,
    /// The digits, as an integer.
    pub(crate) mantissa: u64,
    /// The power of two the digits are scaled by.
    pub(crate) exponent: i32,
}

/// The value of a hexadecimal digit, or 16 for any other byte.
const fn hex_digit(c: u8) -> u64 {
    match c {
        b'0'..=b'9' => (c - b'0') as u64,
        b'a'..=b'f' => (c - b'a' + 10) as u64,
        b'A'..=b'F' => (c - b'A' + 10) as u64,
        _ => 16,
    }
}

/// Parses a C hexadecimal floating constant such as `-0x1.62e42fefa39efp-1`.
///
/// Returns `None` if the text is anything else, or has more than 64 bits of
/// digits.
pub(crate) const fn parse_hex(text: &str) -> Option<Hex> {
    let mut rest = text.as_bytes();
    let mut negative = false;
    if let [sign @ (b'-' | b'+'), tail @ ..] = rest {
        negative = *sign == b'-';
        rest = tail;
    }
    match rest {
        [b'0', b'x' | b'X', tail @ ..] => rest = tail,
        _ => return None,
    }
    let mut mantissa = 0u64;
    let mut exponent = 0i32;
    let mut digits = 0;
    let mut point = false;
    while let [c, tail @ ..] = rest {
        if *c == b'.' && !point {
            point = true;
        } else {
            let digit = hex_digit(*c);
            if digit == 16 {
                break;
            }
            if mantissa >> 60 != 0 {
                return None;
            }
            mantissa = mantissa << 4 | digit;
            if point {
                exponent -= 4;
            }
            digits += 1;
        }
        rest = tail;
    }
    if digits == 0 {
        return None;
    }
    match rest {
        [b'p' | b'P', tail @ ..] => rest = tail,
        _ => return None,
    }
    let mut exponent_negative = false;
    if let [sign @ (b'-' | b'+'), tail @ ..] = rest {
        exponent_negative = *sign == b'-';
        rest = tail;
    }
    let mut power = 0i32;
    let mut power_digits = 0;
    while let [c @ b'0'..=b'9', tail @ ..] = rest {
        if power > 100_000 {
            return None;
        }
        power = power * 10 + (*c - b'0') as i32;
        power_digits += 1;
        rest = tail;
    }
    if power_digits == 0 || !rest.is_empty() {
        return None;
    }
    if exponent_negative {
        power = -power;
    }
    Some(Hex {
        negative,
        mantissa,
        exponent: exponent + power,
    })
}

/// The bits of the binary floating-point number with `mantissa_bits` bits of
/// fraction and largest exponent `max_exponent` that `hex` names exactly, or
/// `None` if there is none.
const fn hex_bits(hex: Hex, mantissa_bits: i32, max_exponent: i32) -> Option<u64> {
    let sign_bit = mantissa_bits + exponent_bits(max_exponent);
    let sign = if hex.negative { 1u64 << sign_bit } else { 0 };
    if hex.mantissa == 0 {
        return Some(sign);
    }
    let top = 63 - hex.mantissa.leading_zeros() as i32;
    // The value is in [2^scale, 2^(scale+1)).
    let scale = hex.exponent + top;
    if scale > max_exponent {
        return None;
    }
    let min_exponent = 1 - max_exponent;
    // Where the lowest mantissa bit must land: bit 0 of the fraction for a
    // normal number, or 2^(min_exponent - mantissa_bits) for a subnormal one.
    let (shift, biased) = if scale >= min_exponent {
        (top - mantissa_bits, (scale + max_exponent) as u64)
    } else {
        (min_exponent - mantissa_bits - hex.exponent, 0)
    };
    let fraction = if shift > 0 {
        if shift >= 64 || hex.mantissa & ((1u64 << shift) - 1) != 0 {
            return None;
        }
        hex.mantissa >> shift
    } else {
        hex.mantissa << -shift
    };
    let fraction_mask = (1u64 << mantissa_bits) - 1;
    Some(sign | biased << mantissa_bits | (fraction & fraction_mask))
}

/// How many exponent bits a format with largest exponent `max_exponent` has.
const fn exponent_bits(max_exponent: i32) -> i32 {
    32 - (max_exponent as u32).leading_zeros() as i32 + 1
}

/// The `double` a hexadecimal constant names exactly, if there is one.
pub(crate) const fn hex_f64(hex: Hex) -> Option<f64> {
    match hex_bits(hex, 52, 1023) {
        Some(bits) => Some(f64::from_bits(bits)),
        None => None,
    }
}

/// The `float` a hexadecimal constant names exactly, if there is one.
pub(crate) const fn hex_f32(hex: Hex) -> Option<f32> {
    match hex_bits(hex, 23, 127) {
        Some(bits) => Some(f32::from_bits(bits as u32)),
        None => None,
    }
}

/// The `double` a C hexadecimal constant names. Use it through [`hexf64`],
/// which evaluates it at compile time.
#[allow(
    clippy::panic,
    reason = "evaluated at compile time, where a panic fails the build"
)]
pub(crate) const fn hex64(text: &str) -> f64 {
    match parse_hex(text) {
        Some(hex) => match hex_f64(hex) {
            Some(value) => value,
            None => panic!("not exactly a double"),
        },
        None => panic!("not a hexadecimal floating constant"),
    }
}

/// The `float` a C hexadecimal constant names. Use it through [`hexf32`],
/// which evaluates it at compile time.
#[allow(
    clippy::panic,
    reason = "evaluated at compile time, where a panic fails the build"
)]
pub(crate) const fn hex32(text: &str) -> f32 {
    match parse_hex(text) {
        Some(hex) => match hex_f32(hex) {
            Some(value) => value,
            None => panic!("not exactly a float"),
        },
        None => panic!("not a hexadecimal floating constant"),
    }
}

/// A `double` written as C writes it in hexadecimal, such as
/// `hexf64!("0x1.62e42fefa39efp-1")`, checked and evaluated at compile time.
/// The digits are musl's, so they can be compared with its source.
macro_rules! hexf64 {
    ($text:literal) => {{
        const VALUE: f64 = $crate::math::support::hex64($text);
        VALUE
    }};
}
pub(crate) use hexf64;

/// A `float` written as C writes it in hexadecimal; see [`hexf64`].
macro_rules! hexf32 {
    ($text:literal) => {{
        const VALUE: f32 = $crate::math::support::hex32($text);
        VALUE
    }};
}
#[allow(
    unused_imports,
    reason = "only the table checks use it until the `float` functions are here"
)]
pub(crate) use hexf32;

/// The high 32 bits of `x`: its sign, exponent and top 20 fraction bits.
#[inline]
pub(crate) const fn high_word(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

/// The low 32 bits of `x`.
#[inline]
pub(crate) const fn low_word(x: f64) -> u32 {
    x.to_bits() as u32
}

/// The `double` with these high and low words.
#[inline]
pub(crate) const fn from_words(high: u32, low: u32) -> f64 {
    f64::from_bits((high as u64) << 32 | low as u64)
}

/// `x` with its high word replaced.
#[inline]
pub(crate) const fn with_high_word(x: f64, high: u32) -> f64 {
    from_words(high, low_word(x))
}

/// `x` with its low word replaced.
#[inline]
pub(crate) const fn with_low_word(x: f64, low: u32) -> f64 {
    from_words(high_word(x), low)
}

/// Whether `x` is a NaN, tested on its bits. A comparison would raise invalid
/// for a signalling NaN.
#[inline]
pub(crate) const fn is_nan(x: f64) -> bool {
    x.to_bits() << 1 > 0x7ff << 53
}

/// [`is_nan`] for `float`.
#[inline]
pub(crate) const fn is_nanf(x: f32) -> bool {
    x.to_bits() << 1 > 0xff << 24
}

/// Whether `x` is neither infinite nor a NaN, tested on its bits.
#[inline]
pub(crate) const fn is_finite(x: f64) -> bool {
    x.to_bits() << 1 < 0x7ff << 53
}

/// [`is_finite`] for `float`.
#[inline]
pub(crate) const fn is_finitef(x: f32) -> bool {
    x.to_bits() << 1 < 0xff << 24
}

/// `x`, hidden from the optimiser, so an operation on it happens at run time
/// in the current rounding mode and raises its exceptions. musl's
/// `fp_barrier`.
#[inline(always)]
pub(crate) fn barrier(x: f64) -> f64 {
    black_box(x)
}

/// [`barrier`] for `float`.
#[inline(always)]
pub(crate) fn barrierf(x: f32) -> f32 {
    black_box(x)
}

/// Keeps an operation whose result is unused, because it is done for the
/// exceptions it raises. musl's `FORCE_EVAL`. A constant operand must also go
/// through [`barrier`], or the operation is folded before this sees it.
#[inline(always)]
pub(crate) fn force_eval(x: f64) {
    let _ = black_box(x);
}

/// [`force_eval`] for `float`.
#[inline(always)]
pub(crate) fn force_evalf(x: f32) {
    let _ = black_box(x);
}

/// A NaN, raising invalid unless `x` is already a NaN. musl's
/// `__math_invalid`.
#[inline(never)]
pub(crate) fn invalid(x: f64) -> f64 {
    let difference = barrier(x) - x;
    difference / barrier(difference)
}

/// [`invalid`] for `float`.
#[inline(never)]
pub(crate) fn invalidf(x: f32) -> f32 {
    let difference = barrierf(x) - x;
    difference / barrierf(difference)
}

/// An infinity with the sign `sign` gives (nonzero for negative), raising
/// divide-by-zero. musl's `__math_divzero`.
#[inline(never)]
pub(crate) fn divzero(sign: u32) -> f64 {
    barrier(if sign != 0 { -1.0 } else { 1.0 }) / 0.0
}

/// [`divzero`] for `float`.
#[inline(never)]
pub(crate) fn divzerof(sign: u32) -> f32 {
    barrierf(if sign != 0 { -1.0 } else { 1.0 }) / 0.0
}

/// `±y × y`, with the sign `sign` gives, computed at run time so it rounds in
/// the current mode and overflows or underflows as it should. musl's
/// `__math_xflow`.
#[inline(never)]
pub(crate) fn xflow(sign: u32, y: f64) -> f64 {
    barrier(if sign != 0 { -y } else { y }) * y
}

/// [`xflow`] for `float`.
#[inline(never)]
pub(crate) fn xflowf(sign: u32, y: f32) -> f32 {
    barrierf(if sign != 0 { -y } else { y }) * y
}

/// A result too large for a `double`: infinity or the largest finite value,
/// as the rounding mode says, raising overflow. musl's `__math_oflow`.
pub(crate) fn oflow(sign: u32) -> f64 {
    xflow(sign, hexf64!("0x1p769"))
}

/// A result too small for a `double`: zero or the smallest subnormal, as the
/// rounding mode says, raising underflow. musl's `__math_uflow`.
pub(crate) fn uflow(sign: u32) -> f64 {
    xflow(sign, hexf64!("0x1p-767"))
}

/// [`oflow`] for `float`.
pub(crate) fn oflowf(sign: u32) -> f32 {
    xflowf(sign, hexf32!("0x1p97"))
}

/// [`uflow`] for `float`.
pub(crate) fn uflowf(sign: u32) -> f32 {
    xflowf(sign, hexf32!("0x1p-95"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn double(text: &str) -> Option<u64> {
        parse_hex(text).and_then(hex_f64).map(f64::to_bits)
    }

    fn float(text: &str) -> Option<u32> {
        parse_hex(text).and_then(hex_f32).map(f32::to_bits)
    }

    #[test]
    fn hexadecimal_constants_parse_exactly() {
        assert_eq!(double("0x1p+0"), Some(1f64.to_bits()));
        assert_eq!(double("-0x0p+0"), Some(0x8000_0000_0000_0000));
        assert_eq!(double("0x1.62e42fefa39efp-1"), Some(0x3fe6_2e42_fefa_39ef));
        assert_eq!(
            double("0x1.fffffffffffffp+1023"),
            Some(0x7fef_ffff_ffff_ffff)
        );
        assert_eq!(double("0x1p-1022"), Some(0x0010_0000_0000_0000));
        assert_eq!(double("0x0.0000000000001p-1022"), Some(1));
        assert_eq!(double("-0x1p-1074"), Some(0x8000_0000_0000_0001));
        assert_eq!(double("0x1.8p-1073"), Some(3));
        assert_eq!(double("0x2p0"), Some(2f64.to_bits()));
        assert_eq!(double("0X1.0P-1"), Some(0.5f64.to_bits()));
        assert_eq!(double(".5"), None);
        assert_eq!(double("0x1p-1075"), None);
        assert_eq!(double("0x1p+1024"), None);
        assert_eq!(double("0x1.00000000000001p0"), None);
        assert_eq!(double("0x1p"), None);
        assert_eq!(double("0x1p1x"), None);
        assert_eq!(float("0x1.fffffep+127"), Some(0x7f7f_ffff));
        assert_eq!(float("0x1p-149"), Some(1));
        assert_eq!(float("-0x1.a7ebep-1"), Some(0xbf53_f5f0));
        assert_eq!(float("0x1.fffffe8p+127"), None);
        assert_eq!(hexf64!("0x1p769"), 2f64.powi(769));
        assert_eq!(hexf32!("0x1p-95"), 2f32.powi(-95));
    }
}
