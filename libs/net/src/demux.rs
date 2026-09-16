//! Which socket a packet belongs to, and what an ICMP message says about one.
//!
//! The matching rules are Linux's, and the order matters: a socket connected
//! to the sender beats one merely bound to the port, and a socket bound to a
//! particular address beats one bound to all of them. Getting that order wrong
//! is how a connected socket stops receiving the moment somebody opens a
//! wildcard one.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::{icmpv4, icmpv6, ipv4, ipv6, tcp, udp};

use crate::addr::{Endpoint, IpAddress, Ipv4, Ipv6};
use crate::socket::{Datagram, Error, Family, Socket};
use crate::stack::{Millis, Stack};

/// How much of the packet that caused an error is quoted back: the header and
/// the first eight bytes, which is what RFC 792 asks for and what is needed to
/// find the socket.
const QUOTE_BYTES: usize = 8;

/// Where a packet arrived, and how: its interface, source, destination and
/// hop limit.
pub(crate) type Arrived = (u32, IpAddress, IpAddress, u8);

impl Stack {
    /// The datagram socket a packet to `local` from `remote` belongs to.
    pub(crate) fn find_datagram_socket(
        &self,
        local: Endpoint,
        remote: Endpoint,
        icmp: bool,
    ) -> Option<u32> {
        let mut best: Option<(u8, u32)> = None;
        for (id, socket) in &self.sockets {
            let candidate = match socket {
                Socket::Udp(datagram) if !icmp => datagram,
                Socket::Icmp(datagram) if icmp => datagram,
                _ => continue,
            };
            if candidate.local.port != local.port {
                continue;
            }
            if !accepts_family(candidate.family, candidate.options.v6_only, local.address) {
                continue;
            }
            if let Some(connected) = candidate.remote
                && (connected.address != remote.address || connected.port != remote.port)
            {
                continue;
            }
            let bound = candidate.local.address;
            if !bound.is_unspecified() && bound != local.address {
                continue;
            }
            // Most specific first: connected beats bound, bound beats
            // wildcard.
            let score =
                u8::from(candidate.remote.is_some()) * 2 + u8::from(!bound.is_unspecified());
            if best.is_none_or(|(held, _)| score > held) {
                best = Some((score, *id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Deliver an ICMPv4 echo reply to the socket that sent the request.
    ///
    /// `arrived` is the interface, the source, the destination and the hop
    /// limit of the packet that carried it.
    pub(crate) fn deliver_echo(
        &mut self,
        arrived: Arrived,
        whole: &[u8],
        message: &icmpv4::Message<'_>,
    ) {
        let Some((identifier, _)) = message.header.echo_fields() else {
            return;
        };
        self.deliver_echo_to(arrived, whole, identifier);
    }

    /// Deliver an ICMPv6 echo reply the same way.
    pub(crate) fn deliver_echo_v6(
        &mut self,
        arrived: Arrived,
        whole: &[u8],
        message: &icmpv6::Message<'_>,
    ) {
        let identifier = message
            .body
            .first_chunk::<2>()
            .map(|bytes| u16::from_be_bytes(*bytes));
        let Some(identifier) = identifier else {
            return;
        };
        self.deliver_echo_to(arrived, whole, identifier);
    }

    /// Put an echo reply in the queue of the socket whose port is its
    /// identifier, which is how Linux's unprivileged ping socket works.
    fn deliver_echo_to(&mut self, arrived: Arrived, whole: &[u8], identifier: u16) {
        let (interface, source, destination, hop_limit) = arrived;
        let local = Endpoint::new(destination, identifier);
        let remote = Endpoint::new(source, identifier);
        let Some(id) = self.find_datagram_socket(local, remote, true) else {
            self.counters.no_socket += 1;
            return;
        };
        if let Some(Socket::Icmp(socket)) = self.sockets.get_mut(&id) {
            let kept = socket.deliver(Datagram {
                remote,
                local: destination,
                interface,
                hop_limit,
                payload: whole.to_vec(),
            });
            if kept {
                self.counters.delivered += 1;
            }
        }
    }

    /// Tell a socket that a packet it sent could not be delivered.
    ///
    /// The message quotes the packet that caused it, so the socket is found
    /// the same way the packet was routed, with the addresses and ports the
    /// other way round.
    pub(crate) fn report_v4_error(&mut self, quoted: &[u8], code: u8) {
        let Ok(packet) = ipv4::Header::parse(quoted) else {
            return;
        };
        let source = IpAddress::V4(Ipv4::new(packet.header.source));
        let destination = IpAddress::V4(Ipv4::new(packet.header.destination));
        self.report_error(
            source,
            destination,
            packet.header.protocol,
            packet.payload,
            unreachable_error(code),
        );
    }

    /// The same, for an ICMPv6 message.
    pub(crate) fn report_v6_error(&mut self, body: &[u8], code: u8) {
        // Past the four unused bytes RFC 4443 puts after the code.
        let Some(quoted) = body.get(4..) else {
            return;
        };
        let Ok(packet) = ipv6::Header::parse(quoted) else {
            return;
        };
        let source = IpAddress::V6(Ipv6::new(packet.header.source));
        let destination = IpAddress::V6(Ipv6::new(packet.header.destination));
        let Ok(upper) = ipv6::upper_layer(packet.header.next_header, packet.payload) else {
            return;
        };
        self.report_error(
            source,
            destination,
            upper.protocol,
            upper.bytes,
            unreachable_error_v6(code),
        );
    }

    /// Find the socket the quoted packet came from and give it the error.
    fn report_error(
        &mut self,
        source: IpAddress,
        destination: IpAddress,
        protocol: u8,
        transport: &[u8],
        error: Error,
    ) {
        let Some(ports) = transport.first_chunk::<4>() else {
            return;
        };
        self.counters.errors_reported += 1;
        let from = u16::from_be_bytes([ports[0], ports[1]]);
        let to = u16::from_be_bytes([ports[2], ports[3]]);
        // The quoted packet is one this host sent, so its source is our local
        // endpoint and its destination the remote one.
        let local = Endpoint::new(source, from);
        let remote = Endpoint::new(destination, to);
        match protocol {
            udp::PROTOCOL => {
                if let Some(id) = self.find_datagram_socket(local, remote, false)
                    && let Some(Socket::Udp(socket)) = self.sockets.get_mut(&id)
                {
                    socket.error = Some(error);
                }
            }
            tcp::PROTOCOL => {
                let Some(id) = self.find_stream_socket(local, remote) else {
                    return;
                };
                if let Some(Socket::Stream(stream)) = self.sockets.get_mut(&id) {
                    // A hard error on a connection that has not been made ends
                    // it; on an open one it is remembered and not acted on,
                    // because a single message must not tear down a working
                    // connection (RFC 1122 section 4.2.3.9).
                    if stream.connection.state() == ferrix_nettcp::State::SynSent {
                        stream.connection.abort();
                        stream.error = Some(error);
                    }
                }
            }
            _ => {}
        }
    }

    /// Answer a datagram nobody was listening for with an unreachable.
    pub(crate) fn send_unreachable(
        &mut self,
        source: IpAddress,
        destination: IpAddress,
        transport: &[u8],
        now: Millis,
    ) {
        let quote = transport
            .get(..QUOTE_BYTES.min(transport.len()))
            .unwrap_or(&[]);
        self.counters.unreachable_sent += 1;
        match (source, destination) {
            (IpAddress::V4(from), IpAddress::V4(to)) => {
                self.unreachable_v4(from, to, quote, now);
            }
            (IpAddress::V6(from), IpAddress::V6(to)) => {
                self.unreachable_v6(from, to, quote, now);
            }
            _ => {}
        }
    }

    /// An ICMPv4 port unreachable, quoting a rebuilt header and eight bytes.
    fn unreachable_v4(&mut self, to: Ipv4, from: Ipv4, quote: &[u8], now: Millis) {
        let header = ipv4::Header {
            dscp: 0,
            ecn: 0,
            identification: 0,
            dont_fragment: false,
            more_fragments: false,
            fragment_offset: 0,
            ttl: self.config.hop_limit,
            protocol: udp::PROTOCOL,
            source: to.octets(),
            destination: from.octets(),
        };
        let mut quoted = vec![0_u8; ipv4::MIN_HEADER_LEN + quote.len()];
        let Ok(written) = header.emit(&[], quote.len(), &mut quoted) else {
            return;
        };
        let Some(body) = quoted.get_mut(written..) else {
            return;
        };
        body.copy_from_slice(quote);
        let message = icmpv4::Header {
            kind: icmpv4::kind::DESTINATION_UNREACHABLE,
            code: 3,
            rest: [0; 4],
        };
        let mut packet = vec![0_u8; icmpv4::HEADER_LEN + quoted.len()];
        if message.emit(&quoted, &mut packet).is_err() {
            return;
        }
        let _ = self.send_ip(
            IpAddress::V4(from),
            IpAddress::V4(to),
            ipv4::protocol::ICMP,
            &packet,
            self.config.hop_limit,
            None,
            now,
        );
    }

    /// An ICMPv6 port unreachable.
    fn unreachable_v6(&mut self, to: Ipv6, from: Ipv6, quote: &[u8], now: Millis) {
        let header = ipv6::Header {
            traffic_class: 0,
            flow_label: 0,
            next_header: udp::PROTOCOL,
            hop_limit: self.config.hop_limit,
            source: to.octets(),
            destination: from.octets(),
        };
        let mut quoted = vec![0_u8; ipv6::HEADER_LEN + quote.len()];
        let Ok(written) = header.emit(quote.len(), &mut quoted) else {
            return;
        };
        let Some(body) = quoted.get_mut(written..) else {
            return;
        };
        body.copy_from_slice(quote);
        // RFC 4443: the four bytes after the code are unused and are part of
        // the body as `libs/netwire` counts it, whose header is the type, the
        // code and the checksum. Leaving them out makes a message whose
        // checksum is taken over a different length than the receiver takes
        // it over, which is a packet nobody can read.
        let mut body = Vec::with_capacity(4 + quoted.len());
        body.extend_from_slice(&[0_u8; 4]);
        body.extend_from_slice(&quoted);
        let mut packet = vec![0_u8; icmpv6::HEADER_LEN + body.len()];
        let message = icmpv6::Message {
            kind: icmpv6::kind::DESTINATION_UNREACHABLE,
            code: 4,
            body: &body,
        };
        if message
            .emit(from.octets(), to.octets(), &mut packet)
            .is_err()
        {
            return;
        }
        let _ = self.send_ip(
            IpAddress::V6(from),
            IpAddress::V6(to),
            icmpv6::PROTOCOL,
            &packet,
            self.config.hop_limit,
            None,
            now,
        );
    }
}

/// Whether a socket of this family accepts a packet to that address.
///
/// An `AF_INET6` socket that is not `IPV6_V6ONLY` accepts IPv4 as well, which
/// is what lets one listener answer both families.
fn accepts_family(family: Family, v6_only: bool, address: IpAddress) -> bool {
    match (family, address) {
        (Family::V4, IpAddress::V4(_)) | (Family::V6, IpAddress::V6(_)) => true,
        (Family::V6, IpAddress::V4(_)) => !v6_only,
        (Family::V4, IpAddress::V6(_)) => false,
    }
}

/// What an ICMPv4 unreachable code means to a program.
const fn unreachable_error(code: u8) -> Error {
    match code {
        3 => Error::PortUnreachable,
        _ => Error::Unreachable,
    }
}

/// What an ICMPv6 unreachable code means to a program.
const fn unreachable_error_v6(code: u8) -> Error {
    match code {
        4 => Error::PortUnreachable,
        _ => Error::Unreachable,
    }
}
