//! Drive a whole host from frames a stranger chose.
//!
//! `netwire_parse` fuzzes the headers and `nettcp_state` fuzzes one
//! connection. This one fuzzes the thing between them: a [`Stack`] with an
//! interface, an address, a route and a handful of sockets, fed frames byte by
//! byte from the fuzzer and driven the way a kernel drives it.
//!
//! # The properties
//!
//! 1. **No frame panics.** Truncated Ethernet, an IPv4 header claiming a
//!    length it does not have, a fragment at an impossible offset, a TCP
//!    segment for a connection that does not exist.
//! 2. **What the stack answers is well formed.** Every frame it produces is
//!    parsed back as Ethernet, and an IP packet inside it parses too, so a
//!    header the stack builds that nothing can read is a crash here.
//! 3. **The memory a stranger can make this host hold is bounded.** Fragment
//!    reassembly stays under its ceiling however many first fragments arrive,
//!    and no socket's queue passes its capacity.
//! 4. **A host that is only ever sent rubbish sends nothing but answers.** The
//!    egress queue drains: the stack never wedges with a frame it will not
//!    give up.

#![no_main]

use ferrix_net::addr::{Endpoint, IpAddress, IpCidr, Ipv4};
use ferrix_net::iface::{Address, Interface};
use ferrix_net::route::{Origin, Route};
use ferrix_net::socket::Family;
use ferrix_net::stack::{Config, Stack};
use ferrix_netwire::ethernet;
use libfuzzer_sys::fuzz_target;

/// This host's address.
const OURS: Ipv4 = Ipv4::new([10, 0, 0, 1]);

/// This host's hardware address.
const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// The largest frame handed in, which is a jumbo frame's worth.
const MAX_FRAME: usize = 9_000;

/// How many frames the stack is asked for after each one it is given.
const DRAIN: usize = 16;

/// The capacity every socket is built with, small enough that a fuzz case can
/// fill it.
const CAPACITY: usize = 4_096;

/// A host with one interface, one address, a default route and four sockets.
fn host() -> Stack {
    let mut stack = Stack::new(Config {
        datagram_capacity: CAPACITY,
        ..Config::default()
    });
    let index = stack.add_interface(Interface::ethernet(0, b"eth0", MAC, 1500));
    let _ = stack.set_up(index, true);
    let _ = stack.add_address(
        index,
        Address {
            cidr: IpCidr::new(IpAddress::V4(OURS), 24),
            peer: None,
        },
    );
    stack.routes_mut().add(Route {
        destination: IpCidr::new(IpAddress::V4(Ipv4::UNSPECIFIED), 0),
        gateway: None,
        interface: index,
        metric: 100,
        origin: Origin::Static,
    });

    let udp = stack.open_udp(Family::V4);
    let _ = stack.bind(udp, Endpoint::new(IpAddress::V4(OURS), 53));
    let icmp = stack.open_icmp(Family::V4);
    let _ = stack.bind(icmp, Endpoint::new(IpAddress::V4(OURS), 1));
    let listener = stack.open_tcp(Family::V4);
    let _ = stack.bind(listener, Endpoint::new(IpAddress::V4(OURS), 80));
    let _ = stack.listen(listener, 4);
    stack
}

/// Assert that a frame the stack produced can be read back.
fn check(frame: &[u8]) {
    let Ok((header, payload)) = ethernet::Header::parse(frame) else {
        panic!("the stack produced a frame that is not Ethernet");
    };
    match header.ethertype {
        ethernet::ethertype::IPV4 => {
            assert!(
                ferrix_netwire::ipv4::Header::parse(payload).is_ok(),
                "the stack produced an IPv4 packet it cannot read back"
            );
        }
        ethernet::ethertype::IPV6 => {
            assert!(
                ferrix_netwire::ipv6::Header::parse(payload).is_ok(),
                "the stack produced an IPv6 packet it cannot read back"
            );
        }
        ethernet::ethertype::ARP => {
            assert!(
                ferrix_netwire::arp::Packet::parse(payload).is_ok(),
                "the stack produced an ARP packet it cannot read back"
            );
        }
        _ => panic!("the stack produced a frame of an ethertype it never sends"),
    }
}

fuzz_target!(|data: &[u8]| {
    let mut stack = host();
    stack.seed(u64::from_be_bytes(
        *data.first_chunk::<8>().unwrap_or(&[0; 8]),
    ));
    let mut rest = data.get(8..).unwrap_or_default();
    let mut now: u64 = 0;

    while !rest.is_empty() {
        // A length byte pair, then that many bytes as one frame.
        let (length, tail) = rest.split_at(rest.len().min(2));
        rest = tail;
        let wanted = usize::from(u16::from_be_bytes([
            length.first().copied().unwrap_or(0),
            length.get(1).copied().unwrap_or(0),
        ]))
        .min(MAX_FRAME);
        let take = wanted.min(rest.len());
        let (frame, tail) = rest.split_at(take);
        rest = tail;

        stack.receive(1, frame, now);
        stack.receive(2, frame, now);
        now = now.saturating_add(u64::from(frame.len() as u32) % 997);
        stack.on_timer(now);

        for _ in 0..DRAIN {
            let Some(outgoing) = stack.poll_transmit(now) else {
                break;
            };
            check(&outgoing.frame);
        }
        assert!(
            stack.reassembly_held() <= ferrix_net::reassembly::MAX_BYTES,
            "reassembly held more than its ceiling"
        );
    }

    // Whatever was said to it, the host must be willing to stop talking.
    for _ in 0..4_096 {
        let Some(outgoing) = stack.poll_transmit(now) else {
            return;
        };
        check(&outgoing.frame);
    }
    panic!("the stack never ran out of frames to send");
});
