//! HELLO, READY, REFUSED, STOP and STOPPED, and the checks on HELLO.

use std::vec::Vec;

use ferrix_native_abi::rights::Rights;

use super::support::device;
use crate::control::{
    HELLO_RIGHTS, Hello, Message, MessageError, PORT_RIGHTS, READY_RIGHTS, Refusal, VMO_RIGHTS,
};

#[test]
fn every_message_round_trips() {
    let hello = Hello::for_device(&device());
    let messages = [
        Message::Hello(hello),
        Message::Ready,
        Message::Refused(Refusal::Rights.raw()),
        Message::Refused(77),
        Message::Stop,
        Message::Stopped,
    ];
    for message in messages {
        let encoded = message.encode();
        assert_eq!(
            Message::decode(encoded.as_bytes()),
            Ok(message),
            "{message:?}"
        );
    }
}

#[test]
fn hello_is_laid_out_as_specified() {
    let hello = Hello {
        version: 1,
        queues: 1,
        block_size: 4096,
        capacity: 0x1122_3344_5566_7788,
        max_sectors: 0x99AA,
        device_flags: 0b101,
        data_vmo_size: 0x0102_0304_0506_0708,
    };
    let encoded = Message::Hello(hello).encode();
    let bytes = encoded.as_bytes();
    assert_eq!(bytes.len(), 40, "HELLO is 40 bytes");
    assert_eq!(bytes[0..4], 1_u32.to_le_bytes(), "type");
    assert_eq!(bytes[4..8], 40_u32.to_le_bytes(), "length");
    assert_eq!(bytes[8..10], 1_u16.to_le_bytes(), "version");
    assert_eq!(bytes[10..12], 1_u16.to_le_bytes(), "queues");
    assert_eq!(bytes[12..16], 4096_u32.to_le_bytes(), "block_size");
    assert_eq!(bytes[16..24], hello.capacity.to_le_bytes(), "capacity");
    assert_eq!(
        bytes[24..28],
        hello.max_sectors.to_le_bytes(),
        "max_sectors"
    );
    assert_eq!(
        bytes[28..32],
        hello.device_flags.to_le_bytes(),
        "device_flags"
    );
    assert_eq!(
        bytes[32..40],
        hello.data_vmo_size.to_le_bytes(),
        "data_vmo_size"
    );
    assert_eq!(
        Message::Hello(hello).handles(),
        3,
        "ring VMO, data VMO, port"
    );
}

#[test]
fn the_short_messages_are_laid_out_as_specified() {
    let cases: [(Message, &[u8], usize); 4] = [
        (Message::Ready, &[2, 0, 0, 0, 8, 0, 0, 0], 1),
        (
            Message::Refused(3),
            &[3, 0, 0, 0, 12, 0, 0, 0, 3, 0, 0, 0],
            0,
        ),
        (Message::Stop, &[4, 0, 0, 0, 8, 0, 0, 0], 0),
        (Message::Stopped, &[5, 0, 0, 0, 8, 0, 0, 0], 0),
    ];
    for (message, bytes, handles) in cases {
        assert_eq!(message.encode().as_bytes(), bytes, "{message:?}");
        assert_eq!(message.handles(), handles, "{message:?} handles");
    }
}

#[test]
fn bytes_that_are_not_a_message_are_malformed() {
    let hello = Message::Hello(Hello::for_device(&device())).encode();
    let mut short_hello = hello.as_bytes()[..39].to_vec();
    short_hello[4..8].copy_from_slice(&39_u32.to_le_bytes());
    let cases: [(&[u8], MessageError); 5] = [
        (&[1, 0, 0], MessageError::Short),
        (&[9, 0, 0, 0, 8, 0, 0, 0], MessageError::UnknownType(9)),
        (&[2, 0, 0, 0, 12, 0, 0, 0], MessageError::Length),
        (&[2, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0], MessageError::Length),
        (&short_hello, MessageError::Length),
    ];
    for (bytes, error) in cases {
        assert_eq!(Message::decode(bytes), Err(error), "{bytes:?}");
        assert_eq!(error.refusal(), Refusal::Malformed, "{error:?}");
    }
}

#[test]
fn a_hello_for_another_version_is_refused_for_its_version_whatever_its_length() {
    let mut hello = Hello::for_device(&device());
    hello.version = 2;
    let mut bytes: Vec<u8> = Message::Hello(hello).encode().as_bytes().to_vec();
    bytes.extend_from_slice(&[0; 8]);
    bytes[4..8].copy_from_slice(&48_u32.to_le_bytes());
    assert_eq!(
        Message::decode(&bytes),
        Err(MessageError::Version(2)),
        "a longer v2 HELLO"
    );
    assert_eq!(
        MessageError::Version(2).refusal(),
        Refusal::Version,
        "refused for its version"
    );
    assert_eq!(
        hello.validate(&HELLO_RIGHTS),
        Err(Refusal::Version),
        "validate"
    );
}

#[test]
fn hello_is_refused_unless_it_announces_exactly_one_queue() {
    for queues in [0, 2, u16::MAX] {
        let mut hello = Hello::for_device(&device());
        hello.queues = queues;
        assert_eq!(
            hello.validate(&HELLO_RIGHTS),
            Err(Refusal::Queues),
            "{queues}"
        );
    }
}

#[test]
fn hello_handles_must_carry_exactly_the_specified_rights() {
    let hello = Hello::for_device(&device());
    assert_eq!(hello.validate(&HELLO_RIGHTS), Ok(device()), "exact rights");
    assert!(
        !VMO_RIGHTS.contains(Rights::DUPLICATE) && !VMO_RIGHTS.contains(Rights::TRANSFER),
        "a VMO handed to the kernel can be given to nobody else"
    );
    assert_eq!(PORT_RIGHTS, Rights::WRITE, "a port is WRITE only");
    assert_eq!(
        READY_RIGHTS,
        [Rights::WRITE],
        "the completion port is WRITE only"
    );

    let every = [
        Rights::DUPLICATE,
        Rights::TRANSFER,
        Rights::READ,
        Rights::WRITE,
        Rights::MAP,
        Rights::WAIT,
        Rights::MANAGE,
    ];
    for handle in 0..3 {
        for right in every {
            let mut more = HELLO_RIGHTS;
            more[handle] = more[handle] | right;
            if more != HELLO_RIGHTS {
                assert_eq!(
                    hello.validate(&more),
                    Err(Refusal::Rights),
                    "handle {handle} + {right:?}"
                );
            }
            let mut fewer = HELLO_RIGHTS;
            fewer[handle] = Rights(fewer[handle].0 & !right.0);
            if fewer != HELLO_RIGHTS {
                assert_eq!(
                    hello.validate(&fewer),
                    Err(Refusal::Rights),
                    "handle {handle} - {right:?}"
                );
            }
        }
    }
    assert_eq!(
        hello.validate(&HELLO_RIGHTS[..2]),
        Err(Refusal::Rights),
        "a handle short"
    );
    let four = [VMO_RIGHTS, VMO_RIGHTS, PORT_RIGHTS, PORT_RIGHTS];
    assert_eq!(
        hello.validate(&four),
        Err(Refusal::Rights),
        "a handle extra"
    );
}

#[test]
fn hello_with_an_unusable_device_is_refused() {
    let usable = Hello::for_device(&device());
    let cases = [
        Hello {
            block_size: 256,
            ..usable
        },
        Hello {
            block_size: 1000,
            ..usable
        },
        Hello {
            block_size: 1 << 17,
            ..usable
        },
        Hello {
            max_sectors: 0,
            ..usable
        },
        Hello {
            device_flags: 1 << 3,
            ..usable
        },
    ];
    for hello in cases {
        assert_eq!(
            hello.validate(&HELLO_RIGHTS),
            Err(Refusal::Device),
            "{hello:?}"
        );
    }
}

#[test]
fn refusal_reasons_round_trip() {
    let all = [
        Refusal::Version,
        Refusal::Queues,
        Refusal::Header,
        Refusal::Rights,
        Refusal::Malformed,
        Refusal::Device,
    ];
    for refusal in all {
        assert_eq!(
            Refusal::from_raw(refusal.raw()),
            Some(refusal),
            "{refusal:?}"
        );
    }
    assert_eq!(Refusal::from_raw(0), None, "zero names nothing");
    assert_eq!(Refusal::from_raw(7), None, "past the last");
}
