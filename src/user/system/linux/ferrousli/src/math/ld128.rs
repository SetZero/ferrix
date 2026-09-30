//! AArch64's `long double`: IEEE binary128, in software, and the shims that
//! pass one to and from C.
//!
//! # The format
//!
//! Sixteen bytes: the sign in bit 127, a 15-bit exponent biased by 16383, and
//! a 112-bit fraction below an implied leading one. [`F128`] holds the bits.
//!
//! # Arithmetic
//!
//! AArch64 has no instruction for the format, and stable Rust no type for it,
//! so every operation here is integer arithmetic on the fields. Only what the
//! functions below need is here: rounding a wide significand into the format
//! ([`round_pack`]), addition, the square root, remainders, and rounding to an
//! integer. Each rounds in the mode FPCR holds and raises its exceptions in
//! FPSR, through `fenv.h`, as glibc's and musl's soft-float routines for the
//! format do. Tininess is detected before rounding, as Arm's hardware detects
//! it. A NaN operand is returned quieted, a signalling one raising invalid,
//! the first of two as Arm's `FPProcessNaNs` picks; an invalid operation
//! returns the default NaN, positive, as Arm's is.
//!
//! The transcendentals are not here, as they are not in the x87 module:
//! `docs/POSIX-2024.md` lists what is still absent. What is here is the
//! x87 module's set, function for function: manipulation, rounding and the
//! remainders, the classification C's macros call, and nothing that needs a
//! polynomial.
//!
//! # Calling convention
//!
//! AAPCS64 passes a `long double` in a vector register, q0 to q7, and returns
//! one in q0. A Rust `u128` travels in a pair of general registers instead,
//! so each exported function is a naked shim that [`export`] writes: it moves
//! the arguments from the vector registers to general ones, calls a Rust
//! adapter taking `u128`s, and moves a `long double` result back to q0. An
//! integer, pointer, `float` or `double` travels in its register as usual.

use core::ffi::{c_char, c_int, c_long, c_longlong};

use crate::fenv::{
    FE_DIVBYZERO, FE_DOWNWARD, FE_INEXACT, FE_INVALID, FE_OVERFLOW, FE_TOWARDZERO, FE_UNDERFLOW,
    FE_UPWARD, fegetround, feraiseexcept,
};
use crate::math::classify::{FP_INFINITE, FP_NAN, FP_NORMAL, FP_SUBNORMAL, FP_ZERO};
use crate::math::manipulate::{FP_ILOGB0, FP_ILOGBNAN};

/// A binary128 `long double`, as its bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct F128(pub u128);

/// The sign bit.
const SIGN: u128 = 1 << 127;
/// The fraction's bits.
const FRACTION: u128 = (1 << 112) - 1;
/// The implied leading one of a normal number.
const HIDDEN: u128 = 1 << 112;
/// The fraction bit that makes a NaN quiet.
const QUIET: u128 = 1 << 111;
/// The exponent field of an infinity and of a NaN.
const SPECIAL: i32 = 0x7fff;
/// The exponent bias.
const BIAS: i32 = 16383;
/// Bits below the implied one.
const FRACTION_BITS: i32 = 112;

impl F128 {
    /// `+0`.
    pub const ZERO: Self = Self(0);
    /// `1.0`.
    pub const ONE: Self = Self((BIAS as u128) << 112);
    /// `+∞`.
    pub const INFINITY: Self = Self((SPECIAL as u128) << 112);
    /// The NaN an invalid operation makes: positive and quiet, as Arm's.
    pub const NAN: Self = Self((SPECIAL as u128) << 112 | QUIET);
    /// The largest finite value.
    pub const MAX: Self = Self(((SPECIAL - 1) as u128) << 112 | FRACTION);

    /// Whether the sign bit is set.
    pub const fn is_negative(self) -> bool {
        self.0 & SIGN != 0
    }

    /// The exponent field.
    const fn field(self) -> i32 {
        ((self.0 >> 112) & 0x7fff) as i32
    }

    /// The fraction field.
    const fn fraction(self) -> u128 {
        self.0 & FRACTION
    }

    /// Whether this is a NaN.
    pub const fn is_nan(self) -> bool {
        self.field() == SPECIAL && self.fraction() != 0
    }

    /// Whether this is a signalling NaN.
    const fn is_signalling(self) -> bool {
        self.is_nan() && self.0 & QUIET == 0
    }

    /// Whether this is an infinity.
    pub const fn is_infinite(self) -> bool {
        self.field() == SPECIAL && self.fraction() == 0
    }

    /// Whether this is a zero of either sign.
    pub const fn is_zero(self) -> bool {
        self.0 & !SIGN == 0
    }

    /// The magnitude.
    pub const fn abs(self) -> Self {
        Self(self.0 & !SIGN)
    }

    /// The value negated.
    pub const fn neg(self) -> Self {
        Self(self.0 ^ SIGN)
    }

    /// This NaN, quiet.
    const fn quiet(self) -> Self {
        Self(self.0 | QUIET)
    }

    /// A finite nonzero value as `(m, e)` with `m × 2^e` its magnitude and
    /// `m`'s leading one at bit 112, subnormals normalised.
    fn unpack(self) -> (u128, i32) {
        let field = self.field();
        if field == 0 {
            let shift = self.fraction().leading_zeros() as i32 - 15;
            (self.fraction() << shift, 1 - BIAS - FRACTION_BITS - shift)
        } else {
            (self.fraction() | HIDDEN, field - BIAS - FRACTION_BITS)
        }
    }

    /// A `double`, exactly.
    pub fn from_f64(x: f64) -> Self {
        let bits = x.to_bits();
        let sign = if bits >> 63 != 0 { SIGN } else { 0 };
        let field = ((bits >> 52) & 0x7ff) as i32;
        let fraction = u128::from(bits & ((1 << 52) - 1));
        if field == 0x7ff {
            return Self(sign | (SPECIAL as u128) << 112 | fraction << 60);
        }
        if field == 0 {
            if fraction == 0 {
                return Self(sign);
            }
            return round_pack(sign != 0, -1074, fraction, false, false);
        }
        Self(sign | ((field - 1023 + BIAS) as u128) << 112 | fraction << 60)
    }

    /// An integer, exactly.
    pub fn from_i64(n: i64) -> Self {
        if n == 0 {
            return Self::ZERO;
        }
        round_pack(n < 0, 0, u128::from(n.unsigned_abs()), false, false)
    }
}

/// A rounding direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Round {
    /// To nearest, ties to even.
    Nearest,
    /// To nearest, ties away from zero: `round`.
    Away,
    /// Toward +∞.
    Up,
    /// Toward -∞.
    Down,
    /// Toward zero.
    Zero,
}

/// The rounding mode FPCR holds.
fn mode() -> Round {
    match fegetround() {
        FE_UPWARD => Round::Up,
        FE_DOWNWARD => Round::Down,
        FE_TOWARDZERO => Round::Zero,
        _ => Round::Nearest,
    }
}

/// Raises `flags` in FPSR.
fn raise(flags: c_int) {
    let _ = feraiseexcept(flags);
}

/// Whether a value of sign `negative`, whose kept part is `kept`, with `rest`
/// out of `half * 2` dropped and more below if `sticky`, rounds up in
/// magnitude.
const fn rounds_up(
    round: Round,
    negative: bool,
    kept: u128,
    rest: u128,
    half: u128,
    sticky: bool,
) -> bool {
    let inexact = rest != 0 || sticky;
    match round {
        Round::Nearest => rest > half || (rest == half && (sticky || kept & 1 == 1)),
        Round::Away => rest >= half && half != 0,
        Round::Up => inexact && !negative,
        Round::Down => inexact && negative,
        Round::Zero => false,
    }
}

/// `sig × 2^exp`, with a nonzero remainder below `sig`'s last bit if
/// `sticky`, rounded to a binary128 of sign `negative` in the current mode.
/// Inexact, underflow and overflow are raised as they occur unless `quiet`.
fn round_pack(negative: bool, exp: i32, sig: u128, sticky: bool, quiet: bool) -> F128 {
    let sign = if negative { SIGN } else { 0 };
    if sig == 0 {
        // Only a sticky remainder: far below the smallest subnormal.
        if !sticky {
            return F128(sign);
        }
        return tiny(negative, quiet);
    }
    // Put the leading one at bit 124, keeping every bit: shift left exactly,
    // or right folding what falls off into `sticky`.
    let top = 127 - sig.leading_zeros() as i32;
    let (mut sig, mut exp, mut sticky) = (sig, exp, sticky);
    if top < 124 {
        sig <<= 124 - top;
        exp -= 124 - top;
    } else if top > 124 {
        let shift = top - 124;
        sticky |= sig & ((1 << shift) - 1) != 0;
        sig >>= shift;
        exp += shift;
    }
    // The value's leading one weighs 2^(exp + 124).
    let mut field = exp + 124 + BIAS;
    let tiny = field < 1;
    // Twelve bits below the 113 kept, more for a subnormal.
    let shift = if tiny { 12 + (1 - field) } else { 12 };
    let (kept, rest, half) = if shift >= 128 {
        sticky |= sig != 0;
        (0, 0, 0)
    } else {
        let mask = (1_u128 << shift) - 1;
        (sig >> shift, sig & mask, 1 << (shift - 1))
    };
    let inexact = rest != 0 || sticky;
    let round = mode();
    let up = if shift >= 128 {
        // Everything fell below the rounding position, less than half of the
        // smallest subnormal: up only if the mode goes away from zero.
        matches!((round, negative), (Round::Up, false) | (Round::Down, true))
    } else {
        rounds_up(round, negative, kept, rest, half, sticky)
    };
    let mut kept = kept + u128::from(up);
    if tiny {
        field = 0;
    } else if kept >> 113 != 0 {
        kept >>= 1;
        field += 1;
    }
    if field >= SPECIAL {
        if !quiet {
            raise(FE_OVERFLOW | FE_INEXACT);
        }
        let infinite = match round {
            Round::Nearest | Round::Away => true,
            Round::Up => !negative,
            Round::Down => negative,
            Round::Zero => false,
        };
        return if infinite {
            F128(sign | F128::INFINITY.0)
        } else {
            F128(sign | F128::MAX.0)
        };
    }
    if inexact && !quiet {
        raise(if tiny {
            FE_UNDERFLOW | FE_INEXACT
        } else {
            FE_INEXACT
        });
    }
    // A subnormal's kept bits are its fraction, and one that rounded up to
    // 2^112 carries into the exponent field as the smallest normal.
    let bits = if field == 0 {
        kept
    } else {
        (field as u128) << 112 | (kept & FRACTION)
    };
    F128(sign | bits)
}

/// A nonzero value too small to show: zero or the smallest subnormal, by the
/// rounding mode, with underflow and inexact.
fn tiny(negative: bool, quiet: bool) -> F128 {
    if !quiet {
        raise(FE_UNDERFLOW | FE_INEXACT);
    }
    let sign = if negative { SIGN } else { 0 };
    let away = matches!((mode(), negative), (Round::Up, false) | (Round::Down, true));
    F128(sign | u128::from(away))
}

/// The NaN two operands give, as Arm's `FPProcessNaNs` picks it: the first
/// signalling one, else the first quiet one, quieted; invalid if either
/// signals.
fn propagate(x: F128, y: F128) -> F128 {
    if x.is_signalling() || y.is_signalling() {
        raise(FE_INVALID);
    }
    if x.is_signalling() {
        x.quiet()
    } else if y.is_signalling() {
        y.quiet()
    } else if x.is_nan() {
        x
    } else {
        y.quiet()
    }
}

/// `x`, quieted if it is a NaN, raising invalid if it signalled.
fn propagate1(x: F128) -> F128 {
    if x.is_signalling() {
        raise(FE_INVALID);
    }
    if x.is_nan() { x.quiet() } else { x }
}

/// Shifts `x` right by `n`, folding what falls off into the lowest bit.
const fn shift_right_jam(x: u128, n: i32) -> u128 {
    if n <= 0 {
        x
    } else if n >= 128 {
        (x != 0) as u128
    } else {
        (x >> n) | ((x & ((1 << n) - 1) != 0) as u128)
    }
}

/// `x + y`, rounded.
pub fn add(x: F128, y: F128) -> F128 {
    if x.is_nan() || y.is_nan() {
        return propagate(x, y);
    }
    if x.is_infinite() || y.is_infinite() {
        if x.is_infinite() && y.is_infinite() && x.is_negative() != y.is_negative() {
            raise(FE_INVALID);
            return F128::NAN;
        }
        return if x.is_infinite() { x } else { y };
    }
    if x.is_zero() && y.is_zero() {
        // Zeros of opposite sign sum to +0, or -0 rounding down.
        return if x.is_negative() == y.is_negative() {
            x
        } else if mode() == Round::Down {
            F128(SIGN)
        } else {
            F128::ZERO
        };
    }
    if x.is_zero() {
        return y;
    }
    if y.is_zero() {
        return x;
    }
    let (mx, ex) = x.unpack();
    let (my, ey) = y.unpack();
    // Eleven guard bits: the leading one at bit 123.
    let (mut a, mut ea, mut sa) = (mx << 11, ex - 11, x.is_negative());
    let (mut b, mut eb, mut sb) = (my << 11, ey - 11, y.is_negative());
    if eb > ea || (eb == ea && b > a) {
        core::mem::swap(&mut a, &mut b);
        core::mem::swap(&mut ea, &mut eb);
        core::mem::swap(&mut sa, &mut sb);
    }
    let b = shift_right_jam(b, ea - eb);
    if sa == sb {
        return round_pack(sa, ea, a + b, false, false);
    }
    let difference = a - b;
    if difference == 0 {
        return if mode() == Round::Down {
            F128(SIGN)
        } else {
            F128::ZERO
        };
    }
    round_pack(sa, ea, difference, false, false)
}

/// `x - y`, rounded.
pub fn sub(x: F128, y: F128) -> F128 {
    // A NaN keeps its own sign through the subtraction, as Arm's does.
    if y.is_nan() {
        return propagate(x, y);
    }
    add(x, y.neg())
}

/// Whether `x < y`, neither a NaN.
fn less(x: F128, y: F128) -> bool {
    if x.is_zero() && y.is_zero() {
        return false;
    }
    match (x.is_negative(), y.is_negative()) {
        (true, false) => true,
        (false, true) => false,
        (false, false) => x.0 < y.0,
        (true, true) => x.0 > y.0,
    }
}

/// The square root, rounded.
pub fn sqrt(x: F128) -> F128 {
    if x.is_nan() {
        return propagate1(x);
    }
    if x.is_zero() {
        return x;
    }
    if x.is_negative() {
        raise(FE_INVALID);
        return F128::NAN;
    }
    if x.is_infinite() {
        return x;
    }
    let (mut m, mut e) = x.unpack();
    if e & 1 != 0 {
        m <<= 1;
        e -= 1;
    }
    // The root of m × 2^120, bit by bit: 118 result bits from 236 of the
    // radicand, two at a time. The remainder stays below 2^120.
    let bit = |p: i32| -> u128 { if p < 120 { 0 } else { (m >> (p - 120)) & 1 } };
    let mut remainder = 0_u128;
    let mut root = 0_u128;
    let mut pair = 117;
    loop {
        remainder = (remainder << 2) | (bit(2 * pair + 1) << 1) | bit(2 * pair);
        let trial = (root << 2) | 1;
        if remainder >= trial {
            remainder -= trial;
            root = (root << 1) | 1;
        } else {
            root <<= 1;
        }
        if pair == 0 {
            break;
        }
        pair -= 1;
    }
    round_pack(false, e / 2 - 60, root, remainder != 0, false)
}

/// `x` rounded to an integer in direction `round`, and whether that changed
/// it. A NaN is quieted, raising invalid if it signalled.
fn round_integer(x: F128, round: Round) -> (F128, bool) {
    if x.is_nan() {
        return (propagate1(x), false);
    }
    let field = x.field();
    if x.is_infinite() || x.is_zero() || field >= BIAS + FRACTION_BITS {
        return (x, false);
    }
    let negative = x.is_negative();
    let sign = x.0 & SIGN;
    if field < BIAS {
        // |x| < 1: the result is ±0 or ±1.
        let half = field == BIAS - 1;
        let above_half = half && x.fraction() != 0;
        let one = match round {
            Round::Nearest => above_half,
            Round::Away => half,
            Round::Up => !negative,
            Round::Down => negative,
            Round::Zero => false,
        };
        let result = if one {
            F128(sign | F128::ONE.0)
        } else {
            F128(sign)
        };
        return (result, true);
    }
    let bits = BIAS + FRACTION_BITS - field;
    let m = x.fraction() | HIDDEN;
    let mask = (1_u128 << bits) - 1;
    let rest = m & mask;
    if rest == 0 {
        return (x, false);
    }
    let kept = m >> bits;
    let up = rounds_up(round, negative, kept, rest, 1 << (bits - 1), false);
    let mut m = (kept + u128::from(up)) << bits;
    let mut field = field;
    if m >> 113 != 0 {
        m >>= 1;
        field += 1;
    }
    (F128(sign | (field as u128) << 112 | (m & FRACTION)), true)
}

/// An integral `x` as an `i64`, or `None` out of range or for a NaN or an
/// infinity.
fn to_i64(x: F128) -> Option<i64> {
    if x.is_nan() || x.is_infinite() {
        return None;
    }
    let field = x.field();
    if field < BIAS {
        return Some(0);
    }
    let exponent = field - BIAS;
    if exponent > 63 {
        return None;
    }
    let magnitude = (x.fraction() | HIDDEN) >> (FRACTION_BITS - exponent);
    if x.is_negative() {
        if magnitude > 1 << 63 {
            return None;
        }
        Some((magnitude as u64).wrapping_neg().cast_signed())
    } else {
        i64::try_from(magnitude).ok()
    }
}

/// `x` rounded in `round` and converted, with only invalid, and `i64::MIN`,
/// out of range, and inexact if `inexact_counts` and the rounding changed it.
fn round_to_i64(x: F128, round: Round, inexact_counts: bool) -> i64 {
    let (rounded, changed) = round_integer(x, round);
    match to_i64(rounded) {
        Some(n) => {
            if changed && inexact_counts {
                raise(FE_INEXACT);
            }
            n
        }
        None => {
            raise(FE_INVALID);
            i64::MIN
        }
    }
}

/// `x` rounded in the current mode, raising inexact if that changed it.
fn rint_work(x: F128) -> F128 {
    let (result, changed) = round_integer(x, mode());
    if changed {
        raise(FE_INEXACT);
    }
    result
}

/// `x` rounded in the current mode, raising nothing for the rounding.
fn nearbyint_work(x: F128) -> F128 {
    round_integer(x, mode()).0
}

/// `x` rounded toward +∞.
fn ceil_work(x: F128) -> F128 {
    round_integer(x, Round::Up).0
}

/// `x` rounded toward -∞.
fn floor_work(x: F128) -> F128 {
    round_integer(x, Round::Down).0
}

/// `x` rounded toward zero.
fn trunc_work(x: F128) -> F128 {
    round_integer(x, Round::Zero).0
}

/// `x` rounded to nearest, ties away from zero.
fn round_work(x: F128) -> F128 {
    round_integer(x, Round::Away).0
}

/// [`rint_work`] as a `long`.
fn lrint_work(x: F128) -> c_long {
    round_to_i64(x, mode(), true)
}

/// [`rint_work`] as a `long long`.
fn llrint_work(x: F128) -> c_longlong {
    round_to_i64(x, mode(), true)
}

/// [`round_work`] as a `long`.
fn lround_work(x: F128) -> c_long {
    round_to_i64(x, Round::Away, false)
}

/// [`round_work`] as a `long long`.
fn llround_work(x: F128) -> c_longlong {
    round_to_i64(x, Round::Away, false)
}

/// The remainder of `x / y` and the low bits of its quotient: `fmod`'s if
/// `nearest` is false, `remquo`'s if it is true. Exact, as both always are.
fn remainder_quotient(x: F128, y: F128, nearest: bool) -> (F128, u64) {
    if x.is_nan() || y.is_nan() {
        return (propagate(x, y), 0);
    }
    if x.is_infinite() || y.is_zero() {
        raise(FE_INVALID);
        return (F128::NAN, 0);
    }
    if y.is_infinite() || x.is_zero() {
        return (x, 0);
    }
    let negative = x.is_negative();
    let (mx, ex) = x.unpack();
    let (my, ey) = y.unpack();
    // The remainder `r` and `my` at the scale 2^scale, and its sign flipped
    // if the nearest multiple was above.
    let (mut r, scale, mut quotient, mut flip) = (mx, ex, 0_u64, false);
    if ex < ey {
        if !nearest || ex + 1 < ey {
            return (x, 0);
        }
        // |y|/2 <= |x| < |y|: y is 2my at x's scale.
        if mx > my {
            r = 2 * my - mx;
            flip = true;
            quotient = 1;
        }
        let result = round_pack(negative != flip, scale, r, false, true);
        return (result, quotient);
    }
    let mut steps = ex - ey;
    loop {
        if r >= my {
            r -= my;
            quotient += 1;
        }
        if steps == 0 {
            break;
        }
        r <<= 1;
        quotient <<= 1;
        steps -= 1;
    }
    if nearest && (2 * r > my || (2 * r == my && quotient & 1 == 1)) {
        r = my - r;
        flip = true;
        quotient += 1;
    }
    if r == 0 {
        return (F128(if negative { SIGN } else { 0 }), quotient);
    }
    (round_pack(negative != flip, ey, r, false, true), quotient)
}

/// `x` less the multiple of `y` toward zero.
fn fmod_work(x: F128, y: F128) -> F128 {
    remainder_quotient(x, y, false).0
}

/// `x` less the nearest multiple of `y`.
fn remainder_work(x: F128, y: F128) -> F128 {
    remainder_quotient(x, y, true).0
}

/// [`remainder_work`], with the quotient's sign and low bits through `quo`.
///
/// # Safety
///
/// `quo` must be valid for a write of an `int`.
unsafe fn remquo_at(x: F128, y: F128, quo: *mut c_int) -> F128 {
    let (result, quotient) = remainder_quotient(x, y, true);
    let low = (quotient & 0x7fff_ffff) as c_int;
    let low = if x.is_negative() != y.is_negative() {
        -low
    } else {
        low
    };
    // SAFETY: the caller vouches for `quo`.
    unsafe { quo.write(low) };
    result
}

/// `|x|`.
fn fabs_work(x: F128) -> F128 {
    x.abs()
}

/// `|x|` with `y`'s sign.
fn copysign_work(x: F128, y: F128) -> F128 {
    F128(x.abs().0 | (y.0 & SIGN))
}

/// The larger, a NaN losing to a number and `+0` to `-0`.
fn fmax_work(x: F128, y: F128) -> F128 {
    if x.is_nan() {
        return y;
    }
    if y.is_nan() {
        return x;
    }
    if x.is_negative() != y.is_negative() {
        return if x.is_negative() { y } else { x };
    }
    if less(x, y) { y } else { x }
}

/// The smaller, a NaN losing to a number and `-0` to `+0`.
fn fmin_work(x: F128, y: F128) -> F128 {
    if x.is_nan() {
        return y;
    }
    if y.is_nan() {
        return x;
    }
    if x.is_negative() != y.is_negative() {
        return if x.is_negative() { x } else { y };
    }
    if less(x, y) { x } else { y }
}

/// `x - y` where that is positive, and `+0` where it is not.
fn fdim_work(x: F128, y: F128) -> F128 {
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return y;
    }
    if less(y, x) { sub(x, y) } else { F128::ZERO }
}

/// A NaN, whatever the string says, as musl's `nanl.c` returns one.
fn nan_work(_tag: *const c_char) -> F128 {
    F128::NAN
}

/// `x × 2^n`, rounding once, and overflowing and underflowing as C says.
fn scalbn_work(x: F128, n: c_int) -> F128 {
    scalbln_work(x, c_long::from(n))
}

/// [`scalbn_work`] with a `long` exponent.
fn scalbln_work(x: F128, n: c_long) -> F128 {
    if x.is_nan() {
        return propagate1(x);
    }
    if x.is_infinite() || x.is_zero() {
        return x;
    }
    let (m, e) = x.unpack();
    // Past ±40000 every result is an overflow or an underflow already.
    let n = n.clamp(-40_000, 40_000) as i32;
    round_pack(x.is_negative(), e + n, m, false, false)
}

/// The significand in [0.5, 1) with `x`'s sign, and the power of two through
/// `e`.
///
/// # Safety
///
/// `e` must be valid for a write of an `int`.
unsafe fn frexp_at(x: F128, e: *mut c_int) -> F128 {
    if x.is_nan() || x.is_infinite() || x.is_zero() {
        // SAFETY: the caller vouches for `e`.
        unsafe { e.write(0) };
        return propagate1(x);
    }
    let (m, exponent) = x.unpack();
    // SAFETY: as above.
    unsafe { e.write(exponent + FRACTION_BITS + 1) };
    F128((x.0 & SIGN) | ((BIAS - 1) as u128) << 112 | (m & FRACTION))
}

/// The exponent of `x`, as `ilogb` reports it.
fn ilogb_work(x: F128) -> c_int {
    if x.is_zero() {
        raise(FE_INVALID);
        return FP_ILOGB0;
    }
    if x.is_nan() {
        raise(FE_INVALID);
        return FP_ILOGBNAN;
    }
    if x.is_infinite() {
        raise(FE_INVALID);
        return c_int::MAX;
    }
    x.unpack().1 + FRACTION_BITS
}

/// [`ilogb_work`] as a `long double`, with the exceptions C asks for.
fn logb_work(x: F128) -> F128 {
    if x.is_nan() {
        return propagate1(x);
    }
    if x.is_infinite() {
        return x.abs();
    }
    if x.is_zero() {
        raise(FE_DIVBYZERO);
        return F128::INFINITY.neg();
    }
    F128::from_i64(i64::from(x.unpack().1 + FRACTION_BITS))
}

/// The fractional part of `x`, with `x`'s sign, and the integral part through
/// `iptr`.
///
/// # Safety
///
/// `iptr` must be valid for a write of a `long double`.
unsafe fn modf_at(x: F128, iptr: *mut F128) -> F128 {
    let whole = round_integer(x, Round::Zero).0;
    // SAFETY: the caller vouches for `iptr`.
    unsafe { iptr.write(whole) };
    if x.is_nan() {
        return whole;
    }
    if x.is_infinite() {
        return F128(x.0 & SIGN);
    }
    // Exact: both are within one binade of each other or the part is zero.
    copysign_work(add(x, whole.neg()), x)
}

/// The next value after `x` toward `y`.
fn nextafter_work(x: F128, y: F128) -> F128 {
    if x.is_nan() || y.is_nan() {
        return propagate(x, y);
    }
    if x == y || (x.is_zero() && y.is_zero()) {
        return y;
    }
    let next = if x.is_zero() {
        F128((y.0 & SIGN) | 1)
    } else if less(x, y) != x.is_negative() {
        F128(x.0 + 1)
    } else {
        F128(x.0 - 1)
    };
    if next.is_infinite() {
        raise(FE_OVERFLOW | FE_INEXACT);
    } else if next.field() == 0 {
        raise(FE_UNDERFLOW | FE_INEXACT);
    }
    next
}

/// The next `double` after `x` toward `y`.
fn nexttoward_double(x: f64, y: F128) -> f64 {
    let wide = F128::from_f64(x);
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return f64::from_bits(0x7ff8_0000_0000_0000 | (y.0 >> 60) as u64);
    }
    if wide == y || (wide.is_zero() && y.is_zero()) {
        return x;
    }
    let bits = x.to_bits();
    let next = if x == 0.0 {
        f64::from_bits(if y.is_negative() { 1 << 63 | 1 } else { 1 })
    } else if less(wide, y) != x.is_sign_negative() {
        f64::from_bits(bits + 1)
    } else {
        f64::from_bits(bits - 1)
    };
    if next.is_infinite() {
        raise(FE_OVERFLOW | FE_INEXACT);
    } else if next.is_subnormal() || next == 0.0 {
        raise(FE_UNDERFLOW | FE_INEXACT);
    }
    next
}

/// The next `float` after `x` toward `y`.
fn nexttoward_float(x: f32, y: F128) -> f32 {
    let wide = F128::from_f64(f64::from(x));
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return f32::from_bits(0x7fc0_0000 | (y.0 >> 89) as u32);
    }
    if wide == y || (wide.is_zero() && y.is_zero()) {
        return x;
    }
    let bits = x.to_bits();
    let next = if x == 0.0 {
        f32::from_bits(if y.is_negative() { 1 << 31 | 1 } else { 1 })
    } else if less(wide, y) != x.is_sign_negative() {
        f32::from_bits(bits + 1)
    } else {
        f32::from_bits(bits - 1)
    };
    if next.is_infinite() {
        raise(FE_OVERFLOW | FE_INEXACT);
    } else if next.is_subnormal() || next == 0.0 {
        raise(FE_UNDERFLOW | FE_INEXACT);
    }
    next
}

/// C's class of `x`: `FP_NAN`, `FP_INFINITE`, `FP_ZERO`, `FP_SUBNORMAL` or
/// `FP_NORMAL`.
fn classify_work(x: F128) -> c_int {
    match (x.field(), x.fraction()) {
        (SPECIAL, 0) => FP_INFINITE,
        (SPECIAL, _) => FP_NAN,
        (0, 0) => FP_ZERO,
        (0, _) => FP_SUBNORMAL,
        _ => FP_NORMAL,
    }
}

/// 1 if the sign bit is set, and 0 otherwise.
fn signbit_work(x: F128) -> c_int {
    c_int::from(x.is_negative())
}

/// Defines an exported `long double` function: a naked shim with the C name,
/// and the `extern "C"` adapter it calls, in a private module of the same
/// name, which calls the Rust function after `=`.
///
/// The shapes, with the registers each shim moves:
///
/// * a `long double` result: q0 → x0:x1 after the call, the shim keeping a
///   frame record to call from;
/// * `long double` arguments: q0 → x0:x1, q1 → x2:x3;
/// * an integer or pointer after them: from x0 to x2, or to x4 after two;
/// * `nexttoward`'s `double` or `float` stays in its register, and the
///   `long double` in q1 moves to x0:x1.
macro_rules! export {
    (@shim $(#[$doc:meta])* fn $name:ident; $($setup:literal),* ; returns) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The caller must pass the arguments C declares, as C does. The Rust
        /// signature shows none of them: a `long double` travels in a vector
        /// register, where no Rust type can name it.
        #[unsafe(naked)]
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                $($setup,)*
                "bl {abi}",
                "fmov d0, x0",
                "mov v0.d[1], x1",
                "ldp x29, x30, [sp], #16",
                "ret",
                abi = sym $name::abi,
            )
        }
    };
    (@shim $(#[$doc:meta])* fn $name:ident; $($setup:literal),* ; jumps) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The caller must pass the arguments C declares, as C does. The Rust
        /// signature shows none of them: a `long double` travels in a vector
        /// register, where no Rust type can name it.
        #[unsafe(naked)]
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                $($setup,)*
                "b {abi}",
                abi = sym $name::abi,
            )
        }
    };
    ($(#[$doc:meta])* fn $name:ident(long double) -> long double = $work:ident;) => {
        mod $name {
            use super::*;
            #[doc = concat!("`", stringify!($work), "` of `x`'s bits.")]
            pub(super) extern "C" fn abi(x: u128) -> u128 {
                $work(F128(x)).0
            }
        }
        export!(@shim $(#[$doc])* fn $name; "fmov x0, d0", "mov x1, v0.d[1]"; returns);
    };
    ($(#[$doc:meta])* fn $name:ident(long double, long double) -> long double = $work:ident;) => {
        mod $name {
            use super::*;
            #[doc = concat!("`", stringify!($work), "` of `x`'s and `y`'s bits.")]
            pub(super) extern "C" fn abi(x: u128, y: u128) -> u128 {
                $work(F128(x), F128(y)).0
            }
        }
        export!(@shim $(#[$doc])* fn $name;
            "fmov x0, d0", "mov x1, v0.d[1]", "fmov x2, d1", "mov x3, v1.d[1]"; returns);
    };
    ($(#[$doc:meta])* fn $name:ident(long double, $arg:ident: $ty:ty) -> long double = $work:ident;) => {
        mod $name {
            use super::*;
            #[doc = concat!("`", stringify!($work), "` of `x`'s bits and `", stringify!($arg), "`.")]
            pub(super) extern "C" fn abi(x: u128, $arg: $ty) -> u128 {
                #[allow(unused_unsafe, reason = "some of the functions are unsafe")]
                // SAFETY: the caller passed what C declares.
                unsafe { $work(F128(x), $arg) }.0
            }
        }
        export!(@shim $(#[$doc])* fn $name;
            "mov x2, x0", "fmov x0, d0", "mov x1, v0.d[1]"; returns);
    };
    ($(#[$doc:meta])* fn $name:ident(long double, long double, $arg:ident: $ty:ty) -> long double = $work:ident;) => {
        mod $name {
            use super::*;
            #[doc = concat!("`", stringify!($work), "` of `x`'s and `y`'s bits and `", stringify!($arg), "`.")]
            pub(super) extern "C" fn abi(x: u128, y: u128, $arg: $ty) -> u128 {
                // SAFETY: the caller passed what C declares.
                unsafe { $work(F128(x), F128(y), $arg) }.0
            }
        }
        export!(@shim $(#[$doc])* fn $name;
            "mov x4, x0", "fmov x0, d0", "mov x1, v0.d[1]", "fmov x2, d1", "mov x3, v1.d[1]"; returns);
    };
    ($(#[$doc:meta])* fn $name:ident($arg:ident: $ty:ty) -> long double = $work:ident;) => {
        mod $name {
            use super::*;
            #[doc = concat!("`", stringify!($work), "` of `", stringify!($arg), "`.")]
            pub(super) extern "C" fn abi($arg: $ty) -> u128 {
                $work($arg).0
            }
        }
        export!(@shim $(#[$doc])* fn $name; ; returns);
    };
    ($(#[$doc:meta])* fn $name:ident(long double) -> $ret:ty = $work:ident;) => {
        mod $name {
            use super::*;
            #[doc = concat!("`", stringify!($work), "` of `x`'s bits.")]
            pub(super) extern "C" fn abi(x: u128) -> $ret {
                $work(F128(x))
            }
        }
        export!(@shim $(#[$doc])* fn $name; "fmov x0, d0", "mov x1, v0.d[1]"; jumps);
    };
    ($(#[$doc:meta])* fn $name:ident($arg:ident: $ty:ty, long double) -> $ret:ty = $work:ident;) => {
        mod $name {
            use super::*;
            #[doc = concat!("`", stringify!($work), "` of `", stringify!($arg), "` and `y`'s bits.")]
            pub(super) extern "C" fn abi($arg: $ty, y: u128) -> $ret {
                $work($arg, F128(y))
            }
        }
        export!(@shim $(#[$doc])* fn $name; "fmov x0, d1", "mov x1, v1.d[1]"; jumps);
    };
}

export!(
    /// `|x|`.
    fn fabsl(long double) -> long double = fabs_work;
);
export!(
    /// `|x|` with `y`'s sign.
    fn copysignl(long double, long double) -> long double = copysign_work;
);
export!(
    /// The larger of `x` and `y`, a NaN losing to a number.
    fn fmaxl(long double, long double) -> long double = fmax_work;
);
export!(
    /// The smaller of `x` and `y`, a NaN losing to a number.
    fn fminl(long double, long double) -> long double = fmin_work;
);
export!(
    /// `x - y` where positive, and `+0` otherwise.
    fn fdiml(long double, long double) -> long double = fdim_work;
);
export!(
    /// The square root, correctly rounded.
    fn sqrtl(long double) -> long double = sqrt;
);
export!(
    /// `x` rounded toward +∞.
    fn ceill(long double) -> long double = ceil_work;
);
export!(
    /// `x` rounded toward -∞.
    fn floorl(long double) -> long double = floor_work;
);
export!(
    /// `x` rounded toward zero.
    fn truncl(long double) -> long double = trunc_work;
);
export!(
    /// `x` rounded to nearest, ties away from zero.
    fn roundl(long double) -> long double = round_work;
);
export!(
    /// `x` rounded in the current mode, raising inexact if that changed it.
    fn rintl(long double) -> long double = rint_work;
);
export!(
    /// `x` rounded in the current mode, raising nothing for it.
    fn nearbyintl(long double) -> long double = nearbyint_work;
);
export!(
    /// [`rintl`] as a `long`.
    fn lrintl(long double) -> c_long = lrint_work;
);
export!(
    /// [`rintl`] as a `long long`.
    fn llrintl(long double) -> c_longlong = llrint_work;
);
export!(
    /// [`roundl`] as a `long`.
    fn lroundl(long double) -> c_long = lround_work;
);
export!(
    /// [`roundl`] as a `long long`.
    fn llroundl(long double) -> c_longlong = llround_work;
);
export!(
    /// `x` less the multiple of `y` toward zero.
    fn fmodl(long double, long double) -> long double = fmod_work;
);
export!(
    /// `x` less the nearest multiple of `y`.
    fn remainderl(long double, long double) -> long double = remainder_work;
);
export!(
    /// [`remainderl`], with the quotient's sign and low bits through `quo`.
    fn remquol(long double, long double, quo: *mut c_int) -> long double = remquo_at;
);
export!(
    /// `x × 2^n`.
    fn scalbnl(long double, n: c_int) -> long double = scalbn_work;
);
export!(
    /// `x × 2^n`, the same as [`scalbnl`].
    fn ldexpl(long double, n: c_int) -> long double = scalbn_work;
);
export!(
    /// `x × 2^n` with a `long` exponent.
    fn scalblnl(long double, n: c_long) -> long double = scalbln_work;
);
export!(
    /// The significand in [0.5, 1), and the power of two through `e`.
    fn frexpl(long double, e: *mut c_int) -> long double = frexp_at;
);
export!(
    /// The exponent of `x` as a `long double`.
    fn logbl(long double) -> long double = logb_work;
);
export!(
    /// The exponent of `x`.
    fn ilogbl(long double) -> c_int = ilogb_work;
);
export!(
    /// The fractional part, and the integral part through `iptr`.
    fn modfl(long double, iptr: *mut F128) -> long double = modf_at;
);
export!(
    /// The next value after `x` toward `y`.
    fn nextafterl(long double, long double) -> long double = nextafter_work;
);
export!(
    /// The next value after `x` toward `y`, the same as [`nextafterl`].
    fn nexttowardl(long double, long double) -> long double = nextafter_work;
);
export!(
    /// The next `double` after `x` toward the `long double` `y`.
    fn nexttoward(x: f64, long double) -> f64 = nexttoward_double;
);
export!(
    /// The next `float` after `x` toward the `long double` `y`.
    fn nexttowardf(x: f32, long double) -> f32 = nexttoward_float;
);
export!(
    /// A quiet NaN.
    fn nanl(tag: *const c_char) -> long double = nan_work;
);
export!(
    /// C's class of `x`, for `fpclassify` and the macros built on it.
    fn __fpclassifyl(long double) -> c_int = classify_work;
);
export!(
    /// 1 if `x`'s sign bit is set, and 0 otherwise.
    fn __signbitl(long double) -> c_int = signbit_work;
);

/// Defines a naked shim `$name` returning a `long double`: it calls `$abi`, an
/// `extern "C"` function returning the value's bits, with the arguments
/// where the caller left them, and moves the bits to q0. `strtold` and
/// `wcstold` are made this way.
macro_rules! returns_long_double {
    ($(#[$doc:meta])* fn $name:ident($($arg:ident: $ty:ty),*) via $abi:path) => {
        $(#[$doc])*
        ///
        /// # Safety
        ///
        /// The caller's contract is the adapter's. The Rust signature shows
        /// no result: a `long double` is returned in q0, where no Rust type
        /// can name it.
        #[unsafe(naked)]
        #[cfg_attr(not(test), unsafe(no_mangle))]
        pub unsafe extern "C" fn $name($($arg: $ty),*) {
            core::arch::naked_asm!(
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                "bl {abi}",
                "fmov d0, x0",
                "mov v0.d[1], x1",
                "ldp x29, x30, [sp], #16",
                "ret",
                abi = sym $abi,
            )
        }
    };
}
pub(crate) use returns_long_double;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fenv::{FE_ALL_EXCEPT, FE_TONEAREST, feclearexcept, fesetround, fetestexcept};

    /// A binary128 of sign, biased exponent and fraction.
    const fn q(negative: bool, field: i32, fraction: u128) -> F128 {
        F128(if negative { SIGN } else { 0 } | (field as u128) << 112 | fraction)
    }

    /// `n` as a binary128.
    fn int(n: i64) -> F128 {
        F128::from_i64(n)
    }

    /// Runs `f` in mode `mode` with the flags clear, and returns its result
    /// and the flags it raised.
    fn under<T>(mode: c_int, f: impl FnOnce() -> T) -> (T, c_int) {
        assert_eq!(fesetround(mode), 0);
        let _ = feclearexcept(FE_ALL_EXCEPT);
        let value = f();
        let raised = fetestexcept(FE_ALL_EXCEPT);
        let _ = fesetround(FE_TONEAREST);
        (value, raised)
    }

    #[test]
    fn integers_and_doubles_convert_exactly() {
        assert_eq!(int(1), F128::ONE);
        assert_eq!(int(-3), q(true, BIAS + 1, 1 << 111));
        assert_eq!(F128::from_f64(0.5), q(false, BIAS - 1, 0));
        assert_eq!(F128::from_f64(-0.0), F128(SIGN));
        assert_eq!(F128::from_f64(f64::from_bits(1)), q(false, BIAS - 1074, 0));
        assert_eq!(F128::from_f64(f64::INFINITY), F128::INFINITY);
    }

    #[test]
    fn addition_rounds_in_every_mode_and_raises_what_it_should() {
        // 1 + 2^-113 is halfway between 1 and its successor: ties to even.
        let tie = q(false, BIAS - 113, 0);
        let (sum, raised) = under(FE_TONEAREST, || add(F128::ONE, tie));
        assert_eq!((sum, raised), (F128::ONE, FE_INEXACT));
        let (sum, _) = under(FE_UPWARD, || add(F128::ONE, tie));
        assert_eq!(sum, q(false, BIAS, 1));
        let (sum, raised) = under(FE_TONEAREST, || add(int(3), int(-5)));
        assert_eq!((sum, raised), (int(-2), 0));
        let (sum, _) = under(FE_DOWNWARD, || add(int(3), int(-3)));
        assert_eq!(sum, F128(SIGN));
        let (sum, raised) = under(FE_TONEAREST, || add(F128::MAX, F128::MAX));
        assert_eq!((sum, raised), (F128::INFINITY, FE_OVERFLOW | FE_INEXACT));
        let (sum, raised) = under(FE_TOWARDZERO, || add(F128::MAX, F128::MAX));
        assert_eq!((sum, raised), (F128::MAX, FE_OVERFLOW | FE_INEXACT));
        let (sum, raised) = under(FE_TONEAREST, || add(F128::INFINITY, F128::INFINITY.neg()));
        assert_eq!((sum, raised), (F128::NAN, FE_INVALID));
        // Two subnormals sum exactly.
        let (sum, raised) = under(FE_TONEAREST, || add(F128(1), F128(2)));
        assert_eq!((sum, raised), (F128(3), 0));
    }

    #[test]
    fn square_roots_are_correctly_rounded() {
        let (root, raised) = under(FE_TONEAREST, || sqrt(int(4)));
        assert_eq!((root, raised), (int(2), 0));
        // √2 = 0x1.6a09e667f3bcc908b2fb1366ea957d3e3adec1751...p0.
        let (root, raised) = under(FE_TONEAREST, || sqrt(int(2)));
        assert_eq!(root, q(false, BIAS, 0x6a09_e667_f3bc_c908_b2fb_1366_ea95));
        assert_eq!(raised, FE_INEXACT);
        let (root, _) = under(FE_UPWARD, || sqrt(int(2)));
        assert_eq!(root, q(false, BIAS, 0x6a09_e667_f3bc_c908_b2fb_1366_ea96));
        // The smallest subnormal, 2^-16494, has the exact root 2^-8247.
        let (root, raised) = under(FE_TONEAREST, || sqrt(F128(1)));
        assert_eq!((root, raised), (q(false, BIAS - 8247, 0), 0));
        let (root, raised) = under(FE_TONEAREST, || sqrt(int(-1)));
        assert_eq!((root, raised), (F128::NAN, FE_INVALID));
        assert_eq!(sqrt(F128(SIGN)), F128(SIGN));
    }

    #[test]
    fn rounding_to_integers_follows_its_direction() {
        let half = F128::from_f64(2.5);
        assert_eq!(round_integer(half, Round::Nearest).0, int(2));
        assert_eq!(round_integer(half, Round::Away).0, int(3));
        assert_eq!(round_integer(half.neg(), Round::Up).0, int(-2));
        assert_eq!(round_integer(half.neg(), Round::Down).0, int(-3));
        assert_eq!(
            round_integer(F128::from_f64(-0.25), Round::Zero).0,
            F128(SIGN)
        );
        assert_eq!(
            round_integer(F128::from_f64(0.5), Round::Nearest).0,
            F128::ZERO
        );
        assert_eq!(
            round_integer(F128::from_f64(0.75), Round::Nearest).0,
            F128::ONE
        );
        let (rounded, raised) = under(FE_TONEAREST, || rint_work(F128::from_f64(1.5)));
        assert_eq!((rounded, raised), (int(2), FE_INEXACT));
        let (rounded, raised) = under(FE_TONEAREST, || nearbyint_work(F128::from_f64(1.5)));
        assert_eq!((rounded, raised), (int(2), 0));
        // 2^112 - 0.5 rounds up across a binade.
        let below = q(false, BIAS + 111, FRACTION);
        assert_eq!(round_integer(below, Round::Up).0, q(false, BIAS + 112, 0));
        let (n, raised) = under(FE_TONEAREST, || lrint_work(F128::from_f64(-2.5)));
        assert_eq!((n, raised), (-2, FE_INEXACT));
        let (n, raised) = under(FE_TONEAREST, || llround_work(q(false, BIAS + 70, 0)));
        assert_eq!((n, raised), (i64::MIN, FE_INVALID));
        assert_eq!(lround_work(q(true, BIAS + 63, 0)), i64::MIN);
    }

    #[test]
    fn remainders_are_exact_with_their_quotients() {
        let (r, raised) = under(FE_TONEAREST, || fmod_work(int(7), int(3)));
        assert_eq!((r, raised), (int(1), 0));
        assert_eq!(fmod_work(int(-7), int(3)), int(-1));
        assert_eq!(fmod_work(int(-6), int(3)), F128(SIGN));
        assert_eq!(remainder_work(int(7), int(2)), int(-1));
        assert_eq!(remainder_work(int(5), int(2)), int(1));
        let mut quo = 0;
        // SAFETY: `quo` is a local.
        let r = unsafe { remquo_at(int(-7), int(2), &raw mut quo) };
        assert_eq!((r, quo), (int(1), -4));
        // 1.5 against 2: nearer 2 than 0.
        assert_eq!(
            remainder_work(F128::from_f64(1.5), int(2)),
            F128::from_f64(-0.5)
        );
        // A huge quotient: 2^16000 mod 3 is 1.
        assert_eq!(fmod_work(q(false, BIAS + 16000, 0), int(3)), int(1));
        let (r, raised) = under(FE_TONEAREST, || fmod_work(int(1), F128::ZERO));
        assert_eq!((r, raised), (F128::NAN, FE_INVALID));
    }

    #[test]
    fn scaling_rounds_once_at_the_bottom_and_overflows_at_the_top() {
        assert_eq!(scalbn_work(F128::ONE, 3), int(8));
        assert_eq!(scalbn_work(F128::ONE, -16494), F128(1));
        let (x, raised) = under(FE_TONEAREST, || scalbn_work(int(3), -16495));
        assert_eq!((x, raised), (F128(2), FE_UNDERFLOW | FE_INEXACT));
        let (x, raised) = under(FE_TONEAREST, || scalbln_work(F128::ONE, c_long::MAX));
        assert_eq!((x, raised), (F128::INFINITY, FE_OVERFLOW | FE_INEXACT));
        let mut e = 0;
        // SAFETY: `e` is a local.
        let m = unsafe { frexp_at(F128(1), &raw mut e) };
        assert_eq!((m, e), (F128::from_f64(0.5), -16493));
        assert_eq!(ilogb_work(F128(1)), -16494);
        assert_eq!(logb_work(int(-8)), int(3));
    }

    #[test]
    fn neighbours_and_classes() {
        let (next, raised) = under(FE_TONEAREST, || nextafter_work(F128::ZERO, F128::ONE));
        assert_eq!((next, raised), (F128(1), FE_UNDERFLOW | FE_INEXACT));
        assert_eq!(
            nextafter_work(F128::ONE, F128::ZERO),
            q(false, BIAS - 1, FRACTION)
        );
        let (next, raised) = under(FE_TONEAREST, || nextafter_work(F128::MAX, F128::INFINITY));
        assert_eq!((next, raised), (F128::INFINITY, FE_OVERFLOW | FE_INEXACT));
        assert_eq!(nexttoward_double(1.0, int(2)), 1.0 + f64::EPSILON);
        assert_eq!(nexttoward_float(1.0, int(0)), 1.0 - f32::EPSILON / 2.0);
        assert_eq!(classify_work(F128(1)), FP_SUBNORMAL);
        assert_eq!(classify_work(F128::NAN), FP_NAN);
        assert_eq!(classify_work(F128::INFINITY.neg()), FP_INFINITE);
        assert_eq!(signbit_work(F128(SIGN)), 1);
        let mut whole = F128::ZERO;
        // SAFETY: `whole` is a local.
        let part = unsafe { modf_at(F128::from_f64(-3.25), &raw mut whole) };
        assert_eq!((part, whole), (F128::from_f64(-0.25), int(-3)));
        // SAFETY: as above.
        let part = unsafe { modf_at(int(-3), &raw mut whole) };
        assert_eq!(part, F128(SIGN));
    }
}
