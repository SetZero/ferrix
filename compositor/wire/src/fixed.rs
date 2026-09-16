//! Wayland's `fixed`: signed 24.8 fixed point.

use core::fmt;

/// A `wl_fixed_t`: a signed number with eight fractional bits, as
/// `wayland-util.h` defines it. Pointer coordinates and surface offsets are
/// carried in it, so the value a compositor works in is whole pixels plus
/// 1/256ths and never a float on the wire.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Fixed(i32);

impl Fixed {
    /// Zero.
    pub const ZERO: Self = Self(0);

    /// The fractional bits.
    const BITS: u32 = 8;

    /// One whole unit.
    pub const ONE: Self = Self(1 << Self::BITS);

    /// The value whose raw representation is `raw`.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    /// The raw representation, which is what the wire carries.
    #[must_use]
    pub const fn to_raw(self) -> i32 {
        self.0
    }

    /// `whole` units, saturating rather than wrapping at the ends of the
    /// 24-bit whole part, since a coordinate that wrapped would put a window
    /// on the other side of the screen.
    #[must_use]
    pub const fn from_int(whole: i32) -> Self {
        Self(whole.saturating_mul(1 << Self::BITS))
    }

    /// The whole part, rounded towards negative infinity as
    /// `wl_fixed_to_int`'s shift is.
    #[must_use]
    pub const fn to_int(self) -> i32 {
        self.0 >> Self::BITS
    }

    /// The value as a double, exactly: every `Fixed` fits in an `f64`.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / f64::from(1 << Self::BITS)
    }

    /// The nearest value to `value`, saturating at the ends of the range and
    /// mapping a NaN to zero, so no caller can make an unrepresentable
    /// coordinate out of arithmetic that went wrong.
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        if value.is_nan() {
            return Self::ZERO;
        }
        let scaled = (value * f64::from(1 << Self::BITS)).round();
        if scaled >= f64::from(i32::MAX) {
            return Self(i32::MAX);
        }
        if scaled <= f64::from(i32::MIN) {
            return Self(i32::MIN);
        }
        // In range by the two tests above, so the cast is exact.
        Self(scaled as i32)
    }
}

impl fmt::Debug for Fixed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.to_f64())
    }
}

impl fmt::Display for Fixed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.to_f64())
    }
}

impl From<i32> for Fixed {
    fn from(whole: i32) -> Self {
        Self::from_int(whole)
    }
}
