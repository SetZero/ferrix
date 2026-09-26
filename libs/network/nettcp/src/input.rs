//! What a connection does with a segment that arrived.
//!
//! RFC 9293 section 3.10.7, in its order: the acceptability check, then reset,
//! then a connection request inside an open connection, then the
//! acknowledgment, then the data, then the peer's close. Doing them out of
//! order is how an implementation ends up accepting a reset it should have
//! ignored, which is the classic way to let a stranger close somebody's
//! connection.

use ferrix_netwire::tcp::{Flags, Header, Segment};

use crate::conn::{Connection, Progress};
use crate::rtt::Millis;
use crate::seq::SeqNumber;
use crate::state::{Failure, State};

impl Connection {
    /// Take a segment that belongs to this connection.
    pub fn on_segment(&mut self, now: Millis, segment: &Segment<'_>) -> Progress {
        match self.state {
            State::Closed | State::Listen => Progress::default(),
            State::SynSent => self.in_syn_sent(now, segment),
            _ => self.synchronised(now, segment),
        }
    }

    /// A segment answering this end's connection request.
    fn in_syn_sent(&mut self, now: Millis, segment: &Segment<'_>) -> Progress {
        let header = &segment.header;
        let acknowledges = header.flags.contains(Flags::ACK);
        if acknowledges && !self.acknowledges_our_syn(header) {
            if !header.flags.contains(Flags::RST) {
                self.reset_pending = true;
            }
            return Progress::default();
        }
        if header.flags.contains(Flags::RST) {
            if acknowledges {
                self.fail(Failure::Refused);
                return Progress {
                    closed: true,
                    readable: true,
                    writable: true,
                };
            }
            return Progress::default();
        }
        if !header.flags.contains(Flags::SYN) {
            return Progress::default();
        }
        self.take_peer_syn(header);
        if acknowledges {
            self.open(now, SeqNumber(header.acknowledgment))
        } else {
            // A simultaneous open: both ends sent a request and neither has
            // answered. This end answers, and waits for the answer to its own.
            self.state = State::SynReceived;
            self.ack_immediately = true;
            Progress::default()
        }
    }

    /// Whether an acknowledgment names a sequence number this end has sent.
    fn acknowledges_our_syn(&self, header: &Header) -> bool {
        let ack = SeqNumber(header.acknowledgment);
        ack.follows(self.iss) && ack.precedes_or_equals(self.snd_nxt)
    }

    /// Record what the peer's connection request said.
    ///
    /// The window on a `SYN` is never scaled, whatever scale the same segment
    /// asks for: RFC 7323 section 2.2 makes the option take effect only from
    /// the segment after the handshake.
    fn take_peer_syn(&mut self, header: &Header) {
        self.irs = SeqNumber(header.sequence);
        self.rcv_nxt = self.irs.advance(1);
        self.snd_wnd = u32::from(header.window);
        self.snd_wl1 = SeqNumber(header.sequence);
        self.snd_wl2 = SeqNumber(header.acknowledgment);
        self.adopt_options(
            header.options.mss,
            header.options.window_scale,
            header.options.sack_permitted,
        );
    }

    /// The handshake finished: the connection is open.
    fn open(&mut self, now: Millis, ack: SeqNumber) -> Progress {
        self.snd_una = ack;
        self.state = State::Established;
        self.ack_immediately = true;
        self.measure_round_trip(now, ack);
        self.retransmit_at = None;
        Progress {
            writable: true,
            ..Progress::default()
        }
    }

    /// A segment for a connection past the request.
    fn synchronised(&mut self, now: Millis, segment: &Segment<'_>) -> Progress {
        if !self.is_acceptable(segment) {
            if !segment.header.flags.contains(Flags::RST) {
                self.ack_immediately = true;
            }
            if self.state == State::TimeWait {
                // A `FIN` sent again because this end's answer was lost. The
                // answer goes out again and the wait starts over, which is
                // what the wait is for.
                self.start_linger(now);
            }
            return Progress::default();
        }
        if segment.header.flags.contains(Flags::RST) {
            return self.take_reset();
        }
        if segment.header.flags.contains(Flags::SYN) {
            // RFC 5961 section 4: answer with an acknowledgment naming what is
            // really expected rather than resetting, so a forged request
            // cannot tear a connection down.
            self.ack_immediately = true;
            return Progress::default();
        }
        if !segment.header.flags.contains(Flags::ACK) {
            return Progress::default();
        }
        let mut progress = self.take_ack(now, &segment.header);
        if self.state == State::Closed {
            return progress;
        }
        progress = progress.or(self.take_data(now, segment));
        progress = progress.or(self.take_fin(now, segment));
        progress
    }

    /// RFC 9293's four-way acceptability test.
    fn is_acceptable(&self, segment: &Segment<'_>) -> bool {
        let sequence = SeqNumber(segment.header.sequence);
        let length = segment.header.sequence_len(segment.payload.len()) as u32;
        let window = self.receive_window();
        let end = self.rcv_nxt.advance(window);
        match (length, window) {
            (0, 0) => sequence == self.rcv_nxt,
            (0, _) => sequence.is_within(self.rcv_nxt, end),
            (_, 0) => false,
            (_, _) => {
                let last = sequence.advance(length - 1);
                sequence.is_within(self.rcv_nxt, end) || last.is_within(self.rcv_nxt, end)
            }
        }
    }

    /// The receive window in bytes, which is what the peer was told, scaled
    /// back up.
    pub(crate) fn receive_window(&self) -> u32 {
        u32::from(self.advertised_window()) << self.rcv_wscale
    }

    /// The peer reset the connection.
    fn take_reset(&mut self) -> Progress {
        let failure = if self.state == State::SynReceived {
            Failure::Refused
        } else {
            Failure::Reset
        };
        self.fail(failure);
        Progress {
            readable: true,
            writable: true,
            closed: true,
        }
    }

    /// The acknowledgment: what it frees, what it says about the window, and
    /// which state it moves the connection to.
    fn take_ack(&mut self, now: Millis, header: &Header) -> Progress {
        let ack = SeqNumber(header.acknowledgment);
        // Any acknowledgment at all is the peer saying it is there. A window
        // probe that is answered with a window of zero is a working connection
        // with a program that has not read yet, and must not be counted
        // towards giving up; only a probe nobody answers is.
        self.probes = 0;
        if self.state == State::SynReceived {
            if !self.acknowledges_our_syn(header) {
                self.reset_pending = true;
                self.fail(Failure::Protocol);
                return Progress {
                    closed: true,
                    ..Progress::default()
                };
            }
            self.state = State::Established;
            self.measure_round_trip(now, ack);
        }
        if ack.follows(self.snd_max) {
            // An acknowledgment of something never sent. Say what is really
            // expected and drop the rest of the segment.
            self.ack_immediately = true;
            return Progress::default();
        }
        let mut progress = self.advance_una(now, header, ack);
        self.update_send_window(header, ack);
        progress = progress.or(self.after_ack(now));
        progress
    }

    /// Move `SND.UNA` forward, or count a duplicate acknowledgment.
    fn advance_una(&mut self, now: Millis, header: &Header, ack: SeqNumber) -> Progress {
        if !ack.follows(self.snd_una) {
            self.count_duplicate(header, ack);
            return Progress::default();
        }
        let previous = self.snd_una;
        let data = self.acknowledged_data(previous, ack);
        self.snd_una = ack;
        self.send.discard(data as usize);
        self.measure_round_trip(now, ack);
        if self.congestion.recovered(ack) {
            self.congestion.leave_recovery();
        }
        self.congestion.on_ack(data);
        self.rtt.clear_backoff();
        // RFC 6298 rule 5.3: an acknowledgment of new data restarts the timer,
        // it does not leave the old deadline standing. Leaving it standing is
        // a timer that expires on every segment of a long transfer, backs off
        // until the retransmission limit, and declares a working connection
        // dead.
        self.retransmit_at = None;
        self.arm_retransmit(now);
        Progress {
            writable: self.state.can_send(),
            ..Progress::default()
        }
    }

    /// How much of what `ack` freed was data from the send queue, rather than
    /// the sequence number a `SYN` or a `FIN` takes.
    fn acknowledged_data(&self, previous: SeqNumber, ack: SeqNumber) -> u32 {
        let total = ack.distance_from(previous);
        let mut control = 0;
        if previous == self.iss {
            control += 1;
        }
        if let Some(fin) = self.fin_seq
            && fin.follows_or_equals(previous)
            && fin.precedes(ack)
        {
            control += 1;
        }
        total.saturating_sub(control)
    }

    /// An acknowledgment that acknowledged nothing new.
    ///
    /// Only a segment with no data, no flags of its own and no window change
    /// counts: RFC 5681 section 2 is precise about it, because a window update
    /// that happens to repeat an acknowledgment is not evidence of loss.
    fn count_duplicate(&mut self, header: &Header, ack: SeqNumber) {
        let window = u32::from(header.window) << self.snd_wscale;
        let bare = ack == self.snd_una
            && window == self.snd_wnd
            && !header.flags.contains(Flags::SYN)
            && !header.flags.contains(Flags::FIN);
        if !bare || self.snd_nxt == self.snd_una {
            return;
        }
        if self.congestion.on_duplicate_ack() {
            let in_flight = self.in_flight();
            let _ = self.congestion.enter_recovery(in_flight, self.snd_max);
            // Fast retransmit: send the segment the peer is missing now rather
            // than when the timer fires.
            self.snd_nxt = self.snd_una;
        }
    }

    /// RFC 9293's `SND.WL1`/`SND.WL2` test, which keeps an old segment from
    /// shrinking a window a newer one opened.
    fn update_send_window(&mut self, header: &Header, ack: SeqNumber) {
        let sequence = SeqNumber(header.sequence);
        let newer = self.snd_wl1.precedes(sequence)
            || (self.snd_wl1 == sequence && self.snd_wl2.precedes_or_equals(ack));
        if !newer {
            return;
        }
        self.snd_wnd = u32::from(header.window) << self.snd_wscale;
        self.snd_wl1 = sequence;
        self.snd_wl2 = ack;
        if self.snd_wnd > 0 {
            self.persist_at = None;
            self.probe_pending = false;
            self.probes = 0;
        }
    }

    /// The state changes an acknowledgment of this end's `FIN` causes.
    fn after_ack(&mut self, now: Millis) -> Progress {
        if !self.fin_acknowledged() {
            return Progress::default();
        }
        match self.state {
            State::FinWait1 => {
                self.state = State::FinWait2;
                self.start_linger(now);
                Progress::default()
            }
            State::Closing => {
                self.enter_time_wait();
                self.start_linger(now);
                Progress {
                    closed: true,
                    ..Progress::default()
                }
            }
            State::LastAck => {
                self.state = State::Closed;
                self.clear_timers();
                Progress {
                    closed: true,
                    readable: true,
                    writable: true,
                }
            }
            _ => Progress::default(),
        }
    }

    /// Whether this end's `FIN` has been acknowledged.
    pub(crate) fn fin_acknowledged(&self) -> bool {
        self.fin_seq.is_some_and(|fin| self.snd_una.follows(fin))
    }

    /// Enter `TIME-WAIT`, where the connection waits out anything still in the
    /// network that carries its numbers.
    pub(crate) fn enter_time_wait(&mut self) {
        self.state = State::TimeWait;
        self.retransmit_at = None;
        self.persist_at = None;
        self.send.clear();
    }

    /// Time a segment whose acknowledgment has arrived.
    ///
    /// Karn's algorithm: a segment that was retransmitted is not timed, which
    /// [`Connection::arm_retransmit`] enforces by clearing the timing on every
    /// retransmission.
    fn measure_round_trip(&mut self, now: Millis, ack: SeqNumber) {
        let Some((sent_at, covers)) = self.timed else {
            return;
        };
        if !ack.follows_or_equals(covers) {
            return;
        }
        self.timed = None;
        self.rtt.measure(now.saturating_sub(sent_at));
    }
}
