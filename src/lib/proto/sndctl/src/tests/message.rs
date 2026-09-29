//! The messages byte for byte: every one encodes at its fixed length and
//! decodes back, and every malformed one is refused.

use super::std::vec::Vec;

use crate::message::{
    DIRECTION_CAPTURE, DIRECTION_PLAYBACK, Elapsed, HALT_BYTES, HELLO_BYTES, Hello, MAX_STREAMS,
    Message, MessageError, Offer, Published, RATES_HZ, READY_BYTES, Ready, Refusal, SUBMIT_BYTES,
    Submit, rate_bit,
};

/// The offer a virtio-snd driver makes for QEMU's two streams.
pub(super) fn qemu_hello() -> Hello {
    // S8 U8 S16 U16 S32 U32 FLOAT as ALSA numbers 0 1 2 4 10 12 14, and all
    // fourteen rates.
    let formats = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 4) | (1 << 10) | (1 << 12) | (1 << 14);
    let offer = |direction| Offer {
        direction,
        channels_min: 1,
        channels_max: 2,
        rates: 0x3fff,
        formats,
    };
    let mut offers = [Offer::default(); MAX_STREAMS];
    offers[0] = offer(DIRECTION_PLAYBACK);
    offers[1] = offer(DIRECTION_CAPTURE);
    Hello {
        version: 1,
        location: 0x0000_0300,
        streams: 2,
        offers,
    }
}

fn every_message() -> Vec<Message> {
    let mut streams = [Published::default(); 2];
    streams[0] = Published {
        stream: 0,
        rate: 48_000,
        format: 2,
        channels: 2,
        period_bytes: 3840,
        buffer_bytes: 15360,
    };
    Vec::from([
        Message::Hello(qemu_hello()),
        Message::Ready(Ready {
            card: 0,
            published: 1,
            streams,
        }),
        Message::Refused(Refusal::Nothing),
        Message::Submit(Submit {
            stream: 0,
            sequence: 7,
            offset: 3840,
            bytes: 2800,
        }),
        Message::Elapsed(Elapsed {
            stream: 0,
            sequence: 7,
            played: true,
            latency_bytes: 2800,
        }),
        Message::Halt { stream: 0 },
        Message::Halted {
            stream: 0,
            unplayed: 3,
        },
        Message::Stop,
        Message::Stopped,
    ])
}

#[test]
fn every_message_round_trips_at_its_length() {
    let lengths = [
        HELLO_BYTES,
        READY_BYTES,
        12,
        SUBMIT_BYTES,
        SUBMIT_BYTES,
        HALT_BYTES,
        HALT_BYTES,
        8,
        8,
    ];
    for (message, length) in every_message().into_iter().zip(lengths) {
        let encoded = message.encode();
        let bytes = encoded.as_bytes();
        assert_eq!(bytes.len(), length, "{message:?}");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize,
            length
        );
        assert_eq!(Message::decode(bytes), Ok(message));
        // One byte short, or one more, is not a message.
        assert!(Message::decode(&bytes[..length - 1]).is_err());
        let mut longer = bytes.to_vec();
        longer.push(0);
        assert_eq!(Message::decode(&longer), Err(MessageError::Length));
    }
    assert_eq!(HELLO_BYTES, 184);
    assert_eq!(READY_BYTES, 56);
}

#[test]
fn the_layout_is_the_one_the_module_comment_draws() {
    let bytes = Message::Hello(qemu_hello()).encode();
    let bytes = bytes.as_bytes();
    assert_eq!(bytes[0..4], 1u32.to_le_bytes(), "HELLO");
    assert_eq!(bytes[8..10], 1u16.to_le_bytes(), "version");
    assert_eq!(bytes[16..20], 2u32.to_le_bytes(), "streams");
    assert_eq!(bytes[24..28], [0, 1, 2, 0], "direction, channels, reserved");
    assert_eq!(bytes[28..32], 0x3fffu32.to_le_bytes(), "rates");
    assert_eq!(bytes[40], 1, "the second stream records");
    let submit = Message::Submit(Submit {
        stream: 1,
        sequence: 2,
        offset: 3,
        bytes: 4,
    })
    .encode();
    assert_eq!(
        submit.as_bytes()[8..24],
        [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0]
    );
}

#[test]
fn malformed_messages_are_refused() {
    assert_eq!(Message::decode(&[1, 0, 0]), Err(MessageError::Short));
    assert_eq!(
        Message::decode(&[99, 0, 0, 0, 8, 0, 0, 0]),
        Err(MessageError::Type(99))
    );

    let hello = Message::Hello(qemu_hello()).encode().as_bytes().to_vec();
    // A reserved half-word, a reserved word, a stream's reserved byte.
    for at in [10, 20, 27] {
        let mut bad = hello.clone();
        bad[at] = 1;
        assert_eq!(Message::decode(&bad), Err(MessageError::Field), "byte {at}");
    }
    // An offer past the count, and a count past the room.
    let mut past = hello.clone();
    past[24 + 2 * 16 + 1] = 1;
    assert_eq!(Message::decode(&past), Err(MessageError::Field));
    let mut many = hello;
    many[16] = 11;
    assert_eq!(Message::decode(&many), Err(MessageError::Field));

    // ELAPSED's `played` is 0 or 1; HALT's second word is reserved.
    let mut elapsed = Message::Elapsed(Elapsed {
        stream: 0,
        sequence: 0,
        played: false,
        latency_bytes: 0,
    })
    .encode()
    .as_bytes()
    .to_vec();
    elapsed[16] = 2;
    assert_eq!(Message::decode(&elapsed), Err(MessageError::Field));
    let mut halt = Message::Halt { stream: 0 }.encode().as_bytes().to_vec();
    halt[12] = 1;
    assert_eq!(Message::decode(&halt), Err(MessageError::Field));
    let mut refused = Message::Refused(Refusal::Hello)
        .encode()
        .as_bytes()
        .to_vec();
    refused[8] = 9;
    assert_eq!(Message::decode(&refused), Err(MessageError::Field));
}

#[test]
fn rates_are_bits_over_the_table() {
    assert_eq!(RATES_HZ.len(), 14);
    assert_eq!(rate_bit(5512), Some(1));
    assert_eq!(rate_bit(48_000), Some(1 << 7));
    assert_eq!(rate_bit(384_000), Some(1 << 13));
    assert_eq!(rate_bit(47_000), None);
    let offer = qemu_hello().offers[0];
    assert!(offer.offers(2, 48_000, 2));
    assert!(!offer.offers(2, 48_000, 3), "three channels");
    assert!(!offer.offers(3, 48_000, 2), "S16_BE is not offered");
    assert!(
        !qemu_hello().offers[1].offers(2, 48_000, 2),
        "capture does not play"
    );
}
