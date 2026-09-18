//! Tests for the render control protocol.
//!
//! Every offset and length below is written out by hand from the diagram at
//! the top of [`crate::message`], not taken from the module's constants: a
//! constant tested against itself proves nothing. What is being pinned down
//! is the wire, because a second driver -- `docs/GPU.md` §4's NVIDIA one --
//! has to speak the same one.

extern crate std;

use std::vec::Vec;

use ferrix_native_abi::rights::Rights;

use crate::message::*;

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn hello() -> Hello {
    Hello {
        version: VERSION,
        location: 0x0000_0800,
        name: Hello::named("virtio_gpu").expect("a name"),
        features: features::SUBMIT | features::FENCES,
        capset: 2,
        object_max: 64 * 1024 * 1024,
    }
}

/// Every message encodes to exactly its type's length, says that length in
/// its own header, and reads back as what it was.
#[test]
fn every_message_is_its_own_length_and_reads_back() {
    let messages = [
        Message::Hello(hello()),
        Message::Ready(Ready {
            renderer: 128,
            work_bytes: 1 << 20,
        }),
        Message::Refused(Refusal::Version),
        Message::MakeContext {
            context: 1,
            capset: 2,
        },
        Message::ContextMade {
            context: 1,
            status: Status::Ok,
        },
        Message::DropContext { context: 1 },
        Message::ContextGone {
            context: 1,
            status: Status::Ok,
        },
        Message::MakeObject(MakeObject {
            object: 4,
            context: 1,
            bytes: 4096,
            flags: flags::MAPPABLE | flags::TO_DEVICE,
            describe: Work { at: 0, len: 48 },
        }),
        Message::ObjectMade {
            object: 4,
            status: Status::OutOfMemory,
        },
        Message::DropObject { object: 4 },
        Message::ObjectGone {
            object: 4,
            status: Status::DeviceRefused,
        },
        Message::Submit(Submit {
            context: 1,
            fence: 9,
            commands: Work { at: 4096, len: 256 },
        }),
        Message::Submitted {
            fence: 9,
            status: Status::Ok,
        },
        Message::Wait {
            context: 1,
            fence: 9,
        },
        Message::Waited {
            fence: 9,
            status: Status::TimedOut,
        },
        Message::Stop,
        Message::Stopped,
    ];
    for message in messages {
        let encoded = message.encode();
        let bytes = encoded.as_bytes();
        assert_eq!(u32_at(bytes, 0), message.kind(), "{message:?}");
        assert_eq!(u32_at(bytes, 4) as usize, bytes.len(), "{message:?}");
        assert_eq!(Message::length_of(message.kind()), Some(bytes.len()));
        assert_eq!(Message::decode(bytes), Some(message), "{message:?}");
    }
}

/// The fields lie where the diagram says.
#[test]
fn fields_lie_where_the_diagram_puts_them() {
    let encoded = Message::Hello(hello()).encode();
    let bytes = encoded.as_bytes();
    assert_eq!(bytes.len(), 48);
    assert_eq!(u32_at(bytes, 0), 1, "HELLO");
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 1, "VERSION");
    assert_eq!(u32_at(bytes, 12), 0x800, "location");
    assert_eq!(&bytes[16..26], b"virtio_gpu");
    assert!(bytes[26..32].iter().all(|&byte| byte == 0), "zero-padded");
    assert_eq!(u32_at(bytes, 32), 0b11, "features");
    assert_eq!(u32_at(bytes, 36), 2, "capset");
    assert_eq!(u64_at(bytes, 40), 64 * 1024 * 1024, "object_max");

    let encoded = Message::Submit(Submit {
        context: 3,
        fence: 0x1234_5678_9abc_def0,
        commands: Work { at: 8192, len: 64 },
    })
    .encode();
    let bytes = encoded.as_bytes();
    assert_eq!(bytes.len(), 32);
    assert_eq!(u32_at(bytes, 8), 3);
    assert_eq!(u64_at(bytes, 16), 0x1234_5678_9abc_def0);
    assert_eq!(u32_at(bytes, 24), 8192);
    assert_eq!(u32_at(bytes, 28), 64);
}

/// A type this protocol has not got, a length that is not the type's, and a
/// reserved word that is not zero are each refused.
#[test]
fn a_message_that_is_not_one_is_refused() {
    assert_eq!(Message::decode(&[]), None);
    assert_eq!(Message::length_of(0), None);
    assert_eq!(Message::length_of(999), None);

    let encoded = Message::Stop.encode();
    let mut bytes: Vec<u8> = encoded.as_bytes().to_vec();
    bytes.push(0);
    assert_eq!(Message::decode(&bytes), None, "longer than STOP");

    // A length word that disagrees with the type.
    let mut bytes: Vec<u8> = Message::Stop.encode().as_bytes().to_vec();
    bytes[4] = 9;
    assert_eq!(Message::decode(&bytes), None);

    // A reserved word with something in it.
    let mut bytes: Vec<u8> = Message::DropObject { object: 1 }
        .encode()
        .as_bytes()
        .to_vec();
    bytes[12] = 1;
    assert_eq!(Message::decode(&bytes), None);

    // A status this version has not got.
    let mut bytes: Vec<u8> = Message::ObjectMade {
        object: 1,
        status: Status::Ok,
    }
    .encode()
    .as_bytes()
    .to_vec();
    bytes[12] = 99;
    assert_eq!(Message::decode(&bytes), None);
}

/// HELLO is checked field by field, and the name hardest of all: it is what
/// userspace picks a driver by, so a name that is not a name is a driver
/// nothing could ask for.
#[test]
fn hello_is_validated_field_by_field() {
    let rights = Hello::HANDLE_RIGHTS;
    assert_eq!(hello().validate(&rights), Ok(()));

    let mut wrong = hello();
    wrong.version = VERSION + 1;
    assert_eq!(wrong.validate(&rights), Err(Refusal::Version));

    let mut wrong = hello();
    wrong.name = [0; NAME_BYTES];
    assert_eq!(wrong.validate(&rights), Err(Refusal::Name));

    // Something after the terminating zero, which would let two drivers
    // with the same name differ on the wire.
    let mut wrong = hello();
    wrong.name[11] = b'x';
    assert_eq!(wrong.validate(&rights), Err(Refusal::Name));

    // A name that is not a name.
    let mut wrong = hello();
    wrong.name = *b"virtio gpu\0\0\0\0\0\0";
    assert_eq!(wrong.validate(&rights), Err(Refusal::Name));

    let mut wrong = hello();
    wrong.features |= 1 << 31;
    assert_eq!(wrong.validate(&rights), Err(Refusal::Features));

    // A driver that cannot take a command buffer has no render node to
    // serve, whatever else it can do.
    let mut wrong = hello();
    wrong.features = features::FENCES;
    assert_eq!(wrong.validate(&rights), Err(Refusal::Features));

    let mut wrong = hello();
    wrong.object_max = 0;
    assert_eq!(wrong.validate(&rights), Err(Refusal::ObjectMax));

    assert_eq!(hello().validate(&[]), Err(Refusal::Rights));
    assert_eq!(
        hello().validate(&[Rights(Rights::READ.0)]),
        Err(Refusal::Rights)
    );
}

/// The name a driver gives is the one userspace reads back, and a second
/// driver's is its own: this is the field the whole seam exists for.
#[test]
fn a_drivers_name_survives_the_wire() {
    for name in ["virtio_gpu", "nvidia", "a"] {
        let mut told = hello();
        told.name = Hello::named(name).expect("a name");
        assert_eq!(told.validate(&Hello::HANDLE_RIGHTS), Ok(()));
        let encoded = Message::Hello(told).encode();
        let Some(Message::Hello(read)) = Message::decode(encoded.as_bytes()) else {
            panic!("a HELLO reads back");
        };
        assert_eq!(read.name(), name);
    }
    // A name longer than the field is not one this protocol can carry, and
    // is refused where it is made rather than cut on the wire.
    assert_eq!(Hello::named("a_very_long_driver_name"), None);
    assert_eq!(Hello::named(""), None);
}

/// The core takes the smaller of what the driver offers and what the
/// protocol allows, so a device with less is believed and one claiming more
/// is not.
#[test]
fn the_object_limit_is_the_smaller_of_the_two() {
    let mut told = hello();
    told.object_max = 1024;
    assert_eq!(told.object_limit(), 1024);
    told.object_max = u64::MAX;
    assert_eq!(told.object_limit(), MAX_OBJECT_BYTES);
}

/// A range of the work VMO has to lie inside it, and not be longer than one
/// message may name.
#[test]
fn a_work_range_lies_inside_the_vmo() {
    assert!(Work { at: 0, len: 0 }.fits(0), "nothing fits anywhere");
    assert!(Work { at: 0, len: 16 }.fits(16));
    assert!(!Work { at: 1, len: 16 }.fits(16));
    assert!(!Work { at: 16, len: 1 }.fits(16));
    assert!(
        !Work {
            at: 0,
            len: MAX_WORK_BYTES + 1,
        }
        .fits(u64::from(MAX_WORK_BYTES) + 1)
    );
    // And a range whose end overflows a u32 is not a range.
    assert!(
        !Work {
            at: u32::MAX,
            len: u32::MAX,
        }
        .fits(u64::MAX)
    );
}

/// Every refusal and status word round-trips, so a reason crossing the wire
/// is the reason that was meant.
#[test]
fn the_enumerations_round_trip() {
    for (raw, refusal) in (1..=8).zip([
        Refusal::Version,
        Refusal::Name,
        Refusal::Features,
        Refusal::ObjectMax,
        Refusal::Rights,
        Refusal::Malformed,
        Refusal::WrongLocation,
        Refusal::Protocol,
    ]) {
        assert_eq!(Refusal::from_raw(raw), Some(refusal));
        assert_eq!(refusal as u32, raw);
    }
    assert_eq!(Refusal::from_raw(0), None);
    assert_eq!(Refusal::from_raw(9), None);

    for (raw, status) in (0..=5).zip([
        Status::Ok,
        Status::DeviceRefused,
        Status::PinFailed,
        Status::OutOfMemory,
        Status::Invalid,
        Status::TimedOut,
    ]) {
        assert_eq!(Status::from_raw(raw), Some(status));
        assert_eq!(status as u32, raw);
    }
    assert_eq!(Status::from_raw(6), None);
}

// -- The session ----------------------------------------------------------

use crate::session::{Event, RequestError, Session};

fn session() -> Session {
    Session::accept(&hello(), &Hello::HANDLE_RIGHTS, 1 << 20).expect("a good HELLO is accepted")
}

/// A context is made before it is used and answered once, and the session
/// hands out exactly the message that says so.
#[test]
fn a_context_is_made_before_anything_uses_it() {
    let mut core = session();
    assert_eq!(core.name(), "virtio_gpu");

    // Nothing may be submitted into a context that is not there.
    assert_eq!(
        core.submit(1, 1, Work { at: 0, len: 16 }),
        Err(RequestError::NoSuchContext)
    );

    let asked = core.make_context(1, 2).expect("a context may be asked for");
    assert_eq!(
        asked,
        Message::MakeContext {
            context: 1,
            capset: 2
        }
    );
    // Still not usable: it has not been made yet.
    assert_eq!(
        core.submit(1, 1, Work { at: 0, len: 16 }),
        Err(RequestError::NoSuchContext)
    );
    // And the id is taken, so it cannot be asked for twice.
    assert_eq!(core.make_context(1, 2), Err(RequestError::InUse));

    assert_eq!(
        core.receive(&Message::ContextMade {
            context: 1,
            status: Status::Ok
        }),
        Ok(Event::ContextMade {
            context: 1,
            status: Status::Ok
        })
    );
    assert!(core.submit(1, 1, Work { at: 0, len: 16 }).is_ok());

    // The same answer twice is a driver answering a question nobody asked.
    assert_eq!(
        core.receive(&Message::ContextMade {
            context: 1,
            status: Status::Ok
        }),
        Err(Refusal::Protocol)
    );
    assert!(core.is_broken());
    assert_eq!(core.make_context(2, 0), Err(RequestError::Closed));
}

/// A context the device refused leaves no context behind, and its id can be
/// asked for again.
#[test]
fn a_refused_context_leaves_nothing() {
    let mut core = session();
    let _asked = core.make_context(1, 0).expect("asked");
    assert_eq!(
        core.receive(&Message::ContextMade {
            context: 1,
            status: Status::OutOfMemory
        }),
        Ok(Event::ContextMade {
            context: 1,
            status: Status::OutOfMemory
        })
    );
    assert!(core.make_context(1, 0).is_ok(), "the id is free again");
}

/// An object belongs to a context, is checked against what the driver said
/// it would make, and its range has to be one the core handed out.
#[test]
fn an_object_is_checked_against_what_the_driver_promised() {
    let mut core = session();
    let _ = core.make_context(1, 0).expect("asked");
    let _ = core
        .receive(&Message::ContextMade {
            context: 1,
            status: Status::Ok,
        })
        .expect("made");

    let fine = Work { at: 0, len: 48 };
    assert_eq!(
        core.make_object(5, 2, 4096, flags::TO_DEVICE, fine),
        Err(RequestError::NoSuchContext),
        "a context that is not there"
    );
    assert_eq!(
        core.make_object(0, 1, 4096, flags::TO_DEVICE, fine),
        Err(RequestError::ZeroId)
    );
    assert_eq!(
        core.make_object(5, 1, 0, flags::TO_DEVICE, fine),
        Err(RequestError::ObjectBytes)
    );
    assert_eq!(
        core.make_object(5, 1, 1 << 30, flags::TO_DEVICE, fine),
        Err(RequestError::ObjectBytes),
        "past what this driver said it would make"
    );
    assert_eq!(
        core.make_object(5, 1, 4096, flags::MAPPABLE, fine),
        Err(RequestError::ObjectFlags),
        "no direction at all"
    );
    assert_eq!(
        core.make_object(5, 1, 4096, 1 << 31, fine),
        Err(RequestError::ObjectFlags)
    );
    assert_eq!(
        core.make_object(
            5,
            1,
            4096,
            flags::TO_DEVICE,
            Work {
                at: 0,
                len: 1 << 30
            }
        ),
        Err(RequestError::Work),
        "a range past the work VMO"
    );

    assert!(core.make_object(5, 1, 4096, flags::TO_DEVICE, fine).is_ok());
    // And a context with an object in it is not one to take away.
    assert_eq!(core.drop_context(1), Err(RequestError::Busy));
}

/// An object the device would not let go of stays live, so the core never
/// hands its memory out again. This is the display's rule, and it belongs
/// to the core rather than to any one device.
#[test]
fn an_object_the_device_kept_is_never_reused() {
    let mut core = session();
    let _ = core.make_context(1, 0).expect("asked");
    let _ = core
        .receive(&Message::ContextMade {
            context: 1,
            status: Status::Ok,
        })
        .expect("made");
    let _ = core
        .make_object(5, 1, 4096, flags::TO_DEVICE, Work { at: 0, len: 48 })
        .expect("asked");
    let _ = core
        .receive(&Message::ObjectMade {
            object: 5,
            status: Status::Ok,
        })
        .expect("made");

    let _ = core.drop_object(5).expect("asked to go");
    let _ = core
        .receive(&Message::ObjectGone {
            object: 5,
            status: Status::DeviceRefused,
        })
        .expect("answered");
    assert_eq!(
        core.make_object(5, 1, 4096, flags::TO_DEVICE, Work { at: 0, len: 48 }),
        Err(RequestError::InUse),
        "the id is still spoken for, because the device may still hold it"
    );

    // Where the device did let go, the id comes back.
    let _ = core
        .make_object(6, 1, 4096, flags::TO_DEVICE, Work { at: 0, len: 48 })
        .expect("asked");
    let _ = core
        .receive(&Message::ObjectMade {
            object: 6,
            status: Status::Ok,
        })
        .expect("made");
    let _ = core.drop_object(6).expect("asked to go");
    let _ = core
        .receive(&Message::ObjectGone {
            object: 6,
            status: Status::Ok,
        })
        .expect("answered");
    assert!(
        core.make_object(6, 1, 4096, flags::TO_DEVICE, Work { at: 0, len: 48 })
            .is_ok()
    );
}

/// A submission and its wait are two separate things in flight, each
/// answered once, and a command buffer of no bytes is nothing to run.
#[test]
fn a_fence_is_answered_once_for_each_thing_it_was_asked_about() {
    let mut core = session();
    let _ = core.make_context(1, 0).expect("asked");
    let _ = core
        .receive(&Message::ContextMade {
            context: 1,
            status: Status::Ok,
        })
        .expect("made");

    assert_eq!(
        core.submit(1, 7, Work { at: 0, len: 0 }),
        Err(RequestError::Work),
        "nothing to run"
    );
    let _ = core
        .submit(1, 7, Work { at: 0, len: 64 })
        .expect("submitted");
    assert_eq!(
        core.submit(1, 7, Work { at: 0, len: 64 }),
        Err(RequestError::InUse),
        "one fence, one submission"
    );
    // The wait is its own thing in flight, under the same fence.
    let _ = core.wait(1, 7).expect("waited");
    assert_eq!(core.wait(1, 7), Err(RequestError::InUse));

    assert_eq!(
        core.receive(&Message::Submitted {
            fence: 7,
            status: Status::Ok
        }),
        Ok(Event::Submitted {
            fence: 7,
            status: Status::Ok
        })
    );
    assert_eq!(
        core.receive(&Message::Waited {
            fence: 7,
            status: Status::TimedOut
        }),
        Ok(Event::Waited {
            fence: 7,
            status: Status::TimedOut
        })
    );
    // And neither is answered twice.
    assert_eq!(
        core.receive(&Message::Waited {
            fence: 7,
            status: Status::Ok
        }),
        Err(Refusal::Protocol)
    );
}

/// STOPPED is only ever an answer to STOP.
#[test]
fn stopped_is_only_an_answer_to_stop() {
    let mut core = session();
    assert_eq!(core.receive(&Message::Stopped), Err(Refusal::Protocol));

    let mut core = session();
    assert_eq!(core.stop(), Ok(Message::Stop));
    assert_eq!(
        core.make_context(1, 0),
        Err(RequestError::Closed),
        "nothing more is asked after STOP"
    );
    assert_eq!(core.receive(&Message::Stopped), Ok(Event::Stopped));
}

/// A HELLO the core refuses never becomes a session at all.
#[test]
fn a_refused_hello_is_no_session() {
    let mut told = hello();
    told.features = features::FENCES;
    assert_eq!(
        Session::accept(&told, &Hello::HANDLE_RIGHTS, 1 << 20).map(|_| ()),
        Err(Refusal::Features)
    );
}
