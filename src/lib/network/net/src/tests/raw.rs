//! Raw IPv4 sockets: what they are given a copy of, what they send, and what
//! they are not given.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::{icmpv4, ipv4, udp};

use crate::addr::{IpAddress, Ipv4};
use crate::socket::{Error, Family, Socket, SocketId};
use crate::stack::Stack;
use crate::tests::harness::{Alone, ONE, TWO, Wire, at};

/// ICMP's protocol number.
const ICMP: u8 = 1;

/// UDP's.
const UDP: u8 = 17;

/// `IPPROTO_RAW`.
const RAW: u8 = 255;

/// An ICMP echo request, checksum and all, as `ping` writes one.
fn echo_request(identifier: u16, sequence: u16, body: &[u8]) -> Vec<u8> {
    let [id_high, id_low] = identifier.to_be_bytes();
    let [seq_high, seq_low] = sequence.to_be_bytes();
    let header = icmpv4::Header {
        kind: icmpv4::kind::ECHO_REQUEST,
        code: 0,
        rest: [id_high, id_low, seq_high, seq_low],
    };
    let mut message = vec![0_u8; icmpv4::HEADER_LEN + body.len()];
    let _ = header
        .emit(body, &mut message)
        .expect("an echo request fits");
    message
}

/// Everything waiting on a raw socket, one packet per entry.
fn packets(stack: &mut Stack, id: SocketId) -> Vec<Vec<u8>> {
    let mut all = Vec::new();
    loop {
        let mut out = [0_u8; 2048];
        match stack.recv(id, &mut out, false) {
            Ok(received) => all.push(out.get(..received.bytes).unwrap_or_default().to_vec()),
            Err(_) => return all,
        }
    }
}

/// The ICMP type of a packet a raw socket read, which follows its IPv4 header.
fn icmp_type(packet: &[u8]) -> Option<u8> {
    let header_len = usize::from(packet.first()? & 0x0F) * 4;
    packet.get(header_len).copied()
}

/// Set a raw socket's `ICMP_FILTER`.
fn filter(stack: &mut Stack, id: SocketId, mask: u32) {
    if let Some(Socket::Raw(raw)) = stack.socket_mut(id) {
        raw.icmp_filter = mask;
    }
}

#[test]
fn a_raw_icmp_socket_reads_the_echo_reply_with_its_ip_header() {
    let mut wire = Wire::new();
    let ping = wire.one.open_raw(Family::V4, ICMP);
    let request = echo_request(0x4242, 1, b"ferrix");
    let now = wire.now;
    let sent = wire
        .one
        .send(ping, &request, Some(at(TWO, 0)), now)
        .expect("a raw ICMP socket sends the message it built");
    assert_eq!(sent, request.len());
    wire.settle();

    let mut out = [0_u8; 256];
    let received = wire
        .one
        .recv(ping, &mut out, false)
        .expect("the reply arrived");
    let packet = out.get(..received.bytes).unwrap_or_default();
    assert_eq!(
        packet.first(),
        Some(&0x45),
        "the packet starts with its IPv4 header"
    );
    assert_eq!(packet.get(9), Some(&ICMP), "and the header names ICMP");
    assert_eq!(
        packet.get(12..16),
        Some(TWO.octets().as_slice()),
        "from the host it pinged"
    );
    assert_eq!(icmp_type(packet), Some(icmpv4::kind::ECHO_REPLY));
    assert_eq!(
        packet.get(24..26),
        Some(0x4242_u16.to_be_bytes().as_slice()),
        "a raw socket's identifier is the program's, not replaced by a port"
    );
    assert_eq!(
        received.remote.map(|remote| (remote.address, remote.port)),
        Some((IpAddress::V4(TWO), 0)),
        "recvfrom names the sender's address and no port"
    );
}

#[test]
fn a_request_to_this_host_is_answered_and_copied_to_a_raw_socket() {
    let mut wire = Wire::new();
    let listener = wire.two.open_raw(Family::V4, ICMP);
    let ping = wire.one.open_raw(Family::V4, ICMP);
    let now = wire.now;
    let _ = wire
        .one
        .send(ping, &echo_request(7, 1, b"copy"), Some(at(TWO, 0)), now)
        .expect("sent");
    wire.settle();

    let seen = packets(&mut wire.two, listener);
    assert_eq!(
        seen.iter()
            .filter_map(|packet| icmp_type(packet))
            .collect::<Vec<_>>(),
        vec![icmpv4::kind::ECHO_REQUEST],
        "the host that was pinged hands its raw socket the request"
    );
    let replies = packets(&mut wire.one, ping);
    assert_eq!(
        replies
            .iter()
            .filter_map(|packet| icmp_type(packet))
            .collect::<Vec<_>>(),
        vec![icmpv4::kind::ECHO_REPLY],
        "and still answers it itself"
    );
}

#[test]
fn icmp_filter_keeps_the_types_it_names_away() {
    let mut wire = Wire::new();
    let ping = wire.one.open_raw(Family::V4, ICMP);
    filter(&mut wire.one, ping, 1 << icmpv4::kind::ECHO_REPLY);
    let now = wire.now;
    let _ = wire
        .one
        .send(
            ping,
            &echo_request(9, 1, b"filtered"),
            Some(at(TWO, 0)),
            now,
        )
        .expect("sent");
    wire.settle();
    assert!(
        packets(&mut wire.one, ping).is_empty(),
        "an echo reply is filtered out when its bit is set"
    );

    filter(&mut wire.one, ping, 1 << icmpv4::kind::ECHO_REQUEST);
    let now = wire.now;
    let _ = wire
        .one
        .send(
            ping,
            &echo_request(9, 2, b"let through"),
            Some(at(TWO, 0)),
            now,
        )
        .expect("sent");
    wire.settle();
    assert_eq!(
        packets(&mut wire.one, ping).len(),
        1,
        "and delivered when only another type's bit is"
    );
}

#[test]
fn a_raw_socket_takes_only_its_own_protocol() {
    let mut wire = Wire::new();
    let icmp = wire.two.open_raw(Family::V4, ICMP);
    let udp_raw = wire.two.open_raw(Family::V4, UDP);
    let nothing = wire.two.open_raw(Family::V4, RAW);
    let server = wire.two.open_udp(Family::V4);
    wire.two.bind(server, at(TWO, 5_000)).expect("bound");
    let client = wire.one.open_udp(Family::V4);
    let now = wire.now;
    let _ = wire
        .one
        .send(client, b"datagram", Some(at(TWO, 5_000)), now)
        .expect("sent");
    wire.settle();

    let udp_copies = packets(&mut wire.two, udp_raw);
    assert_eq!(udp_copies.len(), 1, "a raw UDP socket gets the datagram");
    assert_eq!(
        udp_copies.first().and_then(|packet| packet.get(9)),
        Some(&UDP)
    );
    assert!(
        packets(&mut wire.two, icmp).is_empty(),
        "a raw ICMP socket does not"
    );
    assert!(
        packets(&mut wire.two, nothing).is_empty(),
        "and IPPROTO_RAW receives nothing at all"
    );
    let mut out = [0_u8; 64];
    let received = wire
        .two
        .recv(server, &mut out, false)
        .expect("the UDP socket still has it");
    assert_eq!(out.get(..received.bytes), Some(b"datagram".as_slice()));
}

#[test]
fn a_connected_raw_socket_takes_only_its_peers_packets() {
    let mut wire = Wire::new();
    let elsewhere = IpAddress::V4(Ipv4::new([10, 0, 0, 99]));
    let connected = wire.two.open_raw(Family::V4, ICMP);
    let now = wire.now;
    // A peer that never sends: nothing from ONE may reach this socket.
    wire.two
        .connect(connected, crate::addr::Endpoint::new(elsewhere, 0), now)
        .expect("a raw socket connects to an address");
    let wildcard = wire.two.open_raw(Family::V4, ICMP);
    let ping = wire.one.open_raw(Family::V4, ICMP);
    let _ = wire
        .one
        .send(ping, &echo_request(3, 1, b"x"), Some(at(TWO, 0)), now)
        .expect("sent");
    wire.settle();
    assert!(
        packets(&mut wire.two, connected).is_empty(),
        "a socket connected elsewhere is not given ONE's request"
    );
    assert_eq!(
        packets(&mut wire.two, wildcard).len(),
        1,
        "while an unconnected one is"
    );
}

#[test]
fn a_header_included_packet_is_completed_and_delivered() {
    let mut wire = Wire::new();
    let server = wire.two.open_udp(Family::V4);
    wire.two.bind(server, at(TWO, 6_000)).expect("bound");
    let sender = wire.one.open_raw(Family::V4, RAW);

    // A UDP datagram whose IPv4 header leaves the source, the identification,
    // the total length and the checksum for the stack to fill in.
    let payload = b"built by hand";
    let pseudo = crate::input::pseudo_of(IpAddress::V4(ONE), IpAddress::V4(TWO));
    let mut datagram = vec![0_u8; udp::HEADER_LEN + payload.len()];
    let _ = udp::Header {
        source_port: 7_000,
        destination_port: 6_000,
    }
    .emit(payload, pseudo, &mut datagram)
    .expect("a UDP datagram fits");
    let mut packet = vec![0_u8; ipv4::MIN_HEADER_LEN];
    packet[0] = 0x45;
    packet[8] = 64;
    packet[9] = UDP;
    packet[16..20].copy_from_slice(&TWO.octets());
    packet.extend_from_slice(&datagram);

    let now = wire.now;
    let sent = wire
        .one
        .send(sender, &packet, Some(at(TWO, 0)), now)
        .expect("IPPROTO_RAW sends a packet whose header it wrote");
    assert_eq!(sent, packet.len());
    wire.settle();

    let mut out = [0_u8; 64];
    let received = wire
        .two
        .recv(server, &mut out, false)
        .expect("the datagram arrived, so its header checksum was filled in");
    assert_eq!(out.get(..received.bytes), Some(payload.as_slice()));
    assert_eq!(
        received.remote.map(|remote| remote.address),
        Some(IpAddress::V4(ONE)),
        "with the source address the stack filled in"
    );
}

#[test]
fn a_header_included_send_refuses_what_is_not_an_ipv4_header() {
    let mut host = Alone::new();
    let sender = host.stack.open_raw(Family::V4, RAW);
    let loopback = at(Ipv4::new([127, 0, 0, 1]), 0);
    let now = host.now;
    for (bytes, why) in [
        (vec![0x45_u8; 10], "shorter than a header"),
        (vec![0x65_u8; 20], "version 6"),
        (vec![0x44_u8; 20], "a header length under five words"),
        (vec![0x4F_u8; 20], "a header length past the data"),
    ] {
        assert_eq!(
            host.stack.send(sender, &bytes, Some(loopback), now),
            Err(Error::Invalid),
            "{why}"
        );
    }
}

#[test]
fn a_raw_socket_binds_this_hosts_addresses_only() {
    let mut host = Alone::new();
    let socket = host.stack.open_raw(Family::V4, ICMP);
    assert_eq!(
        host.stack.bind(socket, at(Ipv4::new([192, 0, 2, 1]), 0)),
        Err(Error::AddressNotAvailable)
    );
    host.stack
        .bind(socket, at(Ipv4::new([127, 0, 0, 1]), 0))
        .expect("the loopback is this host's");
    host.stack
        .bind(socket, at(Ipv4::UNSPECIFIED, 99))
        .expect("and a raw socket may be bound again, and ignores the port");
    assert_eq!(
        host.stack.local_endpoint(socket).map(|local| local.port),
        Some(0)
    );
}
