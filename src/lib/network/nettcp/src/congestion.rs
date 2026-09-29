//! How much to have in flight, which is not the same question as how much the
//! peer will accept.
//!
//! The receive window says what the other end has room for. The congestion
//! window says what the path between has room for, and nobody sends it: it is
//! inferred from loss. This is NewReno -- RFC 5681's slow start and congestion
//! avoidance with RFC 6582's fast recovery -- which is not the fastest
//! algorithm in use today and is the one whose behaviour is fully specified in
//! a standard, which is what a first implementation wants.
//!
//! # Why not cubic
//!
//! Linux defaults to cubic and would beat this over a long fat path. Cubic is
//! also a curve fitted to measurements, tuned by constants with no derivation,
//! and wrong in ways that only show up at a scale Ferrix has no way to test.
//! NewReno is arithmetic anyone can check against the RFC, and the interface
//! here -- `on_ack`, `on_loss`, `window` -- is the one a cubic implementation
//! would also have.

use crate::seq::SeqNumber;

/// How many segments a connection may have in flight before it has heard
/// anything: RFC 6928's initial window.
pub const INITIAL_WINDOW_SEGMENTS: u32 = 10;

/// The smallest congestion window a connection is ever reduced to, in
/// segments. RFC 5681 section 3.1 makes the loss window one segment.
pub const LOSS_WINDOW_SEGMENTS: u32 = 1;

/// How many duplicate acknowledgments mean a segment was lost rather than
/// reordered: RFC 5681's three.
pub const DUPLICATE_ACK_THRESHOLD: u32 = 3;

/// Which half of the algorithm a connection is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Below the threshold: the window doubles every round trip.
    SlowStart,
    /// Above it: the window grows by a segment every round trip.
    Avoidance,
    /// A loss is being repaired; the window is held down until the
    /// acknowledgment that covers everything sent when the loss was detected.
    FastRecovery,
}

/// The congestion window and the state behind it.
#[derive(Clone, Copy, Debug)]
pub struct Control {
    /// The window itself, in bytes.
    window: u32,
    /// Where slow start ends and congestion avoidance begins, in bytes.
    threshold: u32,
    /// The maximum segment size the window is counted in.
    segment: u32,
    /// Bytes acknowledged since the window last grew, for congestion
    /// avoidance's per-round-trip increase.
    acked: u32,
    /// How many duplicate acknowledgments have arrived in a row.
    duplicates: u32,
    /// In fast recovery, the highest sequence number sent when the loss was
    /// detected: recovery ends when this is acknowledged.
    recover: SeqNumber,
    /// Which half of the algorithm this is.
    phase: Phase,
}

impl Control {
    /// A window for a connection whose segments are `segment` bytes.
    #[must_use]
    pub const fn new(segment: u32) -> Control {
        let segment = if segment == 0 { 1 } else { segment };
        Control {
            window: segment.saturating_mul(INITIAL_WINDOW_SEGMENTS),
            threshold: u32::MAX,
            segment,
            acked: 0,
            duplicates: 0,
            recover: SeqNumber(0),
            phase: Phase::SlowStart,
        }
    }

    /// The window, in bytes.
    #[must_use]
    pub const fn window(&self) -> u32 {
        self.window
    }

    /// Where slow start ends, in bytes.
    #[must_use]
    pub const fn threshold(&self) -> u32 {
        self.threshold
    }

    /// Which half of the algorithm this is.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// How many duplicate acknowledgments have arrived in a row.
    #[must_use]
    pub const fn duplicates(&self) -> u32 {
        self.duplicates
    }

    /// Tell the window the segment size changed, which the peer's MSS option
    /// or a path-MTU report can do.
    pub fn set_segment(&mut self, segment: u32) {
        self.segment = if segment == 0 { 1 } else { segment };
    }

    /// Account for `bytes` newly acknowledged.
    ///
    /// Slow start adds a segment per acknowledgment, which doubles the window
    /// every round trip. Congestion avoidance adds one segment per window
    /// acknowledged, which adds one per round trip.
    pub fn on_ack(&mut self, bytes: u32) {
        self.duplicates = 0;
        if self.phase == Phase::FastRecovery {
            return;
        }
        if self.window < self.threshold {
            self.phase = Phase::SlowStart;
            self.window = self
                .window
                .saturating_add(bytes.min(self.segment))
                .min(u32::MAX / 2);
        } else {
            self.phase = Phase::Avoidance;
            self.acked = self.acked.saturating_add(bytes);
            if self.acked >= self.window {
                self.acked -= self.window;
                self.window = self.window.saturating_add(self.segment).min(u32::MAX / 2);
            }
        }
    }

    /// Account for an acknowledgment that acknowledged nothing new.
    ///
    /// Answers whether this is the third such in a row, which is the signal to
    /// retransmit without waiting for the timeout.
    pub fn on_duplicate_ack(&mut self) -> bool {
        self.duplicates = self.duplicates.saturating_add(1);
        if self.phase == Phase::FastRecovery {
            // RFC 5681 section 3.2 rule 4: inflate the window by a segment for
            // each duplicate, because each one is a segment that has left the
            // network.
            self.window = self.window.saturating_add(self.segment).min(u32::MAX / 2);
            return false;
        }
        self.duplicates == DUPLICATE_ACK_THRESHOLD
    }

    /// Enter fast recovery, halving the window, and say what it became.
    ///
    /// `in_flight` is what is unacknowledged and `highest` the highest sequence
    /// number sent, which is the point recovery ends at.
    pub fn enter_recovery(&mut self, in_flight: u32, highest: SeqNumber) -> u32 {
        self.threshold = (in_flight / 2).max(self.segment.saturating_mul(2));
        self.window = self
            .threshold
            .saturating_add(self.segment.saturating_mul(DUPLICATE_ACK_THRESHOLD));
        self.recover = highest;
        self.phase = Phase::FastRecovery;
        self.acked = 0;
        self.window
    }

    /// Whether an acknowledgment of `ack` ends fast recovery.
    #[must_use]
    pub fn recovered(&self, ack: SeqNumber) -> bool {
        self.phase == Phase::FastRecovery && ack.follows_or_equals(self.recover)
    }

    /// Leave fast recovery, deflating the window to the threshold.
    pub fn leave_recovery(&mut self) {
        self.window = self.threshold.max(self.segment);
        self.phase = if self.window < self.threshold {
            Phase::SlowStart
        } else {
            Phase::Avoidance
        };
        self.acked = 0;
        self.duplicates = 0;
    }

    /// A retransmission timeout: halve the threshold and start again from one
    /// segment, as RFC 5681 section 3.1 requires.
    pub fn on_timeout(&mut self, in_flight: u32) {
        self.threshold = (in_flight / 2).max(self.segment.saturating_mul(2));
        self.window = self.segment.saturating_mul(LOSS_WINDOW_SEGMENTS);
        self.phase = Phase::SlowStart;
        self.acked = 0;
        self.duplicates = 0;
    }
}
