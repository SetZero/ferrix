//! Walking a buffer of messages, including the lengths a stranger writes.

use alloc::vec::Vec;

use ferrix_linux_abi::netlink::{
    NLM_F_DUMP, NLM_F_REQUEST, NlMsgHdr, RTM_GETADDR, RTM_GETLINK, RTM_GETROUTE,
};

use crate::{Error, Messages};

/// A message of `kind` whose payload is `payload`, with no padding written
/// past the length it declares.
fn message(kind: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
    let header = NlMsgHdr {
        len: u32::try_from(NlMsgHdr::SIZE + payload.len()).expect("a test message is small"),
        kind,
        flags: NLM_F_REQUEST | NLM_F_DUMP,
        seq,
        pid: 0,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend_from_slice(payload);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
}

/// Three requests one after another, as `ip addr` sends them.
fn three() -> Vec<u8> {
    let mut bytes = message(RTM_GETLINK, 1, &[0_u8; 16]);
    bytes.extend(message(RTM_GETADDR, 2, &[0_u8; 8]));
    bytes.extend(message(RTM_GETROUTE, 3, &[0_u8; 12]));
    bytes
}

#[test]
fn a_buffer_of_several_messages_walks_to_exactly_those_messages() {
    let bytes = three();
    let walked: Vec<_> = Messages::new(&bytes)
        .map(|message| message.expect("each message is well formed"))
        .collect();
    assert_eq!(walked.len(), 3, "three messages went in");
    assert_eq!(walked[0].header.kind, RTM_GETLINK);
    assert_eq!(walked[1].header.kind, RTM_GETADDR);
    assert_eq!(walked[2].header.kind, RTM_GETROUTE);
    assert_eq!(walked[0].payload.len(), 16);
    assert_eq!(walked[1].payload.len(), 8);
    assert_eq!(walked[2].payload.len(), 12);
}

#[test]
fn every_payload_is_borrowed_from_the_buffer_that_was_walked() {
    let bytes = three();
    let range = bytes.as_ptr_range();
    for message in Messages::new(&bytes) {
        let payload = message.expect("well formed").payload.as_ptr_range();
        assert!(
            payload.start >= range.start && payload.end <= range.end,
            "a payload came from outside the buffer"
        );
    }
}

#[test]
fn the_sequence_numbers_and_flags_arrive_as_they_were_sent() {
    let bytes = three();
    let seqs: Vec<u32> = Messages::new(&bytes)
        .map(|message| message.expect("well formed").header.seq)
        .collect();
    assert_eq!(seqs, [1, 2, 3]);
    for message in Messages::new(&bytes) {
        let header = message.expect("well formed").header;
        assert_eq!(header.flags, NLM_F_REQUEST | NLM_F_DUMP);
    }
}

#[test]
fn an_empty_buffer_walks_to_nothing() {
    assert_eq!(Messages::new(&[]).count(), 0);
}

#[test]
fn a_length_of_zero_ends_the_walk_rather_than_repeating_it() {
    let mut bytes = message(RTM_GETLINK, 1, &[0_u8; 16]);
    let zero = NlMsgHdr {
        len: 0,
        kind: RTM_GETADDR,
        flags: 0,
        seq: 2,
        pid: 0,
    };
    bytes.extend_from_slice(&zero.to_bytes());
    let walked: Vec<_> = Messages::new(&bytes).collect();
    assert_eq!(walked.len(), 2, "the good message, then the refusal");
    assert!(walked[0].is_ok());
    assert_eq!(walked[1], Err(Error::Truncated));
}

#[test]
fn a_length_below_the_header_is_refused() {
    let short = NlMsgHdr {
        len: 15,
        kind: RTM_GETLINK,
        flags: 0,
        seq: 1,
        pid: 0,
    };
    let bytes = short.to_bytes();
    assert_eq!(
        Messages::new(&bytes).collect::<Vec<_>>(),
        [Err(Error::Truncated)]
    );
}

#[test]
fn a_length_past_the_end_of_the_buffer_is_refused() {
    let header = NlMsgHdr {
        // Four bytes more than are there.
        len: u32::try_from(NlMsgHdr::SIZE + 20).expect("small"),
        kind: RTM_GETLINK,
        flags: NLM_F_REQUEST,
        seq: 1,
        pid: 0,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend_from_slice(&[0_u8; 16]);
    assert_eq!(
        Messages::new(&bytes).collect::<Vec<_>>(),
        [Err(Error::Truncated)]
    );
}

#[test]
fn a_truncated_message_after_two_good_ones_leaves_the_two() {
    let mut bytes = three();
    bytes.truncate(bytes.len() - 4);
    let walked: Vec<_> = Messages::new(&bytes).collect();
    assert_eq!(walked.len(), 3);
    assert!(walked[0].is_ok() && walked[1].is_ok());
    assert_eq!(walked[2], Err(Error::Truncated));
}

#[test]
fn a_tail_too_short_to_hold_a_header_is_refused_rather_than_read() {
    let mut bytes = message(RTM_GETLINK, 1, &[0_u8; 16]);
    bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let walked: Vec<_> = Messages::new(&bytes).collect();
    assert_eq!(walked.len(), 2);
    assert!(walked[0].is_ok());
    assert_eq!(walked[1], Err(Error::Truncated));
}

#[test]
fn an_error_is_the_last_thing_a_walk_yields() {
    let mut bytes = message(RTM_GETLINK, 1, &[0_u8; 16]);
    bytes.extend_from_slice(&[0; 3]);
    bytes.extend(message(RTM_GETADDR, 2, &[0_u8; 8]));
    let mut walk = Messages::new(&bytes);
    assert!(walk.next().expect("one").is_ok());
    assert_eq!(walk.next(), Some(Err(Error::Truncated)));
    assert_eq!(walk.next(), None, "the walk is over");
    assert_eq!(walk.next(), None, "and stays over");
}

#[test]
fn the_padding_between_messages_is_skipped_and_is_not_payload() {
    // A three-byte payload: the message declares 19 bytes and the next one
    // starts at 20.
    let mut bytes = message(RTM_GETLINK, 1, &[1, 2, 3]);
    assert_eq!(bytes.len(), 20, "the message was padded");
    bytes.extend(message(RTM_GETADDR, 2, &[0_u8; 8]));
    let walked: Vec<_> = Messages::new(&bytes)
        .map(|message| message.expect("well formed"))
        .collect();
    assert_eq!(walked.len(), 2);
    assert_eq!(walked[0].payload, &[1, 2, 3]);
    assert_eq!(walked[1].header.kind, RTM_GETADDR);
}

#[test]
fn the_body_a_message_declares_too_short_for_is_none() {
    let bytes = message(RTM_GETLINK, 1, &[0_u8; 4]);
    let message = Messages::new(&bytes)
        .next()
        .expect("one message")
        .expect("well formed");
    assert_eq!(message.body(4).map(<[u8]>::len), Some(4));
    assert_eq!(
        message.body(16),
        None,
        "a four-byte payload holds no ifinfomsg"
    );
}

#[test]
fn a_walk_over_arbitrary_bytes_ends() {
    // Every byte pattern that is not a message still has to terminate: the
    // fuzz target says so for all of them, and this says so for the two that
    // have historically looped.
    for filler in [0x00_u8, 0xFF] {
        let bytes = [filler; 64];
        let count = Messages::new(&bytes).count();
        assert!(count <= 16, "a walk over rubbish yielded {count} messages");
    }
}
