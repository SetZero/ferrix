//! `ICMPv4` messages: echo, the checksum, and the bounds.

use crate::Error;
use crate::icmpv4::{HEADER_LEN, Header, kind};

#[test]
fn an_echo_request_round_trips_with_its_identifier_and_sequence() {
    let header = Header::echo(kind::ECHO_REQUEST, 0x1234, 7);
    let mut out = [0u8; 32];
    let len = header.emit(b"ping data", &mut out).expect("emits");
    assert_eq!(len, HEADER_LEN + 9);
    let message = Header::parse(&out[..len]).expect("parses");
    assert_eq!(message.header, header);
    assert_eq!(message.header.echo_fields(), Some((0x1234, 7)));
    assert_eq!(message.body, b"ping data");
}

#[test]
fn only_echo_messages_have_echo_fields() {
    assert_eq!(
        Header::echo(kind::ECHO_REPLY, 1, 2).echo_fields(),
        Some((1, 2))
    );
    let unreachable = Header {
        kind: kind::DESTINATION_UNREACHABLE,
        code: 3,
        rest: [0; 4],
    };
    assert_eq!(unreachable.echo_fields(), None);
    let odd_code = Header {
        code: 1,
        ..Header::echo(kind::ECHO_REQUEST, 1, 2)
    };
    assert_eq!(odd_code.echo_fields(), None);
}

#[test]
fn a_corrupted_message_fails_its_checksum() {
    let mut out = [0u8; 16];
    let len = Header::echo(kind::ECHO_REQUEST, 1, 1)
        .emit(b"abcd", &mut out)
        .expect("emits");
    out[HEADER_LEN] ^= 0x40;
    assert_eq!(Header::parse(&out[..len]), Err(Error::BadChecksum));
}

#[test]
fn a_short_message_is_truncated_and_a_short_buffer_refused() {
    for cut in 0..HEADER_LEN {
        assert_eq!(Header::parse(&[0u8; 8][..cut]), Err(Error::Truncated));
    }
    let header = Header::echo(kind::ECHO_REQUEST, 1, 1);
    assert_eq!(header.emit(b"abcd", &mut [0u8; 11]), Err(Error::NoSpace));
}
