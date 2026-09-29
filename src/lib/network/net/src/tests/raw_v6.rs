//! Raw IPv6 sockets: the message without its header, the checksum the stack
//! writes and checks, and the ICMPv6 types a filter keeps away.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::icmpv6;

use crate::addr::{IpAddress, Ipv6};
use crate::socket::{Error, Family, Socket, SocketId};
use crate::stack::Stack;
use crate::tests::harness::{Alone, ONE, ONE_V6, TWO_V6, Wire, at, at_v6};

/// ICMPv6's protocol number.
const ICMPV6: u8 = 58;

/// A protocol nothing in the stack handles, for a raw socket of its own.
const EXPERIMENT: u8 = 253;

/// An echo request as `ping6` writes one: the checksum left zero, because the
/// stack computes it.
fn echo_request(identifier: u16, sequence: u16, body: &[u8]) -> Vec<u8> {
    let mut message = vec![icmpv6::kind::ECHO_REQUEST, 0, 0, 0];
    message.extend_from_slice(&identifier.to_be_bytes());
    message.extend_from_slice(&sequence.to_be_bytes());
    message.extend_from_slice(body);
    message
}

/// Everything waiting on a socket, with the hop limit each arrived with.
fn messages(stack: &mut Stack, id: SocketId) -> Vec<(Vec<u8>, Option<u8>)> {
    let mut all = Vec::new();
    loop {
        let mut out = [0_u8; 2048];
        match stack.recv(id, &mut out, false) {
            Ok(received) => all.push((
                out.get(..received.bytes).unwrap_or_default().to_vec(),
                received.hop_limit,
            )),
            Err(_) => return all,
        }
    }
}

/// Block every ICMPv6 type but `kind`, as `ICMP6_FILTER_SETBLOCKALL` and
/// `ICMP6_FILTER_SETPASS` do.
fn pass_only(stack: &mut Stack, id: SocketId, kind: u8) {
    if let Some(Socket::Raw(raw)) = stack.socket_mut(id) {
        raw.icmp6_filter = [u32::MAX; 8];
        if let Some(word) = raw.icmp6_filter.get_mut(usize::from(kind >> 5)) {
            *word &= !(1 << (kind & 31));
        }
    }
}

/// Whether the checksum of `message` from `source` to `destination` verifies.
fn verifies(message: &[u8], source: Ipv6, destination: Ipv6, protocol: u8) -> bool {
    crate::input::upper_layer_sum(
        message,
        IpAddress::V6(source),
        IpAddress::V6(destination),
        protocol,
    ) == Some(0)
}

#[test]
fn ping6_over_loopback_reads_the_reply_without_a_header_and_with_its_hop_limit() {
    let mut host = Alone::new();
    let ping = host.stack.open_raw(Family::V6, ICMPV6);
    let request = echo_request(0x6666, 1, b"ferrix");
    let sent = host
        .stack
        .send(ping, &request, Some(at_v6(Ipv6::LOOPBACK, 0)), host.now)
        .expect("a raw ICMPv6 socket sends the message it built");
    assert_eq!(sent, request.len());
    host.settle();

    let seen = messages(&mut host.stack, ping);
    let kinds = seen
        .iter()
        .filter_map(|(message, _)| message.first().copied())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![icmpv6::kind::ECHO_REQUEST, icmpv6::kind::ECHO_REPLY],
        "the socket reads its own request back over loopback, then the reply, \
         each starting with the ICMPv6 type rather than an IPv6 header"
    );
    for (message, hop_limit) in &seen {
        assert!(
            verifies(message, Ipv6::LOOPBACK, Ipv6::LOOPBACK, ICMPV6),
            "the stack wrote a checksum that verifies: {message:02x?}"
        );
        assert_eq!(
            *hop_limit,
            Some(64),
            "each says the hop limit it came with, the stack's own for the reply"
        );
    }
    let (reply, _) = seen.get(1).expect("the reply");
    assert_eq!(
        reply.get(4..6),
        Some(0x6666_u16.to_be_bytes().as_slice()),
        "a raw socket's identifier is the program's"
    );
}

#[test]
fn icmp6_filter_passes_only_the_types_it_lets_through() {
    let mut host = Alone::new();
    let ping = host.stack.open_raw(Family::V6, ICMPV6);
    pass_only(&mut host.stack, ping, icmpv6::kind::ECHO_REPLY);
    let _ = host
        .stack
        .send(
            ping,
            &echo_request(1, 1, b"only replies"),
            Some(at_v6(Ipv6::LOOPBACK, 0)),
            host.now,
        )
        .expect("sent");
    host.settle();

    let kinds = messages(&mut host.stack, ping)
        .iter()
        .filter_map(|(message, _)| message.first().copied())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![icmpv6::kind::ECHO_REPLY],
        "the request it sent itself is filtered away, and the reply is not"
    );
}

#[test]
fn the_checksum_offset_is_written_on_send_and_checked_on_receipt() {
    let mut wire = Wire::new();
    let checked = wire.two.open_raw(Family::V6, EXPERIMENT);
    let unchecked = wire.two.open_raw(Family::V6, EXPERIMENT);
    let sender = wire.one.open_raw(Family::V6, EXPERIMENT);
    for (stack, id) in [(&mut wire.two, checked), (&mut wire.one, sender)] {
        if let Some(Socket::Raw(raw)) = stack.socket_mut(id) {
            assert_eq!(raw.checksum, None, "only ICMPv6 is summed unasked");
            raw.checksum = Some(4);
        }
    }
    let now = wire.now;
    let _ = wire
        .one
        .send(sender, b"abcd\0\0gh", Some(at_v6(TWO_V6, 0)), now)
        .expect("sent with a checksum");
    wire.settle();
    let good = messages(&mut wire.two, checked);
    assert_eq!(good.len(), 1, "a message whose checksum verifies arrives");
    let (message, _) = good.first().expect("one");
    assert!(
        verifies(message, ONE_V6, TWO_V6, EXPERIMENT),
        "at offset 4: {message:02x?}"
    );
    assert_eq!(
        message.get(..4),
        Some(b"abcd".as_slice()),
        "the rest untouched"
    );
    let _ = messages(&mut wire.two, unchecked);

    // Without the offset set the sender writes no checksum, and a socket that
    // checks one refuses what one that does not still reads.
    if let Some(Socket::Raw(raw)) = wire.one.socket_mut(sender) {
        raw.checksum = None;
    }
    let now = wire.now;
    let _ = wire
        .one
        .send(sender, b"abcd\0\0gh", Some(at_v6(TWO_V6, 0)), now)
        .expect("sent without one");
    wire.settle();
    assert_eq!(
        messages(&mut wire.two, checked).len(),
        0,
        "a checksum that does not verify is not delivered"
    );
    assert_eq!(
        messages(&mut wire.two, unchecked).len(),
        1,
        "to a socket that checks none it is"
    );
}

#[test]
fn a_message_too_short_for_the_checksum_field_is_refused() {
    let mut host = Alone::new();
    let ping = host.stack.open_raw(Family::V6, ICMPV6);
    assert_eq!(
        host.stack
            .send(ping, &[128, 0, 0], Some(at_v6(Ipv6::LOOPBACK, 0)), host.now),
        Err(Error::Invalid),
        "three bytes cannot hold a checksum at offset 2"
    );
}

#[test]
fn a_raw_socket_sends_only_to_its_own_family() {
    let mut host = Alone::new();
    let six = host.stack.open_raw(Family::V6, ICMPV6);
    assert_eq!(
        host.stack
            .send(six, &echo_request(1, 1, b""), Some(at(ONE, 0)), host.now),
        Err(Error::Invalid),
        "an IPv6 raw socket does not send to an IPv4 address"
    );
    let four = host.stack.open_raw(Family::V4, 1);
    assert_eq!(
        host.stack.send(
            four,
            b"\x08\0\0\0\0\0\0\0",
            Some(at_v6(ONE_V6, 0)),
            host.now
        ),
        Err(Error::Invalid),
        "nor an IPv4 one to an IPv6 address"
    );
    if let Some(Socket::Raw(raw)) = host.stack.socket_mut(six) {
        raw.header_included = true;
    }
    assert_eq!(
        host.stack.send(
            six,
            &echo_request(1, 1, b""),
            Some(at_v6(Ipv6::LOOPBACK, 0)),
            host.now
        ),
        Err(Error::Invalid),
        "and an IPv6 one never takes a header from the program"
    );
}

#[test]
fn a_udp_datagram_says_the_hop_limit_it_arrived_with() {
    let mut wire = Wire::new();
    let server = wire.two.open_udp(Family::V6);
    wire.two.bind(server, at_v6(TWO_V6, 9_400)).expect("bound");
    let client = wire.one.open_udp(Family::V6);
    if let Some(socket) = wire.one.socket_mut(client) {
        socket.options_mut().hop_limit = 17;
    }
    let now = wire.now;
    let _ = wire
        .one
        .send(client, b"hops", Some(at_v6(TWO_V6, 9_400)), now)
        .expect("sent");
    wire.settle();
    let got = messages(&mut wire.two, server);
    assert_eq!(
        got,
        vec![(b"hops".to_vec(), Some(17))],
        "the receiver reads the hop limit the sender set"
    );
}
