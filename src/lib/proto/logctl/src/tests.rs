//! Host tests: every message byte for byte, every malformed one refused, and
//! the kernel's half of the conversation.

extern crate std;

use std::vec;
use std::vec::Vec;

use crate::message::{
    DATA, DATA_HEADER_BYTES, MAX_BYTES, MAX_DATA, Message, MessageError, READ, READ_BYTES,
    REFUSED_BYTES, Refusal, data_header,
};
use crate::session::Session;

fn encode(message: &Message<'_>) -> Vec<u8> {
    let mut out = vec![0xEE; MAX_BYTES + 8];
    let len = message.encode_into(&mut out).expect("encodes");
    out.truncate(len);
    out
}

#[test]
fn read_is_sixteen_bytes_little_endian() {
    let bytes = encode(&Message::Read { max: 0x0102_0304 });
    assert_eq!(
        bytes,
        [1, 0, 0, 0, 16, 0, 0, 0, 4, 3, 2, 1, 0, 0, 0, 0],
        "type, length, max, reserved"
    );
    assert_eq!(bytes.len(), READ_BYTES);
    assert_eq!(
        Message::decode(&bytes),
        Ok(Message::Read { max: 0x0102_0304 })
    );
}

#[test]
fn data_carries_its_header_then_the_bytes() {
    let log = b"  boot     hello\n";
    let message = Message::Data {
        lost: 0x1_0000_0002,
        bytes: log,
    };
    let bytes = encode(&message);
    assert_eq!(bytes.len(), DATA_HEADER_BYTES + log.len());
    assert_eq!(&bytes[..4], &DATA.to_le_bytes());
    assert_eq!(&bytes[4..8], &(bytes.len() as u32).to_le_bytes());
    assert_eq!(&bytes[8..16], &0x1_0000_0002u64.to_le_bytes());
    assert_eq!(&bytes[16..20], &(log.len() as u32).to_le_bytes());
    assert_eq!(&bytes[20..24], &[0; 4]);
    assert_eq!(&bytes[24..], log);
    assert_eq!(Message::decode(&bytes), Ok(message));
}

#[test]
fn refused_round_trips() {
    let bytes = encode(&Message::Refused(Refusal::Protocol));
    assert_eq!(bytes.len(), REFUSED_BYTES);
    assert_eq!(bytes, [3, 0, 0, 0, 12, 0, 0, 0, 1, 0, 0, 0]);
    assert_eq!(
        Message::decode(&bytes),
        Ok(Message::Refused(Refusal::Protocol))
    );
}

#[test]
fn the_largest_data_fills_a_page() {
    let log = vec![b'x'; MAX_DATA];
    let bytes = encode(&Message::Data {
        lost: 0,
        bytes: &log,
    });
    assert_eq!(bytes.len(), MAX_BYTES);
    assert!(matches!(
        Message::decode(&bytes),
        Ok(Message::Data { lost: 0, bytes }) if bytes.len() == MAX_DATA
    ));
}

#[test]
fn a_header_written_in_place_decodes_with_the_bytes_after_it() {
    let mut message = vec![0xEE; DATA_HEADER_BYTES];
    message.extend_from_slice(b"statd 1\n");
    data_header(&mut message, 7, 8).expect("room for the header");
    assert_eq!(
        Message::decode(&message),
        Ok(Message::Data {
            lost: 7,
            bytes: b"statd 1\n"
        })
    );
}

#[test]
fn encoding_refuses_what_would_not_decode() {
    let mut out = [0u8; MAX_BYTES + 1];
    assert_eq!(
        Message::Read { max: 0 }.encode_into(&mut out),
        Err(MessageError::Field)
    );
    assert_eq!(
        Message::Data {
            lost: 0,
            bytes: &[]
        }
        .encode_into(&mut out),
        Err(MessageError::Field)
    );
    let too_much = vec![0; MAX_DATA + 1];
    assert_eq!(
        Message::Data {
            lost: 0,
            bytes: &too_much
        }
        .encode_into(&mut out),
        Err(MessageError::Field),
        "more than a DATA carries, with room for it"
    );
    assert_eq!(
        Message::Read { max: 1 }.encode_into(&mut [0; READ_BYTES - 1]),
        Err(MessageError::Room)
    );
    assert_eq!(data_header(&mut [0; 23], 0, 1), Err(MessageError::Room));
    assert_eq!(data_header(&mut [0; 24], 0, 0), Err(MessageError::Field));
    assert_eq!(
        data_header(&mut [0; 24], 0, MAX_DATA + 1),
        Err(MessageError::Field)
    );
}

#[test]
fn every_malformed_message_is_refused() {
    let read = encode(&Message::Read { max: 64 });
    let data = encode(&Message::Data {
        lost: 0,
        bytes: b"ab",
    });
    let refused = encode(&Message::Refused(Refusal::Protocol));

    assert_eq!(Message::decode(&read[..7]), Err(MessageError::Short));
    assert_eq!(Message::decode(&read[..15]), Err(MessageError::Length));
    let mut longer = read.clone();
    longer.push(0);
    assert_eq!(Message::decode(&longer), Err(MessageError::Length));
    let mut both = read.clone();
    both.push(0);
    both[4] = 17;
    assert_eq!(
        Message::decode(&both),
        Err(MessageError::Length),
        "READ is 16"
    );

    let mut unknown = read.clone();
    unknown[0] = 9;
    assert_eq!(Message::decode(&unknown), Err(MessageError::Type(9)));

    let mut none = read.clone();
    none[8..12].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(Message::decode(&none), Err(MessageError::Field));
    let mut reserved = read;
    reserved[12] = 1;
    assert_eq!(Message::decode(&reserved), Err(MessageError::Field));

    let mut count = data.clone();
    count[16] = 3;
    assert_eq!(Message::decode(&count), Err(MessageError::Length));
    let mut reserved = data.clone();
    reserved[23] = 1;
    assert_eq!(Message::decode(&reserved), Err(MessageError::Field));
    let mut empty = data[..DATA_HEADER_BYTES].to_vec();
    empty[4] = DATA_HEADER_BYTES as u8;
    empty[16] = 0;
    assert_eq!(Message::decode(&empty), Err(MessageError::Field));

    let mut reason = refused;
    reason[8] = 2;
    assert_eq!(Message::decode(&reason), Err(MessageError::Field));
}

#[test]
fn every_prefix_of_every_message_is_refused_and_nothing_panics() {
    let log = vec![b'z'; 100];
    for message in [
        Message::Read { max: 1 },
        Message::Data {
            lost: u64::MAX,
            bytes: &log,
        },
        Message::Refused(Refusal::Protocol),
    ] {
        let bytes = encode(&message);
        for cut in 0..bytes.len() {
            assert!(Message::decode(&bytes[..cut]).is_err(), "{cut} bytes");
        }
        assert_eq!(Message::decode(&bytes), Ok(message));
    }
}

#[test]
fn a_read_asks_for_at_most_a_page_less_the_header() {
    let mut session = Session::new();
    assert_eq!(session.wanted(), None);
    session
        .receive(&Message::Read { max: u32::MAX })
        .expect("a first READ");
    assert_eq!(session.wanted(), Some(MAX_DATA));

    let mut small = Session::new();
    small.receive(&Message::Read { max: 10 }).expect("READ");
    assert_eq!(small.wanted(), Some(10));
}

#[test]
fn a_read_is_answered_once_there_is_a_byte_and_carries_what_was_lost() {
    let mut session = Session::new();
    assert_eq!(session.read(5, 3), None, "nothing asked, nothing sent");
    session.receive(&Message::Read { max: 64 }).expect("READ");
    assert_eq!(session.read(0, 0), None, "no byte yet");
    assert_eq!(session.read(0, 2), None, "only lost bytes: carried");
    assert_eq!(
        session.read(10, 1),
        Some(3 + 2 + 1),
        "everything lost before it"
    );
    assert_eq!(session.wanted(), None, "the READ is answered");
    session
        .receive(&Message::Read { max: 64 })
        .expect("the next READ");
    assert_eq!(session.read(1, 0), Some(0), "lost is since the last DATA");
}

#[test]
fn anything_but_one_read_at_a_time_breaks_the_session() {
    for second in [
        Message::Read { max: 1 },
        Message::Data {
            lost: 0,
            bytes: b"x",
        },
        Message::Refused(Refusal::Protocol),
    ] {
        let mut session = Session::new();
        session.receive(&Message::Read { max: 8 }).expect("READ");
        assert_eq!(session.receive(&second), Err(Refusal::Protocol));
        assert_eq!(
            session.receive(&Message::Read { max: 8 }),
            Err(Refusal::Protocol),
            "a broken session stays broken"
        );
    }
    let mut session = Session::new();
    assert_eq!(
        session.receive(&Message::Data {
            lost: 0,
            bytes: b"x"
        }),
        Err(Refusal::Protocol),
        "DATA is the kernel's to send"
    );
}

#[test]
fn types_are_distinct_and_refusals_name_themselves() {
    assert_ne!(READ, DATA);
    assert_eq!(Refusal::from_raw(0), None);
    assert!(!std::format!("{}", Refusal::Protocol).is_empty());
}
