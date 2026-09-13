//! Floating point to text, exactly: `%e`, `%f`, `%g` and `%a`.
//!
//! # Decimal
//!
//! A finite binary number is `m * 2^e` with an integer `m`. For `e >= 0` that
//! is the integer `m * 2^e`; for `e < 0` it is `m * 5^-e / 10^-e`, the integer
//! `m * 5^-e` with the decimal point `-e` places from its end. Either way the
//! value's exact decimal digits are those of one big integer, which
//! [`Decimal`] computes in base 10^9 limbs by multiplying `m` by small powers
//! of 2 or 5. Every digit is then exact: rounding to a precision looks at the
//! first dropped digit and whether anything nonzero follows it, and ties go to
//! the even digit, as glibc and musl do in the default rounding mode.
//!
//! The largest integer is an x87 `long double` near 2^16384, about 4933
//! digits, and the longest fraction the smallest subnormal `long double`,
//! `m * 5^16445` for a 64-bit `m`, 11514 digits. Both fit [`LIMBS`] limbs, a
//! little over 5 KiB on the stack. Digits beyond a value's own are zeros, so
//! `%f` of a huge value and precisions far above 1000 cost only the output.
//!
//! This is the same exact method as musl's `fmt_fp` (MIT) in
//! `src/stdio/vfprintf.c`, organised differently: musl scales the value into
//! limbs with floating-point steps and stops expanding past the precision,
//! where this keeps integer arithmetic throughout.
//!
//! # Hexadecimal
//!
//! `%a` follows glibc, whose output programs compare against. A `double` is
//! `0x1.hhhp+e`, a subnormal `0x0.hhhp-1022`, and precision rounding carries
//! into the leading digit without renormalising, so `%.0a` of 1.5 is `0x2p+0`.
//! An x87 `long double` is printed with its top four significand bits as the
//! leading digit, `0x8p-3` for 1, and a carry out of that digit renormalises.
//!
//! Infinities are `inf` and NaNs `nan`, in capitals for the capital
//! conversions, with the sign of the value: `-nan` is printed for a NaN whose
//! sign bit is set, as glibc does. x87 encodings the processor treats as
//! invalid, unnormals and pseudo-infinities, are NaNs.

use core::ffi::c_int;

use super::printf::{ALT, LEFT, PLUS, SPACE, Sink, Spec, ZERO, begin, end};
use crate::va::LongDouble;

/// A floating-point argument.
#[derive(Debug, Clone, Copy)]
pub enum Float {
    /// A `double`.
    Double(f64),
    /// An x87 `long double`.
    Long(LongDouble),
}

/// What a floating-point value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Not a number.
    Nan,
    /// An infinity.
    Infinite,
    /// `mantissa * 2^exponent`.
    Finite {
        /// The significand as an integer.
        mantissa: u64,
        /// The power of two.
        exponent: i32,
    },
}

/// The sign and kind of `value`.
fn decode(value: Float) -> (bool, Kind) {
    match value {
        Float::Double(value) => {
            let bits = value.to_bits();
            let biased = ((bits >> 52) & 0x7ff) as i32;
            let fraction = bits & ((1 << 52) - 1);
            let kind = match biased {
                0x7ff if fraction == 0 => Kind::Infinite,
                0x7ff => Kind::Nan,
                0 => Kind::Finite {
                    mantissa: fraction,
                    exponent: -1074,
                },
                _ => Kind::Finite {
                    mantissa: fraction | (1 << 52),
                    exponent: biased - 1075,
                },
            };
            (bits >> 63 != 0, kind)
        }
        Float::Long(value) => {
            let biased = i32::from(value.sign_exponent & 0x7fff);
            let mantissa = value.mantissa;
            let kind = if biased == 0x7fff {
                if mantissa == 1 << 63 {
                    Kind::Infinite
                } else {
                    Kind::Nan
                }
            } else if biased == 0 {
                Kind::Finite {
                    mantissa,
                    exponent: -16445,
                }
            } else if mantissa >> 63 == 0 {
                Kind::Nan
            } else {
                Kind::Finite {
                    mantissa,
                    exponent: biased - 16383 - 63,
                }
            };
            (value.sign_exponent >> 15 != 0, kind)
        }
    }
}

/// A limb's base.
const BASE: u32 = 1_000_000_000;
/// Limbs in a [`Decimal`]: room for 11529 digits.
const LIMBS: usize = 1281;
/// The powers of ten that fit a limb.
const POW10: [u32; 10] = [
    1,
    10,
    100,
    1000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
];

/// `10^n` for `n` below 10.
fn pow10(n: usize) -> u32 {
    POW10.get(n).copied().unwrap_or(1)
}

/// A value's exact decimal digits: a big integer in base 10^9, with the
/// decimal point `point` digits from its end.
#[derive(Debug)]
struct Decimal {
    /// Least significant first. Limbs from `len` on are zero.
    limbs: [u32; LIMBS],
    /// Limbs in use; the top one is not zero. Zero for the value zero.
    len: usize,
    /// How many of the digits are after the decimal point.
    point: usize,
}

impl Decimal {
    /// The digits of `mantissa * 2^exponent`.
    fn new(mantissa: u64, exponent: i32) -> Self {
        let mut decimal = Self {
            limbs: [0; LIMBS],
            len: 0,
            point: 0,
        };
        let mut m = mantissa;
        while m != 0 {
            if let Some(limb) = decimal.limbs.get_mut(decimal.len) {
                *limb = (m % u64::from(BASE)) as u32;
            }
            decimal.len += 1;
            m /= u64::from(BASE);
        }
        if mantissa == 0 {
            return decimal;
        }
        if exponent >= 0 {
            let mut left = exponent.unsigned_abs();
            while left != 0 {
                let step = left.min(29);
                decimal.multiply(1 << step);
                left -= step;
            }
        } else {
            let mut left = exponent.unsigned_abs();
            decimal.point = left as usize;
            while left != 0 {
                let step = left.min(13);
                decimal.multiply(5_u32.pow(step));
                left -= step;
            }
        }
        decimal
    }

    /// Multiplies by `factor`, below 2^32.
    fn multiply(&mut self, factor: u32) {
        let mut carry = 0_u64;
        for limb in self.limbs.iter_mut().take(self.len) {
            let product = u64::from(*limb) * u64::from(factor) + carry;
            *limb = (product % u64::from(BASE)) as u32;
            carry = product / u64::from(BASE);
        }
        while carry != 0 {
            let Some(limb) = self.limbs.get_mut(self.len) else {
                return;
            };
            *limb = (carry % u64::from(BASE)) as u32;
            self.len += 1;
            carry /= u64::from(BASE);
        }
    }

    /// How many digits the integer has.
    fn count(&self) -> usize {
        let Some(top) = self.len.checked_sub(1) else {
            return 0;
        };
        let high = self.limbs.get(top).copied().unwrap_or(0);
        let mut digits = 1;
        while digits < 9 && high >= pow10(digits) {
            digits += 1;
        }
        9 * top + digits
    }

    /// The digit at `position`, counted from the end from 0. Zero past the
    /// number's digits.
    fn digit(&self, position: usize) -> u8 {
        let Some(limb) = self.limbs.get(position / 9) else {
            return 0;
        };
        ((limb / pow10(position % 9)) % 10) as u8
    }

    /// The power of ten of the leading digit. Zero for zero.
    fn exponent10(&self) -> i64 {
        let count = self.count();
        if count == 0 {
            return 0;
        }
        count as i64 - 1 - self.point as i64
    }

    /// The position of the lowest nonzero digit.
    fn lowest_nonzero(&self) -> Option<usize> {
        let index = self
            .limbs
            .iter()
            .take(self.len)
            .position(|limb| *limb != 0)?;
        let limb = self.limbs.get(index).copied().unwrap_or(1);
        let mut within = 0;
        while (limb / pow10(within)).is_multiple_of(10) {
            within += 1;
        }
        Some(index * 9 + within)
    }

    /// Whether any digit below `position` is nonzero.
    fn nonzero_below(&self, position: usize) -> bool {
        let index = position / 9;
        let whole = self
            .limbs
            .iter()
            .take(index.min(self.len))
            .any(|limb| *limb != 0);
        let part = self
            .limbs
            .get(index)
            .is_some_and(|limb| limb % pow10(position % 9) != 0);
        whole || part
    }

    /// Drops the last `drop` digits, rounding the rest to nearest with ties
    /// to even. The dropped digits become zeros.
    fn round(&mut self, drop: usize) {
        if drop == 0 || self.len == 0 {
            return;
        }
        let first = self.digit(drop - 1);
        let sticky = self.nonzero_below(drop - 1);
        let odd = self.digit(drop) % 2 == 1;
        let index = drop / 9;
        for limb in self.limbs.iter_mut().take(index) {
            *limb = 0;
        }
        if let Some(limb) = self.limbs.get_mut(index) {
            *limb -= *limb % pow10(drop % 9);
        }
        if first > 5 || (first == 5 && (sticky || odd)) {
            let mut at = index;
            let mut carry = pow10(drop % 9);
            while carry != 0 {
                let Some(limb) = self.limbs.get_mut(at) else {
                    break;
                };
                *limb += carry;
                carry = 0;
                if *limb >= BASE {
                    *limb -= BASE;
                    carry = 1;
                }
                at += 1;
                self.len = self.len.max(at);
            }
        }
        while self.len != 0 && self.limbs.get(self.len - 1) == Some(&0) {
            self.len -= 1;
        }
    }
}

/// Writes the `n` digits from `top` downward.
fn write_digits(sink: &mut dyn Sink, decimal: &Decimal, top: usize, n: usize) {
    let mut chunk = [0_u8; 64];
    let mut done = 0;
    while done < n {
        let take = (n - done).min(chunk.len());
        for (i, slot) in chunk.iter_mut().take(take).enumerate() {
            *slot = b'0' + decimal.digit(top.wrapping_sub(done + i));
        }
        sink.write(chunk.get(..take).unwrap_or_default());
        done += take;
    }
}

/// The decimal digits of `value`, at least `min` of them, at the end of
/// `buf`.
fn exponent_digits(value: u64, min: usize, buf: &mut [u8; 20]) -> &[u8] {
    let mut start = buf.len();
    let mut v = value;
    while v != 0 || buf.len() - start < min {
        start -= 1;
        if let Some(slot) = buf.get_mut(start) {
            *slot = b'0' + (v % 10) as u8;
        }
        v /= 10;
    }
    buf.get(start..).unwrap_or_default()
}

/// Formats a floating-point value for the conversion `conversion`.
pub fn format(
    sink: &mut dyn Sink,
    spec: &Spec,
    conversion: u8,
    value: Float,
    count: usize,
) -> Result<usize, c_int> {
    let (negative, kind) = decode(value);
    let upper = conversion.is_ascii_uppercase();
    let sign: &[u8] = if negative {
        b"-"
    } else if spec.flags & PLUS != 0 {
        b"+"
    } else if spec.flags & SPACE != 0 {
        b" "
    } else {
        b""
    };
    match kind {
        Kind::Nan | Kind::Infinite => {
            let word: &[u8] = match (kind == Kind::Nan, upper) {
                (true, false) => b"nan",
                (true, true) => b"NAN",
                (false, false) => b"inf",
                (false, true) => b"INF",
            };
            let body = sign.len() + word.len();
            let total = begin(sink, spec, body, count)?;
            sink.write(sign);
            sink.write(word);
            end(sink, spec, body, total);
            Ok(total)
        }
        Kind::Finite { mantissa, exponent } => {
            if conversion | 0x20 == b'a' {
                hexadecimal(sink, spec, upper, sign, value, count)
            } else {
                decimal(sink, spec, conversion, sign, mantissa, exponent, count)
            }
        }
    }
}

/// The zeros that pad a numeric field with the `0` flag, and the body with
/// them.
fn zero_padding(spec: &Spec, body: usize) -> (usize, usize) {
    if spec.flags & ZERO != 0 && spec.flags & LEFT == 0 && spec.width > body {
        (spec.width - body, spec.width)
    } else {
        (0, body)
    }
}

/// `%e`, `%f` and `%g`.
fn decimal(
    sink: &mut dyn Sink,
    spec: &Spec,
    conversion: u8,
    sign: &[u8],
    mantissa: u64,
    exponent: i32,
    count: usize,
) -> Result<usize, c_int> {
    let alt = spec.flags & ALT != 0;
    let upper = conversion.is_ascii_uppercase();
    let mut digits = Decimal::new(mantissa, exponent);
    let precision = spec.precision.unwrap_or(6);
    let (scientific, precision) = match conversion | 0x20 {
        b'e' => {
            digits.round(digits.count().saturating_sub(precision.saturating_add(1)));
            (true, precision)
        }
        b'f' => {
            digits.round(digits.point.saturating_sub(precision));
            (false, precision)
        }
        _ => {
            let p = precision.max(1);
            digits.round(digits.count().saturating_sub(p));
            let x = digits.exponent10();
            let (scientific, mut shown) = if (p as i64) > x && x >= -4 {
                (false, (p as i64 - 1 - x) as usize)
            } else {
                (true, p - 1)
            };
            if !alt {
                let needed = match digits.lowest_nonzero() {
                    None => 0,
                    Some(low) if scientific => digits.count() - 1 - low,
                    Some(low) => digits.point.saturating_sub(low),
                };
                shown = shown.min(needed);
            }
            (scientific, shown)
        }
    };
    let dot = precision != 0 || alt;
    let n = digits.count();

    if scientific {
        let x = digits.exponent10();
        let mut buf = [0_u8; 20];
        let exp = exponent_digits(x.unsigned_abs(), 2, &mut buf);
        let body =
            sign.len() + 1 + usize::from(dot) + precision.min(usize::MAX / 2) + 2 + exp.len();
        let (zeros, body) = zero_padding(spec, body);
        let total = begin(sink, spec, body, count)?;
        sink.write(sign);
        sink.pad(b'0', zeros);
        let lead = if n == 0 { 0 } else { digits.digit(n - 1) };
        sink.write(&[b'0' + lead]);
        if dot {
            sink.write(b".");
        }
        let available = precision.min(n.saturating_sub(1));
        if available != 0 {
            write_digits(sink, &digits, n - 2, available);
        }
        sink.pad(b'0', precision - available);
        let marker = if upper { b'E' } else { b'e' };
        let exp_sign = if x < 0 { b'-' } else { b'+' };
        sink.write(&[marker, exp_sign]);
        sink.write(exp);
        end(sink, spec, body, total);
        return Ok(total);
    }

    let point = digits.point;
    let int_len = if n > point { n - point } else { 1 };
    let body = sign.len() + int_len + usize::from(dot) + precision.min(usize::MAX / 2);
    let (zeros, body) = zero_padding(spec, body);
    let total = begin(sink, spec, body, count)?;
    sink.write(sign);
    sink.pad(b'0', zeros);
    if n > point {
        write_digits(sink, &digits, n - 1, n - point);
    } else {
        sink.write(b"0");
    }
    if dot {
        sink.write(b".");
    }
    let available = precision.min(point);
    if available != 0 {
        write_digits(sink, &digits, point - 1, available);
    }
    sink.pad(b'0', precision - available);
    end(sink, spec, body, total);
    Ok(total)
}

/// `%a`.
fn hexadecimal(
    sink: &mut dyn Sink,
    spec: &Spec,
    upper: bool,
    sign: &[u8],
    value: Float,
    count: usize,
) -> Result<usize, c_int> {
    // The leading digit, the fraction's nibbles and how many there are, the
    // power of two, and whether a carry renormalises.
    let (mut lead, mut fraction, mut nibbles, mut exp, long) = match value {
        Float::Double(value) => {
            let bits = value.to_bits();
            let biased = ((bits >> 52) & 0x7ff) as i32;
            let fraction = bits & ((1 << 52) - 1);
            if biased == 0 {
                let exp = if fraction == 0 { 0 } else { -1022 };
                (0_u64, fraction, 13_usize, exp, false)
            } else {
                (1, fraction, 13, biased - 1023, false)
            }
        }
        Float::Long(value) => {
            let biased = i32::from(value.sign_exponent & 0x7fff);
            let m = value.mantissa;
            if biased == 0 && m == 0 {
                (0, 0, 15, 0, true)
            } else {
                (
                    m >> 60,
                    m & ((1 << 60) - 1),
                    15,
                    biased.max(1) - 16383 - 3,
                    true,
                )
            }
        }
    };
    match spec.precision {
        Some(p) if p < nibbles => {
            let shift = 4 * (nibbles - p) as u32;
            let rest = fraction & ((1_u64 << shift) - 1);
            fraction >>= shift;
            let half = 1_u64 << (shift - 1);
            let odd = if p == 0 { lead & 1 } else { fraction & 1 } == 1;
            if rest > half || (rest == half && odd) {
                fraction += 1;
                if fraction >> (4 * p) != 0 {
                    fraction = 0;
                    lead += 1;
                }
            }
            nibbles = p;
            if long && lead >= 16 {
                lead = 1;
                exp += 4;
            }
        }
        Some(_) => {}
        None => {
            while nibbles != 0 && fraction & 15 == 0 {
                fraction >>= 4;
                nibbles -= 1;
            }
        }
    }
    let extra = spec.precision.map_or(0, |p| p.saturating_sub(nibbles));
    let dot = nibbles + extra != 0 || spec.flags & ALT != 0;
    let mut buf = [0_u8; 20];
    let exp_text = exponent_digits(u64::from(exp.unsigned_abs()), 1, &mut buf);
    let body = sign.len()
        + 3
        + usize::from(dot)
        + nibbles
        + extra.min(usize::MAX / 2)
        + 2
        + exp_text.len();
    let (zeros, body) = zero_padding(spec, body);
    let total = begin(sink, spec, body, count)?;
    let hex: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let nibble = |n: u64| hex.get((n & 15) as usize).copied().unwrap_or(b'0');
    sink.write(sign);
    sink.write(if upper { b"0X" } else { b"0x" });
    sink.pad(b'0', zeros);
    sink.write(&[nibble(lead)]);
    if dot {
        sink.write(b".");
    }
    let mut text = [0_u8; 16];
    for (i, slot) in text.iter_mut().take(nibbles).enumerate() {
        *slot = nibble(fraction >> (4 * (nibbles - 1 - i)));
    }
    sink.write(text.get(..nibbles).unwrap_or_default());
    sink.pad(b'0', extra);
    let marker = if upper { b'P' } else { b'p' };
    let exp_sign = if exp < 0 { b'-' } else { b'+' };
    sink.write(&[marker, exp_sign]);
    sink.write(exp_text);
    end(sink, spec, body, total);
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_fraction_and_integer_fit_the_limbs() {
        let tiny = Decimal::new(u64::MAX, -16445);
        assert!(tiny.count() >= 11514 && tiny.len < LIMBS);
        let huge = Decimal::new(u64::MAX, 16320);
        assert_eq!(huge.count(), 4933);
    }

    #[test]
    fn rounding_goes_to_even_on_exact_ties_only() {
        // 2.5, 3.5 and 2.5000001 as integers with one digit after the point.
        let mut two_and_a_half = Decimal::new(5, -1);
        assert_eq!(two_and_a_half.count(), 2);
        two_and_a_half.round(1);
        assert_eq!(two_and_a_half.digit(1), 2);
        let mut three_and_a_half = Decimal::new(7, -1);
        three_and_a_half.round(1);
        assert_eq!(three_and_a_half.digit(1), 4);
        let mut nines = Decimal::new(999_999_999_999, 0);
        nines.round(1);
        assert_eq!(nines.count(), 13);
        assert_eq!(nines.digit(12), 1);
    }
}
