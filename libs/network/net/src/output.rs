//! Putting a packet on a link.
//!
//! One path down from a socket: choose the route, build the IP header, find
//! the hardware address of the next hop, build the frame. Every step can fail
//! for a reason a program has a name for, so each of them answers an
//! [`Error`] rather than dropping the packet quietly.
//!
//! A packet whose next hop is not yet resolved is not an error. It waits in
//! the neighbour cache with a solicitation behind it, and goes out when the
//! answer arrives -- which is why a first packet to a new host is slow and the
//! second is not.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::ethernet::{self, Mac, ethertype};
use ferrix_netwire::{arp, icmpv6, ipv4, ipv6, ndp};

use crate::addr::{IpAddress, Ipv4, Ipv6};
use crate::iface::Medium;
use crate::socket::Error;
use crate::stack::{Millis, Outgoing, Stack};

/// The shortest Ethernet frame a link will carry, without its check sequence.
/// Shorter frames are padded, because a real adapter drops a runt.
const MIN_FRAME: usize = 60;

impl Stack {
    /// Send `payload` as an IP packet from `source` to `destination`.
    #[expect(
        clippy::too_many_arguments,
        reason = "AUDIT: the arguments are an IP header's fields and come from \
                  four different layers -- the socket's addresses, the \
                  protocol, the options and the clock. A struct holding them \
                  would be built at every call site and destructured here"
    )]
    pub(crate) fn send_ip(
        &mut self,
        source: IpAddress,
        destination: IpAddress,
        protocol: u8,
        payload: &[u8],
        hop_limit: u8,
        device: Option<u32>,
        now: Millis,
    ) -> Result<(), Error> {
        let hop = self.routes.lookup(destination).ok_or(Error::Unreachable)?;
        if device.is_some_and(|wanted| wanted != hop.interface) {
            return Err(Error::Unreachable);
        }
        let interface = self.interface(hop.interface).ok_or(Error::Unreachable)?;
        if !interface.is_up() {
            return Err(Error::Unreachable);
        }
        let mtu = interface.mtu as usize;
        let index = hop.interface;
        match (source, destination) {
            (IpAddress::V4(from), IpAddress::V4(to)) => self.send_v4(
                index,
                hop.address,
                from,
                to,
                protocol,
                payload,
                hop_limit,
                mtu,
                now,
            ),
            (IpAddress::V6(from), IpAddress::V6(to)) => self.send_v6(
                index,
                hop.address,
                from,
                to,
                protocol,
                payload,
                hop_limit,
                mtu,
                now,
            ),
            _ => Err(Error::Invalid),
        }
    }

    /// Build an IPv4 packet, fragmenting it if the link will not carry it
    /// whole.
    #[expect(
        clippy::too_many_arguments,
        reason = "AUDIT: an IP header's fields, and every one is chosen by a \
                  different layer; bundling them into a struct would move the \
                  argument list rather than shorten it"
    )]
    fn send_v4(
        &mut self,
        interface: u32,
        next_hop: IpAddress,
        source: Ipv4,
        destination: Ipv4,
        protocol: u8,
        payload: &[u8],
        ttl: u8,
        mtu: usize,
        now: Millis,
    ) -> Result<(), Error> {
        let room = mtu.saturating_sub(ipv4::MIN_HEADER_LEN);
        if room == 0 {
            return Err(Error::TooLarge);
        }
        let identification = self.next_identification();
        if payload.len() <= room {
            let packet = build_v4(
                source,
                destination,
                protocol,
                ttl,
                identification,
                0,
                false,
                payload,
            )?;
            return self.dispatch(interface, next_hop, packet, ethertype::IPV4, now);
        }
        // Fragment offsets are counted in eight-byte units, so every fragment
        // but the last has to be a multiple of eight.
        let chunk = room & !7;
        if chunk == 0 {
            return Err(Error::TooLarge);
        }
        let mut offset = 0;
        while offset < payload.len() {
            let end = (offset + chunk).min(payload.len());
            let piece = payload.get(offset..end).ok_or(Error::Invalid)?;
            let packet = build_v4(
                source,
                destination,
                protocol,
                ttl,
                identification,
                (offset / 8) as u16,
                end < payload.len(),
                piece,
            )?;
            self.dispatch(interface, next_hop, packet, ethertype::IPV4, now)?;
            offset = end;
        }
        Ok(())
    }

    /// Build an IPv6 packet.
    ///
    /// IPv6 has no fragmentation on the path and this stack does not fragment
    /// at the source either: a payload larger than the link will carry is
    /// `EMSGSIZE`, which is what a program sees from Linux with
    /// `IPV6_DONTFRAG` and what path-MTU discovery is for.
    #[expect(
        clippy::too_many_arguments,
        reason = "AUDIT: the same header fields as the IPv4 form above, for the \
                  same reason"
    )]
    fn send_v6(
        &mut self,
        interface: u32,
        next_hop: IpAddress,
        source: Ipv6,
        destination: Ipv6,
        protocol: u8,
        payload: &[u8],
        hop_limit: u8,
        mtu: usize,
        now: Millis,
    ) -> Result<(), Error> {
        if payload.len() + ipv6::HEADER_LEN > mtu {
            return Err(Error::TooLarge);
        }
        let header = ipv6::Header {
            traffic_class: 0,
            flow_label: 0,
            next_header: protocol,
            hop_limit,
            source: source.octets(),
            destination: destination.octets(),
        };
        let mut packet = vec![0_u8; ipv6::HEADER_LEN + payload.len()];
        let written = header
            .emit(payload.len(), &mut packet)
            .map_err(|_| Error::Invalid)?;
        let body = packet.get_mut(written..).ok_or(Error::Invalid)?;
        body.copy_from_slice(payload);
        self.dispatch(interface, next_hop, packet, ethertype::IPV6, now)
    }

    /// The identification the next datagram carries, so that two datagrams
    /// fragmented at once are not reassembled into each other.
    pub(crate) fn next_identification(&mut self) -> u16 {
        self.ip_id = self.ip_id.wrapping_add(1);
        self.ip_id
    }

    /// Put a finished IP packet on its interface, resolving the next hop if
    /// the medium needs it.
    pub(crate) fn dispatch(
        &mut self,
        interface: u32,
        next_hop: IpAddress,
        packet: Vec<u8>,
        kind: u16,
        now: Millis,
    ) -> Result<(), Error> {
        let link = self.interface(interface).ok_or(Error::Unreachable)?;
        if matches!(link.medium, Medium::Loopback) {
            self.count_sent(interface, packet.len());
            self.egress.push_back(Outgoing {
                interface,
                frame: packet,
            });
            return Ok(());
        }
        let Some(destination) = self.hardware_for(interface, next_hop, now) else {
            // Not resolved: hold the packet and ask.
            if self.neighbors.queue(interface, next_hop, packet, now) {
                self.solicit(interface, next_hop, now);
            }
            return Ok(());
        };
        self.emit_frame(interface, destination, kind, &packet);
        Ok(())
    }

    /// The hardware address a packet to `next_hop` goes to, if it is known
    /// without asking.
    fn hardware_for(&mut self, interface: u32, next_hop: IpAddress, now: Millis) -> Option<Mac> {
        if next_hop.is_multicast() {
            return Some(match next_hop {
                IpAddress::V4(address) => address.multicast_mac(),
                IpAddress::V6(address) => address.multicast_mac(),
            });
        }
        if let IpAddress::V4(address) = next_hop
            && (address.is_broadcast() || self.is_directed_broadcast(interface, address))
        {
            return Some(ethernet::BROADCAST);
        }
        let link = self.interface(interface)?;
        if !link.resolves() {
            return Some(ethernet::BROADCAST);
        }
        self.neighbors.lookup(interface, next_hop, now)
    }

    /// Whether an address is the broadcast of one of an interface's prefixes.
    fn is_directed_broadcast(&self, interface: u32, address: Ipv4) -> bool {
        self.interface(interface).is_some_and(|link| {
            link.addresses
                .iter()
                .any(|configured| configured.cidr.broadcast() == Some(address))
        })
    }

    /// Wrap a payload in an Ethernet header and queue it.
    pub(crate) fn emit_frame(&mut self, interface: u32, to: Mac, kind: u16, payload: &[u8]) {
        let Some(link) = self.interface(interface) else {
            return;
        };
        let header = ethernet::Header {
            destination: to,
            source: link.hardware,
            vlan: None,
            ethertype: kind,
        };
        let length = ethernet::HEADER_LEN + payload.len();
        let mut frame = vec![0_u8; length.max(MIN_FRAME)];
        if header.emit(&mut frame).is_err() {
            return;
        }
        let Some(body) = frame.get_mut(ethernet::HEADER_LEN..length) else {
            return;
        };
        body.copy_from_slice(payload);
        self.count_sent(interface, frame.len());
        self.egress.push_back(Outgoing { interface, frame });
    }

    /// Ask who has `target`, in whichever way the family asks.
    pub(crate) fn solicit(&mut self, interface: u32, target: IpAddress, now: Millis) {
        match target {
            IpAddress::V4(address) => self.arp_request(interface, address),
            IpAddress::V6(address) => self.neighbor_solicitation(interface, address, now),
        }
    }

    /// An ARP request, broadcast on the link.
    fn arp_request(&mut self, interface: u32, target: Ipv4) {
        let Some(link) = self.interface(interface) else {
            return;
        };
        let sender = link
            .source_for(IpAddress::V4(target))
            .and_then(|address| match address {
                IpAddress::V4(four) => Some(four),
                IpAddress::V6(_) => None,
            })
            .unwrap_or(Ipv4::UNSPECIFIED);
        let packet = arp::Packet {
            operation: arp::Operation::Request,
            sender_mac: link.hardware,
            sender_ip: sender.octets(),
            target_mac: [0; 6],
            target_ip: target.octets(),
        };
        let mut bytes = [0_u8; arp::PACKET_LEN];
        if packet.emit(&mut bytes).is_err() {
            return;
        }
        self.emit_frame(interface, ethernet::BROADCAST, ethertype::ARP, &bytes);
    }

    /// A neighbour solicitation, to the target's solicited-node group.
    fn neighbor_solicitation(&mut self, interface: u32, target: Ipv6, now: Millis) {
        let Some(link) = self.interface(interface) else {
            return;
        };
        let hardware = link.hardware;
        let source = match link.source_for(IpAddress::V6(target)) {
            Some(IpAddress::V6(address)) => address,
            _ => Ipv6::UNSPECIFIED,
        };
        let group = target.solicited_node();
        let message = ndp::Ndp::NeighborSolicitation {
            target: target.octets(),
            options: match ndp::Options::new(&[]) {
                Ok(options) => options,
                Err(_) => return,
            },
        };
        let mut body = [0_u8; 32];
        let Ok(written) = message.emit_body(&mut body) else {
            return;
        };
        // The source link-layer address option, which lets the answer come
        // back without a solicitation of its own.
        let mut full = Vec::with_capacity(written + 8);
        full.extend_from_slice(body.get(..written).unwrap_or_default());
        full.extend_from_slice(&[ndp::option_kind::SOURCE_LINK_LAYER, 1]);
        full.extend_from_slice(&hardware);
        let icmp = icmpv6::Message {
            kind: icmpv6::kind::NEIGHBOR_SOLICITATION,
            code: 0,
            body: &full,
        };
        let mut packet = vec![0_u8; icmpv6::HEADER_LEN + full.len()];
        if icmp
            .emit(source.octets(), group.octets(), &mut packet)
            .is_err()
        {
            return;
        }
        let _ = self.send_v6_direct(interface, group, source, group, &packet, now);
    }

    /// Send an ICMPv6 message to a group without consulting the routing
    /// table, which a solicitation must not do: the answer to "where is this
    /// address" cannot depend on already knowing.
    fn send_v6_direct(
        &mut self,
        interface: u32,
        next_hop: Ipv6,
        source: Ipv6,
        destination: Ipv6,
        payload: &[u8],
        now: Millis,
    ) -> Result<(), Error> {
        let header = ipv6::Header {
            traffic_class: 0,
            flow_label: 0,
            next_header: icmpv6::PROTOCOL,
            hop_limit: 255,
            source: source.octets(),
            destination: destination.octets(),
        };
        let mut packet = vec![0_u8; ipv6::HEADER_LEN + payload.len()];
        let written = header
            .emit(payload.len(), &mut packet)
            .map_err(|_| Error::Invalid)?;
        let body = packet.get_mut(written..).ok_or(Error::Invalid)?;
        body.copy_from_slice(payload);
        self.dispatch(
            interface,
            IpAddress::V6(next_hop),
            packet,
            ethertype::IPV6,
            now,
        )
    }

    /// Count a frame against an interface.
    pub(crate) fn count_sent(&mut self, interface: u32, bytes: usize) {
        if let Some(link) = self.interface_mut(interface) {
            link.counters.sent += 1;
            link.counters.sent_bytes += bytes as u64;
        }
    }
}

/// Build one IPv4 packet, header and payload.
#[expect(
    clippy::too_many_arguments,
    reason = "AUDIT: this is an IPv4 header; the alternative is a struct that \
              exists only to be destructured at the one call site"
)]
fn build_v4(
    source: Ipv4,
    destination: Ipv4,
    protocol: u8,
    ttl: u8,
    identification: u16,
    fragment_offset: u16,
    more_fragments: bool,
    payload: &[u8],
) -> Result<Vec<u8>, Error> {
    let header = ipv4::Header {
        dscp: 0,
        ecn: 0,
        identification,
        dont_fragment: false,
        more_fragments,
        fragment_offset,
        ttl,
        protocol,
        source: source.octets(),
        destination: destination.octets(),
    };
    let mut packet = vec![0_u8; ipv4::MIN_HEADER_LEN + payload.len()];
    let written = header
        .emit(&[], payload.len(), &mut packet)
        .map_err(|_| Error::TooLarge)?;
    let body = packet.get_mut(written..).ok_or(Error::Invalid)?;
    body.copy_from_slice(payload);
    Ok(packet)
}
