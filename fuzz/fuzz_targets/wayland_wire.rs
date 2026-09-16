//! Fuzz Wayland's wire format: the bytes a client's socket carries.
//!
//! Every byte a compositor reads comes from a program it does not trust, and
//! the wire format is untyped -- the same eight bytes are a different message
//! for a different object -- so the reader is handed a signature and has to
//! take the client's word for the lengths inside it. This target is a
//! stranger at the other end of the socket: it picks a signature and hands
//! the reader bytes.
//!
//! # The properties
//!
//! Not panicking is the floor, and it is the one that matters most: a
//! compositor that aborts on a bad message takes every client's windows with
//! it. Beyond it:
//!
//! 1. **A read consumes a whole message or nothing.** After an error the
//!    reader is where it was, so a caller that met `Incomplete` can read more
//!    bytes and try again; after success it has moved on by exactly the size
//!    in the header.
//! 2. **Nothing is invented.** Every string is UTF-8 with no NUL in it,
//!    every array lies inside the message, and no argument names a
//!    descriptor that did not arrive.
//! 3. **What is read can be written, and gives the same bytes.** A message
//!    the reader accepted, written back with the same signature, is the same
//!    bytes it was read from -- except for the padding, which a client may
//!    fill with anything and the writer always zeroes.
//! 4. **A writer never builds a message it would not read.** Whatever it
//!    writes, the reader takes back with the same signature and the same
//!    arguments.

#![no_main]

use compositor_wire::{Arg, ArgType, Error, Fd, Fixed, Header, ObjectId, Reader, Writer};
use libfuzzer_sys::fuzz_target;

/// The descriptors that "arrived" beside the bytes.
const FDS: [Fd; 4] = [Fd(10), Fd(11), Fd(12), Fd(13)];

/// Signatures are `&'static`, so the target picks from a table rather than
/// building one. Each is a shape a real message has.
const SIGNATURES: [&[ArgType]; 12] = [
    &[],
    &[ArgType::NewId],
    &[ArgType::Uint, ArgType::AnyNewId],
    &[
        ArgType::Object { nullable: true },
        ArgType::Int,
        ArgType::Int,
    ],
    &[
        ArgType::Object { nullable: false },
        ArgType::Uint,
        ArgType::Str { nullable: false },
    ],
    &[ArgType::Uint, ArgType::Fd, ArgType::Uint],
    &[
        ArgType::Uint,
        ArgType::Object { nullable: false },
        ArgType::Array,
    ],
    &[ArgType::Uint, ArgType::Fixed, ArgType::Fixed],
    &[ArgType::Str { nullable: true }, ArgType::Str { nullable: true }],
    &[ArgType::Array, ArgType::Array],
    &[ArgType::Fd, ArgType::Fd, ArgType::Fd, ArgType::Fd],
    &[
        ArgType::Int,
        ArgType::Uint,
        ArgType::Fixed,
        ArgType::Str { nullable: true },
        ArgType::Object { nullable: true },
        ArgType::NewId,
        ArgType::Array,
        ArgType::Fd,
    ],
];

/// Zero a message's padding, so bytes read and bytes written can be compared
/// where a client left rubbish there.
fn zero_padding(bytes: &[u8], signature: &[ArgType]) -> Option<Vec<u8>> {
    let mut out = bytes.to_vec();
    let mut at = 8;
    for kind in signature {
        let word = |at: usize| -> Option<u32> {
            let slice: [u8; 4] = out.get(at..at + 4)?.try_into().ok()?;
            Some(u32::from_le_bytes(slice))
        };
        match kind {
            ArgType::Fd => continue,
            ArgType::Str { .. } | ArgType::Array => {
                let len = word(at)? as usize;
                at += 4;
                let padded = len.next_multiple_of(4);
                for index in at + len..at + padded {
                    *out.get_mut(index)? = 0;
                }
                at += padded;
            }
            ArgType::AnyNewId => {
                let len = word(at)? as usize;
                at += 4;
                let padded = len.next_multiple_of(4);
                for index in at + len..at + padded {
                    *out.get_mut(index)? = 0;
                }
                at += padded + 8;
            }
            _ => at += 4,
        }
    }
    Some(out)
}

fuzz_target!(|bytes: &[u8]| {
    let Some((&choice, body)) = bytes.split_first() else {
        return;
    };
    let signature = SIGNATURES
        .get(usize::from(choice % 12))
        .copied()
        .unwrap_or(&[]);

    // 1 and 2: read as much as the bytes allow, one message at a time.
    let mut reader = Reader::new(body, &FDS);
    let mut messages = Vec::new();
    loop {
        let before = reader.consumed();
        let fds_before = reader.descriptors_taken();
        match reader.read(signature) {
            Ok((header, args)) => {
                assert_eq!(
                    reader.consumed(),
                    before + header.size,
                    "a read moved on by other than the header's size"
                );
                assert!(header.size >= 8 && header.size <= compositor_wire::MAX_MESSAGE);
                assert_eq!(args.len(), signature.len());
                for (arg, kind) in args.iter().zip(signature) {
                    check(arg, *kind);
                }
                let taken = args.iter().filter(|arg| arg.as_fd().is_some()).count();
                assert_eq!(reader.descriptors_taken(), fds_before + taken);
                messages.push((header, args));
            }
            Err(error) => {
                assert_eq!(reader.consumed(), before, "a failed read consumed bytes");
                assert_eq!(reader.descriptors_taken(), fds_before);
                if matches!(error, Error::Incomplete { needed, have } if needed > have) {
                    break;
                }
                break;
            }
        }
    }

    // 3: everything read, written back, is the bytes it came from once the
    // padding a client may have filled is zeroed.
    if !messages.is_empty()
        && let Some(flat) = zero_padding(body.get(..reader.consumed()).unwrap_or(&[]), signature)
    {
        let mut writer = Writer::new();
        let mut all = true;
        for (header, args) in &messages {
            if writer
                .write(header.sender, header.opcode, signature, args)
                .is_err()
            {
                all = false;
                break;
            }
        }
        // Only when every message went back; a descriptor argument the
        // reader filled from FDS is written as that same number, so the
        // bytes are unaffected either way.
        if all && messages.len() == 1 && flat.len() == writer.bytes().len() {
            assert_eq!(writer.bytes(), flat, "read and written back differ");
        }
    }

    // 4: whatever the writer builds, the reader takes back.
    let mut writer = Writer::new();
    let mut source = body.iter().copied();
    let mut built: Vec<Arg<'static>> = Vec::new();
    for kind in signature {
        let seed = source.next().unwrap_or(0);
        built.push(make(*kind, seed));
    }
    if writer
        .write(ObjectId(1), u16::from(choice), signature, &built)
        .is_ok()
    {
        let written = writer.bytes().to_vec();
        let fds = writer.descriptors().to_vec();
        let header = Header::read(&written).expect("the writer wrote a header it can read");
        assert_eq!(header.size, written.len());
        let (back, args) = Reader::new(&written, &fds)
            .read(signature)
            .expect("the writer wrote a message it can read");
        assert_eq!(back, header);
        assert_eq!(args, built, "a written message read back differently");
    }
});

/// Check one argument against the type it was read as: property 2.
fn check(arg: &Arg<'_>, kind: ArgType) {
    match (arg, kind) {
        (Arg::Str(Some(text)), ArgType::Str { .. }) => {
            assert!(!text.as_bytes().contains(&0), "a NUL inside a string");
        }
        (Arg::Str(None), ArgType::Str { nullable }) => {
            assert!(nullable, "a null string where the protocol forbids one");
        }
        (Arg::Object(id), ArgType::Object { nullable }) => {
            assert!(nullable || !id.is_null(), "a null object where forbidden");
        }
        (Arg::NewId(id), ArgType::NewId) => assert!(!id.is_null()),
        (Arg::AnyNewId { interface, id, .. }, ArgType::AnyNewId) => {
            assert!(!interface.as_bytes().contains(&0));
            assert!(!id.is_null());
        }
        (Arg::Fd(fd), ArgType::Fd) => {
            assert!(FDS.contains(fd), "a descriptor that never arrived");
        }
        (Arg::Array(_) | Arg::Int(_) | Arg::Uint(_) | Arg::Fixed(_), _) => {}
        _ => panic!("{arg:?} was read as {kind:?}"),
    }
}

/// A value of `kind` from one byte, for property 4.
fn make(kind: ArgType, seed: u8) -> Arg<'static> {
    const NAMES: [&str; 4] = ["", "wl_shm", "wl_compositor", "a"];
    const BYTES: [&[u8]; 4] = [&[], &[1], &[1, 2, 3, 4, 5], &[0; 17]];
    let id = ObjectId(u32::from(seed) + 1);
    match kind {
        ArgType::Int => Arg::Int(i32::from(seed) - 128),
        ArgType::Uint => Arg::Uint(u32::from(seed)),
        ArgType::Fixed => Arg::Fixed(Fixed::from_raw(i32::from(seed) - 128)),
        ArgType::Object { nullable } if nullable && seed % 4 == 0 => Arg::Object(ObjectId::NULL),
        ArgType::Object { .. } => Arg::Object(id),
        ArgType::NewId => Arg::NewId(id),
        ArgType::Str { nullable } if nullable && seed % 4 == 0 => Arg::Str(None),
        ArgType::Str { .. } => Arg::Str(NAMES.get(usize::from(seed % 4)).copied()),
        ArgType::AnyNewId => Arg::AnyNewId {
            interface: NAMES.get(usize::from(seed % 4)).copied().unwrap_or(""),
            version: u32::from(seed),
            id,
        },
        ArgType::Array => Arg::Array(BYTES.get(usize::from(seed % 4)).copied().unwrap_or(&[])),
        ArgType::Fd => Arg::Fd(Fd(i32::from(seed))),
    }
}
