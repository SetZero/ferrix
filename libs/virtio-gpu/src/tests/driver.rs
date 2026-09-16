//! The driver against the fake device: bring-up, commands, a whole frame from
//! ATTACH to the device's screen, and a device that misbehaves.

use core::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_displayctl::message::{Attach, FORMAT, Message, Rect as CtlRect, Status};
use ferrix_virtio::gpu::{
    CMD_GET_DISPLAY_INFO, CMD_RESOURCE_UNREF, Command, DeviceError as Refusal, MemEntry, PAGE_SIZE,
    Response, backing_entries,
};
use ferrix_virtio::pci::FEATURE_VERSION_1;

use super::fake::{Bus, Device, Handle, PAGE, Region};
use crate::pipeline::{Pipeline, Request, Step};
use crate::{DeviceError, Done, Driver, Options, Parts, SubmitError, Teardown};

type TestDriver = Driver<Handle, Region, Region>;

fn build(mode: (u32, u32)) -> (Rc<Bus>, Rc<RefCell<Device>>, TestDriver) {
    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus), mode)));
    let handle = Handle {
        device: Rc::clone(&device),
        doorbells: Rc::new(RefCell::new(0)),
    };
    let parts = Parts {
        transport: handle,
        rings: bus.pin(2, false),
        area: bus.pin(2, true),
    };
    let driver = Driver::init(parts, Options { reset_polls: 4 }).expect("the device comes up");
    (bus, device, driver)
}

/// Submit, let the device serve, take the outcome.
fn run(driver: &mut TestDriver, device: &Rc<RefCell<Device>>, command: &Command<'_>) -> Done {
    driver.submit(command).expect("submitted");
    device.borrow_mut().serve();
    let (done, _) = driver.on_interrupt().expect("a sound response");
    let done = done.expect("a response");
    assert_eq!(done.command, command.code());
    done
}

#[test]
fn bring_up_negotiates_and_reads_the_displays() {
    let (_bus, device, mut driver) = build((1280, 800));
    assert!(driver.info().features & FEATURE_VERSION_1 != 0);
    assert_eq!(driver.info().features & (1 << 1), 0, "EDID is declined");
    assert_eq!(driver.info().config.num_scanouts, 1);

    let Ok(Response::DisplayInfo(scanouts)) =
        run(&mut driver, &device, &Command::GetDisplayInfo).result
    else {
        panic!("display info");
    };
    assert!(scanouts[0].enabled);
    assert_eq!(
        (scanouts[0].rect.width, scanouts[0].rect.height),
        (1280, 800)
    );
    assert_eq!(device.borrow().commands, [CMD_GET_DISPLAY_INFO]);
    assert!(
        device.borrow().protocol_errors.is_empty(),
        "{:?}",
        device.borrow().protocol_errors
    );
}

#[test]
fn one_command_at_a_time_and_refusals_are_not_faults() {
    let (_bus, device, mut driver) = build((640, 480));
    driver.submit(&Command::GetDisplayInfo).expect("submitted");
    assert_eq!(
        driver.submit(&Command::GetDisplayInfo),
        Err(SubmitError::Busy)
    );
    assert_eq!(driver.on_interrupt().expect("nothing yet").0, None);
    device.borrow_mut().serve();
    assert!(driver.on_interrupt().expect("served").0.is_some());

    assert_eq!(
        run(
            &mut driver,
            &device,
            &Command::ResourceUnref { resource_id: 42 }
        )
        .result,
        Err(Refusal::InvalidResourceId)
    );
    assert_eq!(driver.fault(), None);
    assert_eq!(device.borrow().commands.last(), Some(&CMD_RESOURCE_UNREF));
}

#[test]
fn a_request_larger_than_the_area_is_refused() {
    let (_bus, _device, mut driver) = build((640, 480));
    // Two pages less the response is room for (8192 - 512 - 32) / 16 entries.
    let entries = vec![
        MemEntry {
            addr: 0x1000,
            length: 4096
        };
        480
    ];
    assert_eq!(
        driver.submit(&Command::ResourceAttachBacking {
            resource_id: 1,
            entries: &entries
        }),
        Err(SubmitError::TooLarge)
    );
    assert!(!driver.is_busy());
}

/// Drive the pipeline and the driver together until the pipeline is idle,
/// playing the glue: pins come from the card region, replies are collected.
fn pump(
    pipeline: &mut Pipeline,
    driver: &mut TestDriver,
    device: &Rc<RefCell<Device>>,
    card: &Region,
    replies: &mut Vec<Message>,
) {
    use crate::DevicePages;
    let mut entries = [MemEntry::default(); 64];
    let mut pins = 0;
    for _ in 0..64 {
        let snapshot = entries;
        match pipeline.next(&snapshot) {
            Step::Pin { offset, length, .. } => {
                let first = (offset / PAGE_SIZE) as usize;
                let pages = (length / PAGE_SIZE) as usize;
                let count =
                    backing_entries(&card.device_pages()[first..first + pages], &mut entries)
                        .expect("pinned pages make entries");
                pins += 1;
                pipeline.pinned(Ok(count)).expect("waiting");
            }
            Step::Submit(command) => {
                let done = run(driver, device, &command);
                pipeline.done(done.result).expect("waiting");
            }
            Step::Unpin { .. } => pins -= 1,
            Step::Reply(message) => replies.push(message),
            Step::Wait => panic!("nothing is outstanding here"),
            Step::Idle => break,
        }
    }
    let _ = pins;
}

/// What [`first_frame`] leaves: the device, the driver, the card, the
/// pipeline, the pixels and the replies so far.
type Frame = (
    Rc<RefCell<Device>>,
    TestDriver,
    Region,
    Pipeline,
    Vec<u8>,
    Vec<Message>,
);

/// A 64 × 16 buffer on scattered card pages, attached, shown and flushed
/// once: the driver, the device, the card, the pipeline, the pixels and the
/// replies so far.
fn first_frame() -> Frame {
    let (bus, device, mut driver) = build((64, 16));
    // The card VMO: eight pages, scattered, as the core's allocator and the
    // IOMMU might leave them. The buffer is its first two.
    let card = bus.pin(8, true);
    let mut pixels = Vec::new();
    for index in 0..64u32 * 16 {
        pixels.extend_from_slice(&(0xFF00_0000 | (index * 7919)).to_le_bytes());
    }
    card.write_bytes(0, &pixels);

    let mut pipeline = Pipeline::new();
    let mut replies = Vec::new();
    let attach = Attach {
        buffer: 1,
        format: FORMAT,
        offset: 0,
        length: 2 * PAGE_SIZE,
        width: 64,
        height: 16,
        stride: 256,
    };
    let whole = CtlRect {
        x: 0,
        y: 0,
        width: 64,
        height: 16,
    };
    for request in [
        Request::Attach(attach),
        Request::Scanout {
            scanout: 0,
            buffer: 1,
            rect: whole,
        },
        Request::Flush {
            buffer: 1,
            sequence: 1,
            rect: whole,
        },
    ] {
        pipeline.push(request).expect("room");
    }
    pump(&mut pipeline, &mut driver, &device, &card, &mut replies);

    assert_eq!(
        replies,
        [
            Message::Attached {
                buffer: 1,
                status: Status::Ok
            },
            Message::Flipped {
                sequence: 1,
                status: Status::Ok
            },
        ]
    );
    assert_eq!(
        device.borrow().screen,
        pixels,
        "the screen shows the buffer"
    );
    (device, driver, card, pipeline, pixels, replies)
}

#[test]
fn a_frame_reaches_the_screen() {
    let (device, _driver, _card, _pipeline, pixels, replies) = first_frame();
    assert_eq!(
        replies,
        [
            Message::Attached {
                buffer: 1,
                status: Status::Ok
            },
            Message::Flipped {
                sequence: 1,
                status: Status::Ok
            },
        ]
    );
    assert_eq!(
        device.borrow().screen,
        pixels,
        "the screen shows the buffer"
    );
    assert_eq!(
        device.borrow().resources[&1].backing.len(),
        2,
        "two scattered pages, two entries"
    );
}

#[test]
fn a_partial_flush_then_off_and_detach() {
    let (device, mut driver, card, mut pipeline, pixels, mut replies) = first_frame();
    // A partial flush after a change moves only that rectangle.
    let mut changed = pixels;
    for pixel in changed[256 * 5..256 * 6].chunks_mut(4).skip(10).take(4) {
        pixel.copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    }
    card.write_bytes(0, &changed);
    pipeline
        .push(Request::Flush {
            buffer: 1,
            sequence: 2,
            rect: CtlRect {
                x: 10,
                y: 5,
                width: 4,
                height: 1,
            },
        })
        .expect("room");
    pump(&mut pipeline, &mut driver, &device, &card, &mut replies);
    assert_eq!(device.borrow().screen, changed);

    // Off, then detach.
    for request in [
        Request::Scanout {
            scanout: 0,
            buffer: 0,
            rect: CtlRect::default(),
        },
        Request::Detach { buffer: 1 },
    ] {
        pipeline.push(request).expect("room");
    }
    pump(&mut pipeline, &mut driver, &device, &card, &mut replies);
    assert_eq!(
        replies.last(),
        Some(&Message::Detached {
            buffer: 1,
            status: Status::Ok
        })
    );
    assert!(device.borrow().resources.is_empty());
    assert!(
        device.borrow().protocol_errors.is_empty(),
        "{:?}",
        device.borrow().protocol_errors
    );
}

#[test]
fn a_device_that_breaks_the_protocol_is_failed() {
    type Setup = fn(&mut Device);
    let cases: [(Setup, DeviceError); 3] = [
        (
            |device| device.misbehave.written = Some(8),
            DeviceError::Protocol(ferrix_virtio::gpu::GpuError::ResponseTooShort(8)),
        ),
        (
            |device| device.misbehave.response = Some(0x1107),
            DeviceError::Protocol(ferrix_virtio::gpu::GpuError::UnknownResponse(0x1107)),
        ),
        (
            |device| device.misbehave.needs_reset = true,
            DeviceError::NeedsReset,
        ),
    ];
    for (misbehave, expected) in cases {
        let (_bus, device, mut driver) = build((64, 16));
        misbehave(&mut device.borrow_mut());
        driver
            .submit(&Command::ResourceUnref { resource_id: 1 })
            .expect("submitted");
        device.borrow_mut().serve();
        assert_eq!(driver.on_interrupt().map(|(done, _)| done), Err(expected));
        assert_eq!(driver.fault(), Some(expected));
        assert_eq!(
            driver.submit(&Command::GetDisplayInfo),
            Err(SubmitError::Broken)
        );
        assert!(matches!(driver.shutdown(), Teardown::Released(_)));
    }
}

#[test]
fn display_events_are_read_and_cleared() {
    let (_bus, device, mut driver) = build((64, 16));
    device.borrow_mut().config_mut()[0..4].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(driver.take_events(), 1);
    assert_eq!(driver.take_events(), 0);
    let _ = PAGE;
}
