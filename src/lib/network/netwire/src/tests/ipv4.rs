//! IPv4 headers: a real one, options, the refusals, and emit writing the same
//! bytes a parse read.

use crate::Error;
use crate::checksum::checksum;
use crate::ipv4::{Header, MIN_HEADER_LEN, protocol};

/// The header of a 115-byte UDP packet from 192.168.0.1 to 192.168.0.199, whose
/// checksum field `0xb861` is correct.
const HEADER: [u8; 20] = [
    0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0xb8, 0x61, 0xc0, 0xa8, 0x00, 0x01,
    0xc0, 0xa8, 0x00, 0xc7,
];

/// `HEADER` followed by its 95 bytes of payload.
fn packet() -> [u8; 115] {
    let mut bytes = [0xAAu8; 115];
    bytes[..20].copy_from_slice(&HEADER);
    bytes
}

/// Recompute the header checksum of a header edited by a test.
fn fix_checksum(header: &mut [u8]) {
    header[10] = 0;
    header[11] = 0;
    let sum = checksum(header);
    header[10..12].copy_from_slice(&sum.to_be_bytes());
}

#[test]
fn a_real_header_parses_into_its_fields() {
    let bytes = packet();
    let parsed = Header::parse(&bytes).expect("parses");
    let header = parsed.header;
    assert_eq!((header.dscp, header.ecn), (0, 0));
    assert_eq!(header.identification, 0);
    assert!(header.dont_fragment && !header.more_fragments);
    assert_eq!(header.fragment_offset, 0);
    assert!(!header.is_fragment());
    assert_eq!((header.ttl, header.protocol), (64, protocol::UDP));
    assert_eq!(header.source, [192, 168, 0, 1]);
    assert_eq!(header.destination, [192, 168, 0, 199]);
    assert!(parsed.options.is_empty());
    assert_eq!(parsed.payload.len(), 95);
}

#[test]
fn bytes_past_the_total_length_are_not_payload() {
    let mut padded = [0u8; 130];
    padded[..115].copy_from_slice(&packet());
    assert_eq!(Header::parse(&padded).expect("parses").payload.len(), 95);
}

#[test]
fn emit_writes_the_bytes_a_parse_read() {
    let bytes = packet();
    let header = Header::parse(&bytes).expect("parses").header;
    let mut out = [0u8; 20];
    assert_eq!(header.emit(&[], 95, &mut out), Ok(MIN_HEADER_LEN));
    assert_eq!(out, HEADER, "checksum included");
}

#[test]
fn options_are_walked_kept_and_round_trip() {
    let mut bytes = [0u8; 28];
    bytes[..20].copy_from_slice(&HEADER);
    bytes[0] = 0x46;
    bytes[3] = 28;
    bytes[20..24].copy_from_slice(&[0x01, 0x01, 0x01, 0x00]);
    fix_checksum(&mut bytes[..24]);
    let parsed = Header::parse(&bytes).expect("parses");
    assert_eq!(parsed.options, &[0x01, 0x01, 0x01, 0x00]);
    assert_eq!(parsed.payload.len(), 4);

    let mut out = [0u8; 24];
    assert_eq!(parsed.header.emit(parsed.options, 4, &mut out), Ok(24));
    assert_eq!(out, bytes[..24]);
}

#[test]
fn a_corrupted_header_fails_its_checksum() {
    let mut bytes = packet();
    bytes[8] = 63;
    assert_eq!(Header::parse(&bytes), Err(Error::BadChecksum));
}

#[test]
fn every_cut_short_of_the_total_length_is_truncated() {
    let bytes = packet();
    for cut in 0..bytes.len() {
        assert_eq!(
            Header::parse(&bytes[..cut]),
            Err(Error::Truncated),
            "cut at {cut}"
        );
    }
}

#[test]
fn a_wrong_version_length_flag_or_option_is_refused() {
    let mut cases: [[u8; 24]; 5] = [[0; 24]; 5];
    for case in &mut cases {
        case[..20].copy_from_slice(&HEADER);
        case[3] = 24;
    }
    cases[0][0] = 0x65; // version 6
    cases[1][0] = 0x44; // four words
    cases[2][3] = 19; // total shorter than the header
    cases[3][6] = 0xC0; // the reserved flag
    cases[4][0] = 0x46; // options: an option longer than the header
    cases[4][20..24].copy_from_slice(&[0x07, 0x09, 0x00, 0x00]);
    for (index, case) in cases.iter_mut().enumerate() {
        let header_len = if index == 4 { 24 } else { 20 };
        fix_checksum(&mut case[..header_len]);
        assert!(
            matches!(Header::parse(case), Err(Error::Malformed(_))),
            "case {index}: {:?}",
            Header::parse(case)
        );
    }
}

#[test]
fn emit_refuses_bad_options_values_and_space() {
    let header = Header::parse(&packet()).expect("parses").header;
    let mut out = [0u8; 64];
    assert!(matches!(
        header.emit(&[1, 1, 1], 0, &mut out),
        Err(Error::Malformed(_))
    ));
    assert!(matches!(
        header.emit(&[0; 44], 0, &mut out),
        Err(Error::Malformed(_))
    ));
    let bad_offset = Header {
        fragment_offset: 0x2000,
        ..header
    };
    assert!(matches!(
        bad_offset.emit(&[], 0, &mut out),
        Err(Error::Malformed(_))
    ));
    assert!(matches!(
        header.emit(&[], 65_516, &mut out),
        Err(Error::Malformed(_))
    ));
    assert_eq!(header.emit(&[], 0, &mut [0u8; 19]), Err(Error::NoSpace));
}
