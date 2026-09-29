//! Fuzz the wire formats the net core will read from a stranger's packet.
//!
//! Every parser in `ferrix-netwire` runs on the same input, because a packet
//! reaches each of them as bytes nobody in this tree chose.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **What a parse borrows lies inside the input.**
//! 2. **Parse and emit agree**: every header that parses, written out again with
//!    the fields the parse reported, parses back to exactly the same header and
//!    payload. For TCP that means the options in canonical form; for
//!    Neighbor Discovery, the message body.
//! 3. **The IPv6 extension walk stays inside the payload** and ends at an
//!    offset no larger than it.
//! 4. **A checksum summed in pieces is the checksum of the whole**, split at a
//!    point the input chooses.

#![no_main]

use ferrix_netwire::checksum::{Checksum, Pseudo, checksum};
use ferrix_netwire::ndp::Ndp;
use ferrix_netwire::{arp, ethernet, icmpv4, icmpv6, ipv4, ipv6, tcp, udp};
use libfuzzer_sys::fuzz_target;

/// Addresses for the transport checksums, taken from the start of the input so
/// the fuzzer can steer them.
fn addresses(bytes: &[u8]) -> (Pseudo, Pseudo, [u8; 16], [u8; 16]) {
    let mut pool = [0u8; 40];
    for (slot, byte) in pool.iter_mut().zip(bytes) {
        *slot = *byte;
    }
    let v4_source: [u8; 4] = pool[0..4].try_into().expect("four bytes");
    let v4_destination: [u8; 4] = pool[4..8].try_into().expect("four bytes");
    let v6_source: [u8; 16] = pool[8..24].try_into().expect("sixteen bytes");
    let v6_destination: [u8; 16] = pool[24..40].try_into().expect("sixteen bytes");
    (
        Pseudo::V4 {
            source: v4_source,
            destination: v4_destination,
        },
        Pseudo::V6 {
            source: v6_source,
            destination: v6_destination,
        },
        v6_source,
        v6_destination,
    )
}

/// Property 1: `inner` lies wholly inside `outer`.
fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let outer = outer.as_ptr_range();
    let inner_range = inner.as_ptr_range();
    inner.is_empty() || (inner_range.start >= outer.start && inner_range.end <= outer.end)
}

fn ethernet_and_arp(bytes: &[u8]) {
    if let Ok((header, payload)) = ethernet::Header::parse(bytes) {
        assert!(inside(bytes, payload));
        let mut out = [0u8; 18];
        let len = header.emit(&mut out).expect("a parsed header emits");
        let (again, rest) = ethernet::Header::parse(&out[..len]).expect("re-parses");
        assert_eq!(again, header);
        assert!(rest.is_empty());
    }
    if let Ok(packet) = arp::Packet::parse(bytes) {
        let mut out = [0u8; arp::PACKET_LEN];
        let len = packet.emit(&mut out).expect("a parsed packet emits");
        assert_eq!(arp::Packet::parse(&out[..len]), Ok(packet));
    }
}

fn ip(bytes: &[u8]) {
    if let Ok(packet) = ipv4::Header::parse(bytes) {
        assert!(inside(bytes, packet.options) && inside(bytes, packet.payload));
        let mut out = vec![0u8; ipv4::MAX_HEADER_LEN + packet.payload.len()];
        let header_len = packet
            .header
            .emit(packet.options, packet.payload.len(), &mut out)
            .expect("a parsed header emits");
        out[header_len..header_len + packet.payload.len()].copy_from_slice(packet.payload);
        let again =
            ipv4::Header::parse(&out[..header_len + packet.payload.len()]).expect("re-parses");
        assert_eq!(
            (again.header, again.options, again.payload),
            (packet.header, packet.options, packet.payload)
        );
    }
    if let Ok(packet) = ipv6::Header::parse(bytes) {
        assert!(inside(bytes, packet.payload));
        let mut out = vec![0u8; ipv6::HEADER_LEN + packet.payload.len()];
        let header_len = packet
            .header
            .emit(packet.payload.len(), &mut out)
            .expect("a parsed header emits");
        out[header_len..].copy_from_slice(packet.payload);
        let again = ipv6::Header::parse(&out).expect("re-parses");
        assert_eq!((again.header, again.payload), (packet.header, packet.payload));

        // 3. The walk stays inside the payload.
        if let Ok(upper) = ipv6::upper_layer(packet.header.next_header, packet.payload) {
            assert!(upper.offset <= packet.payload.len());
            assert!(inside(packet.payload, upper.bytes));
            assert_eq!(upper.bytes.len(), packet.payload.len() - upper.offset);
        }
    }
}

fn transport(bytes: &[u8], v4: Pseudo, v6: Pseudo) {
    for pseudo in [v4, v6] {
        if let Ok(datagram) = udp::Header::parse(bytes, pseudo) {
            assert!(inside(bytes, datagram.payload));
            let mut out = vec![0u8; udp::HEADER_LEN + datagram.payload.len()];
            let len = datagram
                .header
                .emit(datagram.payload, pseudo, &mut out)
                .expect("a parsed datagram emits");
            let again = udp::Header::parse(&out[..len], pseudo).expect("re-parses");
            assert_eq!(
                (again.header, again.payload),
                (datagram.header, datagram.payload)
            );
        }
        if let Ok(segment) = tcp::Header::parse(bytes, pseudo) {
            assert!(inside(bytes, segment.payload));
            let mut out = vec![0u8; tcp::MAX_HEADER_LEN + segment.payload.len()];
            let len = segment
                .header
                .emit(segment.payload, pseudo, &mut out)
                .expect("parsed options fit when written canonically");
            let again = tcp::Header::parse(&out[..len], pseudo).expect("re-parses");
            assert_eq!(again.header, segment.header);
            assert_eq!(again.payload, segment.payload);
        }
    }
}

fn icmp(bytes: &[u8], source: [u8; 16], destination: [u8; 16]) {
    if let Ok(message) = icmpv4::Header::parse(bytes) {
        let mut out = vec![0u8; icmpv4::HEADER_LEN + message.body.len()];
        let len = message
            .header
            .emit(message.body, &mut out)
            .expect("a parsed message emits");
        let again = icmpv4::Header::parse(&out[..len]).expect("re-parses");
        assert_eq!((again.header, again.body), (message.header, message.body));
    }
    let Ok(message) = icmpv6::Message::parse(bytes, source, destination) else {
        return;
    };
    let mut out = vec![0u8; icmpv6::HEADER_LEN + message.body.len()];
    let len = message
        .emit(source, destination, &mut out)
        .expect("a parsed message emits");
    assert_eq!(
        icmpv6::Message::parse(&out[..len], source, destination),
        Ok(message)
    );
    if let Ok(Some(ndp)) = Ndp::parse(&message) {
        let mut body = vec![0u8; message.body.len() + 20];
        let body_len = ndp.emit_body(&mut body).expect("a parsed message emits");
        let rewrapped = icmpv6::Message {
            kind: ndp.kind(),
            code: 0,
            body: &body[..body_len],
        };
        assert_eq!(Ndp::parse(&rewrapped), Ok(Some(ndp)));
    }
}

fuzz_target!(|bytes: &[u8]| {
    let (v4, v6, source, destination) = addresses(bytes);
    ethernet_and_arp(bytes);
    ip(bytes);
    transport(bytes, v4, v6);
    icmp(bytes, source, destination);

    // 4. A checksum in pieces is the checksum of the whole.
    if let Some(&last) = bytes.last() {
        let split = bytes.len() * usize::from(last) / 256;
        let mut pieces = Checksum::new();
        pieces.add_bytes(&bytes[..split]);
        pieces.add_bytes(&bytes[split..]);
        assert_eq!(pieces.finish(), checksum(bytes));
    }
});
