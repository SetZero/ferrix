//! What the protocol must do, checked against the layouts `docs/CLIPBOARD.md`
//! §4 fixes and against what QEMU 9.2.4 puts on the wire.

use crate::Error;
use crate::chunk::{self, Reassembler};
use crate::message::{
    AGENT_CAPS, ClipboardType, HEADER_BYTES, Message, Selection, Shape, Types, cap,
};

/// A message encodes and decodes back to itself, under each shape.
#[test]
fn a_message_survives_its_own_encoding() {
    let shapes = [Shape::QEMU_CLIPBOARD, Shape::PLAIN];
    for shape in shapes {
        let messages = [
            Message::AnnounceCapabilities {
                request: true,
                caps: AGENT_CAPS,
            },
            Message::ClipboardGrab {
                selection: Selection::Clipboard,
                serial: shape.serial.then_some(7),
                types: Types::new(&[ClipboardType::Utf8Text]).expect("one type fits"),
            },
            Message::ClipboardRequest {
                selection: Selection::Clipboard,
                kind: ClipboardType::Utf8Text,
            },
            Message::Clipboard {
                selection: Selection::Clipboard,
                kind: ClipboardType::Utf8Text,
                data: b"a line a person copied",
            },
            Message::ClipboardRelease {
                selection: Selection::Clipboard,
            },
        ];
        let mut out = [0_u8; 128];
        for message in messages {
            let written = message.encode(shape, &mut out).expect("room to encode");
            assert_eq!(
                written,
                message.encoded_len(shape),
                "encoded_len disagrees with encode for {message:?}"
            );
            let back = Message::decode(&out[..written], shape).expect("its own bytes decode");
            assert_eq!(
                back, message,
                "a round trip changed the message under {shape:?}"
            );
        }
    }
}

/// The selection capability is what decides the layout, and reading a message
/// under the wrong shape is how an implementation goes wrong. Under the plain
/// shape the same request is four bytes shorter, and its type field is where
/// the selection would otherwise be.
#[test]
fn the_selection_capability_moves_every_field() {
    let message = Message::ClipboardRequest {
        selection: Selection::Primary,
        kind: ClipboardType::Utf8Text,
    };
    let mut with = [0_u8; 32];
    let mut without = [0_u8; 32];
    let with_len = message
        .encode(Shape::QEMU_CLIPBOARD, &mut with)
        .expect("room");
    let without_len = message.encode(Shape::PLAIN, &mut without).expect("room");

    assert_eq!(
        with_len - without_len,
        4,
        "the selection field is four bytes"
    );
    assert_eq!(
        with[HEADER_BYTES],
        Selection::Primary.number(),
        "selection first"
    );
    assert_eq!(
        &with[HEADER_BYTES + 1..HEADER_BYTES + 4],
        &[0, 0, 0],
        "the padding after a selection is written as zero"
    );
    // The plain shape has no selection to carry, so the primary selection is
    // not expressible: it decodes back as the clipboard rather than as an
    // error, since there is only one selection to be.
    let back = Message::decode(&without[..without_len], Shape::PLAIN).expect("decodes");
    assert_eq!(
        back,
        Message::ClipboardRequest {
            selection: Selection::Clipboard,
            kind: ClipboardType::Utf8Text,
        },
        "the plain shape has one selection"
    );
}

/// The padding after a selection is ignored on the way in, deliberately:
/// §4 of the design says why.
#[test]
fn padding_that_is_not_zero_is_still_read() {
    let mut bytes = [0_u8; 32];
    let written = Message::ClipboardRelease {
        selection: Selection::Primary,
    }
    .encode(Shape::QEMU_CLIPBOARD, &mut bytes)
    .expect("room");
    bytes[HEADER_BYTES + 1] = 0xff;
    bytes[HEADER_BYTES + 3] = 0x01;
    assert_eq!(
        Message::decode(&bytes[..written], Shape::QEMU_CLIPBOARD),
        Ok(Message::ClipboardRelease {
            selection: Selection::Primary
        }),
        "a peer's padding is not ours to refuse"
    );
}

/// A grab lists types, and the ones this crate cannot carry are dropped
/// rather than refusing the whole grab: a host offering TIFF and text is a
/// host this agent takes text from.
#[test]
fn a_grab_keeps_the_types_it_understands() {
    // Built by hand, because `Types` cannot hold a type the crate refuses.
    let mut bytes = [0_u8; 40];
    let body = [
        Selection::Clipboard.number(),
        0,
        0,
        0, // selection and padding
        9,
        0,
        0,
        0, // serial
        4,
        0,
        0,
        0, // VD_AGENT_CLIPBOARD_IMAGE_TIFF, which this crate does not carry
        1,
        0,
        0,
        0, // VD_AGENT_CLIPBOARD_UTF8_TEXT
    ];
    bytes[..4].copy_from_slice(&1_u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&crate::message::CLIPBOARD_GRAB.to_le_bytes());
    bytes[12..16].copy_from_slice(&(body.len() as u32).to_le_bytes());
    bytes[HEADER_BYTES..HEADER_BYTES + body.len()].copy_from_slice(&body);

    let decoded = Message::decode(&bytes[..HEADER_BYTES + body.len()], Shape::QEMU_CLIPBOARD)
        .expect("a grab with an unknown type still decodes");
    let Message::ClipboardGrab { serial, types, .. } = decoded else {
        panic!("decoded as {decoded:?}, not a grab");
    };
    assert_eq!(serial, Some(9), "the serial is after the selection");
    assert_eq!(types.len(), 1, "the TIFF was dropped");
    assert!(types.holds(ClipboardType::Utf8Text), "the text was kept");
}

/// A message type this crate does not implement is not an error: QEMU sends
/// the mouse state when it is configured with a mouse, and an agent that
/// treated that as a broken stream would drop the clipboard with it.
#[test]
fn an_unimplemented_message_is_carried_not_refused() {
    let mut bytes = [0_u8; HEADER_BYTES + 12];
    bytes[..4].copy_from_slice(&1_u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&1_u32.to_le_bytes()); // VD_AGENT_MOUSE_STATE
    bytes[12..16].copy_from_slice(&12_u32.to_le_bytes());
    assert_eq!(
        Message::decode(&bytes, Shape::QEMU_CLIPBOARD),
        Ok(Message::Other {
            kind: 1,
            data: &[0; 12]
        }),
        "an unknown type is handed over undecoded"
    );
}

/// The capabilities QEMU announces with `clipboard=on` are the ones the agent
/// answers with, and they decide the shape.
#[test]
fn qemus_capabilities_decide_the_shape() {
    let qemu = cap::bit(cap::CLIPBOARD_BY_DEMAND)
        | cap::bit(cap::CLIPBOARD_SELECTION)
        | cap::bit(cap::CLIPBOARD_GRAB_SERIAL);
    assert_eq!(AGENT_CAPS, qemu, "the agent answers what QEMU announces");
    assert_eq!(
        Shape::from_caps(qemu),
        Shape::QEMU_CLIPBOARD,
        "those three capabilities are that shape"
    );
    assert_eq!(
        Shape::from_caps(cap::bit(cap::CLIPBOARD_BY_DEMAND)),
        Shape::PLAIN,
        "by demand alone moves no field"
    );
}

/// A peer that announces nothing sends the request word and no bitmap, which
/// is a peer with no capabilities rather than a short message.
#[test]
fn an_empty_announcement_is_no_capabilities() {
    let mut bytes = [0_u8; HEADER_BYTES + 4];
    bytes[..4].copy_from_slice(&1_u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&crate::message::ANNOUNCE_CAPABILITIES.to_le_bytes());
    bytes[12..16].copy_from_slice(&4_u32.to_le_bytes());
    bytes[HEADER_BYTES..].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        Message::decode(&bytes, Shape::QEMU_CLIPBOARD),
        Ok(Message::AnnounceCapabilities {
            request: true,
            caps: 0
        }),
        "no bitmap is no capabilities"
    );
}

/// A selection that is neither of the two, and a type nothing carries, are
/// refused with what they were.
#[test]
fn a_field_that_is_not_defined_is_refused() {
    assert_eq!(Selection::from_number(2), Err(Error::Selection(2)));
    assert_eq!(ClipboardType::from_number(6), Err(Error::Type(6)));
}

/// Framing and reassembly are inverses, across the chunk boundary: a message
/// longer than a chunk comes back whole.
#[test]
fn a_message_longer_than_a_chunk_comes_back_whole() {
    let data = [b'x'; chunk::MAX_PAYLOAD * 2 + 5];
    let message = Message::Clipboard {
        selection: Selection::Clipboard,
        kind: ClipboardType::Utf8Text,
        data: &data,
    };
    let mut encoded = [0_u8; chunk::MAX_PAYLOAD * 3];
    let len = message
        .encode(Shape::QEMU_CLIPBOARD, &mut encoded)
        .expect("room");
    let mut framed = [0_u8; chunk::MAX_PAYLOAD * 4];
    let framed_len = chunk::frame(&encoded[..len], &mut framed).expect("room");
    assert_eq!(
        framed_len,
        chunk::framed_len(len),
        "framed_len disagrees with frame"
    );
    // Two full chunks and the remainder, since the message is its body plus
    // a sixteen-byte header and a four-byte selection and type.
    assert_eq!(
        framed_len,
        len + len.div_ceil(chunk::MAX_PAYLOAD) * chunk::HEADER,
        "one header per chunk"
    );
    assert_eq!(len.div_ceil(chunk::MAX_PAYLOAD), 3, "three chunks");

    let mut buffer = [0_u8; chunk::MAX_PAYLOAD * 4];
    let mut assembler = Reassembler::new(&mut buffer);
    let mut left = &framed[..framed_len];
    let mut seen = 0;
    while !left.is_empty() {
        let took = assembler.feed(left).expect("well-formed");
        left = &left[took..];
        if let Some(whole) = assembler.message() {
            assert_eq!(whole, &encoded[..len], "the message came back changed");
            seen += 1;
            assembler.take();
        }
    }
    assert_eq!(seen, 1, "exactly one message");
}

/// A chunk boundary is not a message boundary, and a stream that puts two
/// messages in one chunk is within its rights.
#[test]
fn two_messages_in_one_chunk_are_both_read() {
    let mut encoded = [0_u8; 128];
    let first = Message::ClipboardRelease {
        selection: Selection::Clipboard,
    }
    .encode(Shape::QEMU_CLIPBOARD, &mut encoded)
    .expect("room");
    let second = Message::ClipboardRequest {
        selection: Selection::Primary,
        kind: ClipboardType::Utf8Text,
    }
    .encode(Shape::QEMU_CLIPBOARD, &mut encoded[first..])
    .expect("room");
    let both = first + second;

    // One chunk holding both, which QEMU's sender would never write and a
    // reader must accept.
    let mut framed = [0_u8; 256];
    framed[..4].copy_from_slice(&chunk::CLIENT_PORT.to_le_bytes());
    framed[4..8].copy_from_slice(&(both as u32).to_le_bytes());
    framed[chunk::HEADER..chunk::HEADER + both].copy_from_slice(&encoded[..both]);

    let mut buffer = [0_u8; 128];
    let mut assembler = Reassembler::new(&mut buffer);
    let mut left = &framed[..chunk::HEADER + both];
    let mut kinds = [0_u32; 2];
    let mut seen = 0;
    while !left.is_empty() {
        let took = assembler.feed(left).expect("well-formed");
        left = &left[took..];
        if let Some(whole) = assembler.message() {
            kinds[seen] = Message::decode(whole, Shape::QEMU_CLIPBOARD)
                .expect("decodes")
                .kind();
            seen += 1;
            assembler.take();
        }
    }
    assert_eq!(seen, 2, "both messages were read out of one chunk");
    assert_eq!(
        kinds,
        [
            crate::message::CLIPBOARD_RELEASE,
            crate::message::CLIPBOARD_REQUEST
        ],
        "in the order they were written"
    );
}

/// A stream fed one byte at a time reassembles the same, which is the case a
/// device that delivers in small buffers produces.
#[test]
fn a_byte_at_a_time_is_the_same_message() {
    let message = Message::Clipboard {
        selection: Selection::Primary,
        kind: ClipboardType::Utf8Text,
        data: b"copied one byte at a time",
    };
    let mut encoded = [0_u8; 128];
    let len = message
        .encode(Shape::QEMU_CLIPBOARD, &mut encoded)
        .expect("room");
    let mut framed = [0_u8; 160];
    let framed_len = chunk::frame(&encoded[..len], &mut framed).expect("room");

    let mut buffer = [0_u8; 128];
    let mut assembler = Reassembler::new(&mut buffer);
    let mut seen = 0;
    for at in 0..framed_len {
        let took = assembler.feed(&framed[at..=at]).expect("well-formed");
        assert_eq!(took, 1, "a byte was offered and not taken");
        if let Some(whole) = assembler.message() {
            assert_eq!(
                Message::decode(whole, Shape::QEMU_CLIPBOARD),
                Ok(message),
                "the message came back changed"
            );
            seen += 1;
            assembler.take();
        }
    }
    assert_eq!(seen, 1, "exactly one message");
}

/// A message larger than the buffer is refused rather than truncated, and the
/// reassembler stays refused: the stream is no longer at a known boundary.
#[test]
fn a_message_too_large_breaks_the_stream() {
    let mut header = [0_u8; HEADER_BYTES + chunk::HEADER];
    header[..4].copy_from_slice(&chunk::CLIENT_PORT.to_le_bytes());
    header[4..8].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    let message = &mut header[chunk::HEADER..];
    message[..4].copy_from_slice(&1_u32.to_le_bytes());
    message[4..8].copy_from_slice(&crate::message::CLIPBOARD.to_le_bytes());
    message[12..16].copy_from_slice(&100_000_u32.to_le_bytes());

    let mut buffer = [0_u8; 64];
    let mut assembler = Reassembler::new(&mut buffer);
    assert_eq!(
        assembler.feed(&header),
        Err(Error::TooLong {
            declared: HEADER_BYTES + 100_000,
            limit: 64
        }),
        "a declared size over the buffer is refused"
    );
    assert_eq!(
        assembler.feed(&header),
        Err(Error::Broken),
        "and nothing after it is believed"
    );
}

/// A message whose protocol field is not vdagent's breaks the stream too: it
/// is not a message of a later version, it is bytes that are not this
/// protocol at all.
#[test]
fn a_foreign_protocol_breaks_the_stream() {
    let mut bytes = [0_u8; HEADER_BYTES + chunk::HEADER];
    bytes[..4].copy_from_slice(&chunk::CLIENT_PORT.to_le_bytes());
    bytes[4..8].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    bytes[chunk::HEADER..chunk::HEADER + 4].copy_from_slice(&2_u32.to_le_bytes());

    let mut buffer = [0_u8; 64];
    let mut assembler = Reassembler::new(&mut buffer);
    assert_eq!(assembler.feed(&bytes), Err(Error::Protocol(2)));
    assert_eq!(assembler.feed(&bytes), Err(Error::Broken));
}

/// Encoding into a buffer that is too small says so rather than writing part
/// of a message.
#[test]
fn encoding_refuses_a_buffer_it_would_overrun() {
    let message = Message::Clipboard {
        selection: Selection::Clipboard,
        kind: ClipboardType::Utf8Text,
        data: b"0123456789",
    };
    let mut out = [0_u8; 8];
    assert_eq!(
        message.encode(Shape::QEMU_CLIPBOARD, &mut out),
        Err(Error::Short {
            want: message.encoded_len(Shape::QEMU_CLIPBOARD),
            have: 8
        })
    );
    assert_eq!(out, [0; 8], "nothing was written");
}

/// A grab may not list more types than the bound, which is the check that
/// catches a message being read at the wrong offset.
#[test]
fn a_grab_with_too_many_types_is_refused() {
    let types = [ClipboardType::Utf8Text; crate::message::MAX_TYPES + 1];
    assert_eq!(
        Types::new(&types),
        Err(Error::TooManyTypes(crate::message::MAX_TYPES + 1))
    );
}

/// The MIME types the Wayland side names these by.
#[test]
fn the_mime_types_are_the_compositors() {
    assert_eq!(
        ClipboardType::Utf8Text.mime(),
        Some("text/plain;charset=utf-8"),
        "what `compositor/clip` offers"
    );
    assert_eq!(ClipboardType::None.mime(), None, "the absence of a type");
}
