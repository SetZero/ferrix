//! What a connection wants to send, and when it wants to be asked again.
//!
//! Nothing here writes bytes on a wire. [`Connection::poll_transmit`] answers
//! a header and a payload copied into the caller's buffer; the caller puts an
//! IP header in front and gives it to a device. That is what makes the whole
//! state machine testable: a test drives two connections against each other by
//! passing the answers across, with no network and no clock but the one it
//! chooses.
//!
//! # The order of the decisions
//!
//! A reset first, because a connection that owes one owes nothing else. Then
//! the handshake. Then, for an open connection: a window probe if the peer has
//! closed its window, data if there is any and room for it, this end's close
//! once the data is gone, and last a bare acknowledgment. Last, because an
//! acknowledgment that could have travelled on a segment carrying data is a
//! packet that need not have existed.

use ferrix_netwire::tcp::{Flags, Header, Options, SackBlock};

use crate::conn::{
    Connection, DELAYED_ACK, FIN_WAIT_2_TIMEOUT, MAX_RETRANSMITS, MAX_SYN_RETRANSMITS, Progress,
    TIME_WAIT,
};
use crate::rtt::Millis;
use crate::seq::SeqNumber;
use crate::state::{Failure, State};

/// A segment the connection wants sent.
#[derive(Clone, Copy, Debug)]
pub struct Transmit {
    /// The header, less the checksum and data offset an emit computes.
    pub header: Header,
    /// How many bytes of the caller's buffer the payload filled.
    pub payload_len: usize,
}

impl Connection {
    /// The earliest moment at which [`Connection::on_timer`] has something to
    /// do.
    #[must_use]
    pub fn poll_at(&self) -> Option<Millis> {
        [
            self.retransmit_at,
            self.delayed_ack_at,
            self.linger_at,
            self.persist_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Let the clock reach `now`, and answer what changed.
    ///
    /// A caller drives this from [`Connection::poll_at`]; calling it early is
    /// harmless and does nothing.
    pub fn on_timer(&mut self, now: Millis) -> Progress {
        let mut progress = self.expire_linger(now);
        progress = progress.or(self.expire_retransmit(now));
        if self.delayed_ack_at.is_some_and(|at| now >= at) {
            self.delayed_ack_at = None;
            self.ack_immediately = true;
        }
        if self.persist_at.is_some_and(|at| now >= at) {
            self.persist_at = None;
            self.probe_pending = true;
        }
        progress
    }

    /// `TIME-WAIT` and `FIN-WAIT-2` both end by running out of patience.
    fn expire_linger(&mut self, now: Millis) -> Progress {
        if !self.linger_at.is_some_and(|at| now >= at) {
            return Progress::default();
        }
        self.linger_at = None;
        match self.state {
            State::TimeWait | State::FinWait2 => {
                self.state = State::Closed;
                self.clear_timers();
                Progress {
                    readable: true,
                    writable: true,
                    closed: true,
                }
            }
            _ => Progress::default(),
        }
    }

    /// Nothing was acknowledged in time: retransmit, or give up.
    fn expire_retransmit(&mut self, now: Millis) -> Progress {
        if !self.retransmit_at.is_some_and(|at| now >= at) {
            return Progress::default();
        }
        if self.snd_una == self.snd_nxt {
            // Nothing is outstanding: the timer is stale, not expired.
            self.retransmit_at = None;
            return Progress::default();
        }
        let limit = if self.state == State::SynSent || self.state == State::SynReceived {
            MAX_SYN_RETRANSMITS
        } else {
            MAX_RETRANSMITS
        };
        if self.rtt.backoff() >= limit {
            self.fail(Failure::TimedOut);
            return Progress {
                readable: true,
                writable: true,
                closed: true,
            };
        }
        self.rtt.back_off();
        self.congestion.on_timeout(self.in_flight());
        // Go back to the oldest unacknowledged byte. Anything sent after it is
        // sent again, which is what a sender with no selective acknowledgment
        // of its own can do.
        self.snd_nxt = self.snd_una;
        self.timed = None;
        self.retransmit_at = Some(now.saturating_add(self.rtt.timeout()));
        Progress::default()
    }

    /// The next segment to send, if there is one.
    ///
    /// `payload` is where the data is copied; it should be at least
    /// [`Connection::segment_size`] bytes, and a shorter one simply produces
    /// shorter segments.
    pub fn poll_transmit(&mut self, now: Millis, payload: &mut [u8]) -> Option<Transmit> {
        if self.reset_pending {
            self.reset_pending = false;
            return Some(self.reset_segment());
        }
        match self.state {
            State::Closed | State::Listen => None,
            State::SynSent => self.emit_syn(now, Flags::SYN),
            State::SynReceived => self.emit_syn(now, Flags::SYN.union(Flags::ACK)),
            _ => self.emit_open(now, payload),
        }
    }

    /// A reset, which names the sequence number the peer is expecting.
    fn reset_segment(&mut self) -> Transmit {
        let mut header = self.header(Flags::RST.union(Flags::ACK), self.snd_nxt);
        header.window = 0;
        Transmit {
            header,
            payload_len: 0,
        }
    }

    /// The connection request, or its answer.
    fn emit_syn(&mut self, now: Millis, flags: Flags) -> Option<Transmit> {
        if self.snd_nxt.follows(self.iss) && self.retransmit_at.is_some() {
            // Already sent and not yet due again.
            return None;
        }
        let mut header = self.header(flags, self.iss);
        header.options = Options {
            mss: Some(self.rcv_mss),
            window_scale: Some(self.rcv_wscale),
            sack_permitted: self.config.selective_ack,
            ..Options::default()
        };
        if !flags.contains(Flags::ACK) {
            header.acknowledgment = 0;
        }
        self.sent(now, self.iss, 1, false);
        Some(Transmit {
            header,
            payload_len: 0,
        })
    }

    /// A segment for a connection past the handshake.
    fn emit_open(&mut self, now: Millis, payload: &mut [u8]) -> Option<Transmit> {
        self.watch_for_a_shut_window(now);
        if let Some(transmit) = self.emit_probe(now, payload) {
            return Some(transmit);
        }
        if let Some(transmit) = self.emit_data(now, payload) {
            return Some(transmit);
        }
        if let Some(transmit) = self.emit_fin(now) {
            return Some(transmit);
        }
        self.emit_ack()
    }

    /// One byte past a window the peer has closed, so that the window update
    /// reopening it cannot be the only segment that says so.
    ///
    /// A lost window update with nothing probing it is a connection that waits
    /// for ever with both ends believing it is the other's turn.
    fn emit_probe(&mut self, now: Millis, payload: &mut [u8]) -> Option<Transmit> {
        if !self.probe_pending {
            return None;
        }
        self.probe_pending = false;
        let offset = self.snd_nxt.distance_from(self.snd_una) as usize;
        let mut byte = [0_u8; 1];
        if self.send.peek(offset, &mut byte) == 0 {
            return None;
        }
        let slot = payload.first_mut()?;
        *slot = byte[0];
        let header = self.header(Flags::ACK, self.snd_nxt);
        // The probe is deliberately *not* recorded as sent. A receiver whose
        // window is shut refuses the byte -- RFC 9293's acceptability test
        // rejects any data against a zero window -- so counting it as sent
        // would leave a hole the peer has to ask for. `SND.NXT` stays where it
        // is and the byte goes out again for real when the window opens.
        //
        // Nor does the retransmission timer run for it. An acknowledgment of a
        // probe does not acknowledge new data, so the timer would never be
        // restarted, and a peer that is merely slow to read would be declared
        // dead after fifteen backoffs. The persist timer is what paces this,
        // and [`Connection::probes`] is what eventually gives up.
        self.probes = self.probes.saturating_add(1);
        if self.probes > MAX_RETRANSMITS {
            self.fail(Failure::TimedOut);
            return None;
        }
        self.ack_immediately = false;
        self.delayed_ack_at = None;
        self.arm_persist(now);
        Some(Transmit {
            header,
            payload_len: 1,
        })
    }

    /// As much queued data as the two windows and the segment size allow.
    fn emit_data(&mut self, now: Millis, payload: &mut [u8]) -> Option<Transmit> {
        let count = self.sendable(payload.len())?;
        let offset = self.snd_nxt.distance_from(self.snd_una) as usize;
        let slot = payload.get_mut(..count)?;
        let copied = self.send.peek(offset, slot);
        if copied == 0 {
            return None;
        }
        let sequence = self.snd_nxt;
        let remaining = (self.send.len() as u32).saturating_sub(offset as u32);
        let last = copied as u32 == remaining;
        let flags = if last {
            Flags::ACK.union(Flags::PSH)
        } else {
            Flags::ACK
        };
        let header = self.header(flags, sequence);
        self.sent(now, sequence, copied as u32, true);
        Some(Transmit {
            header,
            payload_len: copied,
        })
    }

    /// Arm the probe when the peer has shut its window on data that is
    /// waiting, so that a lost window update does not stall the connection.
    fn watch_for_a_shut_window(&mut self, now: Millis) {
        let unsent =
            (self.send.len() as u32).saturating_sub(self.snd_nxt.distance_from(self.snd_una));
        let stalled = self.snd_wnd == 0 && unsent > 0 && self.state.can_send();
        if stalled && self.persist_at.is_none() && !self.probe_pending {
            self.arm_persist(now);
        }
    }

    /// How many bytes may go out now, or `None` for "not yet".
    fn sendable(&mut self, room: usize) -> Option<usize> {
        if !self.state.is_synchronised() || self.state == State::TimeWait {
            return None;
        }
        let offset = self.snd_nxt.distance_from(self.snd_una);
        let queued = self.send.len() as u32;
        let available = queued.checked_sub(offset)?;
        if available == 0 {
            return None;
        }
        let window = self.snd_wnd.min(self.congestion.window());
        let allowed = window.saturating_sub(offset);
        if allowed == 0 {
            return None;
        }
        let count = available
            .min(allowed)
            .min(u32::from(self.snd_mss))
            .min(room as u32) as usize;
        if count == 0 || self.nagle_holds(count, available) {
            return None;
        }
        Some(count)
    }

    /// Whether Nagle's algorithm holds a small segment back.
    ///
    /// A segment shorter than the maximum waits while anything is
    /// unacknowledged, so that a program writing a byte at a time sends one
    /// segment per round trip rather than one per byte. `TCP_NODELAY` turns it
    /// off, and a close waiting behind the data overrides it: there is nothing
    /// left to coalesce with.
    fn nagle_holds(&self, count: usize, available: u32) -> bool {
        if self.config.no_delay || self.snd_nxt == self.snd_una {
            return false;
        }
        let whole = count as u32 == u32::from(self.snd_mss);
        let finishes = self.fin_queued && count as u32 == available;
        !whole && !finishes
    }

    /// This end's close, once every byte in front of it has been sent.
    fn emit_fin(&mut self, now: Millis) -> Option<Transmit> {
        if !self.fin_queued {
            return None;
        }
        let queued = self.send.len() as u32;
        let sent = self.snd_nxt.distance_from(self.snd_una);
        match self.fin_seq {
            Some(fin) => {
                // Already sent; send it again only after a retransmission
                // rewound past it.
                if !self.snd_nxt.precedes_or_equals(fin) {
                    return None;
                }
                let header = self.header(Flags::FIN.union(Flags::ACK), fin);
                self.sent(now, fin, 1, false);
                Some(Transmit {
                    header,
                    payload_len: 0,
                })
            }
            None if sent >= queued => {
                let fin = self.snd_nxt;
                self.fin_seq = Some(fin);
                let header = self.header(Flags::FIN.union(Flags::ACK), fin);
                self.sent(now, fin, 1, false);
                Some(Transmit {
                    header,
                    payload_len: 0,
                })
            }
            None => None,
        }
    }

    /// A bare acknowledgment, if one is owed.
    fn emit_ack(&mut self) -> Option<Transmit> {
        if !self.ack_immediately {
            return None;
        }
        self.ack_immediately = false;
        self.delayed_ack_at = None;
        Some(Transmit {
            header: self.header(Flags::ACK, self.snd_nxt),
            payload_len: 0,
        })
    }

    /// A header with this connection's ports, acknowledgment and window.
    fn header(&self, flags: Flags, sequence: SeqNumber) -> Header {
        let mut options = Options::default();
        if self.sack_permitted && !self.holes.is_empty() {
            options = Self::sack_options(&mut [(SeqNumber(0), SeqNumber(0)); 4], &self.holes);
        }
        Header {
            source_port: self.local_port,
            destination_port: self.remote_port,
            sequence: sequence.0,
            acknowledgment: self.rcv_nxt.0,
            flags,
            window: self.advertised_window(),
            urgent_pointer: 0,
            options,
        }
    }

    /// The SACK option naming what arrived out of order.
    fn sack_options(
        scratch: &mut [(SeqNumber, SeqNumber); 4],
        holes: &crate::ring::Reassembly,
    ) -> Options {
        let count = holes.blocks(scratch);
        let mut options = Options {
            sack_blocks: count,
            ..Options::default()
        };
        for (slot, (left, right)) in options.sack.iter_mut().zip(scratch.iter()) {
            *slot = SackBlock {
                left: left.0,
                right: right.0,
            };
        }
        options
    }

    /// Record that `count` sequence numbers went out starting at `sequence`.
    fn sent(&mut self, now: Millis, sequence: SeqNumber, count: u32, is_data: bool) {
        let end = sequence.advance(count);
        if end.follows(self.snd_nxt) {
            self.snd_nxt = end;
        }
        let fresh = end.follows(self.snd_max);
        if fresh {
            self.snd_max = end;
        }
        // Karn's algorithm: only a segment sent for the first time is timed.
        if fresh && self.timed.is_none() && (is_data || count > 0) {
            self.timed = Some((now, end));
        }
        self.ack_immediately = false;
        self.delayed_ack_at = None;
        self.arm_retransmit(now);
    }

    /// Start the retransmission timer if anything is outstanding, and stop it
    /// if nothing is.
    pub(crate) fn arm_retransmit(&mut self, now: Millis) {
        if self.snd_una == self.snd_nxt {
            self.retransmit_at = None;
            return;
        }
        if self.retransmit_at.is_none() {
            self.retransmit_at = Some(now.saturating_add(self.rtt.timeout()));
        }
    }

    /// Arm the window probe, backing off as the retransmission timer does.
    pub(crate) fn arm_persist(&mut self, now: Millis) {
        let delay = self.rtt.timeout().max(DELAYED_ACK);
        self.persist_at = Some(now.saturating_add(delay));
    }

    /// Start the wait that `TIME-WAIT` and `FIN-WAIT-2` end with.
    pub(crate) fn start_linger(&mut self, now: Millis) {
        let wait = match self.state {
            State::TimeWait => TIME_WAIT,
            State::FinWait2 => FIN_WAIT_2_TIMEOUT,
            _ => return,
        };
        self.linger_at = Some(now.saturating_add(wait));
    }
}
