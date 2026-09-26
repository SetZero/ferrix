//! The driver against QEMU's device: bring-up, READY, the samples the device
//! reads, completions, halts, and what a hostile device or core is refused.

use core::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use ferrix_sndctl::message::{
    DIRECTION_CAPTURE, DIRECTION_PLAYBACK, Elapsed, MAX_PUBLISHED, Message, Published, Ready,
    Refusal, Submit,
};
use ferrix_sndctl::pcm::MAX_IN_FLIGHT;
use ferrix_virtio::pci::{STATUS_DRIVER_OK, STATUS_FAILED};

use super::fake::{Bus, Device, Handle, PAGE, Region};
use crate::{
    Control, ControlError, DeviceError, Driver, InitError, Options, Parts, Phase, Refusing,
    Teardown,
};

type Snd = Driver<Handle, Region, Region>;

struct Rig {
    bus: Rc<Bus>,
    device: Rc<RefCell<Device>>,
    driver: Snd,
    buffer: Region,
}

fn options() -> Options {
    Options {
        reset_polls: 4,
        control_polls: 4,
    }
}

fn parts(bus: &Rc<Bus>, device: &Rc<RefCell<Device>>) -> Parts<Handle, Region, Region> {
    Parts {
        transport: Handle {
            device: Rc::clone(device),
        },
        control: bus.pin(2, false),
        tx: bus.pin(4, false),
        scratch: bus.pin(1, false),
    }
}

fn published() -> Published {
    Published {
        stream: 0,
        rate: 48_000,
        format: 2,
        channels: 2,
        period_bytes: 3840,
        buffer_bytes: 15360,
    }
}

fn ready(stream: Published) -> Ready {
    let mut streams = [Published::default(); MAX_PUBLISHED];
    streams[0] = stream;
    Ready {
        card: 0,
        published: 1,
        streams,
    }
}

/// A driver brought up and READY, its buffer's four pages `scattered` or not.
fn rig(scattered: bool) -> Rig {
    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus))));
    let mut driver = Snd::init(parts(&bus, &device), options()).expect("QEMU's device comes up");
    let buffer = bus.pin(4, scattered);
    let pages = buffer.device.clone();
    driver
        .on_ready(&ready(published()), &[&pages])
        .expect("READY");
    Rig {
        bus,
        device,
        driver,
        buffer,
    }
}

fn submit(sequence: u32, offset: u32, bytes: u32) -> Message {
    Message::Submit(Submit {
        stream: 0,
        sequence,
        offset,
        bytes,
    })
}

fn codes(device: &Rc<RefCell<Device>>) -> Vec<u32> {
    device
        .borrow()
        .requests
        .iter()
        .map(|(code, _)| *code)
        .collect()
}

/// Follow a message from the core, which must be followed.
fn follow(driver: &mut Snd, message: &Message) {
    assert_eq!(
        driver.on_control(message),
        Ok(Control::Followed),
        "{message:?}"
    );
}

fn messages(driver: &mut Snd) -> Vec<Message> {
    core::iter::from_fn(|| driver.pop_message()).collect()
}

#[test]
fn qemus_device_comes_up_and_says_what_it_offers() {
    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus))));
    let driver = Snd::init(parts(&bus, &device), options()).expect("QEMU's device comes up");
    assert_ne!(
        device.borrow().status & STATUS_DRIVER_OK,
        0,
        "DRIVER_OK before HELLO"
    );
    assert_eq!(codes(&device), [0x0100], "one PCM_INFO");
    assert_eq!(driver.info().config.streams, 2);
    let hello = driver.hello(0x300);
    assert_eq!(
        (hello.version, hello.location, hello.streams),
        (1, 0x300, 2)
    );
    let playback = hello.offers[0];
    assert_eq!(playback.direction, DIRECTION_PLAYBACK);
    assert_eq!((playback.channels_min, playback.channels_max), (1, 2));
    assert_eq!(playback.rates, 0x3fff);
    // ALSA's S8 U8 S16_LE U16_LE S32_LE U32_LE FLOAT_LE.
    let alsa = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 4) | (1 << 10) | (1 << 12) | (1 << 14);
    assert_eq!(playback.formats, alsa);
    assert_eq!(hello.offers[1].direction, DIRECTION_CAPTURE);
    assert!(device.borrow().protocol_errors.is_empty());
}

#[test]
fn ready_gives_the_stream_its_one_configuration() {
    let rig = rig(false);
    assert_eq!(rig.driver.phase(), Phase::Running);
    let device = rig.device.borrow();
    let requests = &device.requests;
    assert_eq!(requests.len(), 3, "PCM_INFO, SET_PARAMS, PREPARE");
    let (code, params) = &requests[1];
    assert_eq!(*code, 0x0101);
    assert_eq!(params[4..8], 0u32.to_le_bytes(), "stream 0");
    assert_eq!(params[8..12], 15360u32.to_le_bytes(), "buffer_bytes");
    assert_eq!(params[12..16], 3840u32.to_le_bytes(), "period_bytes");
    assert_eq!(params[20..24], [2, 5, 7, 0], "two channels, S16, 48 kHz");
    assert_eq!(requests[2].0, 0x0102);
    assert!(device.protocol_errors.is_empty());
}

#[test]
fn the_device_reads_exactly_the_samples_submitted_and_the_first_starts_it() {
    let mut rig = rig(true);
    let samples: Vec<u8> = (0..15360_u32).map(|index| (index % 251) as u8).collect();
    rig.buffer.fill(0, &samples);
    // A period, then a range crossing the scattered pages' boundary.
    follow(&mut rig.driver, &submit(0, 0, 3840));
    assert_eq!(
        *codes(&rig.device).last().expect("a request"),
        0x0104,
        "START"
    );
    follow(&mut rig.driver, &submit(1, 3840, 2800));
    assert_eq!(
        codes(&rig.device)
            .iter()
            .filter(|code| **code == 0x0104)
            .count(),
        1
    );
    let held = rig.device.borrow().held_data();
    assert_eq!(held[0].len(), 1, "one page");
    assert_eq!(held[1].len(), 2, "two runs: the pages are apart");
    assert_eq!(
        held[1][0].1 as usize,
        PAGE - 3840,
        "to the end of the first page"
    );
    assert_eq!(
        held[1][1].1 as usize,
        2800 - (PAGE - 3840),
        "the rest on the next"
    );
    assert_eq!(rig.device.borrow_mut().consume(2), 2);
    let drained = rig.driver.on_interrupt().expect("completions");
    assert_eq!(drained.taken, 2);
    assert_eq!(
        messages(&mut rig.driver),
        [
            Message::Elapsed(Elapsed {
                stream: 0,
                sequence: 0,
                played: true,
                latency_bytes: 3840
            }),
            Message::Elapsed(Elapsed {
                stream: 0,
                sequence: 1,
                played: true,
                latency_bytes: 2800
            }),
        ]
    );
    assert_eq!(rig.device.borrow().played, samples[..6640]);
    assert!(rig.device.borrow().protocol_errors.is_empty());
}

#[test]
fn contiguous_pages_make_one_run() {
    let mut rig = rig(false);
    follow(&mut rig.driver, &submit(0, 2000, 3840));
    assert_eq!(rig.device.borrow().held_data()[0].len(), 1);
}

#[test]
fn completions_out_of_order_go_back_in_order() {
    let mut rig = rig(false);
    rig.device.borrow_mut().misbehave.reversed = true;
    for sequence in 0..3 {
        follow(&mut rig.driver, &submit(sequence, sequence * 3840, 3840));
    }
    assert_eq!(rig.device.borrow_mut().consume(1), 1);
    let _ = rig.driver.on_interrupt().expect("the newest");
    assert!(
        messages(&mut rig.driver).is_empty(),
        "held until the oldest is done"
    );
    assert_eq!(rig.device.borrow_mut().consume(2), 2);
    let _ = rig.driver.on_interrupt().expect("the rest");
    let order: Vec<u32> = messages(&mut rig.driver)
        .into_iter()
        .map(|message| match message {
            Message::Elapsed(elapsed) => elapsed.sequence,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(order, [0, 1, 2]);
}

#[test]
fn a_halt_stops_releases_reports_and_prepares_again() {
    let mut rig = rig(false);
    for sequence in 0..3 {
        follow(&mut rig.driver, &submit(sequence, sequence * 3840, 3840));
    }
    assert_eq!(rig.device.borrow_mut().consume(1), 1);
    let before = codes(&rig.device).len();
    assert_eq!(
        rig.driver.on_control(&Message::Halt { stream: 0 }),
        Ok(Control::Followed)
    );
    assert_eq!(
        codes(&rig.device)[before..],
        [0x0105, 0x0103, 0x0101, 0x0102],
        "STOP, RELEASE, then SET_PARAMS and PREPARE again"
    );
    let sent = messages(&mut rig.driver);
    assert_eq!(sent.len(), 4, "three ELAPSED, then HALTED: {sent:?}");
    assert_eq!(
        sent[3],
        Message::Halted {
            stream: 0,
            unplayed: 0
        }
    );
    assert_eq!(rig.driver.in_flight(), 0);
    assert_eq!(rig.device.borrow().holding(), 0);
    // The next submission starts the stream again.
    follow(&mut rig.driver, &submit(3, 0, 100));
    assert_eq!(*codes(&rig.device).last().expect("a request"), 0x0104);
}

#[test]
fn a_buffer_the_device_refuses_is_not_played() {
    let mut rig = rig(false);
    rig.device.borrow_mut().misbehave.status = Some(0x8001);
    follow(&mut rig.driver, &submit(0, 0, 3840));
    let _ = rig.device.borrow_mut().consume(1);
    let drained = rig
        .driver
        .on_interrupt()
        .expect("a refusal is not a broken device");
    assert_eq!(drained.refused, 1);
    assert!(matches!(
        messages(&mut rig.driver)[..],
        [Message::Elapsed(Elapsed { played: false, .. })]
    ));
}

#[test]
fn a_device_that_breaks_the_protocol_is_failed() {
    let mut rig = rig(false);
    rig.device.borrow_mut().misbehave.written = Some(3848);
    follow(&mut rig.driver, &submit(0, 0, 3840));
    let _ = rig.device.borrow_mut().consume(1);
    assert!(matches!(
        rig.driver.on_interrupt(),
        Err(DeviceError::Protocol(_))
    ));
    assert_ne!(rig.device.borrow().status & STATUS_FAILED, 0);
    assert!(rig.driver.pop_message().is_none(), "nothing more from it");
    let _ = rig.bus;
}

#[test]
fn a_silent_or_refusing_device_fails_bring_up() {
    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus))));
    device.borrow_mut().misbehave.silent = true;
    let failure = Snd::init(parts(&bus, &device), options()).expect_err("no answer");
    assert_eq!(
        failure.error,
        InitError::Device(DeviceError::ControlTimeout)
    );
    assert!(matches!(failure.teardown, Teardown::Released(_)));

    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus))));
    device.borrow_mut().streams = 11;
    let failure = Snd::init(parts(&bus, &device), options()).expect_err("too many streams");
    assert!(matches!(failure.error, InitError::Config(_)));

    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus))));
    device.borrow_mut().misbehave.refuse_params = true;
    let mut driver = Snd::init(parts(&bus, &device), options()).expect("up");
    let pages = bus.pin(4, false).device;
    assert!(matches!(
        driver.on_ready(&ready(published()), &[&pages]),
        Err(ControlError::Device(DeviceError::Refused {
            request: 0x0101,
            status: 0x8002
        }))
    ));
}

#[test]
fn a_core_that_asks_what_the_device_must_not_do_is_refused() {
    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus))));
    let mut driver = Snd::init(parts(&bus, &device), options()).expect("up");
    assert_eq!(
        driver.on_control(&submit(0, 0, 4)),
        Err(ControlError::Unexpected(4)),
        "SUBMIT before READY"
    );
    let pages = bus.pin(4, false).device;
    let small = bus.pin(3, false).device;
    let cases: [(Published, &[u64], Refusing); 4] = [
        (
            Published {
                stream: 1,
                ..published()
            },
            &pages,
            Refusing::Stream(1),
        ),
        (
            Published {
                stream: 5,
                ..published()
            },
            &pages,
            Refusing::Stream(5),
        ),
        (published(), &small, Refusing::Buffer),
        (
            Published {
                format: 16,
                ..published()
            },
            &pages,
            Refusing::Configuration,
        ),
    ];
    for (stream, buffer, why) in cases {
        assert_eq!(
            driver.on_ready(&ready(stream), &[buffer]),
            Err(ControlError::Refused(why))
        );
    }
    assert_eq!(codes(&device), [0x0100], "nothing asked of the device");
    assert_eq!(
        driver.on_ready(&ready(published()), &[]),
        Err(ControlError::Refused(Refusing::Count))
    );

    let mut rig = rig(false);
    for (message, why) in [
        (submit(0, 15000, 1000), Refusing::Range),
        (submit(0, 0, 0), Refusing::Range),
        (
            Message::Submit(Submit {
                stream: 1,
                sequence: 0,
                offset: 0,
                bytes: 4,
            }),
            Refusing::Range,
        ),
    ] {
        assert_eq!(
            rig.driver.on_control(&message),
            Err(ControlError::Refused(why))
        );
    }
    for sequence in 0..MAX_IN_FLIGHT as u32 {
        follow(&mut rig.driver, &submit(sequence, 0, 4));
    }
    assert_eq!(
        rig.driver.on_control(&submit(99, 0, 4)),
        Err(ControlError::Refused(Refusing::Busy))
    );
}

#[test]
fn refused_and_stop_end_the_driver_and_it_shuts_down_clean() {
    let mut rig = rig(false);
    assert_eq!(rig.driver.on_control(&Message::Stop), Ok(Control::Stop));
    assert_eq!(
        rig.driver.on_control(&submit(0, 0, 4)),
        Err(ControlError::Unexpected(4)),
        "nothing after STOP"
    );
    assert!(matches!(rig.driver.shutdown(), Teardown::Released(_)));
    assert_eq!(rig.device.borrow().status, 0, "reset");

    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(Rc::clone(&bus))));
    let mut driver = Snd::init(parts(&bus, &device), options()).expect("up");
    assert_eq!(
        driver.on_control(&Message::Refused(Refusal::Nothing)),
        Ok(Control::Refused(Refusal::Nothing))
    );
    assert_eq!(driver.phase(), Phase::Refused);
}
