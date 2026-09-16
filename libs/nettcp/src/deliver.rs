//! Turning a segment's bytes into bytes a program can read.
//!
//! Two things happen here, and the order matters. Data that begins exactly
//! where the connection is expecting goes straight into the receive queue, and
//! then anything held out of order that now follows it goes in behind --
//! closing a gap can deliver kilobytes that arrived minutes' worth of
//! retransmissions ago. Data that begins later is held instead, trimmed
//! against what is already there.
//!
//! The peer's close is separate and comes last, because a `FIN` is only
//! accepted once every byte in front of it has been: a `FIN` taken early would
//! tell the program the stream ended while data was still missing.

use ferrix_netwire::tcp::{Flags, Segment};

use crate::conn::{Connection, DELAYED_ACK, Progress};
use crate::rtt::Millis;
use crate::seq::SeqNumber;
use crate::state::State;

impl Connection {
    /// Take the segment's payload.
    pub(crate) fn take_data(&mut self, now: Millis, segment: &Segment<'_>) -> Progress {
        if segment.payload.is_empty() || !self.state.can_receive() {
            return Progress::default();
        }
        let sequence = SeqNumber(segment.header.sequence);
        let Some((start, bytes)) = self.trim(sequence, segment.payload) else {
            self.ack_immediately = true;
            return Progress::default();
        };
        if start == self.rcv_nxt {
            self.deliver(bytes);
            self.drain_holes();
        } else {
            self.holes.insert(start, bytes);
            // A hole means a segment was lost or reordered. The peer learns
            // that only from an acknowledgment, so this one is not held back.
            self.ack_immediately = true;
        }
        if !self.ack_immediately && self.delayed_ack_at.is_none() {
            self.delayed_ack_at = Some(now.saturating_add(DELAYED_ACK));
        }
        Progress {
            readable: !self.recv.is_empty(),
            ..Progress::default()
        }
    }

    /// Cut a payload down to the part that is both new and inside the window.
    ///
    /// Answers `None` when nothing is left, which is a duplicate: every byte
    /// was already received, or every byte is past what this end said it had
    /// room for.
    fn trim<'a>(&self, sequence: SeqNumber, payload: &'a [u8]) -> Option<(SeqNumber, &'a [u8])> {
        let (start, bytes) = if sequence.precedes(self.rcv_nxt) {
            let skip = self.rcv_nxt.distance_from(sequence) as usize;
            (self.rcv_nxt, payload.get(skip..)?)
        } else {
            (sequence, payload)
        };
        let limit = self.rcv_nxt.advance(self.receive_window());
        let room = limit.distance_from(start) as usize;
        let bytes = bytes.get(..bytes.len().min(room))?;
        if bytes.is_empty() {
            return None;
        }
        Some((start, bytes))
    }

    /// Put in-order bytes in the receive queue and move `RCV.NXT` past them.
    fn deliver(&mut self, bytes: &[u8]) {
        let written = self.recv.write(bytes);
        self.rcv_nxt = self.rcv_nxt.advance(written as u32);
    }

    /// Move everything held out of order that now follows `RCV.NXT` into the
    /// receive queue.
    fn drain_holes(&mut self) {
        while let Some(bytes) = self.holes.take_contiguous(self.rcv_nxt) {
            let written = self.recv.write(&bytes);
            self.rcv_nxt = self.rcv_nxt.advance(written as u32);
            if written < bytes.len() {
                // The receive queue filled. Put the rest back, so it is
                // delivered when the program reads.
                if let Some(rest) = bytes.get(written..) {
                    self.holes.insert(self.rcv_nxt, rest);
                }
                break;
            }
        }
        self.holes.trim_before(self.rcv_nxt);
    }

    /// Take the peer's close, if this segment carries one and everything in
    /// front of it has arrived.
    pub(crate) fn take_fin(&mut self, now: Millis, segment: &Segment<'_>) -> Progress {
        if !segment.header.flags.contains(Flags::FIN) || self.fin_received {
            return Progress::default();
        }
        let at = SeqNumber(segment.header.sequence).advance(segment.payload.len() as u32);
        if at != self.rcv_nxt || !self.state.can_receive() {
            self.ack_immediately = true;
            return Progress::default();
        }
        self.fin_received = true;
        self.rcv_nxt = self.rcv_nxt.advance(1);
        self.ack_immediately = true;
        self.holes.clear();
        self.close_from_peer(now);
        Progress {
            readable: true,
            closed: self.state == State::TimeWait,
            ..Progress::default()
        }
    }

    /// The state the peer's close moves this end to.
    fn close_from_peer(&mut self, now: Millis) {
        match self.state {
            State::Established => self.state = State::CloseWait,
            State::FinWait1 => {
                if self.fin_acknowledged() {
                    self.enter_time_wait();
                    self.start_linger(now);
                } else {
                    // Both ends closed at once and this end's close has not
                    // been acknowledged.
                    self.state = State::Closing;
                }
            }
            State::FinWait2 => {
                self.enter_time_wait();
                self.start_linger(now);
            }
            _ => {}
        }
    }
}
