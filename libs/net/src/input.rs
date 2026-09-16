//! What arrives, and where it goes.
//!
//! One path up from an interface: the link header, then the network header,
//! then the transport, then a socket. Every step answers the question "is this
//! for us" before the one above it is asked, because a packet that is not for
//! this host must cost as little as possible -- a link a stranger shares
//! carries their traffic too.
//!
//! # Nothing here sends
//!
//! An answer -- an ARP reply, an echo reply, a reset -- is queued, never
//! written. That keeps the input path free of the routing and resolution the
//! output path does, and it means a flood of requests is bounded by the
//! egress queue rather than by how fast the input loop runs.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::checksum::Pseudo;
use ferrix_netwire::ethernet::{self, ethertype};
use ferrix_netwire::{arp, icmpv4, icmpv6, ipv4, ipv6, ndp, tcp, udp};

use crate::addr::{Endpoint, IpAddress, Ipv4, Ipv6};
use crate::iface::Medium;
use crate::reassembly::Key;
use crate::socket::{Datagram, Socket};
use crate::stack::{Millis, Stack};

impl Stack {
    /// Take a frame that arrived on an interface.
    pub fn receive(&mut self, interface: u32, frame: &[u8], now: Millis) {
        let Some(link) = self.interface_mut(interface) else {
            return;
        };
        link.counters.received += 1;
        link.counters.received_bytes += frame.len() as u64;
        let medium = link.medium;
        match medium {
            Medium::Ethernet => self.ethernet_input(interface, frame, now),
            Medium::Loopback => self.loopback_input(interface, frame, now),
        }
    }

    /// A loopback carries a bare IP packet: the version nibble says which.
    fn loopback_input(&mut self, interface: u32, packet: &[u8], now: Millis) {
        match packet.first().map(|byte| byte >> 4) {
            Some(4) => self.ipv4_input(interface, packet, now),
            Some(6) => self.ipv6_input(interface, packet, now),
            _ => self.count_malformed(interface),
        }
    }

    /// An Ethernet frame: for us or for the broadcast, and then by ethertype.
    fn ethernet_input(&mut self, interface: u32, frame: &[u8], now: Millis) {
        let Ok((header, payload)) = ethernet::Header::parse(frame) else {
            self.count_malformed(interface);
            return;
        };
        let ours = self.interface(interface).is_some_and(|link| {
            header.destination == link.hardware
                || header.destination == ethernet::BROADCAST
                || header.destination.first().is_some_and(|byte| byte & 1 == 1)
        });
        if !ours {
            self.counters.not_ours += 1;
            return;
        }
        match header.ethertype {
            ethertype::ARP => self.arp_input(interface, payload, header.source, now),
            ethertype::IPV4 => self.ipv4_input(interface, payload, now),
            ethertype::IPV6 => self.ipv6_input(interface, payload, now),
            _ => self.counters.not_ours += 1,
        }
    }

    /// An ARP packet: learn from it, and answer a request for one of our
    /// addresses.
    fn arp_input(&mut self, interface: u32, payload: &[u8], from: ethernet::Mac, now: Millis) {
        let Ok(packet) = arp::Packet::parse(payload) else {
            self.count_malformed(interface);
            return;
        };
        let sender = IpAddress::V4(Ipv4::new(packet.sender_ip));
        let target = IpAddress::V4(Ipv4::new(packet.target_ip));
        let ours = self
            .interface(interface)
            .is_some_and(|link| link.owns(target));
        // RFC 826: learn from any packet that names an address we have, and
        // only from those, so a stranger cannot fill the cache with entries
        // for hosts nobody is talking to.
        if ours && !sender.is_unspecified() {
            let waiting = self
                .neighbors
                .record(interface, sender, packet.sender_mac, now);
            self.flush(interface, sender, waiting, ethertype::IPV4, now);
        }
        if !ours || packet.operation != arp::Operation::Request {
            return;
        }
        let Some(link) = self.interface(interface) else {
            return;
        };
        let reply = packet.reply(link.hardware);
        let mut bytes = [0_u8; arp::PACKET_LEN];
        if reply.emit(&mut bytes).is_ok() {
            self.emit_frame(interface, from, ethertype::ARP, &bytes);
        }
    }

    /// An IPv4 packet: reassemble it if it is a fragment, then hand it up.
    fn ipv4_input(&mut self, interface: u32, bytes: &[u8], now: Millis) {
        let Ok(packet) = ipv4::Header::parse(bytes) else {
            self.count_malformed(interface);
            return;
        };
        let source = IpAddress::V4(Ipv4::new(packet.header.source));
        let destination = IpAddress::V4(Ipv4::new(packet.header.destination));
        let accepted = self
            .interface(interface)
            .is_some_and(|link| link.accepts(destination));
        if !accepted {
            self.counters.not_ours += 1;
            return;
        }
        if !packet.header.is_fragment() {
            let length = ipv4::MIN_HEADER_LEN + packet.options.len() + packet.payload.len();
            if let Some(whole) = bytes.get(..length) {
                self.raw_input_v4(interface, whole, packet.header.protocol);
            }
            self.transport_input(
                interface,
                source,
                destination,
                packet.header.protocol,
                packet.payload,
                now,
            );
            return;
        }
        self.counters.fragments += 1;
        let key = Key {
            source: Ipv4::new(packet.header.source),
            destination: Ipv4::new(packet.header.destination),
            identification: packet.header.identification,
            protocol: packet.header.protocol,
        };
        let offset = usize::from(packet.header.fragment_offset) * 8;
        let whole = self.reassembly.insert(
            key,
            offset,
            packet.header.more_fragments,
            packet.payload,
            now,
        );
        let Some(whole) = whole else {
            return;
        };
        self.counters.reassembled += 1;
        self.raw_input_reassembled(interface, &packet.header, &whole);
        self.transport_input(
            interface,
            source,
            destination,
            packet.header.protocol,
            &whole,
            now,
        );
    }

    /// An IPv6 packet: walk the extension headers and hand the rest up.
    fn ipv6_input(&mut self, interface: u32, bytes: &[u8], now: Millis) {
        let Ok(packet) = ipv6::Header::parse(bytes) else {
            self.count_malformed(interface);
            return;
        };
        let source = IpAddress::V6(Ipv6::new(packet.header.source));
        let destination = IpAddress::V6(Ipv6::new(packet.header.destination));
        let accepted = self.interface(interface).is_some_and(|link| {
            link.accepts(destination)
                || matches!(destination, IpAddress::V6(address) if is_ours_solicited(link, address))
        });
        if !accepted {
            self.counters.not_ours += 1;
            return;
        }
        let Ok(upper) = ipv6::upper_layer(packet.header.next_header, packet.payload) else {
            self.count_malformed(interface);
            return;
        };
        if upper.fragment.is_some() {
            // IPv6 fragments are rare and are not reassembled: the sender is
            // told by the path MTU what will fit, and a peer that fragments
            // anyway is answered with nothing rather than with a hole in this
            // host's memory.
            self.counters.malformed += 1;
            return;
        }
        self.transport_input(
            interface,
            source,
            destination,
            upper.protocol,
            upper.bytes,
            now,
        );
    }

    /// Give a copy of an IPv4 packet that reached this host to every raw
    /// socket that asks for it, header and all, as `raw_v4_input` does.
    ///
    /// A raw socket takes a packet when its protocol matches, when it is
    /// bound to no address or to the one the packet came to, when it is
    /// connected to nothing or to the one the packet came from, and when it
    /// is pinned to no interface or to this one. `IPPROTO_RAW` takes nothing,
    /// and a raw ICMP socket skips the types its `ICMP_FILTER` names.
    fn raw_input_v4(&mut self, interface: u32, packet: &[u8], protocol: u8) {
        let address_at = |at: usize| {
            packet
                .get(at..at + 4)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(|octets| IpAddress::V4(Ipv4::new(octets)))
        };
        let (Some(source), Some(destination)) = (address_at(12), address_at(16)) else {
            return;
        };
        let header_len = usize::from(packet.first().map_or(0, |byte| byte & 0x0F)) * 4;
        let icmp_type = packet.get(header_len).copied();
        let mut delivered = false;
        for socket in self.sockets.values_mut() {
            let Socket::Raw(raw) = socket else {
                continue;
            };
            let datagram = &raw.datagram;
            let wanted = raw.protocol == protocol
                && raw.protocol != crate::socket::RawSocket::IPPROTO_RAW
                && matches!(datagram.family, crate::socket::Family::V4)
                && (datagram.local.address.is_unspecified()
                    || datagram.local.address == destination)
                && datagram
                    .remote
                    .is_none_or(|remote| remote.address == source)
                && datagram
                    .options
                    .device
                    .is_none_or(|device| device == interface);
            if !wanted {
                continue;
            }
            if protocol == ipv4::protocol::ICMP
                && icmp_type.is_some_and(|kind| kind < 32 && raw.icmp_filter & (1 << kind) != 0)
            {
                continue;
            }
            delivered |= raw.datagram.deliver(Datagram {
                remote: Endpoint::new(source, 0),
                local: destination,
                interface,
                payload: packet.to_vec(),
            });
        }
        if delivered {
            self.counters.delivered += 1;
        }
    }

    /// The same for a datagram put back together from fragments: the header
    /// is the last fragment's with the fragment fields cleared and the length
    /// of the whole, which is what a raw socket on Linux reads too, less any
    /// options, which only the first fragment is sure to carry.
    fn raw_input_reassembled(&mut self, interface: u32, last: &ipv4::Header, payload: &[u8]) {
        let has_raw = self
            .sockets
            .values()
            .any(|socket| matches!(socket, Socket::Raw(_)));
        if !has_raw {
            return;
        }
        let header = ipv4::Header {
            dont_fragment: false,
            more_fragments: false,
            fragment_offset: 0,
            ..*last
        };
        let mut packet = vec![0_u8; ipv4::MIN_HEADER_LEN + payload.len()];
        let Ok(at) = header.emit(&[], payload.len(), &mut packet) else {
            return;
        };
        if let Some(body) = packet.get_mut(at..) {
            body.copy_from_slice(payload);
        }
        self.raw_input_v4(interface, &packet, header.protocol);
    }

    /// A transport payload, by protocol number.
    fn transport_input(
        &mut self,
        interface: u32,
        source: IpAddress,
        destination: IpAddress,
        protocol: u8,
        payload: &[u8],
        now: Millis,
    ) {
        match (protocol, source) {
            (ipv4::protocol::ICMP, IpAddress::V4(_)) => {
                self.icmpv4_input(interface, source, destination, payload, now);
            }
            (icmpv6::PROTOCOL, IpAddress::V6(_)) => {
                self.icmpv6_input(interface, source, destination, payload, now);
            }
            (udp::PROTOCOL, _) => {
                self.udp_input(interface, source, destination, payload, now);
            }
            (tcp::PROTOCOL, _) => {
                self.tcp_input(interface, source, destination, payload, now);
            }
            _ => self.counters.no_socket += 1,
        }
    }

    /// A UDP datagram: to the socket bound to its port, or an unreachable.
    fn udp_input(
        &mut self,
        interface: u32,
        source: IpAddress,
        destination: IpAddress,
        payload: &[u8],
        now: Millis,
    ) {
        let pseudo = pseudo_of(source, destination);
        let Ok(datagram) = udp::Header::parse(payload, pseudo) else {
            self.count_malformed(interface);
            return;
        };
        let remote = Endpoint::new(source, datagram.header.source_port);
        let local = Endpoint::new(destination, datagram.header.destination_port);
        let Some(id) = self.find_datagram_socket(local, remote, false) else {
            self.counters.no_socket += 1;
            self.send_unreachable(source, destination, payload, now);
            return;
        };
        let Some(Socket::Udp(socket)) = self.sockets.get_mut(&id) else {
            return;
        };
        let kept = socket.deliver(Datagram {
            remote,
            local: destination,
            interface,
            payload: datagram.payload.to_vec(),
        });
        if kept {
            self.counters.delivered += 1;
        }
    }

    /// An ICMPv4 message: answer an echo, deliver a reply, report an error.
    fn icmpv4_input(
        &mut self,
        interface: u32,
        source: IpAddress,
        destination: IpAddress,
        payload: &[u8],
        now: Millis,
    ) {
        let Ok(message) = icmpv4::Header::parse(payload) else {
            self.count_malformed(interface);
            return;
        };
        match message.header.kind {
            icmpv4::kind::ECHO_REQUEST => {
                self.echo_reply_v4(source, destination, &message, now);
            }
            icmpv4::kind::ECHO_REPLY => {
                self.deliver_echo(interface, source, destination, payload, &message);
            }
            icmpv4::kind::DESTINATION_UNREACHABLE => {
                self.report_v4_error(message.body, message.header.code);
            }
            _ => self.counters.no_socket += 1,
        }
    }

    /// Answer an echo request with a reply carrying the same body.
    fn echo_reply_v4(
        &mut self,
        source: IpAddress,
        destination: IpAddress,
        message: &icmpv4::Message<'_>,
        now: Millis,
    ) {
        let reply = icmpv4::Header {
            kind: icmpv4::kind::ECHO_REPLY,
            code: 0,
            rest: message.header.rest,
        };
        let mut packet = vec![0_u8; icmpv4::HEADER_LEN + message.body.len()];
        if reply.emit(message.body, &mut packet).is_err() {
            return;
        }
        // The reply comes from the address the request went to, unless that
        // was a broadcast, in which case the route chooses.
        let from = if destination.is_multicast() {
            match self.source_for(source) {
                Ok(address) => address,
                Err(_) => return,
            }
        } else {
            destination
        };
        let _ = self.send_ip(
            from,
            source,
            ipv4::protocol::ICMP,
            &packet,
            self.config.hop_limit,
            None,
            now,
        );
    }

    /// An ICMPv6 message: Neighbor Discovery, or an echo.
    fn icmpv6_input(
        &mut self,
        interface: u32,
        source: IpAddress,
        destination: IpAddress,
        payload: &[u8],
        now: Millis,
    ) {
        let (IpAddress::V6(from), IpAddress::V6(to)) = (source, destination) else {
            return;
        };
        let Ok(message) = icmpv6::Message::parse(payload, from.octets(), to.octets()) else {
            self.count_malformed(interface);
            return;
        };
        if let Ok(Some(discovery)) = ndp::Ndp::parse(&message) {
            self.neighbor_input(interface, from, &discovery, now);
            return;
        }
        match message.kind {
            icmpv6::kind::ECHO_REQUEST => self.echo_reply_v6(source, destination, &message, now),
            icmpv6::kind::ECHO_REPLY => {
                self.deliver_echo_v6(interface, source, destination, payload, &message);
            }
            icmpv6::kind::DESTINATION_UNREACHABLE => {
                self.report_v6_error(message.body, message.code);
            }
            _ => self.counters.no_socket += 1,
        }
    }

    /// Answer an IPv6 echo request.
    fn echo_reply_v6(
        &mut self,
        source: IpAddress,
        destination: IpAddress,
        message: &icmpv6::Message<'_>,
        now: Millis,
    ) {
        let from = if destination.is_multicast() {
            match self.source_for(source) {
                Ok(address) => address,
                Err(_) => return,
            }
        } else {
            destination
        };
        let (IpAddress::V6(from_v6), IpAddress::V6(to_v6)) = (from, source) else {
            return;
        };
        let reply = icmpv6::Message {
            kind: icmpv6::kind::ECHO_REPLY,
            code: 0,
            body: message.body,
        };
        let mut packet = vec![0_u8; icmpv6::HEADER_LEN + message.body.len()];
        if reply
            .emit(from_v6.octets(), to_v6.octets(), &mut packet)
            .is_err()
        {
            return;
        }
        let _ = self.send_ip(from, source, icmpv6::PROTOCOL, &packet, 255, None, now);
    }

    /// Neighbor Discovery: learn from it, and answer a solicitation for one of
    /// our addresses.
    fn neighbor_input(
        &mut self,
        interface: u32,
        from: Ipv6,
        discovery: &ndp::Ndp<'_>,
        now: Millis,
    ) {
        match discovery {
            ndp::Ndp::NeighborSolicitation { target, options } => {
                if let Some(mac) = options.link_layer(ndp::option_kind::SOURCE_LINK_LAYER)
                    && !from.is_unspecified()
                {
                    let waiting = self
                        .neighbors
                        .record(interface, IpAddress::V6(from), mac, now);
                    self.flush(
                        interface,
                        IpAddress::V6(from),
                        waiting,
                        ethertype::IPV6,
                        now,
                    );
                }
                let target = Ipv6::new(*target);
                let ours = self
                    .interface(interface)
                    .is_some_and(|link| link.owns(IpAddress::V6(target)));
                if ours {
                    self.neighbor_advertisement(interface, target, from, now);
                }
            }
            ndp::Ndp::NeighborAdvertisement {
                target, options, ..
            } => {
                let Some(mac) = options.link_layer(ndp::option_kind::TARGET_LINK_LAYER) else {
                    return;
                };
                let address = IpAddress::V6(Ipv6::new(*target));
                let waiting = self.neighbors.record(interface, address, mac, now);
                self.flush(interface, address, waiting, ethertype::IPV6, now);
            }
            ndp::Ndp::RouterSolicitation { .. } | ndp::Ndp::RouterAdvertisement { .. } => {
                // This host is not a router and does not configure itself from
                // advertisements: addresses come from `ip`, as the roadmap's
                // exit criterion says.
                self.counters.no_socket += 1;
            }
        }
    }

    /// Answer "where is this address" with "here, at this hardware address".
    fn neighbor_advertisement(&mut self, interface: u32, target: Ipv6, to: Ipv6, now: Millis) {
        let Some(link) = self.interface(interface) else {
            return;
        };
        let hardware = link.hardware;
        let Ok(empty) = ndp::Options::new(&[]) else {
            return;
        };
        let message = ndp::Ndp::NeighborAdvertisement {
            router: false,
            solicited: true,
            override_cache: true,
            target: target.octets(),
            options: empty,
        };
        let mut body = [0_u8; 32];
        let Ok(written) = message.emit_body(&mut body) else {
            return;
        };
        let mut full = Vec::with_capacity(written + 8);
        full.extend_from_slice(body.get(..written).unwrap_or_default());
        full.extend_from_slice(&[ndp::option_kind::TARGET_LINK_LAYER, 1]);
        full.extend_from_slice(&hardware);
        let icmp = icmpv6::Message {
            kind: icmpv6::kind::NEIGHBOR_ADVERTISEMENT,
            code: 0,
            body: &full,
        };
        let mut packet = vec![0_u8; icmpv6::HEADER_LEN + full.len()];
        if icmp
            .emit(target.octets(), to.octets(), &mut packet)
            .is_err()
        {
            return;
        }
        let _ = self.send_ip(
            IpAddress::V6(target),
            IpAddress::V6(to),
            icmpv6::PROTOCOL,
            &packet,
            255,
            Some(interface),
            now,
        );
    }

    /// Send the packets that were waiting for an address to be resolved.
    pub(crate) fn flush(
        &mut self,
        interface: u32,
        address: IpAddress,
        waiting: Vec<Vec<u8>>,
        kind: u16,
        now: Millis,
    ) {
        for packet in waiting {
            let _ = self.dispatch(interface, address, packet, kind, now);
        }
    }

    /// Count a frame nobody could make sense of.
    fn count_malformed(&mut self, interface: u32) {
        self.counters.malformed += 1;
        if let Some(link) = self.interface_mut(interface) {
            link.counters.received_errors += 1;
        }
    }
}

/// The pseudo-header a transport checksum is taken over.
pub(crate) fn pseudo_of(source: IpAddress, destination: IpAddress) -> Pseudo {
    match (source, destination) {
        (IpAddress::V4(from), IpAddress::V4(to)) => Pseudo::V4 {
            source: from.octets(),
            destination: to.octets(),
        },
        (IpAddress::V6(from), IpAddress::V6(to)) => Pseudo::V6 {
            source: from.octets(),
            destination: to.octets(),
        },
        // Mixed families cannot happen: both come out of the same header.
        (from, _) => match from {
            IpAddress::V4(address) => Pseudo::V4 {
                source: address.octets(),
                destination: [0; 4],
            },
            IpAddress::V6(address) => Pseudo::V6 {
                source: address.octets(),
                destination: [0; 16],
            },
        },
    }
}

/// Whether an address is the solicited-node group of one of an interface's
/// own addresses, which is where a neighbour solicitation for it arrives.
fn is_ours_solicited(link: &crate::iface::Interface, address: Ipv6) -> bool {
    link.addresses.iter().any(|configured| {
        matches!(
            configured.cidr.address(),
            IpAddress::V6(own) if own.solicited_node() == address
        )
    })
}
