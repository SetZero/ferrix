//! Ethernet II headers, tagged and untagged, and the inputs they refuse.

use crate::Error;
use crate::ethernet::{BROADCAST, HEADER_LEN, Header, VLAN_TAG_LEN, VlanTag, ethertype};

const SOURCE: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

#[test]
fn an_untagged_header_parses_and_leaves_the_payload() {
    let frame = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 0x86, 0xdd, 0x60,
    ];
    let (header, payload) = Header::parse(&frame).expect("parses");
    assert_eq!(header.destination, BROADCAST);
    assert_eq!(header.source, SOURCE);
    assert_eq!(header.vlan, None);
    assert_eq!(header.ethertype, ethertype::IPV6);
    assert_eq!(payload, &[0x60]);
    assert_eq!(header.len(), HEADER_LEN);
}

#[test]
fn a_tagged_header_parses_its_priority_eligibility_and_id() {
    let frame = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 0x81, 0x00, 0xb0,
        0x64, 0x08, 0x00, 0x45,
    ];
    let (header, payload) = Header::parse(&frame).expect("parses");
    assert_eq!(
        header.vlan,
        Some(VlanTag {
            priority: 5,
            drop_eligible: true,
            id: 100
        })
    );
    assert_eq!(header.ethertype, ethertype::IPV4);
    assert_eq!(payload, &[0x45]);
    assert_eq!(header.len(), HEADER_LEN + VLAN_TAG_LEN);
}

#[test]
fn headers_round_trip_through_emit() {
    for vlan in [
        None,
        Some(VlanTag {
            priority: 0,
            drop_eligible: false,
            id: 0,
        }),
        Some(VlanTag {
            priority: 7,
            drop_eligible: true,
            id: 4095,
        }),
    ] {
        let header = Header {
            destination: BROADCAST,
            source: SOURCE,
            vlan,
            ethertype: ethertype::ARP,
        };
        let mut out = [0u8; 32];
        let written = header.emit(&mut out).expect("emits");
        assert_eq!(written, header.len());
        let (parsed, payload) = Header::parse(&out[..written]).expect("parses back");
        assert_eq!(parsed, header);
        assert!(payload.is_empty());
    }
}

#[test]
fn every_cut_of_a_header_is_truncated() {
    let frame = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 0x81, 0x00, 0x00,
        0x01, 0x08, 0x06,
    ];
    for cut in 0..frame.len() {
        assert_eq!(
            Header::parse(&frame[..cut]),
            Err(Error::Truncated),
            "cut at {cut}"
        );
    }
}

#[test]
fn a_length_field_and_stacked_tags_are_refused() {
    let mut frame = [0u8; 18];
    frame[12..14].copy_from_slice(&0x05dcu16.to_be_bytes());
    assert!(
        matches!(Header::parse(&frame), Err(Error::Malformed(_))),
        "802.3 length"
    );

    frame[12..14].copy_from_slice(&0x8100u16.to_be_bytes());
    frame[16..18].copy_from_slice(&0x8100u16.to_be_bytes());
    assert!(
        matches!(Header::parse(&frame), Err(Error::Malformed(_))),
        "QinQ"
    );
}

#[test]
fn emit_refuses_a_short_buffer_and_values_out_of_range() {
    let header = Header {
        destination: BROADCAST,
        source: SOURCE,
        vlan: None,
        ethertype: ethertype::IPV4,
    };
    assert_eq!(header.emit(&mut [0u8; 13]), Err(Error::NoSpace));

    let bad_id = Header {
        vlan: Some(VlanTag {
            priority: 0,
            drop_eligible: false,
            id: 4096,
        }),
        ..header
    };
    assert!(matches!(
        bad_id.emit(&mut [0u8; 32]),
        Err(Error::Malformed(_))
    ));

    let a_tag_as_type = Header {
        ethertype: ethertype::VLAN,
        ..header
    };
    assert!(matches!(
        a_tag_as_type.emit(&mut [0u8; 32]),
        Err(Error::Malformed(_))
    ));
}
