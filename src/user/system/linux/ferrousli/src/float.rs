//! The format-generic core of converting decimal and hexadecimal numbers to
//! binary floating point, correctly rounded to nearest, ties to even.
//!
//! Nothing here uses the machine's floating-point types except the fast path,
//! so one algorithm serves `float`, `double`, x87's 80-bit `long double` and
//! IEEE binary128, the `long double` of AArch64. A [`Format`] names the
//! significand and exponent widths, and every result is its bit pattern in a
//! `u128`.
//!
//! # Decimal to binary
//!
//! A decimal input is an integer `D` of significant digits and a power of ten.
//! The value `D * 10^e` is written as a fraction `A / B` of big integers, both
//! scaled by a power of two so that the quotient has three or four bits more
//! than the significand. Long division gives that quotient and whether a
//! remainder is left, and [`round`] does the rest. The division is exact, so
//! every halfway case is seen as one.
//!
//! Only the first [`Format::max_digits`] significant digits are kept. Any
//! value exactly representable, or exactly halfway between two that are, has
//! no more significant digits than that, so a longer input lies strictly
//! between the same two such points as its truncation with a 1 appended, and
//! rounds the same way.

mod big;

use big::Big;

/// A binary floating-point format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Format {
    /// Bits in the significand, counting the leading one: 53 for `double`.
    pub(crate) mantissa_bits: u32,
    /// Bits in the biased exponent.
    pub(crate) exponent_bits: u32,
    /// Whether the leading one is stored, as x87's extended format does,
    /// rather than implied.
    pub(crate) explicit_one: bool,
}

/// IEEE binary32, `float`.
pub(crate) const BINARY32: Format = Format {
    mantissa_bits: 24,
    exponent_bits: 8,
    explicit_one: false,
};

/// IEEE binary64, `double`.
pub(crate) const BINARY64: Format = Format {
    mantissa_bits: 53,
    exponent_bits: 11,
    explicit_one: false,
};

/// The x87 80-bit extended format, `long double` on x86-64: a 64-bit
/// significand with its integer bit stored, and a 15-bit exponent.
pub(crate) const X87_EXTENDED: Format = Format {
    mantissa_bits: 64,
    exponent_bits: 15,
    explicit_one: true,
};

/// IEEE binary128, `long double` on AArch64. x86-64 uses it only to check the
/// big-number capacity, so that AArch64's `strtold` needs no change here.
pub(crate) const BINARY128: Format = Format {
    mantissa_bits: 113,
    exponent_bits: 15,
    explicit_one: false,
};

impl Format {
    /// The exponent bias.
    const fn bias(&self) -> i64 {
        (1 << (self.exponent_bits - 1)) - 1
    }

    /// The exponent of the smallest normal number's leading bit.
    const fn min_exponent(&self) -> i64 {
        1 - self.bias()
    }

    /// Bits below the exponent field.
    const fn fraction_field(&self) -> u32 {
        if self.explicit_one {
            self.mantissa_bits
        } else {
            self.mantissa_bits - 1
        }
    }

    /// The exponent of the smallest subnormal number, which is the lowest bit
    /// any value has.
    const fn min_lsb_exponent(&self) -> i64 {
        self.min_exponent() - (self.mantissa_bits as i64 - 1)
    }

    /// The most significant digits a value that is exactly representable, or
    /// exactly halfway between two that are, can have.
    ///
    /// Such a value is an odd integer times `2^-q`, with `q` at most
    /// `mantissa_bits - min_exponent`, so it has exactly `q` decimal places.
    /// Its leading digit is at least `floor(min_exponent * log10 2)` places
    /// below the point, which bounds the digits between. Two more make the
    /// bound safe from the rounding of `log10 2`.
    pub(crate) const fn max_digits(&self) -> u64 {
        let q = self.mantissa_bits as i64 - self.min_exponent();
        let below = -self.min_exponent() * 30103 / 100_000;
        (q - below + 2) as u64
    }

    /// A leading decimal exponent above this overflows whatever the digits.
    const fn overflow_decimal_exponent(&self) -> i64 {
        (self.bias() + 1) * 30103 / 100_000 + 1
    }

    /// A leading decimal exponent below this rounds to zero whatever the
    /// digits: the value is below half the smallest subnormal.
    const fn underflow_decimal_exponent(&self) -> i64 {
        (self.min_lsb_exponent() - 1) * 30103 / 100_000 - 2
    }

    /// The limbs [`decimal`] needs for the worst input, with one to spare.
    ///
    /// The larger number is `10^-e` shifted left by `mantissa_bits + 3`, and
    /// `-e` is at most the digits kept, one sticky digit, and the distance of
    /// the leading digit below the point.
    const fn limbs_needed(&self) -> u64 {
        let ten_power = self.max_digits() as i64 + 1 - self.underflow_decimal_exponent();
        // log2(10) < 3.3220.
        let bits = ten_power * 33220 / 10_000 + self.mantissa_bits as i64 + 4;
        (bits as u64).div_ceil(64) + 1
    }

    /// Zero with the given sign.
    pub(crate) const fn zero(&self, negative: bool) -> u128 {
        self.sign(negative)
    }

    /// Infinity with the given sign.
    pub(crate) const fn infinity(&self, negative: bool) -> u128 {
        let explicit = if self.explicit_one {
            1 << (self.mantissa_bits - 1)
        } else {
            0
        };
        self.sign(negative) | self.max_field() << self.fraction_field() | explicit
    }

    /// The default quiet NaN with the given sign.
    pub(crate) const fn nan(&self, negative: bool) -> u128 {
        // The quiet bit is the fraction's highest.
        let quiet = 1 << (self.mantissa_bits - 2);
        self.infinity(negative) | quiet
    }

    /// The sign bit, if `negative`.
    const fn sign(&self, negative: bool) -> u128 {
        (negative as u128) << (self.fraction_field() + self.exponent_bits)
    }

    /// The all-ones exponent field of infinities and NaNs.
    const fn max_field(&self) -> u128 {
        (1 << self.exponent_bits) - 1
    }
}

/// What kind of number a conversion produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// A signed zero.
    Zero,
    /// Below the smallest normal number.
    Subnormal,
    /// A finite normal number.
    Normal,
    /// An infinity: the value overflowed.
    Infinite,
}

/// A converted number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rounded {
    /// The bit pattern.
    pub(crate) bits: u128,
    /// What kind of number it is.
    pub(crate) class: Class,
    /// Whether it differs from the exact value.
    pub(crate) inexact: bool,
}

/// Rounds `significand * 2^exponent`, plus a nonzero amount smaller than
/// `2^exponent` when `sticky`, to the nearest number in `format`, ties to
/// even.
///
/// A `sticky` input must have at least `mantissa_bits + 2` significant bits,
/// so that the part below them only breaks ties. `significand` must not be
/// zero.
pub(crate) fn round(
    format: &Format,
    negative: bool,
    significand: u128,
    exponent: i64,
    sticky: bool,
) -> Rounded {
    let precision = i64::from(format.mantissa_bits);
    let length = i64::from(128 - significand.leading_zeros());
    let leading = length - 1 + exponent;
    // The exponent of the lowest bit kept: as many bits as the format holds,
    // or fewer in the subnormal range.
    let mut lowest = (leading - (precision - 1)).max(format.min_lsb_exponent());
    let shift = lowest - exponent;

    let (mut kept, half, below) = if shift <= 0 {
        // Exact, and within `precision` bits of the top.
        (significand << -shift, false, false)
    } else {
        let bits = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
        let kept = significand.checked_shr(bits(shift)).unwrap_or(0);
        let half = significand.checked_shr(bits(shift - 1)).unwrap_or(0) & 1 != 0;
        let mask = 1_u128
            .checked_shl(bits(shift - 1))
            .map_or(u128::MAX, |bit| bit - 1);
        (kept, half, significand & mask != 0)
    };
    let inexact = half || below || sticky;
    if half && (below || sticky || kept & 1 != 0) {
        kept += 1;
        if kept >> precision != 0 {
            kept >>= 1;
            lowest += 1;
        }
    }

    let sign = format.sign(negative);
    if kept == 0 {
        return Rounded {
            bits: sign,
            class: Class::Zero,
            inexact,
        };
    }
    let fraction = if format.explicit_one {
        kept
    } else {
        kept & ((1 << (precision - 1)) - 1)
    };
    if kept >> (precision - 1) == 0 {
        return Rounded {
            bits: sign | fraction,
            class: Class::Subnormal,
            inexact,
        };
    }
    let biased = lowest + (precision - 1) + format.bias();
    if biased >= format.max_field() as i64 {
        return Rounded {
            bits: format.infinity(negative),
            class: Class::Infinite,
            inexact: true,
        };
    }
    Rounded {
        // Between 1 and the maximum field, so it fits.
        bits: sign | (biased as u128) << format.fraction_field() | fraction,
        class: Class::Normal,
        inexact,
    }
}

/// Limbs for `float` and `double`.
const SMALL_LIMBS: usize = 64;
/// Limbs for 80-bit and 128-bit `long double`.
const LARGE_LIMBS: usize = 880;

const _: () = assert!(BINARY32.limbs_needed() <= SMALL_LIMBS as u64);
const _: () = assert!(BINARY64.limbs_needed() <= SMALL_LIMBS as u64);
const _: () = assert!(X87_EXTENDED.limbs_needed() <= LARGE_LIMBS as u64);
const _: () = assert!(BINARY128.limbs_needed() <= LARGE_LIMBS as u64);

/// Converts `D * 10^exponent`, where `D` is the integer whose `count`
/// significant decimal digits `digits` yields, most significant first.
///
/// `digits` must yield at least `count` values, each below ten, and the first
/// of them should not be zero. The result is correctly rounded. A value so
/// large or small that it certainly overflows or underflows is that infinity or
/// zero, marked inexact, without the digits being read.
pub(crate) fn decimal(
    format: &Format,
    negative: bool,
    mut digits: impl Iterator<Item = u8>,
    count: u64,
    exponent: i64,
) -> Rounded {
    let sign = format.sign(negative);
    if count == 0 {
        return Rounded {
            bits: sign,
            class: Class::Zero,
            inexact: false,
        };
    }
    let leading = exponent.saturating_add_unsigned(count - 1);
    if leading > format.overflow_decimal_exponent() {
        return Rounded {
            bits: format.infinity(negative),
            class: Class::Infinite,
            inexact: true,
        };
    }
    if leading < format.underflow_decimal_exponent() {
        return Rounded {
            bits: sign,
            class: Class::Zero,
            inexact: true,
        };
    }

    // Up to 19 digits fit a `u64`: 10^19 - 1 < 2^64.
    let head_count = count.min(19);
    let mut head = 0_u64;
    for _ in 0..head_count {
        head = head * 10 + u64::from(digits.next().unwrap_or(0));
    }
    if let Some(rounded) = fast_path(format, negative, head, count, exponent) {
        return rounded;
    }
    let result = if format.limbs_needed() <= SMALL_LIMBS as u64 {
        exact::<SMALL_LIMBS>(format, negative, head, digits, count, exponent)
    } else {
        exact::<LARGE_LIMBS>(format, negative, head, digits, count, exponent)
    };
    match result {
        Ok(rounded) => rounded,
        // The capacities are asserted against the worst input above, so this
        // is a bug, and stopping at it is better than a wrong number.
        Err(big::Full) => crate::syscall::trap(),
    }
}

/// Exactly representable powers of ten, for the fast path.
const POWERS_F64: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];
/// Exactly representable powers of ten in a `float`.
const POWERS_F32: [f32; 11] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10];

/// Converts small inputs with the machine's own arithmetic, or declines.
///
/// When `D` and `10^|e|` are both exactly representable, one correctly
/// rounded multiplication or division gives the correctly rounded result
/// (Clinger's fast path). That holds for `double` with 15 digits and
/// `|e| <= 22`, and for `float` with 7 digits and `|e| <= 10`. The result is
/// always normal. Its `inexact` is not computed, since only a zero or
/// subnormal result's is ever used.
fn fast_path(
    format: &Format,
    negative: bool,
    head: u64,
    count: u64,
    exponent: i64,
) -> Option<Rounded> {
    let power = usize::try_from(exponent.unsigned_abs()).ok()?;
    let bits = if *format == BINARY64 && count <= 15 {
        let scale = *POWERS_F64.get(power)?;
        // Below 10^15 < 2^53, so exact.
        let digits = head as f64;
        let value = if exponent < 0 {
            digits / scale
        } else {
            digits * scale
        };
        u128::from(if negative { -value } else { value }.to_bits())
    } else if *format == BINARY32 && count <= 7 {
        let scale = *POWERS_F32.get(power)?;
        // Below 10^7 < 2^24, so exact.
        let digits = head as f32;
        let value = if exponent < 0 {
            digits / scale
        } else {
            digits * scale
        };
        u128::from(if negative { -value } else { value }.to_bits())
    } else {
        return None;
    };
    Some(Rounded {
        bits,
        class: Class::Normal,
        inexact: false,
    })
}

/// The exact conversion: `head` holds the first `min(count, 19)` digits, and
/// `rest` yields the others.
pub(crate) fn exact<const L: usize>(
    format: &Format,
    negative: bool,
    head: u64,
    mut rest: impl Iterator<Item = u8>,
    count: u64,
    exponent: i64,
) -> Result<Rounded, big::Full> {
    let kept = count.min(format.max_digits());
    let mut a = Big::<L>::from_u64(head);
    let mut remaining = kept.saturating_sub(19);
    while remaining > 0 {
        let chunk_digits = remaining.min(19);
        let mut chunk = 0_u64;
        for _ in 0..chunk_digits {
            chunk = chunk * 10 + u64::from(rest.next().unwrap_or(0));
        }
        // At most 19, so the power fits.
        a.mul_add(10_u64.pow(chunk_digits as u32), chunk)?;
        remaining -= chunk_digits;
    }
    let mut exponent = exponent.saturating_add_unsigned(count - kept);
    if count > kept {
        // The dropped digits are not all zero, since the last digit of the
        // input is not: a 1 below the kept ones stands for them.
        a.mul_add(10, 1)?;
        exponent -= 1;
    }

    let mut b = Big::<L>::from_u64(1);
    if exponent >= 0 {
        a.mul_pow10(exponent.unsigned_abs())?;
    } else {
        b.mul_pow10(exponent.unsigned_abs())?;
    }
    // Scale so that `a / b` is in `[2^(p+2), 2^(p+4))`: the bit lengths give
    // `2^(la-lb-1) < a / b < 2^(la-lb+1)`.
    let precision = u64::from(format.mantissa_bits);
    let lengths = i128::from(a.bit_len()) - i128::from(b.bit_len());
    let scale = i128::from(precision + 3) - lengths;
    // The bit lengths are bounded by the capacity, far inside `i64`.
    let scale = scale as i64;
    if scale > 0 {
        a.shl(scale.unsigned_abs())?;
    } else {
        b.shl(scale.unsigned_abs())?;
    }

    // Long division, one quotient bit at a time from bit p + 3 down.
    b.shl(precision + 3)?;
    let mut quotient = 0_u128;
    let mut bit = precision + 4;
    while bit > 0 {
        bit -= 1;
        if a.compare(&b).is_ge() {
            a.sub(&b);
            quotient |= 1 << bit;
        }
        if bit > 0 {
            b.shr1();
        }
    }
    Ok(round(format, negative, quotient, -scale, !a.is_zero()))
}
