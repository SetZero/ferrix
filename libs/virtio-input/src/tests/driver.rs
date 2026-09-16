//! The driver against the fake device: bring-up to HELLO, READY, events
//! through to the core's session, the queue kept full, what is dropped, and a
//! device that misbehaves.

use core::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_inputctl::message::{Hello, MAX_EVENTS, Message, RawEvent, Ready, Refusal};
use ferrix_inputctl::session::{MAX_REPORT, Received, Session};
use ferrix_linux_abi::input::{
    ABS_X, ABS_Y, BTN_LEFT, EV_ABS, EV_KEY, EV_REL, EV_SND, EV_SYN, KEY_A, REL_X, SYN_MT_REPORT,
    SYN_REPORT,
};
use ferrix_virtio::QueueError;
use ferrix_virtio::input::{Event, InputError};
use ferrix_virtio::pci::{
    FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1, STATUS_DRIVER_OK, STATUS_FAILED,
    STATUS_FEATURES_OK, TransportError,
};

use super::fake::{Answer, Bus, Device, Handle, keyboard, qemu_abs, tablet};
use crate::{
    Control, ControlError, DeviceError, Driver, InitError, Options, Parts, Phase, Rings, Teardown,
};

type TestDriver = Driver<Handle, Region, Region>;
use super::fake::Region;

struct Rig {
    device: Rc<RefCell<Device>>,
    doorbells: Rc<RefCell<usize>>,
    driver: TestDriver,
}

fn parts(bus: &Rc<Bus>, device: &Rc<RefCell<Device>>) -> (Parts<Handle, Region, Region>, Rc<RefCell<usize>>) {
    let doorbells = Rc::new(RefCell::new(0));
    let handle = Handle {
        device: Rc::clone(device),
        doorbells: Rc::clone(&doorbells),
    };
    (
        Parts {
            transport: handle,
            rings: bus.pin(2, false),
            area: bus.pin(1, true),
        },
        doorbells,
    )
}

fn build_with(answers: Vec<Answer>, setup: impl FnOnce(&mut Device)) -> Rig {
    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus), answers)));
    setup(&mut device.borrow_mut());
    let (parts, doorbells) = parts(&bus, &device);
    let driver = Driver::init(parts, Options { reset_polls: 4 }).expect("the device comes up");
    Rig {
        device,
        doorbells,
        driver,
    }
}

/// Brought up, READY answered.
fn running(answers: Vec<Answer>, setup: impl FnOnce(&mut Device)) -> Rig {
    let mut rig = build_with(answers, setup);
    assert_eq!(
        rig.driver.on_control(&Message::Ready(Ready { node: 0 })),
        Ok(Control::Started { node: 0 })
    );
    rig
}

fn event(kind: u16, code: u16, value: i32) -> Event {
    Event { kind, code, value }
}

fn syn() -> Event {
    event(EV_SYN, SYN_REPORT, 0)
}

/// Every EVENTS the driver has ready, after an interrupt.
fn messages(driver: &mut TestDriver) -> Vec<Vec<RawEvent>> {
    let _ = driver.on_interrupt().expect("a sound device");
    let mut out = Vec::new();
    while let Some(events) = driver.pop_events() {
        out.push(events.as_slice().to_vec());
    }
    out
}

fn raw(kind: u16, code: u16, value: i32) -> RawEvent {
    RawEvent::new(kind, code, value)
}

fn no_errors(device: &Rc<RefCell<Device>>) {
    assert!(
        device.borrow().protocol_errors.is_empty(),
        "{:?}",
        device.borrow().protocol_errors
    );
}

#[test]
fn bring_up_stops_at_hello_and_the_device_discards_events() {
    let mut rig = build_with(keyboard(), |_| {});
    let features = rig.driver.info().features;
    assert_eq!(features, FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM);
    assert_eq!(rig.driver.info().queue_size, 64);
    assert_eq!(rig.driver.info().vector, 1);
    assert_eq!(rig.driver.phase(), Phase::Introduced);
    let status = rig.device.borrow().status;
    assert!(status & STATUS_FEATURES_OK != 0);
    assert_eq!(status & STATUS_DRIVER_OK, 0, "DRIVER_OK waits for READY");
    assert_eq!(rig.device.borrow_mut().buffers(), 0, "nothing posted yet");
    assert_eq!(*rig.doorbells.borrow(), 0);

    let hello = rig.driver.hello(0x0000_0800);
    assert_eq!(hello.location, 0x0000_0800);
    assert_eq!(hello.name.as_bytes(), b"QEMU Virtio Keyboard");
    hello.validate(&Hello::HANDLE_RIGHTS).expect("accepted");

    // QEMU discards what it is sent before DRIVER_OK.
    rig.device.borrow_mut().send(event(EV_KEY, KEY_A, 1));
    assert_eq!(rig.device.borrow().inactive, 1);
    let drained = rig.driver.on_interrupt().expect("nothing to take");
    assert_eq!((drained.taken, drained.posted), (0, 0));
    assert!(rig.driver.pop_events().is_none());
    no_errors(&rig.device);
}

#[test]
fn ready_sets_driver_ok_and_posts_every_buffer() {
    let mut rig = build_with(tablet(), |_| {});
    assert_eq!(
        rig.driver.on_control(&Message::Ready(Ready { node: 7 })),
        Ok(Control::Started { node: 7 })
    );
    assert_eq!(rig.driver.phase(), Phase::Running);
    assert!(rig.device.borrow().status & STATUS_DRIVER_OK != 0);
    assert_eq!(rig.driver.posted(), 64);
    assert_eq!(rig.device.borrow_mut().buffers(), 64);
    assert_eq!(*rig.doorbells.borrow(), 1);
    no_errors(&rig.device);
}

#[test]
fn test_inputs_sequence_reaches_the_core_as_whole_reports() {
    // docs/INPUT.md §4: key a down, up; the tablet's x and y; left down, up.
    let mut keyboard = running(keyboard(), |_| {});
    let mut tablet = running(tablet(), |_| {});
    let mut sessions = [&keyboard, &tablet].map(|rig| {
        Session::accept(&rig.driver.hello(0), &Hello::HANDLE_RIGHTS).expect("accepted")
    });
    for value in [1, 0] {
        keyboard.device.borrow_mut().send(event(EV_KEY, KEY_A, value));
        keyboard.device.borrow_mut().send(syn());
    }
    for report in [
        vec![event(EV_ABS, ABS_X, 0x1234), event(EV_ABS, ABS_Y, 0x2345)],
        vec![event(EV_KEY, BTN_LEFT, 1)],
        vec![event(EV_KEY, BTN_LEFT, 0)],
    ] {
        for event in report {
            tablet.device.borrow_mut().send(event);
        }
        tablet.device.borrow_mut().send(syn());
    }

    let mut delivered: [Vec<Vec<RawEvent>>; 2] = [Vec::new(), Vec::new()];
    for (index, rig) in [&mut keyboard, &mut tablet].into_iter().enumerate() {
        for message in messages(&mut rig.driver) {
            let events = ferrix_inputctl::message::Events::new(&message).expect("fits");
            let received = sessions[index]
                .receive(&Message::Events(events), 1, |report| {
                    delivered[index].push(report.events().to_vec());
                })
                .expect("the core accepts every message");
            assert!(matches!(received, Received::Events { .. }));
        }
        no_errors(&rig.device);
    }
    assert_eq!(
        delivered[0],
        [
            vec![raw(EV_KEY, KEY_A, 1), raw(EV_SYN, SYN_REPORT, 0)],
            vec![raw(EV_KEY, KEY_A, 0), raw(EV_SYN, SYN_REPORT, 0)],
        ]
    );
    assert_eq!(
        delivered[1],
        [
            vec![
                raw(EV_ABS, ABS_X, 0x1234),
                raw(EV_ABS, ABS_Y, 0x2345),
                raw(EV_SYN, SYN_REPORT, 0)
            ],
            vec![raw(EV_KEY, BTN_LEFT, 1), raw(EV_SYN, SYN_REPORT, 0)],
            vec![raw(EV_KEY, BTN_LEFT, 0), raw(EV_SYN, SYN_REPORT, 0)],
        ]
    );
}

#[test]
fn the_queue_is_refilled_before_anything_is_forwarded() {
    let mut rig = running(keyboard(), |_| {});
    // 32 two-event reports fill all 64 buffers; the 33rd has none and QEMU
    // drops it whole.
    for index in 0..33 {
        rig.device.borrow_mut().send(event(EV_KEY, KEY_A, index % 2 ^ 1));
        rig.device.borrow_mut().send(syn());
    }
    assert_eq!(rig.device.borrow().delivered, 64);
    assert_eq!(rig.device.borrow().dropped_reports, 1);

    let drained = rig.driver.on_interrupt().expect("sound");
    assert_eq!((drained.taken, drained.posted), (64, 64));
    assert_eq!(rig.device.borrow_mut().buffers(), 64, "every buffer back");
    assert!(*rig.doorbells.borrow() >= 2);
    // The next report arrives.
    rig.device.borrow_mut().send(event(EV_KEY, KEY_A, 1));
    rig.device.borrow_mut().send(syn());
    assert_eq!(rig.device.borrow().dropped_reports, 1);

    let mut total = 0;
    while let Some(events) = rig.driver.pop_events() {
        assert!(events.as_slice().len() <= MAX_EVENTS);
        assert!(events.as_slice().last().is_some_and(RawEvent::is_report));
        total += events.as_slice().len();
    }
    assert_eq!(total, 64);
    no_errors(&rig.device);
}

#[test]
fn what_the_core_would_not_publish_is_dropped_here() {
    let mut rig = running(keyboard(), |_| {});
    for sent in [
        event(EV_REL, REL_X, 3),
        event(EV_KEY, 31, 1),
        event(EV_SYN, SYN_MT_REPORT, 0),
        event(EV_SND, 0, 1),
        event(EV_KEY, 0, 1),
        event(EV_KEY, KEY_A, 1),
        syn(),
        // A report of nothing the core publishes sends nothing at all.
        event(EV_REL, REL_X, 3),
        syn(),
    ] {
        rig.device.borrow_mut().send(sent);
    }
    let drained = rig.driver.on_interrupt().expect("sound");
    assert_eq!((drained.taken, drained.dropped), (9, 6));
    assert_eq!(rig.driver.stats().dropped, 6);
    assert_eq!(
        rig.driver.pop_events().map(|events| events.as_slice().to_vec()),
        Some(vec![raw(EV_KEY, KEY_A, 1), raw(EV_SYN, SYN_REPORT, 0)])
    );
    assert!(rig.driver.pop_events().is_none());
}

#[test]
fn a_report_longer_than_a_message_or_the_core_is_sent_whole() {
    let mut rig = running(keyboard(), |device| device.misbehave.no_hold = true);
    let mut session =
        Session::accept(&rig.driver.hello(0), &Hello::HANDLE_RIGHTS).expect("accepted");
    let mut delivered = Vec::new();
    let mut messages_sent = 0;
    // 300 key events then one SYN_REPORT, taken 64 at a time.
    let mut sent = 0;
    while sent < 301 {
        for _ in 0..64.min(301 - sent) {
            let next = if sent == 300 {
                syn()
            } else {
                event(EV_KEY, KEY_A, (sent % 2) as i32 ^ 1)
            };
            rig.device.borrow_mut().send(next);
            sent += 1;
        }
        for message in messages(&mut rig.driver) {
            assert!(message.len() <= MAX_EVENTS);
            messages_sent += 1;
            let events = ferrix_inputctl::message::Events::new(&message).expect("fits");
            let _ = session
                .receive(&Message::Events(events), 0, |report| {
                    delivered.push(report.events().len());
                })
                .expect("the core never refuses the driver");
        }
    }
    assert!(messages_sent >= 5);
    assert_eq!(rig.driver.split_reports(), 1);
    // 255 events and a SYN_REPORT the batch added, then 45 and the device's.
    assert_eq!(delivered, [MAX_REPORT, 46]);
    assert_eq!(rig.device.borrow().dropped_reports, 0);
}

#[test]
fn a_full_batch_leaves_completions_in_the_ring_until_messages_are_taken() {
    let mut rig = running(keyboard(), |device| device.misbehave.no_hold = true);
    let mut taken = 0;
    let mut sent = 0;
    // Never a SYN_REPORT, never a message taken: the batch fills.
    for round in 0..5 {
        for _ in 0..64 {
            rig.device.borrow_mut().send(event(EV_KEY, KEY_A, (sent % 2) ^ 1));
            sent += 1;
        }
        let drained = rig.driver.on_interrupt().expect("sound");
        taken += drained.taken;
        if round < 3 {
            assert!(!drained.more);
        }
    }
    assert_eq!(taken, 254, "two short of the batch");
    let drained = rig.driver.on_interrupt().expect("sound");
    assert!(drained.more);
    assert_eq!(drained.taken, 0);

    // Taking messages makes room, and nothing was lost.
    let mut forwarded = 0;
    loop {
        while let Some(events) = rig.driver.pop_events() {
            forwarded += events.as_slice().len();
        }
        let drained = rig.driver.on_interrupt().expect("sound");
        if drained.taken == 0 && !drained.more {
            break;
        }
    }
    assert_eq!(forwarded + rig.driver.pending(), sent as usize);
    assert_eq!(rig.device.borrow().dropped_reports, 64);
}

#[test]
fn a_device_that_breaks_the_protocol_is_failed() {
    type Setup = fn(&mut Device);
    let cases: [(Setup, DeviceError); 3] = [
        (
            |device| device.misbehave.written = Some(4),
            DeviceError::Protocol(InputError::EventWritten(4)),
        ),
        (
            |device| device.misbehave.written = Some(16),
            DeviceError::Protocol(InputError::EventWritten(16)),
        ),
        (
            |device| device.misbehave.needs_reset = true,
            DeviceError::NeedsReset,
        ),
    ];
    for (misbehave, expected) in cases {
        let mut rig = running(keyboard(), |_| {});
        misbehave(&mut rig.device.borrow_mut());
        rig.device.borrow_mut().send(event(EV_KEY, KEY_A, 1));
        rig.device.borrow_mut().send(syn());
        assert_eq!(rig.driver.on_interrupt(), Err(expected));
        assert_eq!(rig.driver.fault(), Some(expected));
        assert!(rig.device.borrow().status & STATUS_FAILED != 0);
        assert_eq!(rig.driver.on_interrupt(), Err(expected));
        assert!(rig.driver.pop_events().is_none());
        rig.device.borrow_mut().misbehave.needs_reset = false;
        assert!(matches!(rig.driver.shutdown(), Teardown::Released(_)));
    }

    let mut rig = running(keyboard(), |_| {});
    rig.device.borrow_mut().jump_used_index(100);
    assert_eq!(
        rig.driver.on_interrupt(),
        Err(DeviceError::Queue(QueueError::UsedIndexJumped))
    );
}

#[test]
fn messages_from_the_core_are_followed_in_turn() {
    let mut rig = build_with(keyboard(), |_| {});
    for message in [
        Message::Stopped,
        Message::Events(ferrix_inputctl::message::Events::new(&[]).expect("empty")),
    ] {
        assert_eq!(
            rig.driver.on_control(&message),
            Err(ControlError::Unexpected(message.kind()))
        );
    }
    let ready = Message::Ready(Ready { node: 1 });
    assert_eq!(rig.driver.on_control(&ready), Ok(Control::Started { node: 1 }));
    assert_eq!(
        rig.driver.on_control(&ready),
        Err(ControlError::Unexpected(ready.kind()))
    );
    rig.device.borrow_mut().send(event(EV_KEY, KEY_A, 1));
    rig.device.borrow_mut().send(syn());
    let _ = rig.driver.on_interrupt().expect("sound");
    assert_eq!(rig.driver.on_control(&Message::Stop), Ok(Control::Stop));
    assert_eq!(rig.driver.phase(), Phase::Stopping);
    assert!(rig.driver.pop_events().is_none(), "stopping discards");
    assert_eq!(rig.driver.on_interrupt().expect("acknowledged").taken, 0);
    assert_eq!(
        rig.driver.on_control(&Message::Stop),
        Err(ControlError::Unexpected(Message::Stop.kind()))
    );
    assert!(matches!(rig.driver.shutdown(), Teardown::Released(_)));

    // Refused before READY: DRIVER_OK is never set.
    let mut rig = build_with(keyboard(), |_| {});
    assert_eq!(
        rig.driver.on_control(&Message::Refused(Refusal::Bits)),
        Ok(Control::Refused(Refusal::Bits))
    );
    assert_eq!(
        rig.driver.on_control(&ready),
        Err(ControlError::Unexpected(ready.kind()))
    );
    assert_eq!(rig.device.borrow().status & STATUS_DRIVER_OK, 0);

    // READY for a device that has given up.
    let mut rig = build_with(keyboard(), |_| {});
    rig.device.borrow_mut().misbehave.needs_reset = true;
    assert_eq!(
        rig.driver.on_control(&ready),
        Err(ControlError::Device(DeviceError::NeedsReset))
    );
}

#[test]
fn bring_up_failures_hand_the_memory_back() {
    let bus = Bus::new();
    let fail = |setup: fn(&mut Device), answers: Vec<Answer>, area_pages: usize| {
        let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus), answers)));
        setup(&mut device.borrow_mut());
        let (mut parts, _) = parts(&bus, &device);
        parts.area = bus.pin(area_pages, false);
        let failure = Driver::init(parts, Options { reset_polls: 4 }).expect_err("refused");
        (failure, device)
    };

    let (failure, _) = fail(|device| device.offered = FEATURE_ACCESS_PLATFORM, keyboard(), 1);
    assert_eq!(
        failure.error,
        InitError::Transport(TransportError::MissingFeatures {
            missing: FEATURE_VERSION_1
        })
    );
    let Teardown::Released(released) = failure.teardown else {
        panic!("a device that reset is released");
    };
    assert!(matches!(released.rings, Rings::Unused(_)));

    let (failure, _) = fail(|_| {}, vec![(0x11, 0x03, vec![0b1]), qemu_abs(0, 9, 3)], 1);
    assert_eq!(
        failure.error,
        InitError::Config(InputError::AbsRange {
            axis: 0,
            min: 9,
            max: 3
        })
    );

    let (failure, _) = fail(|_| {}, keyboard(), 0);
    assert_eq!(failure.error, InitError::NoRoom);

    let (failure, device) = fail(|device| device.misbehave.drop_vector = true, keyboard(), 1);
    assert_eq!(
        failure.error,
        InitError::VectorRefused {
            asked: 1,
            kept: 0xFFFF
        }
    );
    let Teardown::Released(released) = failure.teardown else {
        panic!("released");
    };
    assert!(matches!(released.rings, Rings::Queue(_)));
    assert!(device.borrow().status == 0);
}

#[test]
fn a_smaller_queue_is_a_power_of_two_and_a_device_that_will_not_reset_keeps_the_memory() {
    let rig = running(tablet(), |device| device.set_queue_max(48));
    assert_eq!(rig.driver.info().queue_size, 32);
    assert_eq!(rig.driver.posted(), 32);
    no_errors(&rig.device);

    rig.device.borrow_mut().misbehave.never_reset = true;
    let Teardown::Wedged(parts) = rig.driver.shutdown() else {
        panic!("a device that did not reset may still write");
    };
    // Never dropped: leaked on purpose, as the process would.
    core::mem::forget(parts);
}
