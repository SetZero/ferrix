//! UDP datagrams over both IP versions, their checksums and their bounds.

use crate::Error;
use crate::checksum::Pseudo;
use crate::udp::{HEADER_LEN, Header};

const V4: Pseudo = Pseudo::V4 {
    source: [10, 0, 2, 15],
    destination: [10, 0, 2, 3],
};

const V6: Pseudo = Pseudo::V6 {
    source: [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
    destination: [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
};

const HEADER: Header = Header {
    source_port: 49_152,
    destination_port: 53,
};

#[test]
fn datagrams_round_trip_over_both_versions() {
    for pseudo in [V4, V6] {
        for payload in [&b""[..], b"x", b"hello, resolver"] {
            let mut out = [0u8; 64];
            let len = HEADER.emit(payload, pseudo, &mut out).expect("emits");
            assert_eq!(len, HEADER_LEN + payload.len());
            let datagram = Header::parse(&out[..len], pseudo).expect("parses");
            assert_eq!(datagram.header, HEADER);
            assert_eq!(datagram.payload, payload);
            assert_ne!(datagram.checksum, 0, "a checksum is always sent");
        }
    }
}

#[test]
fn a_corrupted_byte_or_the_wrong_addresses_fail_the_checksum() {
    let mut out = [0u8; 32];
    let len = HEADER.emit(b"payload", V4, &mut out).expect("emits");
    let mut corrupted = out;
    corrupted[HEADER_LEN] ^= 0x01;
    assert_eq!(
        Header::parse(&corrupted[..len], V4),
        Err(Error::BadChecksum)
    );
    let elsewhere = Pseudo::V4 {
        source: [10, 0, 2, 16],
        destination: [10, 0, 2, 3],
    };
    assert_eq!(
        Header::parse(&out[..len], elsewhere),
        Err(Error::BadChecksum)
    );
}

#[test]
fn a_zero_checksum_is_none_over_ipv4_and_refused_over_ipv6() {
    let mut out = [0u8; 16];
    let len = HEADER.emit(b"hi", V4, &mut out).expect("emits");
    out[6] = 0;
    out[7] = 0;
    let datagram = Header::parse(&out[..len], V4).expect("no checksum over IPv4");
    assert_eq!((datagram.checksum, datagram.payload), (0, &b"hi"[..]));
    assert!(matches!(
        Header::parse(&out[..len], V6),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn the_length_field_bounds_the_data_and_is_checked() {
    let mut out = [0u8; 32];
    let len = HEADER.emit(b"data", V4, &mut out).expect("emits");
    assert_eq!(
        Header::parse(&out, V4).expect("parses").payload,
        b"data",
        "trailing bytes ignored"
    );
    for cut in 0..len {
        assert_eq!(
            Header::parse(&out[..cut], V4),
            Err(Error::Truncated),
            "cut at {cut}"
        );
    }
    let mut short = out;
    short[4] = 0;
    short[5] = 7;
    assert!(matches!(
        Header::parse(&short, V4),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn emit_refuses_a_short_buffer() {
    assert_eq!(
        HEADER.emit(b"data", V4, &mut [0u8; 11]),
        Err(Error::NoSpace)
    );
}
