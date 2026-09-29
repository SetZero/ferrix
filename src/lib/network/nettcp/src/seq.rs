//! Sequence numbers, compared the way RFC 9293 requires.
//!
//! A TCP sequence number is a 32-bit counter that wraps, so `a < b` on the raw
//! integer is wrong the moment a connection passes 4 GiB -- and a long-lived
//! one does. Every comparison in this crate goes through [`SeqNumber`], whose
//! ordering is the standard's: `a` precedes `b` when `(b - a)` read as a signed
//! 32-bit number is positive. That makes the order a *window* half the space
//! wide rather than a total order, which is why this type deliberately does not
//! implement [`Ord`]: sorting sequence numbers is not a meaningful operation,
//! and a `sort` on them would compile and be subtly wrong.

use core::fmt;
use core::ops::{Add, AddAssign, Sub};

/// A TCP sequence number.
///
/// Arithmetic wraps, and comparison is the modular one of RFC 9293 section
/// 3.4, reached through [`SeqNumber::precedes`] and its friends rather than
/// through `<`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct SeqNumber(pub u32);

impl SeqNumber {
    /// The number `self` becomes after `count` bytes of sequence space.
    #[must_use]
    pub const fn advance(self, count: u32) -> SeqNumber {
        SeqNumber(self.0.wrapping_add(count))
    }

    /// How far `self` is ahead of `earlier`, as an unsigned count.
    ///
    /// Only meaningful when `earlier` precedes or equals `self`; a caller that
    /// is not sure asks [`SeqNumber::precedes`] first.
    #[must_use]
    pub const fn distance_from(self, earlier: SeqNumber) -> u32 {
        self.0.wrapping_sub(earlier.0)
    }

    /// Whether `self` comes strictly before `other` in the modular order.
    #[must_use]
    pub const fn precedes(self, other: SeqNumber) -> bool {
        (other.0.wrapping_sub(self.0) as i32) > 0
    }

    /// Whether `self` comes before `other`, or is the same number.
    #[must_use]
    pub const fn precedes_or_equals(self, other: SeqNumber) -> bool {
        (other.0.wrapping_sub(self.0) as i32) >= 0
    }

    /// Whether `self` comes strictly after `other` in the modular order.
    #[must_use]
    pub const fn follows(self, other: SeqNumber) -> bool {
        other.precedes(self)
    }

    /// Whether `self` comes after `other`, or is the same number.
    #[must_use]
    pub const fn follows_or_equals(self, other: SeqNumber) -> bool {
        other.precedes_or_equals(self)
    }

    /// Whether `self` lies in the half-open range `[start, end)`.
    ///
    /// An empty range -- `start == end` -- contains nothing, which is what the
    /// receive-window check wants when the window has closed.
    #[must_use]
    pub const fn is_within(self, start: SeqNumber, end: SeqNumber) -> bool {
        self.follows_or_equals(start) && self.precedes(end)
    }

    /// The earlier of two numbers.
    #[must_use]
    pub const fn min(self, other: SeqNumber) -> SeqNumber {
        if self.precedes(other) { self } else { other }
    }

    /// The later of two numbers.
    #[must_use]
    pub const fn max(self, other: SeqNumber) -> SeqNumber {
        if self.follows(other) { self } else { other }
    }
}

impl Add<u32> for SeqNumber {
    type Output = SeqNumber;

    fn add(self, count: u32) -> SeqNumber {
        self.advance(count)
    }
}

impl AddAssign<u32> for SeqNumber {
    fn add_assign(&mut self, count: u32) {
        *self = self.advance(count);
    }
}

impl Sub for SeqNumber {
    type Output = u32;

    fn sub(self, earlier: SeqNumber) -> u32 {
        self.distance_from(earlier)
    }
}

impl fmt::Debug for SeqNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "seq {}", self.0)
    }
}
