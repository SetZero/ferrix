extern crate std;

use std::format;
use std::vec::Vec;

use crate::{
    DecodeError, EncodeError, MAX_RECORD, MAX_SECRET, MAX_TEXT, Record, Response, Secret, VERSION,
    method,
};

fn every_kind() -> Vec<Record<'static>> {
    std::vec![
        Record::Begin {
            service: "hyprlock",
            account: "",
            method: "",
        },
        Record::Begin {
            service: "login",
            account: "ferrix",
            method: "fingerprint",
        },
        Record::Respond(Response(b"correct horse")),
        Record::Respond(Response(&[0xFF; MAX_SECRET])),
        Record::Cancel,
        Record::Status { account: "" },
        Record::Reset { account: "root" },
        Record::UnlockSeat,
        Record::Prompt {
            visible: false,
            text: "Password: ",
        },
        Record::Prompt {
            visible: true,
            text: "Code from your phone: ",
        },
        Record::Info("wait 16 s"),
        Record::Error("the caps lock is on"),
        Record::Accepted {
            uid: 1000,
            account: "ferrix",
        },
        Record::Failed {
            retry_after_ms: 2000,
            text: "Authentication failed",
        },
        Record::Unavailable("no password is set for ferrix"),
        Record::State {
            credential: true,
            methods: method::PASSWORD,
            throttled_ms: 16_000,
        },
    ]
}

#[test]
fn every_kind_round_trips() {
    for record in every_kind() {
        let mut buffer = [0_u8; MAX_RECORD];
        let len = record.encode(&mut buffer).unwrap();
        assert_eq!(Record::decode(&buffer[..len]), Ok(record));
        assert_eq!(buffer[1], VERSION);
    }
}

#[test]
fn a_record_has_one_spelling() {
    // Decode, encode, compare bytes: whatever decodes encodes back the same.
    for record in every_kind() {
        let mut buffer = [0_u8; MAX_RECORD];
        let len = record.encode(&mut buffer).unwrap();
        let decoded = Record::decode(&buffer[..len]).unwrap();
        let mut again = [0_u8; MAX_RECORD];
        let len2 = decoded.encode(&mut again).unwrap();
        assert_eq!(&buffer[..len], &again[..len2]);
    }
}

#[test]
fn the_layout_is_fixed() {
    let mut buffer = [0_u8; 64];
    let begin = Record::Begin {
        service: "passwd",
        account: "ab",
        method: "",
    };
    let len = begin.encode(&mut buffer).unwrap();
    assert_eq!(
        &buffer[..len],
        b"\x01\x01\x06\x00passwd\x02\x00ab\x00\x00",
        "kind, version, then u16-prefixed strings"
    );
    let failed = Record::Failed {
        retry_after_ms: 0x0102_0304,
        text: "no",
    };
    let len = failed.encode(&mut buffer).unwrap();
    assert_eq!(&buffer[..len], b"\x24\x01\x04\x03\x02\x01\x02\x00no");
}

#[test]
fn decoding_refuses_every_malformed_packet() {
    let cases: [(&[u8], DecodeError); 12] = [
        (b"", DecodeError::Length),
        (&[0x03], DecodeError::Short),
        (&[0x03, 0x02], DecodeError::Version),
        (&[0x07, 0x01], DecodeError::Kind),
        (&[0x03, 0x01, 0x00], DecodeError::Trailing),
        (&[0x20, 0x01, 0x02, 0x00, 0x00], DecodeError::Flag),
        (&[0x04, 0x01, 0x05, 0x00, b'a'], DecodeError::Short),
        // An account may not start with `-` or hold `/` or a space.
        (b"\x04\x01\x02\x00-r", DecodeError::Field),
        (b"\x04\x01\x03\x00a/b", DecodeError::Field),
        // A service name is lower case.
        (b"\x01\x01\x04\x00Sudo\x00\x00\x00\x00", DecodeError::Field),
        // Text may not carry an escape sequence.
        (b"\x21\x01\x04\x00\x1b[2J", DecodeError::Field),
        // A method bit that is not defined.
        (
            b"\x26\x01\x01\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            DecodeError::Field,
        ),
    ];
    for (packet, why) in cases {
        assert_eq!(Record::decode(packet), Err(why), "{packet:?}");
    }
    assert_eq!(
        Record::decode(&[0; MAX_RECORD + 1]),
        Err(DecodeError::Length)
    );
    // A string one byte past its field's limit.
    let mut long = std::vec![0x21, 0x01];
    long.extend_from_slice(&u16::try_from(MAX_TEXT + 1).unwrap().to_le_bytes());
    long.extend(std::iter::repeat_n(b'a', MAX_TEXT + 1));
    assert_eq!(Record::decode(&long), Err(DecodeError::Field));
    // Text that is not UTF-8.
    assert_eq!(
        Record::decode(b"\x21\x01\x01\x00\xff"),
        Err(DecodeError::Field)
    );
}

#[test]
fn encoding_refuses_what_would_not_decode() {
    let mut buffer = [0_u8; MAX_RECORD];
    let too_long = [0_u8; MAX_SECRET + 1];
    assert_eq!(
        Record::Respond(Response(&too_long)).encode(&mut buffer),
        Err(EncodeError::Field)
    );
    assert_eq!(
        Record::Status { account: "a b" }.encode(&mut buffer),
        Err(EncodeError::Field)
    );
    assert_eq!(
        Record::Info("\x07").encode(&mut buffer),
        Err(EncodeError::Field)
    );
    assert_eq!(
        Record::Cancel.encode(&mut buffer[..1]),
        Err(EncodeError::Room)
    );
    assert_eq!(
        Record::State {
            credential: false,
            methods: 1 << 20,
            throttled_ms: 0,
        }
        .encode(&mut buffer),
        Err(EncodeError::Field)
    );
}

#[test]
fn only_verdicts_and_answers_end_a_conversation() {
    for record in every_kind() {
        let expected = matches!(
            record,
            Record::Accepted { .. }
                | Record::Failed { .. }
                | Record::Unavailable(_)
                | Record::State { .. }
        );
        assert_eq!(record.is_final(), expected, "{record:?}");
    }
}

#[test]
fn debug_never_shows_a_secret() {
    let shown = format!("{:?}", Record::Respond(Response(b"hunter2")));
    assert_eq!(shown, "Respond(Response(7 bytes))");
    let secret = Secret::from_bytes(b"hunter2").unwrap();
    assert_eq!(format!("{secret:?}"), "Secret(..)");
}

#[test]
fn a_secret_is_typed_and_forgotten() {
    let mut secret = Secret::new();
    for &byte in b"pass" {
        assert!(secret.push(byte));
    }
    secret.pop();
    assert_eq!(secret.expose(), b"pas");
    assert_eq!(secret.len(), 3);
    secret.clear();
    assert!(secret.is_empty());
    let mut full = Secret::from_bytes(&[1; MAX_SECRET]).unwrap();
    assert!(!full.push(2), "a full secret takes no more");
    assert!(Secret::from_bytes(&[1; MAX_SECRET + 1]).is_none());
    full.pop();
    assert_eq!(full.len(), MAX_SECRET - 1);
}
