//! ARP requests and replies, and the packets that are not IPv4 over Ethernet.

use crate::Error;
use crate::arp::{Operation, PACKET_LEN, Packet};

const REQUEST: [u8; PACKET_LEN] = [
    0x00, 0x01, 0x08, 0x00, 0x06, 0x04, 0x00, 0x01, // Ethernet, IPv4, 6, 4, request
    0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 10, 0, 2, 15, // sender
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 10, 0, 2, 2, // target
];

#[test]
fn a_request_parses_into_its_fields() {
    let packet = Packet::parse(&REQUEST).expect("parses");
    assert_eq!(packet.operation, Operation::Request);
    assert_eq!(packet.sender_mac, [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    assert_eq!(packet.sender_ip, [10, 0, 2, 15]);
    assert_eq!(packet.target_mac, [0; 6]);
    assert_eq!(packet.target_ip, [10, 0, 2, 2]);
}

#[test]
fn ethernet_padding_after_the_packet_is_ignored() {
    let mut padded = [0u8; 46];
    padded[..PACKET_LEN].copy_from_slice(&REQUEST);
    assert_eq!(Packet::parse(&padded), Packet::parse(&REQUEST));
}

#[test]
fn the_reply_swaps_the_addresses_and_names_our_mac() {
    let request = Packet::parse(&REQUEST).expect("parses");
    let ours = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
    let reply = request.reply(ours);
    assert_eq!(reply.operation, Operation::Reply);
    assert_eq!((reply.sender_mac, reply.sender_ip), (ours, [10, 0, 2, 2]));
    assert_eq!(
        (reply.target_mac, reply.target_ip),
        (request.sender_mac, request.sender_ip)
    );
}

#[test]
fn packets_round_trip_through_emit() {
    let request = Packet::parse(&REQUEST).expect("parses");
    for packet in [request, request.reply([1, 2, 3, 4, 5, 6])] {
        let mut out = [0u8; PACKET_LEN];
        assert_eq!(packet.emit(&mut out), Ok(PACKET_LEN));
        assert_eq!(Packet::parse(&out), Ok(packet));
    }
    let mut out = [0u8; PACKET_LEN];
    let _ = request.emit(&mut out);
    assert_eq!(out, REQUEST, "emit writes the bytes parse read");
}

#[test]
fn every_short_packet_is_truncated_and_a_short_buffer_refused() {
    for cut in 0..PACKET_LEN {
        assert_eq!(
            Packet::parse(&REQUEST[..cut]),
            Err(Error::Truncated),
            "cut at {cut}"
        );
    }
    let request = Packet::parse(&REQUEST).expect("parses");
    assert_eq!(
        request.emit(&mut [0u8; PACKET_LEN - 1]),
        Err(Error::NoSpace)
    );
}

#[test]
fn other_hardware_protocols_lengths_and_operations_are_refused() {
    for (at, value) in [(1, 6u8), (2, 0x86), (4, 8), (5, 16), (7, 3), (7, 0)] {
        let mut packet = REQUEST;
        packet[at] = value;
        assert!(
            matches!(Packet::parse(&packet), Err(Error::Malformed(_))),
            "byte {at} = {value:#x}"
        );
    }
}
