//! The messages byte for byte, strict decoding, and a session driven through
//! a frame's life and through each way a driver can break it.

extern crate std;

use std::vec::Vec;

use ferrix_native_abi::rights::Rights;

use crate::message::*;
use crate::session::*;

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn hello() -> Hello {
    let mut modes = [ScanoutMode::default(); MAX_SCANOUTS];
    modes[0] = ScanoutMode {
        width: 1280,
        height: 800,
        enabled: true,
    };
    Hello {
        version: VERSION,
        scanouts: 1,
        location: 0x0000_0800,
        modes,
    }
}

fn buffer(id: u32, offset: u64) -> Attach {
    Attach {
        buffer: id,
        format: FORMAT,
        offset,
        // 1280 × 800 × 4 = 4 096 000 bytes, rounded up to 1000 pages.
        length: 1000 * PAGE_SIZE,
        width: 1280,
        height: 800,
        stride: 5120,
    }
}

const CARD: u64 = 256 * 1024 * 1024;

fn every_message() -> Vec<Message> {
    let rect = Rect {
        x: 1,
        y: 2,
        width: 3,
        height: 4,
    };
    std::vec![
        Message::Hello(hello()),
        Message::Ready(Ready {
            card: 0,
            card_bytes: CARD,
        }),
        Message::Refused(Refusal::Protocol),
        Message::Attach(buffer(7, 4096)),
        Message::Attached {
            buffer: 7,
            status: Status::PinFailed,
        },
        Message::Scanout {
            scanout: 0,
            buffer: 7,
            rect,
        },
        Message::Flush {
            buffer: 7,
            sequence: 1 << 40,
            rect,
        },
        Message::Flipped {
            sequence: 1 << 40,
            status: Status::Ok,
        },
        Message::Detach { buffer: 7 },
        Message::Detached {
            buffer: 7,
            status: Status::DeviceRefused,
        },
        Message::Stop,
        Message::Stopped,
    ]
}

// -- Bytes ----------------------------------------------------------------------

#[test]
fn every_message_round_trips_at_its_fixed_length() {
    for message in every_message() {
        let encoded = message.encode();
        let bytes = encoded.as_bytes();
        assert_eq!(u32_at(bytes, 0), message.kind());
        assert_eq!(u32_at(bytes, 4) as usize, bytes.len());
        assert_eq!(Message::length_of(message.kind()), Some(bytes.len()));
        assert_eq!(Message::decode(bytes), Ok(message), "{message:?}");
    }
}

#[test]
fn fields_lie_where_the_specification_puts_them() {
    let hello = Message::Hello(hello()).encode();
    let bytes = hello.as_bytes();
    assert_eq!(bytes.len(), 208);
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 1);
    assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), 1);
    assert_eq!(u32_at(bytes, 12), 0x800);
    assert_eq!(
        [u32_at(bytes, 16), u32_at(bytes, 20), u32_at(bytes, 24)],
        [1280, 800, 1]
    );
    assert!(bytes[28..].iter().all(|&byte| byte == 0));

    let attach = Message::Attach(buffer(7, 8192)).encode();
    let bytes = attach.as_bytes();
    assert_eq!(bytes.len(), 48);
    assert_eq!([u32_at(bytes, 8), u32_at(bytes, 12)], [7, 0x3432_5258]);
    assert_eq!([u64_at(bytes, 16), u64_at(bytes, 24)], [8192, 4_096_000]);
    assert_eq!(
        [u32_at(bytes, 32), u32_at(bytes, 36), u32_at(bytes, 40)],
        [1280, 800, 5120]
    );

    let flush = Message::Flush {
        buffer: 7,
        sequence: 9,
        rect: Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        },
    }
    .encode();
    let bytes = flush.as_bytes();
    assert_eq!(bytes.len(), 40);
    assert_eq!((u32_at(bytes, 8), u64_at(bytes, 16)), (7, 9));
    assert_eq!([24, 28, 32, 36].map(|at| u32_at(bytes, at)), [1, 2, 3, 4]);

    let ready = Message::Ready(Ready {
        card: 3,
        card_bytes: CARD,
    })
    .encode();
    assert_eq!(
        (u32_at(ready.as_bytes(), 8), u64_at(ready.as_bytes(), 16)),
        (3, CARD)
    );
}

#[test]
fn decoding_is_strict() {
    let stop = Message::Stop.encode();
    assert_eq!(
        Message::decode(&stop.as_bytes()[..7]),
        Err(MessageError::Short)
    );

    let mut unknown = stop.as_bytes().to_vec();
    unknown[0] = 99;
    assert_eq!(Message::decode(&unknown), Err(MessageError::Type(99)));

    // The length field disagrees with the type, or the bytes run on.
    let mut lying = stop.as_bytes().to_vec();
    lying[4] = 9;
    assert_eq!(Message::decode(&lying), Err(MessageError::Length));
    let mut longer = stop.as_bytes().to_vec();
    longer.push(0);
    assert_eq!(Message::decode(&longer), Err(MessageError::Length));

    // A reserved byte set, a status nobody defined, an enabled flag of 2.
    let mut reserved = Message::Detach { buffer: 1 }.encode().as_bytes().to_vec();
    reserved[12] = 1;
    assert_eq!(Message::decode(&reserved), Err(MessageError::Field));
    let mut status = Message::Attached {
        buffer: 1,
        status: Status::Ok,
    }
    .encode()
    .as_bytes()
    .to_vec();
    status[12] = 5;
    assert_eq!(Message::decode(&status), Err(MessageError::Field));
    let mut enabled = Message::Hello(hello()).encode().as_bytes().to_vec();
    enabled[24] = 2;
    assert_eq!(Message::decode(&enabled), Err(MessageError::Field));
    let mut refusal = Message::Refused(Refusal::Version)
        .encode()
        .as_bytes()
        .to_vec();
    refusal[8] = 0;
    assert_eq!(Message::decode(&refusal), Err(MessageError::Field));
}

// -- HELLO and ATTACH ------------------------------------------------------------

#[test]
fn hello_is_validated_field_by_field_and_by_its_rights() {
    let rights = Hello::HANDLE_RIGHTS;
    assert_eq!(hello().validate(&rights), Ok(()));

    let mut wrong = hello();
    wrong.version = 2;
    assert_eq!(wrong.validate(&rights), Err(Refusal::Version));

    for scanouts in [0, 17] {
        let mut wrong = hello();
        wrong.scanouts = scanouts;
        assert_eq!(wrong.validate(&rights), Err(Refusal::Scanouts));
    }

    for mode in [
        ScanoutMode {
            width: 0,
            height: 800,
            enabled: true,
        },
        ScanoutMode {
            width: 8193,
            height: 800,
            enabled: true,
        },
        ScanoutMode {
            width: 8193,
            height: 0,
            enabled: false,
        },
    ] {
        let mut wrong = hello();
        wrong.modes[0] = mode;
        assert_eq!(wrong.validate(&rights), Err(Refusal::Mode), "{mode:?}");
    }
    let mut past = hello();
    past.modes[1].width = 1;
    assert_eq!(
        past.validate(&rights),
        Err(Refusal::Mode),
        "a mode past the count"
    );

    let mut disconnected = hello();
    disconnected.modes[0] = ScanoutMode::default();
    assert_eq!(
        disconnected.validate(&rights),
        Ok(()),
        "nothing attached is fine"
    );

    for handles in [
        &[][..],
        &[Rights::WRITE],
        &[PORT_RIGHTS, PORT_RIGHTS],
        &[Rights(PORT_RIGHTS.0 | Rights::DUPLICATE.0)],
    ] {
        assert_eq!(hello().validate(handles), Err(Refusal::Rights));
    }
}

#[test]
fn attach_is_validated_against_itself_and_the_card() {
    assert_eq!(buffer(7, 0).validate(CARD), Ok(()));
    let with = |change: fn(&mut Attach)| {
        let mut attach = buffer(7, 0);
        change(&mut attach);
        attach.validate(CARD)
    };
    assert_eq!(with(|a| a.buffer = 0), Err(AttachError::Id));
    assert_eq!(with(|a| a.format = 0x3432_4241), Err(AttachError::Format));
    assert_eq!(with(|a| a.width = 0), Err(AttachError::Size));
    assert_eq!(with(|a| a.height = 8193), Err(AttachError::Size));
    assert_eq!(with(|a| a.stride = 5119), Err(AttachError::Stride));
    assert_eq!(
        with(|a| a.length = 999 * PAGE_SIZE),
        Err(AttachError::Stride)
    );
    assert_eq!(with(|a| a.offset = 1), Err(AttachError::Range));
    assert_eq!(
        with(|a| a.length = 1000 * PAGE_SIZE + 1),
        Err(AttachError::Range)
    );
    assert_eq!(
        with(|a| a.offset = CARD - 999 * PAGE_SIZE),
        Err(AttachError::Range)
    );
    assert_eq!(
        with(|a| a.offset = u64::MAX - 4095),
        Err(AttachError::Range)
    );
    assert_eq!(with(|a| a.offset = CARD - 1000 * PAGE_SIZE), Ok(()));
}

// -- The session -----------------------------------------------------------------

fn session() -> Session {
    Session::accept(&hello(), &Hello::HANDLE_RIGHTS, CARD).expect("a good HELLO")
}

fn full() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 1280,
        height: 800,
    }
}

#[test]
fn a_frame_from_attach_to_detach() {
    let mut session = session();
    assert_eq!(session.modes().len(), 1);

    assert_eq!(
        session.attach(buffer(7, 0)),
        Ok(Message::Attach(buffer(7, 0)))
    );
    assert_eq!(
        session.scanout(0, 7, full()),
        Err(RequestError::NotAttached),
        "not before the driver says it attached"
    );
    assert_eq!(
        session.receive(&Message::Attached {
            buffer: 7,
            status: Status::Ok
        }),
        Ok(Event::Attached {
            buffer: 7,
            status: Status::Ok
        })
    );

    assert!(session.scanout(0, 7, full()).is_ok());
    let Ok(Message::Flush { sequence, .. }) = session.flush(7, full()) else {
        panic!("a flush");
    };
    assert_eq!(sequence, 1);
    assert_eq!(
        session.detach(7),
        Err(RequestError::Busy),
        "shown and in flight"
    );
    assert_eq!(
        session.receive(&Message::Flipped {
            sequence: 1,
            status: Status::Ok
        }),
        Ok(Event::Flipped {
            sequence: 1,
            buffer: 7,
            status: Status::Ok
        })
    );
    assert_eq!(session.detach(7), Err(RequestError::Busy), "still shown");
    assert!(session.scanout(0, 0, Rect::default()).is_ok());
    assert_eq!(session.detach(7), Ok(Message::Detach { buffer: 7 }));
    assert_eq!(
        session.receive(&Message::Detached {
            buffer: 7,
            status: Status::Ok
        }),
        Ok(Event::Detached {
            buffer: 7,
            status: Status::Ok
        })
    );
    assert_eq!(
        session.attach(buffer(7, 0)),
        Ok(Message::Attach(buffer(7, 0))),
        "id free again"
    );

    assert_eq!(session.stop(), Ok(Message::Stop));
    assert_eq!(session.attach(buffer(8, 0)), Err(RequestError::Closed));
    assert_eq!(session.receive(&Message::Stopped), Ok(Event::Stopped));
}

#[test]
fn a_failed_attach_frees_the_id() {
    let mut session = session();
    assert!(session.attach(buffer(7, 0)).is_ok());
    assert_eq!(
        session.receive(&Message::Attached {
            buffer: 7,
            status: Status::PinFailed
        }),
        Ok(Event::Attached {
            buffer: 7,
            status: Status::PinFailed
        })
    );
    assert_eq!(session.flush(7, full()), Err(RequestError::NotAttached));
    assert!(session.attach(buffer(7, 0)).is_ok());
}

#[test]
fn requests_the_protocol_has_no_state_for_are_refused() {
    let mut session = session();
    assert_eq!(
        session.attach(buffer(0, 0)),
        Err(RequestError::Attach(AttachError::Id))
    );
    assert!(session.attach(buffer(7, 0)).is_ok());
    assert_eq!(session.attach(buffer(7, 0)), Err(RequestError::InUse));
    assert!(
        session
            .receive(&Message::Attached {
                buffer: 7,
                status: Status::Ok
            })
            .is_ok()
    );

    assert_eq!(
        session.scanout(1, 7, full()),
        Err(RequestError::NoSuchScanout)
    );
    let outside = Rect { x: 1, ..full() };
    assert_eq!(session.scanout(0, 7, outside), Err(RequestError::Rect));
    assert_eq!(session.flush(7, Rect::default()), Err(RequestError::Rect));
    assert_eq!(session.flush(8, full()), Err(RequestError::NotAttached));

    for _ in 0..MAX_IN_FLIGHT {
        assert!(session.flush(7, full()).is_ok());
    }
    assert_eq!(session.flush(7, full()), Err(RequestError::TooManyFlushes));

    for id in 8..=u32::try_from(MAX_BUFFERS + 6).expect("small") {
        assert!(session.attach(buffer(id, 0)).is_ok(), "buffer {id}");
    }
    assert_eq!(session.attach(buffer(99, 0)), Err(RequestError::Full));
}

#[test]
fn a_driver_that_answers_what_nobody_asked_breaks_the_session() {
    type Setup = fn(&mut Session);
    let cases: [(Setup, Message); 7] = [
        (
            |_| {},
            Message::Attached {
                buffer: 7,
                status: Status::Ok,
            },
        ),
        (
            |_| {},
            Message::Flipped {
                sequence: 1,
                status: Status::Ok,
            },
        ),
        (
            |_| {},
            Message::Detached {
                buffer: 7,
                status: Status::Ok,
            },
        ),
        (|_| {}, Message::Stopped),
        (|_| {}, Message::Hello(hello())),
        (|_| {}, Message::Attach(buffer(7, 0))),
        // Flushes 1 and 2 in flight; the driver finishes 2 first.
        (
            |session| {
                assert!(session.attach(buffer(7, 0)).is_ok());
                assert!(
                    session
                        .receive(&Message::Attached {
                            buffer: 7,
                            status: Status::Ok
                        })
                        .is_ok()
                );
                assert!(session.flush(7, full()).is_ok());
                assert!(session.flush(7, full()).is_ok());
            },
            Message::Flipped {
                sequence: 2,
                status: Status::Ok,
            },
        ),
    ];
    for (setup, message) in cases {
        let mut session = session();
        setup(&mut session);
        assert_eq!(
            session.receive(&message),
            Err(Refusal::Protocol),
            "{message:?}"
        );
        assert!(session.is_broken());
        assert_eq!(session.attach(buffer(50, 0)), Err(RequestError::Closed));
        assert_eq!(
            session.receive(&Message::Stopped),
            Err(Refusal::Protocol),
            "a broken session stays broken"
        );
    }

    // Detached for a buffer that is attached but was never asked to detach.
    let mut session = session();
    assert!(session.attach(buffer(7, 0)).is_ok());
    assert_eq!(
        session.receive(&Message::Detached {
            buffer: 7,
            status: Status::Ok
        }),
        Err(Refusal::Protocol)
    );
}

#[test]
fn accept_refuses_what_validate_refuses() {
    let mut wrong = hello();
    wrong.scanouts = 0;
    assert_eq!(
        Session::accept(&wrong, &Hello::HANDLE_RIGHTS, CARD).map(|_| ()),
        Err(Refusal::Scanouts)
    );
}
