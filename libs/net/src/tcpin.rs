//! Finding the connection a segment belongs to, and making one when it starts
//! a connection.
//!
//! A segment names four numbers, and exactly one connection can match all
//! four. If none does, a listener on the local port may take it -- but only if
//! it is a connection request. Anything else with nowhere to go is answered
//! with a reset, which is what tells the other end to stop retransmitting
//! rather than leaving it to time out.

use alloc::vec;

use ferrix_nettcp::{Connection, Request, SeqNumber, State};
use ferrix_netwire::tcp::{self, Flags};

use crate::addr::{Endpoint, IpAddress};
use crate::input::pseudo_of;
use crate::socket::{Error, Socket, SocketId, StreamSocket};
use crate::stack::{Millis, Stack};

impl Stack {
    /// Take a TCP segment.
    pub(crate) fn tcp_input(
        &mut self,
        interface: u32,
        source: IpAddress,
        destination: IpAddress,
        payload: &[u8],
        now: Millis,
    ) {
        let pseudo = pseudo_of(source, destination);
        let Ok(segment) = tcp::Header::parse(payload, pseudo) else {
            self.counters.malformed += 1;
            return;
        };
        let remote = Endpoint::new(source, segment.header.source_port);
        let local = Endpoint::new(destination, segment.header.destination_port);
        let _ = interface;

        if let Some(id) = self.find_stream_socket(local, remote) {
            if let Some(Socket::Stream(stream)) = self.sockets.get_mut(&id) {
                let _ = stream.connection.on_segment(now, &segment);
                discard_if_shut(stream);
            }
            self.counters.delivered += 1;
            self.promote(SocketId(id));
            return;
        }

        if segment.header.flags.contains(Flags::SYN)
            && !segment.header.flags.contains(Flags::RST)
            && !segment.header.flags.contains(Flags::ACK)
            && let Some(listener) = self.find_listener(local)
        {
            self.open_from_request(listener, local, remote, &segment, now);
            return;
        }

        self.counters.no_socket += 1;
        if !segment.header.flags.contains(Flags::RST) {
            self.send_reset(local, remote, &segment, now);
        }
    }

    /// The connection those four numbers name.
    pub(crate) fn find_stream_socket(&self, local: Endpoint, remote: Endpoint) -> Option<u32> {
        self.sockets.iter().find_map(|(id, socket)| {
            let Socket::Stream(stream) = socket else {
                return None;
            };
            let matches = stream.local.port == local.port
                && stream.remote.port == remote.port
                && stream.remote.address == remote.address
                && (stream.local.address == local.address || stream.local.address.is_unspecified());
            matches.then_some(*id)
        })
    }

    /// The listener that would take a connection to `local`.
    fn find_listener(&self, local: Endpoint) -> Option<SocketId> {
        let mut best: Option<(u8, u32)> = None;
        for (id, socket) in &self.sockets {
            let Socket::Listen(listener) = socket else {
                continue;
            };
            if listener.backlog == 0 || listener.local.port != local.port {
                continue;
            }
            let bound = listener.local.address;
            let family_ok = match (listener.family, local.address) {
                (crate::socket::Family::V4, IpAddress::V4(_)) => true,
                (crate::socket::Family::V6, IpAddress::V6(_)) => true,
                (crate::socket::Family::V6, IpAddress::V4(_)) => !listener.options.v6_only,
                (crate::socket::Family::V4, IpAddress::V6(_)) => false,
            };
            if !family_ok {
                continue;
            }
            if !bound.is_unspecified() && bound != local.address {
                continue;
            }
            let score = u8::from(!bound.is_unspecified());
            if best.is_none_or(|(held, _)| score > held) {
                best = Some((score, *id));
            }
        }
        best.map(|(_, id)| SocketId(id))
    }

    /// Answer a connection request with a connection of its own.
    fn open_from_request(
        &mut self,
        listener: SocketId,
        local: Endpoint,
        remote: Endpoint,
        segment: &tcp::Segment<'_>,
        now: Millis,
    ) {
        let Some(Socket::Listen(waiting)) = self.sockets.get(&listener.0) else {
            return;
        };
        if waiting.ready.len() >= waiting.backlog {
            // The backlog is full. Dropping the request is what Linux does
            // without SYN cookies: the peer retransmits, and by then the
            // program may have accepted one.
            self.counters.no_socket += 1;
            return;
        }
        let family = waiting.family;
        let options = waiting.options;
        let request = Request {
            sequence: SeqNumber(segment.header.sequence),
            window: segment.header.window,
            mss: segment.header.options.mss,
            window_scale: segment.header.options.window_scale,
            selective_ack: segment.header.options.sack_permitted,
        };
        let iss = SeqNumber(self.random.next_u32());
        let connection =
            Connection::accept(self.config.tcp, local.port, remote.port, iss, &request);
        let id = self.install_stream(StreamSocket {
            family,
            local,
            remote,
            connection,
            options,
            error: None,
            read_shut: false,
            listener: Some(listener),
            closing: false,
        });
        if let Some(Socket::Listen(waiting)) = self.sockets.get_mut(&listener.0) {
            waiting.pending.push(id);
        }
        self.counters.accepted += 1;
        let _ = now;
    }

    /// Move a connection that finished its handshake onto its listener's
    /// queue, and take a failed one off.
    pub(crate) fn promote(&mut self, id: SocketId) {
        let Some(Socket::Stream(stream)) = self.sockets.get(&id.0) else {
            return;
        };
        let Some(listener) = stream.listener else {
            return;
        };
        let state = stream.connection.state();
        let ready = state.is_synchronised();
        let dead = state == State::Closed;
        if !ready && !dead {
            return;
        }
        if let Some(Socket::Listen(waiting)) = self.sockets.get_mut(&listener.0) {
            waiting.pending.retain(|held| *held != id);
            if ready && !dead {
                waiting.ready.push_back(id);
            }
        }
        if let Some(Socket::Stream(stream)) = self.sockets.get_mut(&id.0) {
            stream.listener = None;
        }
    }

    /// Put a stream socket in the table.
    fn install_stream(&mut self, stream: StreamSocket) -> SocketId {
        self.install_socket(Socket::Stream(alloc::boxed::Box::new(stream)))
    }

    /// Answer a segment that belongs to no connection.
    ///
    /// RFC 9293 section 3.10.7.1: a segment carrying an acknowledgment is
    /// answered with a reset at that acknowledgment; one that does not is
    /// answered at sequence zero, acknowledging everything it carried.
    fn send_reset(
        &mut self,
        local: Endpoint,
        remote: Endpoint,
        segment: &tcp::Segment<'_>,
        now: Millis,
    ) {
        let carried = segment.header.sequence_len(segment.payload.len()) as u32;
        let (sequence, acknowledgment, flags) = if segment.header.flags.contains(Flags::ACK) {
            (segment.header.acknowledgment, 0, Flags::RST)
        } else {
            (
                0,
                segment.header.sequence.wrapping_add(carried),
                Flags::RST.union(Flags::ACK),
            )
        };
        let header = tcp::Header {
            source_port: local.port,
            destination_port: remote.port,
            sequence,
            acknowledgment,
            flags,
            window: 0,
            urgent_pointer: 0,
            options: tcp::Options::default(),
        };
        let pseudo = pseudo_of(local.address, remote.address);
        let mut packet = vec![0_u8; tcp::MIN_HEADER_LEN];
        if header.emit(&[], pseudo, &mut packet).is_err() {
            return;
        }
        self.counters.resets_sent += 1;
        let _ = self.send_ip(
            local.address,
            remote.address,
            tcp::PROTOCOL,
            &packet,
            self.config.hop_limit,
            None,
            now,
        );
    }

    /// The error a failed connection reports.
    pub(crate) fn stream_error(connection: &Connection) -> Option<Error> {
        connection.failure().map(|failure| match failure {
            ferrix_nettcp::Failure::Refused => Error::Refused,
            ferrix_nettcp::Failure::TimedOut => Error::TimedOut,
            ferrix_nettcp::Failure::Reset | ferrix_nettcp::Failure::Protocol => Error::Reset,
        })
    }
}

/// Throw away what arrives on a connection whose reader has gone.
///
/// The bytes have to be taken out rather than left: leaving them shuts the
/// receive window, and a peer with a shut window stops sending and waits for a
/// program that is never going to read.
fn discard_if_shut(stream: &mut StreamSocket) {
    if !stream.read_shut {
        return;
    }
    let mut sink = [0_u8; 1024];
    while stream.connection.read(&mut sink) > 0 {}
}
