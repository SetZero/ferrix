//! Walking the attributes after a message's fixed body, and reading the
//! payload shapes `ip` uses.

use alloc::vec::Vec;

use ferrix_linux_abi::netlink::{
    IFA_ADDRESS, IFLA_ADDRESS, IFLA_IFNAME, IFLA_MTU, IFLA_OPERSTATE, NLA_F_NESTED, NlAttr,
};

use crate::{Address, Attributes, Error};

/// One attribute, padded as the next one's start requires.
fn attribute(kind: u16, payload: &[u8]) -> Vec<u8> {
    let header = NlAttr {
        len: u16::try_from(NlAttr::SIZE + payload.len()).expect("a test attribute is small"),
        kind,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend_from_slice(payload);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
}

/// What a link message carries: a name, a hardware address and an MTU.
fn link_attributes() -> Vec<u8> {
    let mut bytes = attribute(IFLA_IFNAME, b"eth0\0");
    bytes.extend(attribute(
        IFLA_ADDRESS,
        &[0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
    ));
    bytes.extend(attribute(IFLA_MTU, &1500_u32.to_le_bytes()));
    bytes
}

#[test]
fn the_attributes_of_a_link_walk_to_exactly_those_attributes() {
    let bytes = link_attributes();
    let kinds: Vec<u16> = Attributes::new(&bytes)
        .map(|attribute| attribute.expect("well formed").kind())
        .collect();
    assert_eq!(kinds, [IFLA_IFNAME, IFLA_ADDRESS, IFLA_MTU]);
}

#[test]
fn a_name_is_read_without_its_terminator() {
    let bytes = link_attributes();
    let name = Attributes::new(&bytes)
        .find(IFLA_IFNAME)
        .expect("the name is there");
    assert_eq!(name.as_name(), b"eth0");
    assert_eq!(name.as_bytes(), b"eth0\0", "the payload keeps the nul");
}

#[test]
fn a_name_without_a_terminator_is_the_whole_payload() {
    let bytes = attribute(IFLA_IFNAME, b"eth0");
    let name = Attributes::new(&bytes).find(IFLA_IFNAME).expect("there");
    assert_eq!(name.as_name(), b"eth0");
}

#[test]
fn a_u32_is_read_in_host_order() {
    let bytes = link_attributes();
    let mtu = Attributes::new(&bytes).find(IFLA_MTU).expect("the mtu");
    assert_eq!(mtu.as_u32(), Some(1500));
    assert_eq!(mtu.as_u8(), None, "four bytes are not one");
}

#[test]
fn a_u8_is_read_only_from_a_payload_of_one_byte() {
    let bytes = attribute(IFLA_OPERSTATE, &[6]);
    let state = Attributes::new(&bytes).find(IFLA_OPERSTATE).expect("there");
    assert_eq!(state.as_u8(), Some(6));
    assert_eq!(state.as_u32(), None);
}

#[test]
fn a_hardware_address_is_read_as_the_bytes_it_is() {
    let bytes = link_attributes();
    let hardware = Attributes::new(&bytes).find(IFLA_ADDRESS).expect("there");
    assert_eq!(hardware.as_bytes(), &[0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    assert_eq!(hardware.as_address(), None, "six bytes are no IP address");
}

#[test]
fn an_address_is_read_by_its_length_alone() {
    let four = attribute(IFA_ADDRESS, &[10, 0, 2, 15]);
    assert_eq!(
        Attributes::new(&four)
            .find(IFA_ADDRESS)
            .and_then(|attribute| attribute.as_address()),
        Some(Address::V4([10, 0, 2, 15]))
    );
    let six = attribute(
        IFA_ADDRESS,
        &[0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
    );
    assert_eq!(
        Attributes::new(&six)
            .find(IFA_ADDRESS)
            .and_then(|attribute| attribute.as_address()),
        Some(Address::V6([
            0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1
        ]))
    );
}

#[test]
fn the_nested_bit_is_not_part_of_an_attributes_number() {
    let bytes = attribute(IFLA_MTU | NLA_F_NESTED, &1500_u32.to_le_bytes());
    let attribute = Attributes::new(&bytes)
        .next()
        .expect("one")
        .expect("well formed");
    assert_eq!(attribute.kind(), IFLA_MTU);
    assert_eq!(attribute.header.kind, IFLA_MTU | NLA_F_NESTED);
}

#[test]
fn an_attribute_claiming_a_length_past_the_end_is_refused() {
    let mut bytes = attribute(IFLA_MTU, &1500_u32.to_le_bytes());
    let liar = NlAttr {
        len: u16::try_from(bytes.len() + 4).expect("small"),
        kind: IFLA_IFNAME,
    };
    bytes.extend_from_slice(&liar.to_bytes());
    bytes.extend_from_slice(&[0_u8; 4]);
    let walked: Vec<_> = Attributes::new(&bytes).collect();
    assert_eq!(walked.len(), 2);
    assert!(walked[0].is_ok());
    assert_eq!(walked[1], Err(Error::Truncated));
}

#[test]
fn an_attribute_length_below_its_header_is_refused() {
    for len in 0..NlAttr::SIZE {
        let short = NlAttr {
            len: u16::try_from(len).expect("small"),
            kind: IFLA_MTU,
        };
        let mut bytes = short.to_bytes().to_vec();
        bytes.extend_from_slice(&[0_u8; 8]);
        assert_eq!(
            Attributes::new(&bytes).next(),
            Some(Err(Error::Truncated)),
            "an attribute of {len} bytes should be refused"
        );
    }
}

#[test]
fn an_attribute_with_no_payload_is_allowed() {
    let bytes = attribute(IFLA_MTU, &[]);
    let attribute = Attributes::new(&bytes)
        .next()
        .expect("one")
        .expect("well formed");
    assert_eq!(attribute.as_bytes(), b"");
    assert_eq!(attribute.as_u32(), None);
}

#[test]
fn a_tail_too_short_to_hold_an_attribute_header_is_refused() {
    let mut bytes = attribute(IFLA_MTU, &1500_u32.to_le_bytes());
    bytes.extend_from_slice(&[0, 0]);
    let walked: Vec<_> = Attributes::new(&bytes).collect();
    assert_eq!(walked.len(), 2);
    assert_eq!(walked[1], Err(Error::Truncated));
}

#[test]
fn looking_for_an_attribute_that_is_not_there_answers_nothing() {
    let bytes = link_attributes();
    assert_eq!(Attributes::new(&bytes).find(IFLA_OPERSTATE), None);
}

#[test]
fn a_repeated_attribute_is_read_as_its_first() {
    let mut bytes = attribute(IFLA_MTU, &1500_u32.to_le_bytes());
    bytes.extend(attribute(IFLA_MTU, &9000_u32.to_le_bytes()));
    assert_eq!(
        Attributes::new(&bytes)
            .find(IFLA_MTU)
            .and_then(|attribute| attribute.as_u32()),
        Some(1500)
    );
}

#[test]
fn the_attributes_of_a_message_start_after_its_aligned_body() {
    use ferrix_linux_abi::netlink::{NlMsgHdr, RTM_NEWLINK};

    use crate::Messages;

    // An `ndmsg` is twelve bytes, which is already aligned; an `ifaddrmsg` is
    // eight. Both are walked from the same payload to show the offset is the
    // body's and not a guess.
    let attributes = link_attributes();
    let mut payload = Vec::from([0_u8; 12]);
    payload.extend_from_slice(&attributes);
    let header = NlMsgHdr {
        len: u32::try_from(NlMsgHdr::SIZE + payload.len()).expect("small"),
        kind: RTM_NEWLINK,
        flags: 0,
        seq: 1,
        pid: 0,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    let message = Messages::new(&bytes)
        .next()
        .expect("one")
        .expect("well formed");
    let kinds: Vec<u16> = message
        .attributes(12)
        .map(|attribute| attribute.expect("well formed").kind())
        .collect();
    assert_eq!(kinds, [IFLA_IFNAME, IFLA_ADDRESS, IFLA_MTU]);
    assert_eq!(
        message.attributes(payload.len() + 8).count(),
        0,
        "a body past the end leaves no attributes rather than reading some"
    );
}
