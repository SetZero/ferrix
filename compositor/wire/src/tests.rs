//! The wire format against libwayland's own encoding, argument by argument.
//!
//! The expected bytes are written out by hand from `connection.c`'s
//! `serialize_closure` and `wl_connection_demarshal`, and the messages are
//! real ones out of `/usr/share/wayland/wayland.xml`, so a byte here is
//! wrong only if this crate disagrees with the protocol rather than with
//! itself.

use crate::{
    Arg, ArgType, Entry, Error, Fd, Fixed, Header, Interface, Method, ObjectError, ObjectId,
    Objects, Reader, Writer,
};

/// `wl_display.get_registry`: `<arg name="registry" type="new_id"/>`.
const GET_REGISTRY: &[ArgType] = &[ArgType::NewId];

/// `wl_registry.bind`: `<arg name="name" type="uint"/>` and a `new_id` with
/// no interface, the one message in the protocol that has one.
const BIND: &[ArgType] = &[ArgType::Uint, ArgType::AnyNewId];

/// `wl_surface.attach`: a nullable buffer and two ints.
const ATTACH: &[ArgType] = &[
    ArgType::Object { nullable: true },
    ArgType::Int,
    ArgType::Int,
];

/// `wl_display.error`: an object, a code and a message.
const ERROR: &[ArgType] = &[
    ArgType::Object { nullable: false },
    ArgType::Uint,
    ArgType::Str { nullable: false },
];

/// `wl_keyboard.keymap`: a format, a descriptor and a size.
const KEYMAP: &[ArgType] = &[ArgType::Uint, ArgType::Fd, ArgType::Uint];

/// `wl_keyboard.enter`: a serial, a surface and the pressed keys.
const ENTER: &[ArgType] = &[
    ArgType::Uint,
    ArgType::Object { nullable: false },
    ArgType::Array,
];

/// `wl_pointer.motion`: a time and two fixed coordinates.
const MOTION: &[ArgType] = &[ArgType::Uint, ArgType::Fixed, ArgType::Fixed];

const WL_CALLBACK: Interface = Interface {
    name: "wl_callback",
    version: 1,
    requests: &[],
    events: &[Method {
        name: "done",
        since: 1,
        destructor: false,
        signature: &[ArgType::Uint],
    }],
};

const WL_SURFACE: Interface = Interface {
    name: "wl_surface",
    version: 6,
    requests: &[
        Method {
            name: "destroy",
            since: 1,
            destructor: false,
            signature: &[],
        },
        Method {
            name: "attach",
            since: 1,
            destructor: false,
            signature: ATTACH,
        },
    ],
    events: &[Method {
        name: "enter",
        since: 1,
        destructor: false,
        signature: &[ArgType::Object { nullable: false }],
    }],
};

fn write_one(sender: u32, opcode: u16, signature: &'static [ArgType], args: &[Arg<'_>]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer
        .write(ObjectId(sender), opcode, signature, args)
        .expect("a message this crate can write");
    writer.bytes().to_vec()
}

#[test]
fn the_header_is_the_sender_then_size_and_opcode_in_one_word() {
    // wl_display.get_registry(2): object 1, opcode 1, twelve bytes.
    let bytes = write_one(1, 1, GET_REGISTRY, &[Arg::NewId(ObjectId(2))]);
    assert_eq!(
        bytes,
        [
            0x01, 0x00, 0x00, 0x00, // sender
            0x01, 0x00, 0x0C, 0x00, // (12 << 16) | 1, little-endian
            0x02, 0x00, 0x00, 0x00, // the new id
        ]
    );

    let header = Header::read(&bytes).expect("a header");
    assert_eq!(
        header,
        Header {
            sender: ObjectId::DISPLAY,
            opcode: 1,
            size: 12
        }
    );
    assert_eq!(header.write(), bytes[..8]);
}

#[test]
fn a_string_carries_its_nul_in_its_length_and_is_padded_to_four() {
    // wl_registry.bind(1, "wl_compositor", 6, 3). "wl_compositor" is
    // thirteen bytes, fourteen with the NUL, sixteen padded.
    let bytes = write_one(
        2,
        0,
        BIND,
        &[
            Arg::Uint(1),
            Arg::AnyNewId {
                interface: "wl_compositor",
                version: 6,
                id: ObjectId(3),
            },
        ],
    );
    // 8 header + 4 name + 4 length + 16 string + 4 version + 4 id.
    assert_eq!(bytes.len(), 40);
    assert_eq!(bytes.get(8..12), Some(&[1, 0, 0, 0][..]));
    assert_eq!(bytes.get(12..16), Some(&14u32.to_le_bytes()[..]));
    assert_eq!(bytes.get(16..29), Some(&b"wl_compositor"[..]));
    assert_eq!(
        bytes.get(29..32),
        Some(&[0, 0, 0][..]),
        "the NUL and padding"
    );
    assert_eq!(bytes.get(32..36), Some(&6u32.to_le_bytes()[..]));
    assert_eq!(bytes.get(36..40), Some(&3u32.to_le_bytes()[..]));

    let fds = [];
    let mut reader = Reader::new(&bytes, &fds);
    let (header, args) = reader.read(BIND).expect("read back");
    assert_eq!(header.sender, ObjectId(2));
    assert_eq!(
        args,
        [
            Arg::Uint(1),
            Arg::AnyNewId {
                interface: "wl_compositor",
                version: 6,
                id: ObjectId(3)
            }
        ]
    );
    assert!(reader.is_done());
    assert_eq!(reader.consumed(), 40);
}

#[test]
fn a_string_of_exactly_three_bytes_still_takes_a_whole_word_for_its_nul() {
    // "abc" is four bytes with its NUL, which is a whole word: no padding.
    let bytes = write_one(
        1,
        0,
        ERROR,
        &[
            Arg::Object(ObjectId(7)),
            Arg::Uint(1),
            Arg::Str(Some("abc")),
        ],
    );
    assert_eq!(bytes.len(), 8 + 4 + 4 + 4 + 4);
    assert_eq!(bytes.get(16..20), Some(&4u32.to_le_bytes()[..]));
    assert_eq!(bytes.get(20..24), Some(&b"abc\0"[..]));
}

#[test]
fn padding_bytes_are_not_looked_at_because_libwayland_leaves_them_as_heap() {
    let mut bytes = write_one(
        1,
        0,
        ERROR,
        &[Arg::Object(ObjectId(7)), Arg::Uint(2), Arg::Str(Some("hi"))],
    );
    // "hi\0" is three bytes and one of padding. `wl_closure_queue` allocates
    // its buffer with malloc, so a real client's padding is whatever was
    // there.
    let pad = bytes.len() - 1;
    assert_eq!(bytes.get(pad), Some(&0));
    if let Some(byte) = bytes.get_mut(pad) {
        *byte = 0xA5;
    }
    let fds = [];
    let (_, args) = Reader::new(&bytes, &fds).read(ERROR).expect("read back");
    assert_eq!(args.get(2), Some(&Arg::Str(Some("hi"))));
}

#[test]
fn an_empty_string_is_not_a_null_one() {
    let empty = write_one(
        1,
        0,
        ERROR,
        &[Arg::Object(ObjectId(1)), Arg::Uint(0), Arg::Str(Some(""))],
    );
    // Length one: the NUL alone, padded to a word.
    assert_eq!(empty.get(16..20), Some(&1u32.to_le_bytes()[..]));
    assert_eq!(empty.len(), 24);
    let fds = [];
    let (_, args) = Reader::new(&empty, &fds).read(ERROR).expect("read back");
    assert_eq!(args.get(2), Some(&Arg::Str(Some(""))));

    // A null string in the same place is refused, since wl_display.error's
    // message is not nullable.
    let mut writer = Writer::new();
    assert_eq!(
        writer.write(
            ObjectId(1),
            0,
            ERROR,
            &[Arg::Object(ObjectId(1)), Arg::Uint(0), Arg::Str(None)]
        ),
        Err(Error::NotNullable { argument: 2 })
    );
    assert!(writer.is_empty(), "a refused message leaves nothing behind");
}

#[test]
fn a_nullable_object_may_be_zero_and_a_new_id_may_not() {
    // wl_surface.attach(NULL, 0, 0) is how a client unmaps a surface.
    let bytes = write_one(
        3,
        1,
        ATTACH,
        &[Arg::Object(ObjectId::NULL), Arg::Int(0), Arg::Int(0)],
    );
    assert_eq!(bytes.len(), 20);
    let fds = [];
    let (_, args) = Reader::new(&bytes, &fds).read(ATTACH).expect("read back");
    assert_eq!(args.first(), Some(&Arg::Object(ObjectId::NULL)));

    let mut writer = Writer::new();
    assert_eq!(
        writer.write(ObjectId(1), 1, GET_REGISTRY, &[Arg::NewId(ObjectId::NULL)]),
        Err(Error::NotNullable { argument: 0 })
    );
}

#[test]
fn a_null_object_where_the_protocol_forbids_one_is_refused_on_the_way_in() {
    // wl_display.error's object argument is not nullable, so a zero there is
    // a client that lied rather than a surface being unmapped.
    let mut bytes = write_one(
        1,
        0,
        ERROR,
        &[Arg::Object(ObjectId(9)), Arg::Uint(0), Arg::Str(Some("x"))],
    );
    if let Some(word) = bytes.get_mut(8..12) {
        word.copy_from_slice(&0u32.to_le_bytes());
    }
    let fds = [];
    assert_eq!(
        Reader::new(&bytes, &fds).read(ERROR),
        Err(Error::NotNullable { argument: 0 })
    );
}

#[test]
fn ints_and_fixed_round_trip_through_every_corner_of_their_range() {
    for value in [0, 1, -1, i32::MIN, i32::MAX, -7654321] {
        let bytes = write_one(
            3,
            1,
            ATTACH,
            &[
                Arg::Object(ObjectId(4)),
                Arg::Int(value),
                Arg::Int(-value.saturating_add(1)),
            ],
        );
        let fds = [];
        let (_, args) = Reader::new(&bytes, &fds).read(ATTACH).expect("read back");
        assert_eq!(args.get(1), Some(&Arg::Int(value)));
    }

    for raw in [0, 1, -1, i32::MIN, i32::MAX, 0x1234_5678] {
        let value = Fixed::from_raw(raw);
        let bytes = write_one(
            5,
            2,
            MOTION,
            &[Arg::Uint(99), Arg::Fixed(value), Arg::Fixed(Fixed::ZERO)],
        );
        let fds = [];
        let (_, args) = Reader::new(&bytes, &fds).read(MOTION).expect("read back");
        assert_eq!(args.get(1), Some(&Arg::Fixed(value)));
    }
}

#[test]
fn fixed_is_wayland_s_signed_24_8() {
    assert_eq!(Fixed::ONE.to_raw(), 256);
    assert_eq!(Fixed::from_int(1), Fixed::ONE);
    assert_eq!(Fixed::from_int(-3).to_raw(), -768);
    assert_eq!(Fixed::from_raw(384).to_f64(), 1.5);
    assert_eq!(Fixed::from_f64(1.5).to_raw(), 384);
    // wl_fixed_to_int is a shift, so it rounds towards negative infinity and
    // not towards zero: -0.5 is -1, as in wayland-util.h.
    assert_eq!(Fixed::from_f64(-0.5).to_int(), -1);
    assert_eq!(Fixed::from_f64(0.5).to_int(), 0);
    assert_eq!(Fixed::from_f64(-1.0).to_int(), -1);
    // A value that cannot be represented saturates rather than wrapping a
    // pointer to the other side of the screen.
    assert_eq!(Fixed::from_f64(1e30).to_raw(), i32::MAX);
    assert_eq!(Fixed::from_f64(-1e30).to_raw(), i32::MIN);
    assert_eq!(Fixed::from_f64(f64::NAN), Fixed::ZERO);
    assert_eq!(Fixed::from_int(i32::MAX).to_raw(), i32::MAX);
    assert_eq!(format!("{}", Fixed::from_raw(384)), "1.5");
}

#[test]
fn an_array_carries_its_length_without_a_nul() {
    // wl_keyboard.enter's pressed keys: three keycodes, one word each.
    let keys: Vec<u8> = [30u32, 48, 46]
        .iter()
        .flat_map(|k| k.to_le_bytes())
        .collect();
    let bytes = write_one(
        8,
        1,
        ENTER,
        &[Arg::Uint(42), Arg::Object(ObjectId(3)), Arg::Array(&keys)],
    );
    assert_eq!(bytes.len(), 8 + 4 + 4 + 4 + 12);
    assert_eq!(bytes.get(16..20), Some(&12u32.to_le_bytes()[..]));

    let fds = [];
    let (_, args) = Reader::new(&bytes, &fds).read(ENTER).expect("read back");
    assert_eq!(args.get(2), Some(&Arg::Array(&keys[..])));

    // An empty array is a length of zero and no bytes, which is how a
    // keyboard enter with nothing held is sent.
    let empty = write_one(
        8,
        1,
        ENTER,
        &[Arg::Uint(1), Arg::Object(ObjectId(3)), Arg::Array(&[])],
    );
    assert_eq!(empty.len(), 20);
    let (_, args) = Reader::new(&empty, &fds).read(ENTER).expect("read back");
    assert_eq!(args.get(2), Some(&Arg::Array(&[][..])));
}

#[test]
fn a_descriptor_takes_no_room_in_the_stream_and_is_taken_in_order() {
    // wl_keyboard.keymap(1, fd, 4096).
    let bytes = write_one(
        8,
        0,
        KEYMAP,
        &[Arg::Uint(1), Arg::Fd(Fd(7)), Arg::Uint(4096)],
    );
    assert_eq!(bytes.len(), 16, "the descriptor is not in the bytes");

    let mut writer = Writer::new();
    writer
        .write(
            ObjectId(8),
            0,
            KEYMAP,
            &[Arg::Uint(1), Arg::Fd(Fd(7)), Arg::Uint(4096)],
        )
        .expect("written");
    assert_eq!(writer.descriptors(), [Fd(7)]);

    let fds = [Fd(11), Fd(12)];
    let mut reader = Reader::new(&bytes, &fds);
    let (_, args) = reader.read(KEYMAP).expect("read back");
    assert_eq!(
        args.get(1),
        Some(&Arg::Fd(Fd(11))),
        "the first that arrived"
    );
    assert_eq!(reader.descriptors_taken(), 1);

    // A message wanting a descriptor that did not arrive is refused, and
    // consumes nothing.
    let none = [];
    let mut starved = Reader::new(&bytes, &none);
    assert_eq!(
        starved.read(KEYMAP),
        Err(Error::NoDescriptor { argument: 1 })
    );
    assert_eq!(starved.consumed(), 0);
}

#[test]
fn several_messages_are_read_out_of_one_buffer_in_order() {
    let mut writer = Writer::new();
    writer
        .write(
            ObjectId::DISPLAY,
            1,
            GET_REGISTRY,
            &[Arg::NewId(ObjectId(2))],
        )
        .expect("written");
    writer
        .write(
            ObjectId(2),
            0,
            BIND,
            &[
                Arg::Uint(1),
                Arg::AnyNewId {
                    interface: "wl_shm",
                    version: 1,
                    id: ObjectId(3),
                },
            ],
        )
        .expect("written");
    writer
        .write(
            ObjectId(3),
            1,
            ATTACH,
            &[Arg::Object(ObjectId(4)), Arg::Int(-1), Arg::Int(2)],
        )
        .expect("written");
    let (bytes, fds) = writer.take();
    assert!(writer.is_empty(), "taking leaves the writer empty");

    let mut reader = Reader::new(&bytes, &fds);
    let (first, _) = reader.read(GET_REGISTRY).expect("first");
    assert_eq!((first.sender, first.opcode), (ObjectId::DISPLAY, 1));
    let (second, args) = reader.read(BIND).expect("second");
    assert_eq!(second.sender, ObjectId(2));
    assert_eq!(args.get(1).and_then(Arg::as_str), Some("wl_shm"));
    let (third, args) = reader.read(ATTACH).expect("third");
    assert_eq!(third.sender, ObjectId(3));
    assert_eq!(args.get(1).and_then(Arg::as_int), Some(-1));
    assert!(reader.is_done());
    assert_eq!(reader.consumed(), bytes.len());
}

#[test]
fn a_message_that_has_not_all_arrived_is_left_for_the_next_read() {
    let whole = write_one(1, 1, GET_REGISTRY, &[Arg::NewId(ObjectId(2))]);
    let fds = [];
    for cut in 0..whole.len() {
        let part = whole.get(..cut).unwrap_or(&[]);
        let mut reader = Reader::new(part, &fds);
        let error = reader.read(GET_REGISTRY).expect_err("not all arrived");
        assert!(
            matches!(error, Error::Incomplete { .. }),
            "{cut} bytes gave {error:?}"
        );
        assert_eq!(reader.consumed(), 0, "nothing is consumed on a short read");
    }
    let mut reader = Reader::new(&whole, &fds);
    assert!(reader.read(GET_REGISTRY).is_ok());
}

#[test]
fn a_size_that_is_not_a_whole_message_is_refused() {
    let mut bytes = write_one(1, 1, GET_REGISTRY, &[Arg::NewId(ObjectId(2))]);
    let set_size = |bytes: &mut Vec<u8>, size: u32| {
        let packed = (size << 16) | 1;
        if let Some(word) = bytes.get_mut(4..8) {
            word.copy_from_slice(&packed.to_le_bytes());
        }
    };
    for size in [0, 1, 4, 7, 9, 10, 11] {
        set_size(&mut bytes, size);
        assert_eq!(Header::read(&bytes), Err(Error::Size(size as usize)));
    }
    // Larger than the format's own buffer.
    set_size(&mut bytes, 4100);
    assert_eq!(Header::read(&bytes), Err(Error::Size(4100)));
    // A whole header on its own is the smallest message there is.
    set_size(&mut bytes, 8);
    assert_eq!(Header::read(&bytes).map(|header| header.size), Ok(8));
}

#[test]
fn a_message_with_bytes_left_over_is_refused() {
    // Twelve bytes of header and one id, claiming sixteen.
    let mut bytes = write_one(1, 1, GET_REGISTRY, &[Arg::NewId(ObjectId(2))]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    let packed = (16u32 << 16) | 1;
    if let Some(word) = bytes.get_mut(4..8) {
        word.copy_from_slice(&packed.to_le_bytes());
    }
    let fds = [];
    assert_eq!(
        Reader::new(&bytes, &fds).read(GET_REGISTRY),
        Err(Error::Size(16))
    );
}

#[test]
fn a_string_that_runs_past_the_message_or_is_not_a_string_is_refused() {
    let good = write_one(
        1,
        0,
        ERROR,
        &[
            Arg::Object(ObjectId(9)),
            Arg::Uint(0),
            Arg::Str(Some("oops")),
        ],
    );
    let fds = [];

    // A length longer than the bytes that follow it.
    let mut long = good.clone();
    if let Some(word) = long.get_mut(16..20) {
        word.copy_from_slice(&64u32.to_le_bytes());
    }
    assert_eq!(
        Reader::new(&long, &fds).read(ERROR),
        Err(Error::Truncated { argument: 2 })
    );

    // A length the size_t cast cannot hold is caught before the read.
    let mut huge = good.clone();
    if let Some(word) = huge.get_mut(16..20) {
        word.copy_from_slice(&u32::MAX.to_le_bytes());
    }
    assert!(matches!(
        Reader::new(&huge, &fds).read(ERROR),
        Err(Error::Truncated { argument: 2 } | Error::Length { argument: 2, .. })
    ));

    // No NUL where the length says there is one: libwayland's
    // `s[length - 1] != '\0'` check.
    let mut unterminated = good.clone();
    if let Some(byte) = unterminated.get_mut(24) {
        *byte = b'!';
    }
    assert_eq!(
        Reader::new(&unterminated, &fds).read(ERROR),
        Err(Error::NotAString { argument: 2 })
    );

    // A NUL inside it: libwayland's `strlen(s) != length - 1`.
    let mut embedded = good.clone();
    if let Some(byte) = embedded.get_mut(21) {
        *byte = 0;
    }
    assert_eq!(
        Reader::new(&embedded, &fds).read(ERROR),
        Err(Error::NotAString { argument: 2 })
    );

    // Not UTF-8, which libwayland allows and a Rust server does not.
    let mut latin = good;
    if let Some(byte) = latin.get_mut(21) {
        *byte = 0xFF;
    }
    assert_eq!(
        Reader::new(&latin, &fds).read(ERROR),
        Err(Error::NotAString { argument: 2 })
    );
}

#[test]
fn a_string_with_a_nul_in_it_cannot_be_written_either() {
    let mut writer = Writer::new();
    assert_eq!(
        writer.write(
            ObjectId(1),
            0,
            ERROR,
            &[
                Arg::Object(ObjectId(1)),
                Arg::Uint(0),
                Arg::Str(Some("a\0b"))
            ]
        ),
        Err(Error::BadArgument { argument: 2 })
    );
    assert!(writer.is_empty());
}

#[test]
fn a_message_too_long_for_the_format_is_refused_and_leaves_nothing_behind() {
    let mut writer = Writer::new();
    writer
        .write(ObjectId(1), 1, GET_REGISTRY, &[Arg::NewId(ObjectId(2))])
        .expect("the first fits");
    let before = writer.bytes().len();

    let huge = vec![0u8; crate::MAX_MESSAGE];
    assert_eq!(
        writer.write(
            ObjectId(8),
            1,
            ENTER,
            &[Arg::Uint(1), Arg::Object(ObjectId(3)), Arg::Array(&huge)]
        ),
        Err(Error::TooLarge)
    );
    assert_eq!(
        writer.bytes().len(),
        before,
        "the first message is untouched"
    );

    // The largest array that does fit is written.
    let room = crate::MAX_MESSAGE - 8 - 4 - 4 - 4;
    let big = vec![7u8; room];
    writer
        .write(
            ObjectId(8),
            1,
            ENTER,
            &[Arg::Uint(1), Arg::Object(ObjectId(3)), Arg::Array(&big)],
        )
        .expect("exactly the limit fits");
    assert_eq!(writer.bytes().len(), before + crate::MAX_MESSAGE);
}

#[test]
fn an_argument_of_the_wrong_type_is_refused_by_the_writer() {
    let mut writer = Writer::new();
    assert_eq!(
        writer.write(ObjectId(1), 1, GET_REGISTRY, &[Arg::Uint(2)]),
        Err(Error::BadArgument { argument: 0 })
    );
    assert_eq!(
        writer.write(ObjectId(1), 1, GET_REGISTRY, &[]),
        Err(Error::BadArgument { argument: 0 })
    );
    assert_eq!(
        writer.write(
            ObjectId(1),
            1,
            GET_REGISTRY,
            &[Arg::NewId(ObjectId(2)), Arg::Uint(1)]
        ),
        Err(Error::BadArgument { argument: 1 })
    );
    assert!(writer.is_empty());
}

#[test]
fn an_unknown_message_is_skipped_with_the_descriptors_it_carried() {
    let mut writer = Writer::new();
    writer
        .write(
            ObjectId(8),
            0,
            KEYMAP,
            &[Arg::Uint(1), Arg::Fd(Fd(5)), Arg::Uint(16)],
        )
        .expect("written");
    writer
        .write(
            ObjectId::DISPLAY,
            1,
            GET_REGISTRY,
            &[Arg::NewId(ObjectId(2))],
        )
        .expect("written");
    let (bytes, _) = writer.take();

    let fds = [Fd(21), Fd(22)];
    let mut reader = Reader::new(&bytes, &fds);
    let skipped = reader.skip(1).expect("skipped");
    assert_eq!(skipped.sender, ObjectId(8));
    assert_eq!(reader.descriptors_taken(), 1);
    let (next, args) = reader.read(GET_REGISTRY).expect("the one after it");
    assert_eq!(next.sender, ObjectId::DISPLAY);
    assert_eq!(args.first().and_then(Arg::as_object), Some(ObjectId(2)));

    // Skipping more descriptors than arrived is refused.
    let mut short = Reader::new(&bytes, &fds);
    assert!(short.skip(3).is_err());
    assert_eq!(short.consumed(), 0);
}

#[test]
fn every_argument_says_what_type_it_is() {
    let keys = [1u8, 2, 3, 4];
    let args = [
        Arg::Int(-1),
        Arg::Uint(1),
        Arg::Fixed(Fixed::ONE),
        Arg::Str(Some("a")),
        Arg::Str(None),
        Arg::Object(ObjectId(1)),
        Arg::Object(ObjectId::NULL),
        Arg::NewId(ObjectId(2)),
        Arg::Array(&keys),
        Arg::Fd(Fd(3)),
    ];
    let kinds: Vec<ArgType> = args.iter().map(Arg::kind).collect();
    assert_eq!(
        kinds,
        [
            ArgType::Int,
            ArgType::Uint,
            ArgType::Fixed,
            ArgType::Str { nullable: false },
            ArgType::Str { nullable: true },
            ArgType::Object { nullable: false },
            ArgType::Object { nullable: true },
            ArgType::NewId,
            ArgType::Array,
            ArgType::Fd,
        ]
    );
    assert_eq!(args.first().and_then(Arg::as_int), Some(-1));
    assert_eq!(args.get(1).and_then(Arg::as_uint), Some(1));
    assert_eq!(args.get(3).and_then(Arg::as_str), Some("a"));
    assert_eq!(args.get(9).and_then(Arg::as_fd), Some(Fd(3)));
    assert_eq!(args.first().and_then(Arg::as_uint), None);
}

#[test]
fn ids_stay_in_the_half_that_made_them() {
    assert!(ObjectId::DISPLAY.is_client());
    assert!(!ObjectId::NULL.is_client() && !ObjectId::NULL.is_server());
    assert!(ObjectId::NULL.is_null());
    assert!(ObjectId(ObjectId::SERVER_BASE).is_server());
    assert!(ObjectId(ObjectId::SERVER_BASE - 1).is_client());
    assert!(ObjectId(u32::MAX).is_server());
    assert_eq!(ObjectId::SERVER_BASE, 0xFF00_0000);
}

#[test]
fn an_object_map_refuses_what_a_client_may_not_do() {
    let mut objects = Objects::new();
    assert!(objects.is_empty());

    assert_eq!(
        objects.insert(ObjectId::NULL, &WL_SURFACE, 1),
        Err(ObjectError::Null)
    );
    assert_eq!(
        objects.insert(ObjectId(ObjectId::SERVER_BASE), &WL_SURFACE, 1),
        Err(ObjectError::WrongHalf(ObjectId(ObjectId::SERVER_BASE)))
    );
    assert_eq!(
        objects.insert(ObjectId(4), &WL_SURFACE, 7),
        Err(ObjectError::Version {
            asked: 7,
            offered: 6
        })
    );
    assert_eq!(
        objects.insert(ObjectId(4), &WL_SURFACE, 0),
        Err(ObjectError::Version {
            asked: 0,
            offered: 6
        })
    );
    assert!(objects.is_empty(), "nothing refused was added");

    objects.insert(ObjectId(4), &WL_SURFACE, 6).expect("made");
    assert_eq!(
        objects.get(ObjectId(4)),
        Some(&Entry {
            interface: &WL_SURFACE,
            version: 6
        })
    );
    assert_eq!(
        objects.insert(ObjectId(4), &WL_CALLBACK, 1),
        Err(ObjectError::InUse(ObjectId(4)))
    );
    assert_eq!(objects.len(), 1);

    assert_eq!(
        objects.remove(ObjectId(5)),
        Err(ObjectError::Unknown(ObjectId(5)))
    );
    assert_eq!(
        objects.remove(ObjectId(4)).map(|entry| entry.version),
        Ok(6)
    );
    assert!(!objects.contains(ObjectId(4)));
}

#[test]
fn the_server_names_its_own_objects_without_reusing_one() {
    let mut objects = Objects::new();
    let first = objects.create(&WL_CALLBACK, 1).expect("made");
    let second = objects.create(&WL_CALLBACK, 1).expect("made");
    assert_eq!(first, ObjectId(ObjectId::SERVER_BASE));
    assert_eq!(second, ObjectId(ObjectId::SERVER_BASE + 1));
    assert!(first.is_server() && second.is_server());

    // A dropped id is not handed out again while the counter is climbing, so
    // a client that kept a stale reference names nothing rather than
    // something new.
    let _ = objects.remove(first).expect("dropped");
    let third = objects.create(&WL_CALLBACK, 1).expect("made");
    assert_eq!(third, ObjectId(ObjectId::SERVER_BASE + 2));

    objects
        .insert(ObjectId(1), &WL_SURFACE, 1)
        .expect("a client's");
    let ids: Vec<ObjectId> = objects.iter().map(|(id, _)| id).collect();
    assert_eq!(ids, [ObjectId(1), second, third], "in id order");
}

#[test]
fn an_interface_finds_its_methods_by_opcode_and_by_name() {
    assert_eq!(WL_SURFACE.request_opcode("destroy"), Some(0));
    assert_eq!(WL_SURFACE.request_opcode("attach"), Some(1));
    assert_eq!(WL_SURFACE.request_opcode("commit"), None);
    assert_eq!(WL_SURFACE.event_opcode("enter"), Some(0));
    assert_eq!(
        WL_SURFACE.request(1).map(|method| method.name),
        Some("attach")
    );
    assert_eq!(WL_SURFACE.request(2), None);
    assert_eq!(
        WL_SURFACE.event(0).map(|method| method.signature),
        Some(&[ArgType::Object { nullable: false }][..])
    );
    assert_eq!(
        WL_CALLBACK.request(0),
        None,
        "wl_callback takes no requests"
    );
}

#[test]
fn padding_rounds_up_to_the_next_word() {
    assert_eq!(crate::padded(0), 0);
    for (len, want) in [(1, 4), (3, 4), (4, 4), (5, 8), (8, 8), (13, 16)] {
        assert_eq!(crate::padded(len), want, "{len}");
    }
}

// ---------------------------------------------------------------------------
// Against libwayland itself
//
// `probe/wire.c` drives a real libwayland client and a real libwayland server
// over socket pairs and prints what they wrote; `probe/wire.txt` is that
// output, committed. Everything below requires this crate to write the same
// bytes and to read them back, so the format is pinned to an implementation
// rather than to this crate's reading of connection.c.
// ---------------------------------------------------------------------------

/// The probe's output, one named message per line.
const PROBE: &str = include_str!("../probe/wire.txt");

/// `wl_surface.damage`: four ints.
const DAMAGE: &[ArgType] = &[ArgType::Int, ArgType::Int, ArgType::Int, ArgType::Int];

/// `wl_shm.create_pool`: a `new_id`, a descriptor and a size.
const CREATE_POOL: &[ArgType] = &[ArgType::NewId, ArgType::Fd, ArgType::Int];

/// `wl_registry.global`: a name, an interface and a version.
const GLOBAL: &[ArgType] = &[
    ArgType::Uint,
    ArgType::Str { nullable: false },
    ArgType::Uint,
];

/// `wl_surface.set_buffer_scale` and `set_buffer_transform`: one int each.
const ONE_INT: &[ArgType] = &[ArgType::Int];

/// `wl_display.sync` and `wl_compositor.create_surface`: one `new_id`.
const ONE_NEW_ID: &[ArgType] = &[ArgType::NewId];

/// One message as the probe made it: who sent it, which opcode, its
/// signature and its arguments.
struct Sent<'a> {
    sender: u32,
    opcode: u16,
    signature: &'static [ArgType],
    args: Vec<Arg<'a>>,
}

fn sent<'a>(
    sender: u32,
    opcode: u16,
    signature: &'static [ArgType],
    args: Vec<Arg<'a>>,
) -> Sent<'a> {
    Sent {
        sender,
        opcode,
        signature,
        args,
    }
}

/// The bytes and descriptor count the probe printed for `name`.
fn probed(name: &str) -> (Vec<u8>, usize) {
    let line = PROBE
        .lines()
        .find(|line| line.starts_with(name) && line.get(name.len()..=name.len()) == Some(" "))
        .unwrap_or_else(|| panic!("{name} is not in probe/wire.txt"));
    let mut fds = 0;
    let mut bytes = Vec::new();
    for field in line.split_whitespace().skip(1) {
        if let Some(count) = field.strip_prefix("fds=") {
            fds = count.parse().expect("a count");
        } else if let Some(hex) = field.strip_prefix("bytes=") {
            assert_eq!(hex.len() % 2, 0, "{name}: an odd number of hex digits");
            bytes = (0..hex.len() / 2)
                .map(|index| {
                    let pair = hex.get(index * 2..index * 2 + 2).expect("in range");
                    u8::from_str_radix(pair, 16).expect("hex")
                })
                .collect();
        }
    }
    assert!(!bytes.is_empty(), "{name}: no bytes");
    (bytes, fds)
}

/// The keycodes `probe/wire.c` sends in `wl_keyboard.enter`.
fn probe_keys() -> Vec<u8> {
    [30u32, 48, 46]
        .iter()
        .flat_map(|code| code.to_le_bytes())
        .collect()
}

#[test]
fn the_probe_names_the_libwayland_it_came_from() {
    let first = PROBE.lines().next().expect("a first line");
    assert!(
        first.starts_with("# libwayland "),
        "probe/wire.txt should say which libwayland wrote it: {first}"
    );
}

#[test]
fn every_message_libwayland_sent_is_written_the_same_way_here() {
    let keys = probe_keys();
    let cases: Vec<(&str, Vec<Sent<'_>>)> = vec![
        (
            "wl_display.get_registry",
            vec![sent(1, 1, ONE_NEW_ID, vec![Arg::NewId(ObjectId(2))])],
        ),
        (
            "wl_display.sync",
            vec![sent(1, 0, ONE_NEW_ID, vec![Arg::NewId(ObjectId(3))])],
        ),
        (
            "wl_registry.bind:wl_compositor:6",
            vec![sent(
                2,
                0,
                BIND,
                vec![
                    Arg::Uint(1),
                    Arg::AnyNewId {
                        interface: "wl_compositor",
                        version: 6,
                        id: ObjectId(4),
                    },
                ],
            )],
        ),
        (
            "wl_registry.bind:wl_shm:1",
            vec![sent(
                2,
                0,
                BIND,
                vec![
                    Arg::Uint(2),
                    Arg::AnyNewId {
                        interface: "wl_shm",
                        version: 1,
                        id: ObjectId(5),
                    },
                ],
            )],
        ),
        (
            "wl_compositor.create_surface",
            vec![sent(4, 0, ONE_NEW_ID, vec![Arg::NewId(ObjectId(6))])],
        ),
        (
            "wl_surface.attach:null",
            vec![sent(
                6,
                1,
                ATTACH,
                vec![Arg::Object(ObjectId::NULL), Arg::Int(0), Arg::Int(0)],
            )],
        ),
        (
            "wl_surface.damage:negative",
            vec![sent(
                6,
                2,
                DAMAGE,
                vec![Arg::Int(-1), Arg::Int(-2), Arg::Int(3), Arg::Int(4)],
            )],
        ),
        (
            "wl_shm.create_pool",
            vec![sent(
                5,
                0,
                CREATE_POOL,
                vec![Arg::NewId(ObjectId(7)), Arg::Fd(Fd(9)), Arg::Int(4096)],
            )],
        ),
        (
            "wl_surface.scale+transform+commit",
            vec![
                sent(6, 8, ONE_INT, vec![Arg::Int(2)]),
                sent(6, 7, ONE_INT, vec![Arg::Int(1)]),
                sent(6, 6, &[], vec![]),
            ],
        ),
        (
            "wl_registry.global:wl_compositor",
            vec![sent(
                2,
                0,
                GLOBAL,
                vec![Arg::Uint(1), Arg::Str(Some("wl_compositor")), Arg::Uint(6)],
            )],
        ),
        (
            "wl_keyboard.enter:3keys",
            vec![sent(
                4,
                1,
                ENTER,
                vec![Arg::Uint(42), Arg::Object(ObjectId(3)), Arg::Array(&keys)],
            )],
        ),
        (
            "wl_keyboard.enter:nokeys",
            vec![sent(
                4,
                1,
                ENTER,
                vec![Arg::Uint(43), Arg::Object(ObjectId(3)), Arg::Array(&[])],
            )],
        ),
        (
            "wl_keyboard.enter:5bytes",
            vec![sent(
                4,
                1,
                ENTER,
                vec![
                    Arg::Uint(44),
                    Arg::Object(ObjectId(3)),
                    Arg::Array(b"abcde"),
                ],
            )],
        ),
        (
            "wl_pointer.motion:1.5,-2.25",
            vec![sent(
                5,
                2,
                MOTION,
                vec![
                    Arg::Uint(1000),
                    Arg::Fixed(Fixed::from_f64(1.5)),
                    Arg::Fixed(Fixed::from_f64(-2.25)),
                ],
            )],
        ),
        (
            "wl_display.error:invalid_method",
            vec![sent(
                1,
                0,
                ERROR,
                vec![
                    Arg::Object(ObjectId(1)),
                    Arg::Uint(1),
                    Arg::Str(Some("no such thing")),
                ],
            )],
        ),
    ];

    for (name, messages) in cases {
        let (expected, fds) = probed(name);
        let mut writer = Writer::new();
        for message in &messages {
            writer
                .write(
                    ObjectId(message.sender),
                    message.opcode,
                    message.signature,
                    &message.args,
                )
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        }
        assert_eq!(
            writer.bytes(),
            expected,
            "{name}: this crate writes other bytes than libwayland"
        );
        assert_eq!(
            writer.descriptors().len(),
            fds,
            "{name}: a different number of descriptors"
        );

        // And read libwayland's own bytes back, which is the direction the
        // server actually runs in.
        let taken: Vec<Fd> = (0..fds).map(|index| Fd(index as i32)).collect();
        let mut reader = Reader::new(&expected, &taken);
        for message in &messages {
            let (header, args) = reader
                .read(message.signature)
                .unwrap_or_else(|error| panic!("{name}: reading back: {error:?}"));
            assert_eq!(header.sender, ObjectId(message.sender), "{name}");
            assert_eq!(header.opcode, message.opcode, "{name}");
            // The descriptor read back is the one that arrived, not the one
            // that was written, so those are compared by type alone.
            for (got, want) in args.iter().zip(&message.args) {
                match (got, want) {
                    (Arg::Fd(_), Arg::Fd(_)) => {}
                    _ => assert_eq!(got, want, "{name}"),
                }
            }
            assert_eq!(args.len(), message.args.len(), "{name}");
        }
        assert!(reader.is_done(), "{name}: bytes left over");
        assert_eq!(reader.consumed(), expected.len(), "{name}");
    }
}

#[test]
fn libwayland_pads_a_string_and_an_array_the_same_way_this_crate_does() {
    // "wl_compositor" is 13 bytes, 14 with its NUL, 16 padded; "wl_shm" is
    // 6, 7 and 8. Both appear in the probe, so the two padding cases are
    // libwayland's own bytes and not this crate's arithmetic.
    let (long, _) = probed("wl_registry.bind:wl_compositor:6");
    let (short, _) = probed("wl_registry.bind:wl_shm:1");
    assert_eq!(long.len(), 40);
    assert_eq!(short.len(), 32);
    assert_eq!(long.get(12..16), Some(&14u32.to_le_bytes()[..]));
    assert_eq!(short.get(12..16), Some(&7u32.to_le_bytes()[..]));

    // An array of five bytes takes eight, and its length carries no NUL.
    let (odd, _) = probed("wl_keyboard.enter:5bytes");
    assert_eq!(odd.len(), 28);
    assert_eq!(odd.get(16..20), Some(&5u32.to_le_bytes()[..]));
    assert_eq!(odd.get(20..25), Some(&b"abcde"[..]));
}

#[test]
fn a_descriptor_libwayland_sent_is_not_in_its_bytes() {
    let (bytes, fds) = probed("wl_shm.create_pool");
    assert_eq!(fds, 1, "the probe sent one descriptor");
    assert_eq!(
        bytes.len(),
        16,
        "a header, a new_id and a size, and nothing for the descriptor"
    );
}
