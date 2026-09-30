//! `logf128` on x86-64: the natural logarithm of a `_Float128`, which
//! LLVM's `libLLVM.so` imports to fold constants, and so Mesa's software
//! renderer and everything that opens it.
//!
//! # The method
//!
//! A positive finite `x` is `2^k × f` with `f` in `[√½, √2)`, and
//! `log x = k·ln 2 + 2·atanh(s)` where `s = (f − 1)/(f + 1)`, so `|s| <
//! 0.172`. `atanh(s)/s` is the series `Σ s^(2n)/(2n + 1)`, 26 terms of which
//! reach below `2^-128` of the sum.
//!
//! Every step is done in [`Wide`], a float with a 128-bit significand: fifteen
//! bits more than binary128's 113. Each operation truncates, so the value is
//! within a few units of `2^-125` of `log x`, relatively, before it is rounded
//! once to binary128 in the rounding mode MXCSR holds. That is below half a
//! unit in the last place but where `log x` falls within those few units of a
//! halfway point, as glibc's own `logf128` is not correctly rounded either.
//!
//! `f − 1` is exact, and for `k = 0` nothing else is subtracted, so a value
//! near 1 keeps its relative precision: `log(1 + 2^-112)` is `2^-112` less a
//! half of its square, not a difference of two numbers near `ln 2`.
//!
//! # Calling convention
//!
//! The SysV ABI passes and returns a `_Float128` in `xmm0`, which no Rust type
//! reaches on stable, so the C name is a shim that stores `xmm0` on the stack,
//! calls [`log_bits`] on those sixteen bytes, and loads the result back, as
//! [`crate::float128`]'s conversions do.

use crate::fenv::{
    FE_DIVBYZERO, FE_DOWNWARD, FE_INEXACT, FE_INVALID, FE_TOWARDZERO, FE_UPWARD, fegetround,
    feraiseexcept,
};

/// The sign bit.
const SIGN: u128 = 1 << 127;
/// The fraction's bits.
const FRACTION: u128 = (1 << 112) - 1;
/// The exponent field of an infinity and of a NaN.
const SPECIAL: u128 = 0x7fff;
/// The exponent bias.
const BIAS: i32 = 16383;
/// The fraction bit that makes a NaN quiet.
const QUIET: u128 = 1 << 111;
/// The NaN an invalid operation returns: quiet, with the sign set, as the
/// x86 default NaN is.
const DEFAULT_NAN: u128 = SIGN | SPECIAL << 112 | QUIET;

/// `√2 × 2^112`, truncated: the significand above which `f` is halved.
const SQRT2: u128 = 0x1_6a09_e667_f3bc_c908_b2fb_1366_ea95;

/// `ln 2 × 2^128`, truncated, from Python's `decimal` at 80 digits.
const LN2: Wide = Wide {
    negative: false,
    exp: -128,
    sig: 0xb172_17f7_d1cf_79ab_c9e3_b398_03f2_f6af,
};

/// Terms of the series: `s^52/53` is below `2^-132` for `|s| < 0.172`.
const TERMS: u32 = 26;

/// `sig × 2^exp`, negated if `negative`: nonzero with `sig`'s top bit set, or
/// zero with `sig` zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wide {
    negative: bool,
    exp: i32,
    sig: u128,
}

impl Wide {
    const ZERO: Self = Self {
        negative: false,
        exp: 0,
        sig: 0,
    };

    /// `sig × 2^exp`, normalised.
    fn new(negative: bool, exp: i32, sig: u128) -> Self {
        if sig == 0 {
            return Self::ZERO;
        }
        let shift = sig.leading_zeros() as i32;
        Self {
            negative,
            exp: exp - shift,
            sig: sig << shift,
        }
    }

    /// An integer, exactly.
    fn from_i32(n: i32) -> Self {
        Self::new(n < 0, 0, u128::from(n.unsigned_abs()))
    }

    /// `self × other`, truncated to 128 bits.
    fn mul(self, other: Self) -> Self {
        if self.sig == 0 || other.sig == 0 {
            return Self::ZERO;
        }
        let high = mul_high(self.sig, other.sig);
        Self::new(
            self.negative != other.negative,
            self.exp + other.exp + 128,
            high,
        )
    }

    /// `self + other`, truncated to 128 bits.
    fn add(self, other: Self) -> Self {
        if self.sig == 0 {
            return other;
        }
        if other.sig == 0 {
            return self;
        }
        let (big, small) = if (self.exp, self.sig) >= (other.exp, other.sig) {
            (self, other)
        } else {
            (other, self)
        };
        // One bit of headroom for the carry; the shift drops a bit of each.
        let a = big.sig >> 1;
        let distance = big.exp - small.exp;
        let b = if distance >= 127 {
            0
        } else {
            (small.sig >> 1) >> distance
        };
        if big.negative == small.negative {
            Self::new(big.negative, big.exp + 1, a + b)
        } else {
            Self::new(big.negative, big.exp + 1, a - b)
        }
    }

    /// `self − other`.
    fn sub(self, other: Self) -> Self {
        self.add(Self {
            negative: !other.negative,
            ..other
        })
    }

    /// `self / other`, `other` nonzero, truncated to 128 bits: restoring
    /// division, a quotient bit a step.
    fn div(self, other: Self) -> Self {
        if self.sig == 0 {
            return Self::ZERO;
        }
        let divisor = other.sig;
        let mut rest = self.sig;
        let mut quotient: u128 = 0;
        if rest >= divisor {
            quotient = 1 << 127;
            rest -= divisor;
        }
        for bit in (0..127).rev() {
            let carry = rest >> 127 != 0;
            rest <<= 1;
            if carry || rest >= divisor {
                rest = rest.wrapping_sub(divisor);
                quotient |= 1 << bit;
            }
        }
        // The quotient weighs 2^-127 of `self.sig / other.sig`.
        Self::new(
            self.negative != other.negative,
            self.exp - other.exp - 127,
            quotient,
        )
    }

    /// `1 / n`, for a small odd `n`.
    fn reciprocal(n: u32) -> Self {
        Self::new(false, 0, 1).div(Self::from_i32(n as i32))
    }
}

/// The high 128 bits of `a × b`.
const fn mul_high(a: u128, b: u128) -> u128 {
    let (a1, a0) = (a >> 64, a & u64::MAX as u128);
    let (b1, b0) = (b >> 64, b & u64::MAX as u128);
    let low = a0 * b0;
    let cross1 = a1 * b0;
    let cross0 = a0 * b1;
    // The middle column, with the low product's carry.
    let middle = (low >> 64) + (cross1 & u64::MAX as u128) + (cross0 & u64::MAX as u128);
    a1 * b1 + (cross1 >> 64) + (cross0 >> 64) + (middle >> 64)
}

/// A rounding direction, from MXCSR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Round {
    Nearest,
    Up,
    Down,
    Zero,
}

/// The rounding mode in force.
fn mode() -> Round {
    match fegetround() {
        FE_UPWARD => Round::Up,
        FE_DOWNWARD => Round::Down,
        FE_TOWARDZERO => Round::Zero,
        _ => Round::Nearest,
    }
}

/// `value`, a nonzero logarithm, rounded to binary128 and inexact: every
/// logarithm but `log 1` is irrational, and none is too large or too small
/// for a normal binary128.
fn round(value: Wide) -> u128 {
    // 113 bits kept, 15 below them, and the truncations' error below those.
    let kept = value.sig >> 15;
    let rest = value.sig & 0x7fff;
    let up = match mode() {
        // A remainder of exactly one half here is a truncated value above
        // it, as nothing computed is exact.
        Round::Nearest => rest >= 0x4000,
        Round::Up => !value.negative,
        Round::Down => value.negative,
        Round::Zero => false,
    };
    let mut kept = kept + u128::from(up);
    let mut exp = value.exp + 15;
    if kept >> 113 != 0 {
        kept >>= 1;
        exp += 1;
    }
    let field = (exp + 112 + BIAS) as u128;
    let _ = feraiseexcept(FE_INEXACT);
    let sign = if value.negative { SIGN } else { 0 };
    sign | field << 112 | (kept & FRACTION)
}

/// The natural logarithm of the binary128 whose bits are `x`.
fn log(x: u128) -> u128 {
    let field = (x >> 112) & SPECIAL;
    let fraction = x & FRACTION;
    let negative = x & SIGN != 0;
    if field == SPECIAL {
        if fraction != 0 {
            // A NaN, quieted; a signalling one raises invalid.
            if x & QUIET == 0 {
                let _ = feraiseexcept(FE_INVALID);
            }
            return x | QUIET;
        }
        if negative {
            let _ = feraiseexcept(FE_INVALID);
            return DEFAULT_NAN;
        }
        return x;
    }
    if field == 0 && fraction == 0 {
        let _ = feraiseexcept(FE_DIVBYZERO);
        return SIGN | SPECIAL << 112;
    }
    if negative {
        let _ = feraiseexcept(FE_INVALID);
        return DEFAULT_NAN;
    }
    // x = m × 2^e, m's leading one at bit 112; subnormals normalised.
    let (m, e) = if field == 0 {
        let shift = fraction.leading_zeros() as i32 - 15;
        (fraction << shift, 1 - BIAS - 112 - shift)
    } else {
        (fraction | 1 << 112, field as i32 - BIAS - 112)
    };
    if m == 1 << 112 && e == -112 {
        return 0;
    }
    // x = 2^k × f, with f = m × 2^(e - k) in [√½, √2).
    let k = if m > SQRT2 { e + 113 } else { e + 112 };
    let f = Wide::new(false, e - k, m);
    let one = Wide::new(false, 0, 1);
    let s = f.sub(one).div(f.add(one));
    let s2 = s.mul(s);
    let mut series = Wide::reciprocal(2 * TERMS + 1);
    for n in (0..TERMS).rev() {
        series = series.mul(s2).add(Wide::reciprocal(2 * n + 1));
    }
    // log f = 2 × s × series.
    let half = s.mul(series);
    let log_f = Wide {
        exp: half.exp + 1,
        ..half
    };
    let result = Wide::from_i32(k).mul(LN2).add(log_f);
    if result.sig == 0 {
        return 0;
    }
    round(result)
}

/// Replaces the sixteen bytes at `x`, a binary128, with its logarithm.
///
/// # Safety
///
/// `x` must be valid to read and write sixteen bytes.
unsafe extern "C" fn log_bits(x: *mut [u8; 16]) {
    // SAFETY: the caller vouches for the bytes; they are read unaligned.
    let bits = u128::from_le_bytes(unsafe { x.read_unaligned() });
    // SAFETY: as above.
    unsafe { x.write_unaligned(log(bits).to_le_bytes()) };
}

/// `logf128`: the argument and the result in `xmm0`. C declares it
/// `_Float128 logf128(_Float128)`; the Rust signature shows neither, as
/// neither is in a register Rust knows.
///
/// # Safety
///
/// None beyond the ABI's: it reads and writes only its own stack.
#[unsafe(naked)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn logf128() {
    // On entry the stack is 8 below a multiple of 16; taking 24 aligns it
    // for the call and leaves 16 bytes for the value.
    core::arch::naked_asm!(
        "sub rsp, 24",
        "movdqu xmmword ptr [rsp], xmm0",
        "mov rdi, rsp",
        "call {work}",
        "movdqu xmm0, xmmword ptr [rsp]",
        "add rsp, 24",
        "ret",
        work = sym log_bits,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary128 from its sign, unbiased exponent and fraction.
    const fn bits(negative: bool, exp: i32, fraction: u128) -> u128 {
        (if negative { SIGN } else { 0 }) | ((exp + BIAS) as u128) << 112 | fraction
    }

    #[test]
    fn the_logarithms_of_some_values_are_their_rounded_values() {
        for (x, want) in EXPECTED {
            assert_eq!(log(x), want, "log of {x:#034x}");
        }
    }

    #[test]
    fn the_special_values_are_ieee_754s() {
        assert_eq!(log(bits(false, 0, 0)), 0, "log 1 is +0");
        assert_eq!(log(0), SIGN | SPECIAL << 112, "log +0 is -inf");
        assert_eq!(log(SIGN), SIGN | SPECIAL << 112, "log -0 is -inf");
        assert_eq!(log(SPECIAL << 112), SPECIAL << 112, "log +inf is +inf");
        assert_eq!(log(SIGN | SPECIAL << 112), DEFAULT_NAN, "log -inf is NaN");
        assert_eq!(log(bits(true, 0, 0)), DEFAULT_NAN, "log -1 is NaN");
        let nan = SPECIAL << 112 | QUIET | 5;
        assert_eq!(log(nan), nan, "a quiet NaN passes through");
        assert_eq!(log(SPECIAL << 112 | 5), nan, "a signalling NaN is quieted");
    }

    #[test]
    fn the_series_terms_are_exact_enough() {
        // 1/3 × 3 is one, less at most the truncation's last bit.
        let third = Wide::reciprocal(3);
        let product = third.mul(Wide::from_i32(3));
        let one = Wide::new(false, 0, 1);
        assert_eq!(product.exp, one.exp - 1);
        assert!(product.sig >= u128::MAX - 3, "{:#x}", product.sig);
    }

    /// (x, log x rounded to nearest), from Python's `decimal` at 120
    /// digits.
    const EXPECTED: [(u128, u128); 18] = [
        // 2
        (
            0x40000000000000000000000000000000,
            0x3ffe62e42fefa39ef35793c7673007e6,
        ),
        // 3
        (
            0x40008000000000000000000000000000,
            0x3fff193ea7aad030a976a4198d55053b,
        ),
        // 10
        (
            0x40024000000000000000000000000000,
            0x400026bb1bbb5551582dd4adac5705a6,
        ),
        // 0.5
        (
            0x3ffe0000000000000000000000000000,
            0xbffe62e42fefa39ef35793c7673007e6,
        ),
        // 1.5
        (
            0x3fff8000000000000000000000000000,
            0x3ffd9f323ecbf984bf2b68d766f40522,
        ),
        // 0.75
        (
            0x3ffe8000000000000000000000000000,
            0xbffd269621134db92783beb7676c0aaa,
        ),
        // 1 + 2^-112
        (
            0x3fff0000000000000000000000000001,
            0x3f8effffffffffffffffffffffffffff,
        ),
        // 1 - 2^-113
        (
            0x3ffeffffffffffffffffffffffffffff,
            0xbf8e0000000000000000000000000000,
        ),
        // 1 + 2^-50
        (
            0x3fff0000000000004000000000000000,
            0x3fccffffffffffffc000000000000aab,
        ),
        // the largest finite
        (
            0x7ffeffffffffffffffffffffffffffff,
            0x400c62e42fefa39ef35793c7673007e6,
        ),
        // the smallest normal
        (
            0x00010000000000000000000000000000,
            0xc00c62d918ce2421d65ff90ac8f4ce66,
        ),
        // the smallest subnormal
        (
            0x00000000000000000000000000000001,
            0xc00c6546282207802c89d24d65e96274,
        ),
        // 2^100
        (
            0x40630000000000000000000000000000,
            0x40051542457337d42e1c6b73c89d862c,
        ),
        // sqrt 2 above
        (
            0x3fff6a09e667f3bcc908b2fb1366ea96,
            0x3ffd62e42fefa39ef35793c7673007e7,
        ),
        // sqrt 2 below
        (
            0x3fff6a09e667f3bcc908b2fb1366ea95,
            0x3ffd62e42fefa39ef35793c7673007e5,
        ),
        // 0.1
        (
            0x3ffb999999999999999999999999999a,
            0xc00026bb1bbb5551582dd4adac5705a6,
        ),
        // e
        (
            0x40005bf0a8b1457695355fb8ac404e7a,
            0x3ffeffffffffffffffffffffffffffff,
        ),
        // 123456.789
        (
            0x400fe240c9fbe76c8b4395810624dd2f,
            0x400277281cad8a843bfd90b7bb949070,
        ),
    ];
}
