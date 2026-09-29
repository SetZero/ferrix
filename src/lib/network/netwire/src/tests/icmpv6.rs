//! `ICMPv6` messages and Neighbor Discovery over them.

use crate::Error;
use crate::icmpv6::{Message, kind};
use crate::ndp::{Ndp, Options, option_kind};

const HOST: [u8; 16] = [
    0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x50, 0x54, 0, 0xff, 0xfe, 0x12, 0x34, 0x56,
];
const ROUTER: [u8; 16] = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// Emit `message` from `source` to `destination` and parse it back.
fn round_trip<'a>(
    message: &Message<'_>,
    source: [u8; 16],
    destination: [u8; 16],
    out: &'a mut [u8],
) -> Message<'a> {
    let len = message.emit(source, destination, out).expect("emits");
    Message::parse(&out[..len], source, destination).expect("parses")
}

#[test]
fn an_echo_round_trips_and_needs_the_addresses_it_was_sent_with() {
    let body = [0x12, 0x34, 0x00, 0x01, b'h', b'i'];
    let echo = Message {
        kind: kind::ECHO_REQUEST,
        code: 0,
        body: &body,
    };
    let mut out = [0u8; 16];
    assert_eq!(round_trip(&echo, HOST, ROUTER, &mut out), echo);

    let len = echo.emit(HOST, ROUTER, &mut out).expect("emits");
    let elsewhere = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3];
    assert_eq!(
        Message::parse(&out[..len], HOST, elsewhere),
        Err(Error::BadChecksum),
        "another destination changes the pseudo-header"
    );
    // One's-complement addition does not care about order, so the same two
    // addresses the other way round sum the same: a reply's checksum covers
    // exactly the addresses of the message it answers.
    assert_eq!(Message::parse(&out[..len], ROUTER, HOST), Ok(echo));
    assert_eq!(
        Message::parse(&out[..3], HOST, ROUTER),
        Err(Error::Truncated)
    );
}

#[test]
fn a_neighbor_solicitation_carries_its_target_and_source_link_layer_address() {
    let mut options = [0u8; 8];
    options[..2].copy_from_slice(&[option_kind::SOURCE_LINK_LAYER, 1]);
    options[2..].copy_from_slice(&MAC);
    let solicitation = Ndp::NeighborSolicitation {
        target: ROUTER,
        options: Options::new(&options).expect("options"),
    };
    let mut body = [0u8; 32];
    let body_len = solicitation.emit_body(&mut body).expect("emits body");
    assert_eq!(body_len, 28);
    let message = Message {
        kind: solicitation.kind(),
        code: 0,
        body: &body[..body_len],
    };
    let mut out = [0u8; 40];
    let parsed = round_trip(&message, HOST, ROUTER, &mut out);
    let ndp = Ndp::parse(&parsed).expect("valid").expect("is NDP");
    assert_eq!(ndp, solicitation);
    let Ndp::NeighborSolicitation { target, options } = ndp else {
        panic!("not a solicitation: {ndp:?}");
    };
    assert_eq!(target, ROUTER);
    assert_eq!(
        options.link_layer(option_kind::SOURCE_LINK_LAYER),
        Some(MAC)
    );
    assert_eq!(options.link_layer(option_kind::TARGET_LINK_LAYER), None);
}

#[test]
fn a_neighbor_advertisement_keeps_its_three_flags() {
    let advertisement = Ndp::NeighborAdvertisement {
        router: true,
        solicited: true,
        override_cache: false,
        target: HOST,
        options: Options::new(&[]).expect("empty"),
    };
    let mut body = [0u8; 20];
    assert_eq!(advertisement.emit_body(&mut body), Ok(20));
    assert_eq!(body[0], 0xC0);
    let message = Message {
        kind: kind::NEIGHBOR_ADVERTISEMENT,
        code: 0,
        body: &body,
    };
    assert_eq!(Ndp::parse(&message), Ok(Some(advertisement)));
}

#[test]
fn a_router_advertisement_yields_its_prefix_and_mtu() {
    let mut options = [0u8; 40];
    options[..16].copy_from_slice(&[
        option_kind::PREFIX_INFORMATION,
        4,
        64,
        0xC0,
        0,
        0,
        0x0e,
        0x10,
        0,
        0,
        0x07,
        0x08,
        0,
        0,
        0,
        0,
    ]);
    options[16..32].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    options[32..40].copy_from_slice(&[option_kind::MTU, 1, 0, 0, 0, 0, 0x05, 0xdc]);
    let advertisement = Ndp::RouterAdvertisement {
        managed: false,
        other: true,
        options: Options::new(&options).expect("options"),
    };
    let mut body = [0u8; 44];
    let len = advertisement.emit_body(&mut body).expect("emits body");
    let message = Message {
        kind: kind::ROUTER_ADVERTISEMENT,
        code: 0,
        body: &body[..len],
    };
    let Some(Ndp::RouterAdvertisement {
        managed,
        other,
        options,
    }) = Ndp::parse(&message).expect("valid")
    else {
        panic!("not an advertisement");
    };
    assert!(!managed && other);
    assert_eq!(options.mtu(), Some(1500));
    let prefix = options.prefix().expect("a prefix option");
    assert_eq!(prefix.prefix_len, 64);
    assert!(prefix.on_link && prefix.autonomous);
    assert_eq!(
        (prefix.valid_lifetime, prefix.preferred_lifetime),
        (3600, 1800)
    );
    assert_eq!(&prefix.prefix[..4], &[0x20, 0x01, 0x0d, 0xb8]);
    assert_eq!(options.iter().count(), 2);
}

#[test]
fn a_zero_length_or_overrunning_option_a_bad_code_and_a_short_body_are_refused() {
    assert!(matches!(
        Options::new(&[1, 0, 0, 0, 0, 0, 0, 0]),
        Err(Error::Malformed(_))
    ));
    assert_eq!(
        Options::new(&[1, 2, 0, 0, 0, 0, 0, 0]),
        Err(Error::Truncated)
    );

    let body = [0u8; 20];
    let odd_code = Message {
        kind: kind::NEIGHBOR_SOLICITATION,
        code: 1,
        body: &body,
    };
    assert!(matches!(Ndp::parse(&odd_code), Err(Error::Malformed(_))));
    let short = Message {
        kind: kind::NEIGHBOR_SOLICITATION,
        code: 0,
        body: &body[..19],
    };
    assert_eq!(Ndp::parse(&short), Err(Error::Truncated));
    let echo = Message {
        kind: kind::ECHO_REPLY,
        code: 0,
        body: &body,
    };
    assert_eq!(Ndp::parse(&echo), Ok(None), "not Neighbor Discovery");
}
