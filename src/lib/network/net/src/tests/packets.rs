//! Packet sockets: frames taken below IP, and frames put on a link by hand.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::ethernet::{self, ethertype};

use crate::packet::{ALL_PROTOCOLS, LinkAddress, PacketKind, PacketType};
use crate::socket::{Error, Family, SocketId};
use crate::stack::Stack;
use crate::tests::harness::{TWO, Wire, at};

/// The first host's hardware address, as the harness gives it.
const ONE_MAC: ethernet::Mac = [0x52, 0x54, 0, 0, 0, 1];

/// The second host's.
const TWO_MAC: ethernet::Mac = [0x52, 0x54, 0, 0, 0, 2];

/// The index of a host's Ethernet interface.
fn eth0(stack: &Stack) -> u32 {
    stack
        .interface_by_name(b"eth0")
        .map(|interface| interface.index)
        .expect("the harness gives every host an eth0")
}

/// Every frame waiting on a packet socket.
fn frames(stack: &mut Stack, id: SocketId) -> Vec<crate::packet::Received> {
    let mut all = Vec::new();
    let mut out = [0_u8; 2048];
    while let Ok(received) = stack.recv_packet(id, &mut out, false) {
        all.push(received);
    }
    all
}

/// An IPv4 packet to `destination` carrying `protocol`, with a valid header
/// and nothing after it, which is all a link needs to carry.
fn ipv4_packet(destination: [u8; 4], protocol: u8) -> Vec<u8> {
    let header = ferrix_netwire::ipv4::Header {
        dscp: 0,
        ecn: 0,
        identification: 7,
        dont_fragment: true,
        more_fragments: false,
        fragment_offset: 0,
        ttl: 64,
        protocol,
        source: [0, 0, 0, 0],
        destination,
    };
    let mut packet = vec![0_u8; ferrix_netwire::ipv4::MIN_HEADER_LEN + 8];
    let _ = header
        .emit(&[], 8, &mut packet)
        .expect("an IPv4 header fits");
    packet
}

#[test]
fn a_datagram_socket_reads_the_ip_packet_a_frame_carried() {
    let mut wire = Wire::new();
    let index = eth0(&wire.two);
    let listener = wire.two.open_packet(PacketKind::Datagram, ethertype::IPV4);
    wire.two
        .bind_packet(listener, 0, index)
        .expect("bound to eth0");
    let client = wire.one.open_udp(Family::V4);
    let now = wire.now;
    let _ = wire
        .one
        .send(client, b"below ip", Some(at(TWO, 9_999)), now)
        .expect("sent");
    wire.settle();

    let seen = frames(&mut wire.two, listener);
    let udp = seen
        .iter()
        .find(|frame| frame.protocol == ethertype::IPV4)
        .expect("the datagram's frame was copied to the packet socket");
    assert_eq!(udp.interface, index);
    assert_eq!(
        udp.source, ONE_MAC,
        "from the sending host's hardware address"
    );
    assert_eq!(udp.kind, PacketType::Host, "addressed to this interface");
    assert!(
        seen.iter().all(|frame| frame.protocol == ethertype::IPV4),
        "and nothing of another protocol, such as the ARP that preceded it"
    );
}

#[test]
fn a_frame_for_an_address_this_host_lacks_still_reaches_a_packet_socket() {
    // What a DHCP client depends on: the offer comes for an address the
    // interface does not have, which IP drops and a packet socket must not.
    let mut wire = Wire::new();
    let listener = wire.two.open_packet(PacketKind::Datagram, ethertype::IPV4);
    let index = eth0(&wire.two);
    wire.two.bind_packet(listener, 0, index).expect("bound");
    let sender = wire.one.open_packet(PacketKind::Datagram, ethertype::IPV4);
    let packet = ipv4_packet([10, 0, 0, 77], 17);
    let sent = wire
        .one
        .send_packet(
            sender,
            &packet,
            Some(LinkAddress {
                interface: eth0(&wire.one),
                protocol: ethertype::IPV4,
                address: ethernet::BROADCAST,
            }),
        )
        .expect("a SOCK_DGRAM packet socket sends to a hardware address");
    assert_eq!(sent, packet.len());
    let not_ours_before = wire.two.counters().not_ours;
    wire.settle();

    let seen = frames(&mut wire.two, listener);
    assert_eq!(seen.len(), 1, "the frame was copied");
    let frame = seen.first().expect("one frame");
    assert_eq!(
        frame.kind,
        PacketType::Broadcast,
        "sent to the broadcast address"
    );
    assert_eq!(
        frame.bytes,
        packet.len().max(46),
        "a datagram socket reads the payload, padded as the link padded it"
    );
    assert!(
        wire.two.counters().not_ours > not_ours_before,
        "while IP dropped the packet as not for this host"
    );
}

#[test]
fn a_raw_socket_reads_the_whole_frame_and_sends_one() {
    let mut wire = Wire::new();
    let listener = wire.two.open_packet(PacketKind::Raw, ALL_PROTOCOLS);
    let sender = wire.one.open_packet(PacketKind::Raw, ALL_PROTOCOLS);
    let index = eth0(&wire.one);
    wire.one.bind_packet(sender, 0, index).expect("bound");

    let mut frame = vec![0_u8; ethernet::HEADER_LEN + 4];
    let _ = ethernet::Header {
        destination: TWO_MAC,
        source: ONE_MAC,
        vlan: None,
        ethertype: 0x88B5,
    }
    .emit(&mut frame)
    .expect("a header fits");
    frame
        .get_mut(ethernet::HEADER_LEN..)
        .expect("room for a body")
        .copy_from_slice(b"raw!");
    let sent = wire
        .one
        .send_packet(sender, &frame, None)
        .expect("a raw packet socket sends the frame it wrote");
    assert_eq!(sent, frame.len());
    wire.settle();

    let mut out = [0_u8; 128];
    let received = wire
        .two
        .recv_packet(listener, &mut out, false)
        .expect("ETH_P_ALL took the frame");
    assert_eq!(received.protocol, 0x88B5);
    assert_eq!(
        out.get(..ethernet::HEADER_LEN + 4),
        Some(frame.as_slice()),
        "a raw socket reads the link header and all"
    );
    assert_eq!(received.bytes, 60, "padded to Ethernet's minimum, as sent");
}

#[test]
fn a_socket_takes_only_its_protocol_and_its_interface() {
    let mut wire = Wire::new();
    let arp_only = wire.two.open_packet(PacketKind::Datagram, ethertype::ARP);
    let nothing = wire.two.open_packet(PacketKind::Datagram, 0);
    let elsewhere = wire.two.open_packet(PacketKind::Datagram, ALL_PROTOCOLS);
    wire.two
        .bind_packet(elsewhere, 0, 1)
        .expect("the loopback exists");
    let client = wire.one.open_udp(Family::V4);
    let now = wire.now;
    let _ = wire
        .one
        .send(client, b"x", Some(at(TWO, 9_998)), now)
        .expect("sent");
    wire.settle();

    let arp = frames(&mut wire.two, arp_only);
    assert!(
        !arp.is_empty(),
        "the ARP request that resolved TWO was taken"
    );
    assert!(
        arp.iter().all(|frame| frame.protocol == ethertype::ARP),
        "and nothing but ARP"
    );
    assert!(
        frames(&mut wire.two, nothing).is_empty(),
        "a socket at protocol zero takes nothing"
    );
    assert!(
        frames(&mut wire.two, elsewhere).is_empty(),
        "and one bound to another interface takes nothing from eth0"
    );
}

#[test]
fn a_send_needs_an_interface_that_is_up_and_a_frame_that_fits() {
    let mut wire = Wire::new();
    let index = eth0(&wire.one);
    let unbound = wire.one.open_packet(PacketKind::Datagram, ethertype::IPV4);
    assert_eq!(
        wire.one.send_packet(unbound, b"nowhere", None),
        Err(Error::NoDevice),
        "an unbound socket names no interface"
    );
    assert_eq!(
        wire.one.bind_packet(unbound, 0, 99),
        Err(Error::NoDevice),
        "nor may it be bound to one that does not exist"
    );
    wire.one.bind_packet(unbound, 0, index).expect("bound");
    assert_eq!(
        wire.one.send_packet(unbound, &vec![0_u8; 1_501], None),
        Err(Error::TooLarge),
        "a payload past the MTU"
    );
    let raw = wire.one.open_packet(PacketKind::Raw, ALL_PROTOCOLS);
    wire.one.bind_packet(raw, 0, index).expect("bound");
    assert_eq!(
        wire.one.send_packet(raw, &[0_u8; 13], None),
        Err(Error::Invalid),
        "a raw frame shorter than its link header"
    );
    wire.one.set_up(index, false).expect("the interface exists");
    assert_eq!(
        wire.one.send_packet(unbound, b"down", None),
        Err(Error::NetworkDown),
        "an interface that is down"
    );
}

#[test]
fn a_closed_packet_socket_is_gone_and_its_identifier_is_not_reused_while_held() {
    let mut wire = Wire::new();
    let packet = wire.one.open_packet(PacketKind::Datagram, ethertype::IPV4);
    let udp = wire.one.open_udp(Family::V4);
    assert_ne!(packet, udp, "the two tables share one identifier space");
    wire.one.close(packet);
    assert!(wire.one.packet_socket(packet).is_none());
    assert!(
        wire.one.socket(udp).is_some(),
        "closing a packet socket leaves the other table alone"
    );
}
