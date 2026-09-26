//! The internet checksum against RFC 1071's worked example and a real header.

use crate::checksum::{Checksum, checksum, ipv4_pseudo_header, ipv6_pseudo_header};

/// RFC 1071 section 3's example: the sum of these bytes is `0xddf2`.
const RFC1071: [u8; 8] = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];

/// An IPv4 header whose checksum field, `0xb861`, is correct.
const IPV4_HEADER: [u8; 20] = [
    0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0xb8, 0x61, 0xc0, 0xa8, 0x00, 0x01,
    0xc0, 0xa8, 0x00, 0xc7,
];

#[test]
fn rfc1071s_example_sums_to_ddf2() {
    assert_eq!(checksum(&RFC1071), !0xddf2);
}

#[test]
fn a_header_verifies_by_summing_to_zero_with_its_checksum() {
    assert_eq!(checksum(&IPV4_HEADER), 0);

    let mut zeroed = IPV4_HEADER;
    zeroed[10] = 0;
    zeroed[11] = 0;
    assert_eq!(
        checksum(&zeroed),
        0xb861,
        "the checksum of the header without it"
    );
}

#[test]
fn pieces_of_any_length_sum_as_one() {
    for split in 0..=IPV4_HEADER.len() {
        for second in split..=IPV4_HEADER.len() {
            let mut sum = Checksum::new();
            sum.add_bytes(&IPV4_HEADER[..split]);
            sum.add_bytes(&IPV4_HEADER[split..second]);
            sum.add_bytes(&[]);
            sum.add_bytes(&IPV4_HEADER[second..]);
            assert_eq!(sum.finish(), 0, "split at {split} and {second}");
        }
    }
}

#[test]
fn an_odd_trailing_byte_is_padded_with_zero() {
    assert_eq!(
        checksum(&[0x12, 0x34, 0x56]),
        checksum(&[0x12, 0x34, 0x56, 0x00])
    );
}

#[test]
fn carries_fold_back_in() {
    // 0xffff + 0xffff = 0x1fffe, which folds to 0xffff and complements to 0.
    assert_eq!(checksum(&[0xff; 4]), 0);
    assert_eq!(checksum(&[]), 0xffff, "nothing sums to zero, complemented");
}

#[test]
fn the_ipv4_pseudo_header_is_the_fields_rfc768_names() {
    let pseudo = ipv4_pseudo_header([192, 168, 0, 1], [192, 168, 0, 199], 17, 0x005f);
    let mut by_hand = Checksum::new();
    by_hand.add_bytes(&[192, 168, 0, 1, 192, 168, 0, 199, 0, 17, 0x00, 0x5f]);
    assert_eq!(pseudo.finish(), by_hand.finish());
}

#[test]
fn the_ipv6_pseudo_header_is_the_fields_rfc8200_names() {
    let source = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let destination = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
    let pseudo = ipv6_pseudo_header(source, destination, 58, 16);
    let mut by_hand = Checksum::new();
    by_hand.add_bytes(&source);
    by_hand.add_bytes(&destination);
    by_hand.add_bytes(&[0, 0, 0, 16, 0, 0, 0, 58]);
    assert_eq!(pseudo.finish(), by_hand.finish());
}
