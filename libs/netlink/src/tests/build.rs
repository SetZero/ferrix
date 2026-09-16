//! Building messages into a caller's buffer: the lengths, the padding, and
//! the refusal when there is no room.

use ferrix_linux_abi::netlink::{
    IFLA_ADDRESS, IFLA_IFNAME, IFLA_MTU, NLM_F_MULTI, NLMSG_DONE, NLMSG_ERROR, NlMsgErr, NlMsgHdr,
    RTM_GETLINK, RTM_NEWLINK, nlmsg_align,
};

use crate::{Address, Attr, Error, Value, Writer};

/// The header a reply carries, before the writer fills in its length.
fn reply(kind: u16) -> NlMsgHdr {
    NlMsgHdr {
        len: 0,
        kind,
        flags: NLM_F_MULTI,
        seq: 42,
        pid: 7,
    }
}

#[test]
fn a_message_declares_the_length_it_was_written_with() {
    let mut buffer = [0_u8; 256];
    let mut writer = Writer::new(&mut buffer);
    let attributes = [Attr::new(IFLA_MTU, Value::U32(1500))];
    let written = writer
        .message(reply(RTM_NEWLINK), &[0_u8; 16], &attributes)
        .expect("there is room");
    assert_eq!(written, 16 + 16 + 8, "header, ifinfomsg, one u32 attribute");
    let header = NlMsgHdr::from_bytes(writer.written()).expect("a header was written");
    assert_eq!(usize::try_from(header.len), Ok(written));
    assert_eq!(header.kind, RTM_NEWLINK);
    assert_eq!(header.seq, 42);
    assert_eq!(header.pid, 7);
}

#[test]
fn the_writer_counts_what_it_has_written() {
    let mut buffer = [0_u8; 256];
    let mut writer = Writer::new(&mut buffer);
    assert!(writer.is_empty());
    let first = writer
        .message(reply(RTM_NEWLINK), &[0_u8; 16], &[])
        .expect("room");
    assert_eq!(writer.len(), first);
    let second = writer
        .message(reply(RTM_NEWLINK), &[0_u8; 16], &[])
        .expect("room");
    assert_eq!(writer.len(), first + second);
    assert!(!writer.is_empty());
}

#[test]
fn an_attribute_is_padded_and_the_next_one_starts_aligned() {
    let mut buffer = [0_u8; 256];
    let mut writer = Writer::new(&mut buffer);
    // Five bytes of name plus its terminator is six: the next attribute
    // starts two bytes later.
    let attributes = [
        Attr::new(IFLA_IFNAME, Value::Name(b"wlan0")),
        Attr::new(IFLA_MTU, Value::U32(1500)),
    ];
    let written = writer
        .message(reply(RTM_NEWLINK), &[0_u8; 16], &attributes)
        .expect("room");
    assert_eq!(written, 16 + 16 + nlmsg_align(4 + 6) + 8);
}

#[test]
fn a_body_that_is_not_aligned_is_padded_and_the_padding_is_counted() {
    let mut buffer = [0_u8; 256];
    let mut writer = Writer::new(&mut buffer);
    let attributes = [Attr::new(IFLA_MTU, Value::U32(1500))];
    let written = writer
        .message(reply(RTM_NEWLINK), &[1, 2, 3], &attributes)
        .expect("room");
    assert_eq!(written, 16 + 4 + 8, "the three-byte body takes four");
    let header = NlMsgHdr::from_bytes(writer.written()).expect("a header");
    assert_eq!(
        usize::try_from(header.len),
        Ok(written),
        "the length counts the body's padding, where nlmsg_end leaves it"
    );
    assert_eq!(
        writer.written().get(19),
        Some(&0),
        "the padding is zero, not whatever the buffer held"
    );
    assert_eq!(
        writer.written().get(20..24),
        Some([8, 0, 4, 0].as_slice()),
        "and the attribute starts after it"
    );
}

#[test]
fn the_padding_is_zeroed_even_when_the_buffer_was_not() {
    let mut buffer = [0xFF_u8; 64];
    let mut writer = Writer::new(&mut buffer);
    let _written = writer.message(reply(RTM_NEWLINK), &[1], &[]).expect("room");
    assert_eq!(writer.written().get(17..20), Some([0, 0, 0].as_slice()));
}

#[test]
fn a_buffer_with_no_room_is_refused_and_nothing_is_written() {
    let mut buffer = [0_u8; 24];
    let mut writer = Writer::new(&mut buffer);
    assert_eq!(
        writer.message(reply(RTM_NEWLINK), &[0_u8; 16], &[]),
        Err(Error::NoSpace)
    );
    assert_eq!(writer.len(), 0, "a refused message leaves no half of one");
    assert!(writer.written().is_empty());
}

#[test]
fn a_dump_that_runs_out_of_room_keeps_the_messages_that_fitted() {
    let mut buffer = [0_u8; 40];
    let mut writer = Writer::new(&mut buffer);
    let first = writer
        .message(reply(RTM_NEWLINK), &[0_u8; 16], &[])
        .expect("the first fits");
    assert_eq!(
        writer.message(reply(RTM_NEWLINK), &[0_u8; 16], &[]),
        Err(Error::NoSpace)
    );
    assert_eq!(writer.len(), first);
}

#[test]
fn an_attribute_that_does_not_fit_is_refused_whole() {
    let mut buffer = [0_u8; 36];
    let mut writer = Writer::new(&mut buffer);
    let attributes = [Attr::new(IFLA_IFNAME, Value::Name(b"eth0"))];
    assert_eq!(
        writer.message(reply(RTM_NEWLINK), &[0_u8; 16], &attributes),
        Err(Error::NoSpace)
    );
    assert_eq!(writer.len(), 0);
}

#[test]
fn an_error_carries_the_negative_errno_and_the_request_it_answers() {
    let mut buffer = [0_u8; 64];
    let mut writer = Writer::new(&mut buffer);
    let request = NlMsgHdr {
        len: 32,
        kind: RTM_GETLINK,
        flags: 1,
        seq: 99,
        pid: 3,
    };
    let written = writer.error(request, 95, 3).expect("room");
    assert_eq!(written, 16 + NlMsgErr::SIZE);
    let header = NlMsgHdr::from_bytes(writer.written()).expect("a header");
    assert_eq!(header.kind, NLMSG_ERROR);
    assert_eq!(header.seq, 99);
    let body = NlMsgErr::from_bytes(writer.written().get(16..).expect("a body")).expect("a body");
    assert_eq!(body.error, -95, "EOPNOTSUPP, as Linux carries it");
    assert_eq!(body.msg, request, "the request's header is echoed");
}

#[test]
fn an_acknowledgement_is_an_error_of_zero() {
    let mut buffer = [0_u8; 64];
    let mut writer = Writer::new(&mut buffer);
    let request = reply(RTM_GETLINK);
    let _written = writer.error(request, 0, 3).expect("room");
    let body = NlMsgErr::from_bytes(writer.written().get(16..).expect("a body")).expect("a body");
    assert_eq!(body.error, 0);
}

#[test]
fn the_end_of_a_dump_is_a_multipart_done() {
    let mut buffer = [0_u8; 64];
    let mut writer = Writer::new(&mut buffer);
    let request = reply(RTM_GETLINK);
    let written = writer.done(request, 3).expect("room");
    assert_eq!(
        written, 20,
        "the header and the four bytes Linux puts there"
    );
    let header = NlMsgHdr::from_bytes(writer.written()).expect("a header");
    assert_eq!(header.kind, NLMSG_DONE);
    assert_eq!(header.flags & NLM_F_MULTI, NLM_F_MULTI);
    assert_eq!(header.seq, request.seq);
}

#[test]
fn a_value_knows_the_length_it_writes() {
    assert_eq!(Value::U32(0).len(), 4);
    assert_eq!(Value::U8(0).len(), 1);
    assert_eq!(Value::Bytes(&[1, 2, 3]).len(), 3);
    assert_eq!(Value::Name(b"eth0").len(), 5, "the terminator counts");
    assert_eq!(Value::Name(b"eth0\0junk").len(), 5, "and nothing after it");
    assert_eq!(Value::Address(Address::V4([10, 0, 2, 15])).len(), 4);
    assert_eq!(Value::Address(Address::V6([0; 16])).len(), 16);
    assert!(Value::Bytes(&[]).is_empty());
    assert!(!Value::U8(0).is_empty());
}

#[test]
fn a_name_is_written_with_one_terminator_and_nothing_after_it() {
    let mut buffer = [0_u8; 64];
    let mut writer = Writer::new(&mut buffer);
    let attributes = [Attr::new(IFLA_IFNAME, Value::Name(b"lo\0rubbish"))];
    let _written = writer
        .message(reply(RTM_NEWLINK), &[], &attributes)
        .expect("room");
    assert_eq!(
        writer.written().get(16..23),
        Some([7, 0, 3, 0, b'l', b'o', 0].as_slice()),
        "a seven-byte attribute: length, type, the name and its nul"
    );
}

#[test]
fn a_hardware_address_is_written_as_the_bytes_it_is() {
    let mut buffer = [0_u8; 64];
    let mut writer = Writer::new(&mut buffer);
    let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let attributes = [Attr::new(IFLA_ADDRESS, Value::Bytes(&mac))];
    let _written = writer
        .message(reply(RTM_NEWLINK), &[], &attributes)
        .expect("room");
    assert_eq!(writer.written().get(20..26), Some(mac.as_slice()));
}
