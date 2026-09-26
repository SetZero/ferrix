//! Fuzz the virtio-gpu driver's pipeline: the display core's requests against
//! a device that accepts or refuses each command as the input says. The
//! requests are ones the core's session would send: an ATTACH for a buffer
//! not attached, anything else only for an attached one.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **Every request that has a reply gets exactly one**: ATTACH an
//!    ATTACHED, FLUSH a FLIPPED, DETACH a DETACHED, in the order they were
//!    queued; SCANOUT none.
//! 2. **Nothing is unpinned that was not pinned**: a buffer's pins are closed
//!    no more often than they were taken.
//! 3. **Pages the device may still hold stay pinned**: a refused
//!    `RESOURCE_DETACH_BACKING` is followed by its DETACHED, not by an unpin.
//! 4. **Only what the pipeline waits for is accepted**: a report it was not
//!    waiting for is refused and changes nothing.

#![no_main]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ferrix_displayctl::message::{Attach, FORMAT, Message, PAGE_SIZE, Rect};
use ferrix_virtio::gpu::{Command, DeviceError, MemEntry, Response};
use ferrix_virtio_gpu::pipeline::{Pipeline, Request, Step};
use libfuzzer_sys::fuzz_target;

const ENTRIES: [MemEntry; 1] = [MemEntry {
    addr: 0x1000,
    length: 4096,
}];

fuzz_target!(|bytes: &[u8]| {
    let mut pipeline = Pipeline::new();
    let mut expected: VecDeque<(u32, u32)> = VecDeque::new();
    let mut pinned: BTreeMap<u32, u32> = BTreeMap::new();
    let mut refused_detach: Option<u32> = None;
    // The core's view: attached buffers, and ids with an ATTACH or DETACH
    // not yet answered.
    let mut attached = BTreeSet::new();
    let mut pending = BTreeSet::new();
    let mut input = bytes.iter().copied();
    let rect = Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };

    for _ in 0..512 {
        let Some(byte) = input.next() else {
            break;
        };
        let buffer = u32::from(byte >> 4) + 1;
        // Queue a request now and then.
        if byte & 0x7 == 0 {
            let (request, reply) = match byte >> 3 & 0x7 {
                0 => (
                    Request::Attach(Attach {
                        buffer,
                        format: FORMAT,
                        offset: 0,
                        length: PAGE_SIZE,
                        width: 1,
                        height: 1,
                        stride: 4,
                    }),
                    Some(5),
                ),
                1 => (
                    Request::Scanout {
                        scanout: 0,
                        buffer,
                        rect,
                    },
                    None,
                ),
                2 => (
                    Request::Flush {
                        buffer,
                        sequence: u64::from(byte),
                        rect,
                    },
                    Some(8),
                ),
                // A cursor of a buffer, or none at all: answered by FLIPPED.
                3 | 4 => (
                    Request::Cursor {
                        scanout: 0,
                        buffer: if byte >> 3 & 0x7 == 3 { buffer } else { 0 },
                        sequence: u64::from(byte),
                        hot_x: 0,
                        hot_y: 0,
                    },
                    Some(8),
                ),
                _ => (Request::Detach { buffer }, Some(10)),
            };
            let allowed = match request {
                Request::Attach(_) => !attached.contains(&buffer) && !pending.contains(&buffer),
                Request::Detach { .. } => attached.contains(&buffer) && !pending.contains(&buffer),
                Request::Cursor { buffer: 0, .. } => true,
                _ => attached.contains(&buffer) && !pending.contains(&buffer),
            };
            if allowed && pipeline.push(request).is_ok() {
                if let Some(kind) = reply {
                    // A cursor of none is answered for buffer 0.
                    let named = if let Request::Cursor { buffer: shown, .. } = request {
                        shown
                    } else {
                        buffer
                    };
                    expected.push_back((kind, named));
                }
                if matches!(request, Request::Attach(_) | Request::Detach { .. }) {
                    let _ = pending.insert(buffer);
                }
            }
            continue;
        }
        // 4. A report nobody waits for changes nothing.
        let busy = !pipeline.is_idle();
        match pipeline.next(&ENTRIES) {
            Step::Pin { buffer, .. } => {
                assert!(pipeline.done(Ok(Response::NoData)).is_err());
                if byte & 1 == 0 {
                    *pinned.entry(buffer).or_default() += 1;
                    pipeline.pinned(Ok(1)).expect("waiting for a pin");
                } else {
                    pipeline.pinned(Err(())).expect("waiting for a pin");
                }
            }
            Step::Submit(command) => {
                assert!(pipeline.pinned(Ok(1)).is_err());
                let refused = byte & 1 == 1;
                assert!(!matches!(command, Command::GetDisplayInfo));
                if let Command::ResourceDetachBacking { resource_id } = command
                    && refused
                {
                    refused_detach = Some(resource_id);
                }
                let result = if refused {
                    Err(DeviceError::InvalidParameter)
                } else {
                    Ok(Response::NoData)
                };
                pipeline.done(result).expect("waiting for a command");
                continue;
            }
            // A cursor is shown only once its image is on the device, which
            // is its own buffer's resource or none.
            Step::Cursor { resource, .. } => {
                assert!(resource == 0 || attached.contains(&resource));
            }
            // 2 and 3.
            Step::Unpin { buffer } => {
                assert_ne!(refused_detach, Some(buffer), "unpinned pages the device kept");
                let count = pinned.get_mut(&buffer).expect("unpinned what was not pinned");
                *count -= 1;
                if *count == 0 {
                    let _ = pinned.remove(&buffer);
                }
            }
            // 1.
            Step::Reply(message) => {
                if let Some(buffer) = refused_detach.take() {
                    assert!(matches!(
                        message,
                        Message::Detached { buffer: got, status }
                            if got == buffer && status != ferrix_displayctl::message::Status::Ok
                    ));
                }
                let (kind, buffer) = expected.pop_front().expect("a reply nobody asked for");
                assert_eq!(message.kind(), kind, "{message:?}");
                match message {
                    Message::Attached { buffer: got, status } => {
                        assert_eq!(got, buffer);
                        let _ = pending.remove(&got);
                        if status == ferrix_displayctl::message::Status::Ok {
                            let _ = attached.insert(got);
                        }
                    }
                    Message::Detached { buffer: got, .. } => {
                        assert_eq!(got, buffer);
                        let _ = pending.remove(&got);
                        let _ = attached.remove(&got);
                    }
                    _ => {}
                }
            }
            Step::Wait => panic!("the fuzzer reports everything at once"),
            Step::Idle => assert!(!busy || expected.is_empty()),
        }
    }
});
