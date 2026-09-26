//! Two connections and a wire between them, both under the test's control.
//!
//! Every segment goes out through `ferrix_netwire`'s emit and comes back in
//! through its parse, checksum and all, so a test exercises the same path a
//! packet takes -- and a header this crate builds that the wire format refuses
//! fails a test here rather than on a network.
//!
//! The clock never runs. It moves when [`Link::settle`] finds both ends idle
//! and a timer armed, which is what makes a retransmission test finish in
//! microseconds rather than in the seconds it describes.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::checksum::Pseudo;
use ferrix_netwire::tcp::Header;

use crate::conn::{Config, Connection, Request};
use crate::rtt::Millis;
use crate::seq::SeqNumber;

/// The address the connecting end has.
pub(crate) const CLIENT: [u8; 4] = [10, 0, 0, 1];

/// The address the listening end has.
pub(crate) const SERVER: [u8; 4] = [10, 0, 0, 2];

/// The port the listening end is on.
pub(crate) const SERVER_PORT: u16 = 80;

/// The port the connecting end chose.
pub(crate) const CLIENT_PORT: u16 = 40_000;

/// The most segments one `settle` will carry before the test is declared
/// stuck.
const BUDGET: usize = 4_000;

/// How many times one `settle` will move the clock.
const TICKS_PER_SETTLE: usize = 64;

/// A segment on the wire.
struct Packet {
    /// Whether it is travelling towards the server.
    inbound: bool,
    /// The bytes as they would appear after the IP header.
    bytes: Vec<u8>,
}

/// Which end a test is talking about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum End {
    /// The end that connected.
    Client,
    /// The end that accepted.
    Server,
}

/// Two connections, the wire between them, and the clock.
pub(crate) struct Link {
    /// The connecting end.
    pub(crate) client: Connection,
    /// The listening end, once it has answered.
    pub(crate) server: Option<Connection>,
    /// The clock.
    pub(crate) now: Millis,
    /// Segments not yet delivered.
    wire: Vec<Packet>,
    /// How many segments have been put on the wire.
    pub(crate) sent: usize,
    /// The ordinals of the segments to throw away.
    pub(crate) lose: Vec<usize>,
    /// Whether the server end has been made yet.
    listening: bool,
    /// What the server is built with once it answers.
    server_config: Config,
    /// The first sequence number the server will choose.
    server_iss: SeqNumber,
}

impl Link {
    /// A client that has just called connect, and a listener waiting.
    pub(crate) fn new(client: Config, server: Config) -> Link {
        Link {
            client: Connection::connect(client, CLIENT_PORT, SERVER_PORT, SeqNumber(1_000)),
            server: None,
            now: 0,
            wire: Vec::new(),
            sent: 0,
            lose: Vec::new(),
            listening: true,
            server_config: server,
            server_iss: SeqNumber(9_000_000),
        }
    }

    /// The default pair.
    pub(crate) fn pair() -> Link {
        Link::new(Config::default(), Config::default())
    }

    /// The end named, which the server end must exist for.
    pub(crate) fn end(&mut self, which: End) -> &mut Connection {
        match which {
            End::Client => &mut self.client,
            End::Server => self
                .server
                .as_mut()
                .unwrap_or_else(|| panic!("the server has not answered yet")),
        }
    }

    /// Carry segments until both ends are idle, moving the clock forward when
    /// they are idle and a timer is armed.
    pub(crate) fn settle(&mut self) {
        let mut ticks = 0;
        for _ in 0..BUDGET {
            if self.collect() {
                continue;
            }
            if self.deliver() {
                continue;
            }
            // The clock is moved a bounded number of times, because a
            // connection stalled on a window the test has not opened yet is
            // not idle: it probes for ever, and a settle that followed it
            // would never come back. The caller reads and settles again.
            ticks += 1;
            if ticks > TICKS_PER_SETTLE || !self.tick() {
                return;
            }
        }
        panic!("the connection never settled");
    }

    /// Carry segments until both ends are idle, without letting the clock
    /// move: what a test wants when it is about to inspect a timer.
    pub(crate) fn exchange(&mut self) {
        for _ in 0..BUDGET {
            if self.collect() {
                continue;
            }
            if !self.deliver() {
                return;
            }
        }
        panic!("the exchange never ended");
    }

    /// Take one segment from whichever end has one.
    fn collect(&mut self) -> bool {
        let mut payload = [0_u8; 2048];
        if let Some(transmit) = self.client.poll_transmit(self.now, &mut payload) {
            let body = payload.get(..transmit.payload_len).unwrap_or_default();
            self.put(true, &transmit.header, body);
            return true;
        }
        let Some(server) = self.server.as_mut() else {
            return false;
        };
        if let Some(transmit) = server.poll_transmit(self.now, &mut payload) {
            let body = payload.get(..transmit.payload_len).unwrap_or_default();
            self.put(false, &transmit.header, body);
            return true;
        }
        false
    }

    /// Put a segment on the wire, unless this one is scheduled to be lost.
    fn put(&mut self, inbound: bool, header: &Header, payload: &[u8]) {
        let ordinal = self.sent;
        self.sent += 1;
        if self.lose.contains(&ordinal) {
            return;
        }
        let (source, destination) = if inbound {
            (CLIENT, SERVER)
        } else {
            (SERVER, CLIENT)
        };
        let pseudo = Pseudo::V4 {
            source,
            destination,
        };
        let mut bytes = vec![0_u8; header.options.len() + 20 + payload.len() + 4];
        let written = header
            .emit(payload, pseudo, &mut bytes)
            .expect("the header this crate built must be one netwire can write");
        bytes.truncate(written);
        self.wire.push(Packet { inbound, bytes });
    }

    /// Hand the oldest segment to the end it is addressed to.
    fn deliver(&mut self) -> bool {
        if self.wire.is_empty() {
            return false;
        }
        let packet = self.wire.remove(0);
        let (source, destination) = if packet.inbound {
            (CLIENT, SERVER)
        } else {
            (SERVER, CLIENT)
        };
        let pseudo = Pseudo::V4 {
            source,
            destination,
        };
        let segment =
            Header::parse(&packet.bytes, pseudo).expect("a segment this crate wrote must parse");
        if packet.inbound {
            self.accept_or_feed(&segment);
        } else {
            let _ = self.client.on_segment(self.now, &segment);
        }
        true
    }

    /// The first segment to the server makes the server.
    fn accept_or_feed(&mut self, segment: &ferrix_netwire::tcp::Segment<'_>) {
        if let Some(server) = self.server.as_mut() {
            let _ = server.on_segment(self.now, segment);
            return;
        }
        if !self.listening
            || !segment
                .header
                .flags
                .contains(ferrix_netwire::tcp::Flags::SYN)
        {
            return;
        }
        let request = Request {
            sequence: SeqNumber(segment.header.sequence),
            window: segment.header.window,
            mss: segment.header.options.mss,
            window_scale: segment.header.options.window_scale,
            selective_ack: segment.header.options.sack_permitted,
        };
        self.server = Some(Connection::accept(
            self.server_config,
            SERVER_PORT,
            CLIENT_PORT,
            self.server_iss,
            &request,
        ));
    }

    /// Move the clock to the earliest armed timer, other than the two that
    /// only wait a connection out. Answers whether there was one.
    ///
    /// `TIME-WAIT` and `FIN-WAIT-2` are deliberately skipped: a test that ran
    /// them would spend a settle finishing every connection it opened, and
    /// could never look at the state a close left behind. The test that cares
    /// about them moves the clock itself with [`Link::advance`].
    fn tick(&mut self) -> bool {
        let next = [
            Link::working_timer(&self.client),
            self.server.as_ref().and_then(Link::working_timer),
        ]
        .into_iter()
        .flatten()
        .min();
        let Some(at) = next else {
            return false;
        };
        self.now = at.max(self.now);
        let _ = self.client.on_timer(self.now);
        if let Some(server) = self.server.as_mut() {
            let _ = server.on_timer(self.now);
        }
        true
    }

    /// The earliest timer that is not one of the two lingering ones.
    fn working_timer(connection: &Connection) -> Option<Millis> {
        [
            connection.retransmit_at,
            connection.delayed_ack_at,
            connection.persist_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Move the clock by hand.
    pub(crate) fn advance(&mut self, millis: Millis) {
        self.now += millis;
        let _ = self.client.on_timer(self.now);
        if let Some(server) = self.server.as_mut() {
            let _ = server.on_timer(self.now);
        }
    }

    /// Read everything one end has, as a vector.
    pub(crate) fn drain(&mut self, which: End) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let taken = self.end(which).read(&mut chunk);
            if taken == 0 {
                return out;
            }
            out.extend(chunk.iter().take(taken).copied());
        }
    }

    /// Write everything, settling as often as it takes to make room.
    ///
    /// The other end is read as the writing goes on, because a send buffer
    /// that nobody drains fills and stays full: a transfer larger than the two
    /// buffers only finishes if somebody is reading.
    pub(crate) fn transfer(&mut self, from: End, data: &[u8]) -> Vec<u8> {
        let to = match from {
            End::Client => End::Server,
            End::Server => End::Client,
        };
        let mut offset = 0;
        let mut received = Vec::new();
        for _ in 0..BUDGET {
            let rest = data.get(offset..).unwrap_or_default();
            if !rest.is_empty() {
                offset += self.end(from).write(rest);
            }
            self.settle();
            received.extend(self.drain(to));
            self.settle();
            if offset == data.len() && received.len() >= data.len() {
                return received;
            }
        }
        panic!(
            "the transfer never finished: {offset} written, {} read; \
             client {:?} {:?} queued {} in flight {} wnd {} cwnd {} rtx {:?}; \
             server {:?} queued {}",
            received.len(),
            self.client.state(),
            self.client.failure(),
            self.client.send_queued(),
            self.client.in_flight(),
            self.client.snd_wnd,
            self.client.congestion_window(),
            self.client.retransmit_at,
            self.server.as_ref().map(Connection::state),
            self.server.as_ref().map_or(0, Connection::receive_queued),
        );
    }
}
