//! The pipeline, step by step: each request's commands in order, each failure
//! undone as far as is safe, and reports it was not waiting for refused.

use ferrix_displayctl::message::{Attach, FORMAT, Message, PAGE_SIZE, Rect as CtlRect, Status};
use ferrix_virtio::gpu::{Command, DeviceError as Refusal, Format, MemEntry, Rect, Response};

use crate::pipeline::{Pipeline, PipelineError, QUEUE_DEPTH, Request, Step};

const ENTRIES: [MemEntry; 2] = [
    MemEntry {
        addr: 0x1000,
        length: 4096,
    },
    MemEntry {
        addr: 0x9000,
        length: 4096,
    },
];

fn attach(buffer: u32) -> Attach {
    Attach {
        buffer,
        format: FORMAT,
        offset: 8 * PAGE_SIZE,
        length: 2 * PAGE_SIZE,
        width: 64,
        height: 16,
        stride: 256,
    }
}

const REFUSED: Result<Response, Refusal> = Err(Refusal::InvalidParameter);
const OK: Result<Response, Refusal> = Ok(Response::NoData);

/// Run an ATTACH through to its reply, the device accepting everything.
fn attached(pipeline: &mut Pipeline, buffer: u32) {
    pipeline
        .push(Request::Attach(attach(buffer)))
        .expect("room");
    assert!(matches!(pipeline.next(&ENTRIES), Step::Pin { .. }));
    pipeline.pinned(Ok(2)).expect("waiting for the pin");
    assert!(matches!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceCreate2d { .. })
    ));
    pipeline.done(OK).expect("waiting");
    assert!(matches!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceAttachBacking { .. })
    ));
    pipeline.done(OK).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Attached {
            buffer,
            status: Status::Ok
        })
    );
}

#[test]
fn attach_pins_creates_and_backs() {
    let mut pipeline = Pipeline::new();
    assert_eq!(pipeline.next(&ENTRIES), Step::Idle);
    pipeline.push(Request::Attach(attach(7))).expect("room");

    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Pin {
            buffer: 7,
            offset: 8 * PAGE_SIZE,
            length: 2 * PAGE_SIZE
        }
    );
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Wait,
        "until the pin is reported"
    );
    assert_eq!(
        pipeline.done(OK),
        Err(PipelineError::NotWaiting),
        "a pin, not a command"
    );
    pipeline.pinned(Ok(2)).expect("waiting");

    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceCreate2d {
            resource_id: 7,
            format: Format::B8G8R8X8,
            width: 64,
            height: 16
        })
    );
    assert_eq!(pipeline.pinned(Ok(2)), Err(PipelineError::NotWaiting));
    pipeline.done(OK).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceAttachBacking {
            resource_id: 7,
            entries: &ENTRIES
        })
    );
    pipeline.done(OK).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Attached {
            buffer: 7,
            status: Status::Ok
        })
    );
    assert_eq!(pipeline.next(&ENTRIES), Step::Idle);
    assert!(pipeline.is_idle());
}

#[test]
fn a_failed_attach_is_undone_and_reported() {
    // The pin fails: nothing to undo.
    let mut pipeline = Pipeline::new();
    pipeline.push(Request::Attach(attach(7))).expect("room");
    let _ = pipeline.next(&ENTRIES);
    pipeline.pinned(Err(())).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Attached {
            buffer: 7,
            status: Status::PinFailed
        })
    );

    // The create is refused: unpin.
    let mut pipeline = Pipeline::new();
    pipeline.push(Request::Attach(attach(7))).expect("room");
    let _ = pipeline.next(&ENTRIES);
    pipeline.pinned(Ok(2)).expect("waiting");
    let _ = pipeline.next(&ENTRIES);
    pipeline.done(Err(Refusal::OutOfMemory)).expect("waiting");
    assert_eq!(pipeline.next(&ENTRIES), Step::Unpin { buffer: 7 });
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Attached {
            buffer: 7,
            status: Status::OutOfMemory
        })
    );

    // The backing is refused: unreference, then unpin.
    let mut pipeline = Pipeline::new();
    pipeline.push(Request::Attach(attach(7))).expect("room");
    let _ = pipeline.next(&ENTRIES);
    pipeline.pinned(Ok(2)).expect("waiting");
    let _ = pipeline.next(&ENTRIES);
    pipeline.done(OK).expect("waiting");
    let _ = pipeline.next(&ENTRIES);
    pipeline.done(REFUSED).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceUnref { resource_id: 7 })
    );
    pipeline.done(OK).expect("waiting");
    assert_eq!(pipeline.next(&ENTRIES), Step::Unpin { buffer: 7 });
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Attached {
            buffer: 7,
            status: Status::DeviceRefused
        })
    );
}

#[test]
fn flush_transfers_from_the_rectangles_offset_then_flushes() {
    let mut pipeline = Pipeline::new();
    attached(&mut pipeline, 7);
    let area = CtlRect {
        x: 4,
        y: 2,
        width: 8,
        height: 3,
    };
    pipeline
        .push(Request::Flush {
            buffer: 7,
            sequence: 9,
            rect: area,
        })
        .expect("room");
    let rect = Rect {
        x: 4,
        y: 2,
        width: 8,
        height: 3,
    };
    // Row 2 at a 256-byte stride, then 4 pixels of 4 bytes.
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::TransferToHost2d {
            rect,
            offset: 2 * 256 + 16,
            resource_id: 7
        })
    );
    pipeline.done(OK).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceFlush {
            rect,
            resource_id: 7
        })
    );
    pipeline.done(OK).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Flipped {
            sequence: 9,
            status: Status::Ok
        })
    );

    // A refused transfer skips the flush and says so.
    pipeline
        .push(Request::Flush {
            buffer: 7,
            sequence: 10,
            rect: area,
        })
        .expect("room");
    let _ = pipeline.next(&ENTRIES);
    pipeline.done(REFUSED).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Flipped {
            sequence: 10,
            status: Status::DeviceRefused
        })
    );
}

#[test]
fn scanout_has_no_reply_and_off_is_resource_zero() {
    let mut pipeline = Pipeline::new();
    pipeline
        .push(Request::Scanout {
            scanout: 0,
            buffer: 0,
            rect: CtlRect {
                x: 1,
                y: 1,
                width: 1,
                height: 1,
            },
        })
        .expect("room");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::SetScanout {
            rect: Rect::default(),
            scanout_id: 0,
            resource_id: 0
        })
    );
    pipeline.done(REFUSED).expect("waiting");
    assert_eq!(pipeline.refused_scanouts(), 1);
    assert_eq!(pipeline.next(&ENTRIES), Step::Idle);
}

#[test]
fn detach_unpins_only_what_the_device_let_go_of() {
    let mut pipeline = Pipeline::new();
    attached(&mut pipeline, 7);
    pipeline.push(Request::Detach { buffer: 7 }).expect("room");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceDetachBacking { resource_id: 7 })
    );
    pipeline.done(OK).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Submit(Command::ResourceUnref { resource_id: 7 })
    );
    pipeline.done(OK).expect("waiting");
    assert_eq!(pipeline.next(&ENTRIES), Step::Unpin { buffer: 7 });
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Detached {
            buffer: 7,
            status: Status::Ok
        })
    );

    // Refused: no unreference, no unpin, and the reply says the device kept it.
    attached(&mut pipeline, 8);
    pipeline.push(Request::Detach { buffer: 8 }).expect("room");
    let _ = pipeline.next(&ENTRIES);
    pipeline.done(REFUSED).expect("waiting");
    assert_eq!(
        pipeline.next(&ENTRIES),
        Step::Reply(Message::Detached {
            buffer: 8,
            status: Status::DeviceRefused
        })
    );
    assert_eq!(pipeline.next(&ENTRIES), Step::Idle);
}

#[test]
fn requests_run_in_order_and_the_queue_is_bounded() {
    let mut pipeline = Pipeline::new();
    for buffer in 1..=u32::try_from(QUEUE_DEPTH).expect("small") {
        pipeline.push(Request::Detach { buffer }).expect("room");
    }
    assert_eq!(
        pipeline.push(Request::Detach { buffer: 99 }),
        Err(PipelineError::Full)
    );
    for buffer in 1..=3 {
        assert_eq!(
            pipeline.next(&ENTRIES),
            Step::Submit(Command::ResourceDetachBacking {
                resource_id: buffer
            })
        );
        pipeline.done(REFUSED).expect("waiting");
        assert!(matches!(pipeline.next(&ENTRIES), Step::Reply(_)));
    }
    assert_eq!(
        Pipeline::new().pinned(Ok(1)),
        Err(PipelineError::NotWaiting)
    );
    assert_eq!(Pipeline::new().done(OK), Err(PipelineError::NotWaiting));
}

#[test]
fn requests_come_from_core_messages_only() {
    assert_eq!(
        Request::from_message(&Message::Detach { buffer: 3 }),
        Some(Request::Detach { buffer: 3 })
    );
    assert_eq!(Request::from_message(&Message::Stop), None);
    assert_eq!(
        Request::from_message(&Message::Attached {
            buffer: 3,
            status: Status::Ok
        }),
        None
    );
}
