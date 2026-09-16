//! How long to wait for an acknowledgment, estimated from how long they have
//! taken.
//!
//! RFC 6298's estimator, in milliseconds. A connection measures the round trip
//! once per window at most, smooths it into `srtt` with its variation `rttvar`,
//! and waits `srtt + 4 * rttvar` for the next acknowledgment. The two bounds
//! matter as much as the formula: without a floor a fast local link produces a
//! timeout shorter than the peer's delayed-acknowledgment timer and the
//! connection retransmits everything it sends, and without a ceiling a single
//! bad measurement parks the connection for an hour.
//!
//! Karn's algorithm is why [`Estimator::measure`] is only ever called with a
//! sample from a segment that was sent once. An acknowledgment after a
//! retransmission does not say which copy it answers, so timing it would fold
//! the retransmission delay into the estimate and make the next timeout longer
//! still.

/// A duration in milliseconds.
pub type Millis = u64;

/// The shortest retransmission timeout, which is Linux's `TCP_RTO_MIN`.
///
/// RFC 6298 says one second. Linux uses a fifth of that, because a second is
/// an age on any link built since the specification was written, and because
/// every peer Ferrix will talk to is a Linux one that has made the same choice.
pub const MIN_RTO: Millis = 200;

/// The longest retransmission timeout, Linux's `TCP_RTO_MAX`.
pub const MAX_RTO: Millis = 120_000;

/// The timeout before any round trip has been measured, from RFC 6298 rule 2.1.
pub const INITIAL_RTO: Millis = 1_000;

/// A smoothed round-trip time and the timeout that follows from it.
#[derive(Clone, Copy, Debug)]
pub struct Estimator {
    /// The smoothed round-trip time, once one has been measured.
    smoothed: Option<Millis>,
    /// The mean deviation of the samples from `smoothed`.
    variation: Millis,
    /// What the connection waits now, before any backoff.
    timeout: Millis,
    /// How many times the timeout has been doubled without a measurement.
    backoff: u32,
}

impl Default for Estimator {
    fn default() -> Estimator {
        Estimator::new()
    }
}

impl Estimator {
    /// An estimator that has measured nothing yet.
    #[must_use]
    pub const fn new() -> Estimator {
        Estimator {
            smoothed: None,
            variation: 0,
            timeout: INITIAL_RTO,
            backoff: 0,
        }
    }

    /// The smoothed round-trip time, if one has been measured.
    #[must_use]
    pub const fn smoothed(&self) -> Option<Millis> {
        self.smoothed
    }

    /// What to wait for the next acknowledgment, backoff included.
    #[must_use]
    pub const fn timeout(&self) -> Millis {
        self.timeout
    }

    /// How many times in a row the timeout has expired.
    #[must_use]
    pub const fn backoff(&self) -> u32 {
        self.backoff
    }

    /// Fold in a round trip of `sample` milliseconds.
    ///
    /// The first sample sets the estimate outright, as RFC 6298 rule 2.2 says;
    /// later ones move it by a weighted eighth. Measuring also clears the
    /// backoff, because an acknowledgment arriving is the evidence that the
    /// path is working again.
    pub fn measure(&mut self, sample: Millis) {
        match self.smoothed {
            None => {
                self.smoothed = Some(sample);
                self.variation = sample / 2;
            }
            Some(smoothed) => {
                let difference = smoothed.abs_diff(sample);
                self.variation = (self.variation * 3).saturating_add(difference) / 4;
                self.smoothed = Some((smoothed * 7).saturating_add(sample) / 8);
            }
        }
        self.backoff = 0;
        self.timeout = self.computed();
    }

    /// `SRTT + 4 * RTTVAR`, held between the two bounds.
    fn computed(&self) -> Millis {
        let smoothed = self.smoothed.unwrap_or(INITIAL_RTO);
        let spread = self.variation.saturating_mul(4).max(1);
        smoothed.saturating_add(spread).clamp(MIN_RTO, MAX_RTO)
    }

    /// Double the timeout after it expired, up to the ceiling.
    ///
    /// RFC 6298 rule 5.5. The doubling is what keeps a connection to a host
    /// that has gone away from sending a segment every fifth of a second until
    /// it gives up.
    pub fn back_off(&mut self) {
        self.backoff = self.backoff.saturating_add(1);
        self.timeout = self.timeout.saturating_mul(2).min(MAX_RTO);
    }

    /// Forget the backoff without forgetting the measurements.
    pub fn clear_backoff(&mut self) {
        self.backoff = 0;
        self.timeout = self.computed();
    }
}
