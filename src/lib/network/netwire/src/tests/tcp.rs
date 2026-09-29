//! TCP headers: negotiated options, the ones refused, and the bounds.

use crate::Error;
use crate::checksum::Pseudo;
use crate::tcp::{Flags, Header, MAX_SACK_BLOCKS, MIN_HEADER_LEN, Options, SackBlock};

const V4: Pseudo = Pseudo::V4 {
    source: [10, 0, 2, 15],
    destination: [93, 184, 216, 34],
};

fn syn() -> Header {
    Header {
        source_port: 49_152,
        destination_port: 80,
        sequence: 0x0102_0304,
        acknowledgment: 0,
        flags: Flags::SYN,
        window: 64_240,
        urgent_pointer: 0,
        options: Options {
            mss: Some(1460),
            window_scale: Some(7),
            sack_permitted: true,
            timestamp: Some((0x1111_2222, 0)),
            ..Options::default()
        },
    }
}

/// A PSH+ACK segment with `options` (a whole number of words) and `payload`,
/// checksummed over `V4`.
fn segment(options: &[u8], payload: &[u8]) -> ([u8; 96], usize) {
    let mut bytes = [0u8; 96];
    let header_len = MIN_HEADER_LEN + options.len();
    let len = header_len + payload.len();
    bytes[0..2].copy_from_slice(&49_152u16.to_be_bytes());
    bytes[2..4].copy_from_slice(&80u16.to_be_bytes());
    bytes[4..8].copy_from_slice(&7u32.to_be_bytes());
    bytes[8..12].copy_from_slice(&9u32.to_be_bytes());
    bytes[12] = u8::try_from(header_len / 4).expect("fits") << 4;
    bytes[13] = 0x18;
    bytes[14..16].copy_from_slice(&1024u16.to_be_bytes());
    bytes[MIN_HEADER_LEN..header_len].copy_from_slice(options);
    bytes[header_len..len].copy_from_slice(payload);
    let mut sum = V4.sum(6, len).expect("fits");
    sum.add_bytes(&bytes[..len]);
    bytes[16..18].copy_from_slice(&sum.finish().to_be_bytes());
    (bytes, len)
}

#[test]
fn a_syn_with_negotiated_options_round_trips() {
    let mut out = [0u8; 64];
    let len = syn().emit(&[], V4, &mut out).expect("emits");
    assert_eq!(len, 40, "19 bytes of options padded to 20");
    let parsed = Header::parse(&out[..len], V4).expect("parses");
    assert_eq!(parsed.header, syn());
    assert!(parsed.payload.is_empty());
    assert_eq!(syn().sequence_len(0), 1, "SYN takes one sequence number");
}

#[test]
fn data_and_sack_blocks_round_trip() {
    let mut header = syn();
    header.flags = Flags::ACK.union(Flags::FIN);
    header.options = Options {
        sack: [
            SackBlock {
                left: 10,
                right: 20,
            },
            SackBlock {
                left: 30,
                right: 40,
            },
            SackBlock {
                left: 50,
                right: 60,
            },
            SackBlock::default(),
        ],
        sack_blocks: 3,
        timestamp: Some((5, 6)),
        ..Options::default()
    };
    let mut out = [0u8; 96];
    let len = header.emit(b"hello", V4, &mut out).expect("emits");
    let parsed = Header::parse(&out[..len], V4).expect("parses");
    assert_eq!(parsed.header, header);
    assert_eq!(parsed.header.options.sack().len(), 3);
    assert_eq!(parsed.payload, b"hello");
    assert_eq!(header.sequence_len(5), 6, "data plus FIN");
}

#[test]
fn hand_written_options_parse_and_unknown_ones_are_skipped() {
    let options = [2, 4, 0x05, 0xb4, 1, 3, 3, 7, 30, 3, 0xff, 4, 2, 0, 0, 0];
    let (bytes, len) = segment(&options, b"x");
    let parsed = Header::parse(&bytes[..len], V4).expect("parses");
    let header = parsed.header;
    assert_eq!(
        (header.options.mss, header.options.window_scale),
        (Some(1460), Some(7))
    );
    assert!(header.options.sack_permitted);
    assert_eq!(header.flags, Flags::PSH.union(Flags::ACK));
    assert_eq!((header.sequence, header.acknowledgment), (7, 9));
    assert_eq!(parsed.payload, b"x");
}

#[test]
fn options_of_the_wrong_length_or_running_past_the_header_are_refused() {
    let cases: [&[u8]; 6] = [
        &[2, 3, 5, 0],  // MSS of three bytes
        &[3, 4, 7, 0],  // window scale of four
        &[5, 3, 0, 0],  // SACK of three
        &[8, 10, 0, 0], // timestamps running past the header
        &[2, 0, 0, 0],  // a length of zero
        &[30],          // a type with no length byte
    ];
    for options in cases {
        let mut padded = [1u8; 4];
        padded[..options.len()].copy_from_slice(options);
        let (bytes, len) = segment(&padded, b"");
        assert!(
            matches!(Header::parse(&bytes[..len], V4), Err(Error::Malformed(_))),
            "{options:?}"
        );
    }
}

#[test]
fn a_bad_offset_a_short_segment_and_a_bad_checksum_are_refused() {
    let (mut bytes, len) = segment(&[], b"data");
    bytes[12] = 0x40;
    assert!(matches!(
        Header::parse(&bytes[..len], V4),
        Err(Error::Malformed(_))
    ));

    let (bytes, _) = segment(&[1, 1, 1, 1], b"");
    assert_eq!(Header::parse(&bytes[..22], V4), Err(Error::Truncated));

    let (mut bytes, len) = segment(&[], b"data");
    bytes[len - 1] ^= 1;
    assert_eq!(Header::parse(&bytes[..len], V4), Err(Error::BadChecksum));
}

#[test]
fn emit_refuses_too_many_options_or_blocks_and_a_short_buffer() {
    let mut crowded = syn();
    crowded.options.sack_blocks = MAX_SACK_BLOCKS;
    assert!(matches!(
        crowded.emit(&[], V4, &mut [0u8; 96]),
        Err(Error::Malformed(_))
    ));
    crowded.options = Options {
        sack_blocks: MAX_SACK_BLOCKS + 1,
        ..Options::default()
    };
    assert!(matches!(
        crowded.emit(&[], V4, &mut [0u8; 96]),
        Err(Error::Malformed(_))
    ));
    assert_eq!(syn().emit(&[], V4, &mut [0u8; 39]), Err(Error::NoSpace));
}
