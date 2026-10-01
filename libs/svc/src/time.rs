//! The manager's clock: a point on a monotonic clock the backend reads.
//!
//! The core never reads a clock. Every [`step`](crate::Manager::step) is
//! told the time, so a test replays time as it replays events, and a
//! native root task can serve it from a timer object as well as a Linux
//! init serves it from `clock_gettime(CLOCK_MONOTONIC)`.

use core::fmt;
use core::ops::Add;
use core::time::Duration;

/// A point in time: nanoseconds on a monotonic clock whose start is the
/// backend's business.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    /// The clock's start.
    pub const ZERO: Instant = Instant(0);

    /// The instant `nanos` after the clock's start.
    pub const fn from_nanos(nanos: u64) -> Instant {
        Instant(nanos)
    }

    /// The instant `millis` after the clock's start, for tests and for a
    /// clock that counts in milliseconds.
    pub const fn from_millis(millis: u64) -> Instant {
        Instant(millis.saturating_mul(1_000_000))
    }

    /// Nanoseconds since the clock's start.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// How long after `earlier` this is; zero if it is not after it.
    pub fn saturating_since(self, earlier: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    /// Saturating: a deadline past the clock's end never comes, which is
    /// what an `infinity` timeout means anyway.
    fn add(self, duration: Duration) -> Instant {
        let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        Instant(self.0.saturating_add(nanos))
    }
}

impl fmt::Display for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = self.0 / 1_000_000;
        write!(f, "{}.{:03}s", millis / 1000, millis % 1000)
    }
}
