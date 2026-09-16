//! Fuzz the input control protocol: its messages, the core's session and the
//! per-open queues.
//!
//! The kernel's input core reads these messages from a ring-3 driver, so every
//! byte of them is the driver's word; the session is what stands between a
//! lying driver and the device's state, and the queues are what a program
//! reads.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **Decoding is exact**: bytes that decode encode back to the same bytes.
//! 2. **Reports are whole and declared**: every report the session delivers
//!    ends in its one `SYN_REPORT`, holds at least one event before it and at
//!    most `MAX_REPORT` in all, holds only events the core publishes, and goes
//!    to the open holding the grab when one does. A broken session stays
//!    broken.
//! 3. **A reader sees whole reports**: whatever an open reads, however the
//!    reads are split, ends at a `SYN_REPORT` once the queue is drained. Of an
//!    open whose queue was never flushed by type, each run of events up to a
//!    `SYN_REPORT` that holds no `SYN_DROPPED` is exactly a report delivered
//!    to that open, in delivery order.

#![no_main]

use ferrix_inputctl::message::{
    AXES, AxisRange, Bitmaps, DeviceId, Events, Hello, MAX_EVENTS, Message, RawEvent, Text, VERSION,
};
use ferrix_inputctl::queue::{Clock, Clocks, Queue, ReadError, ReadFlags, Stamped};
use ferrix_inputctl::session::{MAX_REPORT, OpenId, Report, Session};
use ferrix_linux_abi::input::{
    ABS_MT_SLOT, ABS_X, ABS_Y, BTN_LEFT, EV_ABS, EV_KEY, EV_LED, EV_MSC, EV_REL, EV_REP, EV_SYN,
    Event, KEY_A, LED_CAPSL, MSC_SCAN, REL_WHEEL, REL_X, SYN_DROPPED, SYN_REPORT,
};
use ferrix_linux_abi::socket::Width;
use libfuzzer_sys::fuzz_target;

const OPENS: usize = 3;

fn set(bits: &mut [u8], index: u16) {
    bits[usize::from(index / 8)] |= 1 << (index % 8);
}

fn hello() -> Hello {
    let mut bits = Bitmaps::EMPTY;
    for kind in [EV_KEY, EV_REL, EV_ABS, EV_MSC, EV_LED, EV_REP] {
        set(&mut bits.types, kind);
    }
    set(&mut bits.keys, KEY_A);
    set(&mut bits.keys, BTN_LEFT);
    set(&mut bits.rels, REL_X);
    set(&mut bits.rels, REL_WHEEL);
    for axis in [ABS_X, ABS_Y, ABS_MT_SLOT] {
        set(&mut bits.abs, axis);
    }
    set(&mut bits.msc, MSC_SCAN);
    set(&mut bits.leds, LED_CAPSL);
    let mut axes = [AxisRange::default(); AXES];
    for (axis, fuzz) in [(ABS_X, 0), (ABS_Y, 16), (ABS_MT_SLOT, 0)] {
        axes[usize::from(axis)] = AxisRange {
            minimum: 0,
            maximum: 255,
            fuzz,
            ..AxisRange::default()
        };
    }
    Hello {
        version: VERSION,
        location: 0,
        id: DeviceId::default(),
        name: Text::new(b"fuzz").expect("fits"),
        serial: Text::NONE,
        bits,
        axes,
    }
}

/// An event from two bytes: mostly declared, sometimes not.
fn event(a: u8, b: u8) -> RawEvent {
    let value = i32::from(b) - 64;
    match a % 12 {
        0..=2 => RawEvent::new(EV_SYN, SYN_REPORT, 0),
        3 => RawEvent::new(EV_KEY, KEY_A, value & 3),
        4 => RawEvent::new(EV_KEY, BTN_LEFT, value & 1),
        5 => RawEvent::new(EV_REL, REL_X, value),
        6 => RawEvent::new(EV_ABS, ABS_Y, i32::from(b)),
        7 => RawEvent::new(EV_ABS, ABS_X + u16::from(b & 1) * ABS_MT_SLOT, i32::from(b)),
        8 => RawEvent::new(EV_MSC, MSC_SCAN, value),
        9 => RawEvent::new(EV_LED, LED_CAPSL, value & 1),
        10 => RawEvent::new(EV_REP, u16::from(b & 1), value),
        // Rarely, anything at all, which is mostly a lie that breaks the
        // session.
        _ if b % 8 == 0 => RawEvent::new(u16::from(b % 24), u16::from(b), value),
        _ => RawEvent::new(EV_SYN, SYN_REPORT, 0),
    }
}

struct Open {
    queue: Queue<Vec<Stamped>>,
    delivered: Vec<Vec<RawEvent>>,
    stream: Vec<RawEvent>,
    flushed: bool,
}

fn drain(open: &mut Open, width: Width, chunk: usize) {
    let size = Event::size(width);
    let mut buffer = vec![0u8; size * chunk.max(1)];
    let flags = ReadFlags {
        width,
        nonblocking: true,
        gone: false,
    };
    loop {
        match open.queue.read(&mut buffer, flags, &Clocks::default()) {
            Ok(len) => {
                assert!(len > 0 && len % size == 0, "a read of {len} bytes");
                for bytes in buffer[..len].chunks_exact(size) {
                    let read = Event::read(width, bytes).expect("a whole event");
                    open.stream
                        .push(RawEvent::new(read.r#type, read.code, read.value));
                }
            }
            Err(ReadError::WouldBlock) => break,
            Err(other) => panic!("a live open read {other:?}"),
        }
    }
    if let Some(last) = open.stream.last() {
        assert!(last.is_report(), "a drained read ended in {last:?}");
    }
}

/// Property 3's second half.
fn check_stream(open: &Open) {
    if open.flushed {
        return;
    }
    let mut cursor = 0;
    for segment in open.stream.split_inclusive(RawEvent::is_report) {
        if segment
            .iter()
            .any(|event| event.kind == EV_SYN && event.code == SYN_DROPPED)
        {
            continue;
        }
        let found = open.delivered[cursor..]
            .iter()
            .position(|report| report.as_slice() == segment);
        let Some(offset) = found else {
            panic!("a read report {segment:?} was never delivered in that order");
        };
        cursor += offset + 1;
    }
}

fuzz_target!(|bytes: &[u8]| {
    // 1.
    if let Ok(message) = Message::decode(bytes) {
        assert_eq!(message.encode().as_bytes(), bytes);
    }

    // 2 and 3.
    let mut session =
        Session::accept(&hello(), &Hello::HANDLE_RIGHTS).expect("a good HELLO is accepted");
    let published = *session.capabilities();
    let mut opens: Vec<Open> = (0..OPENS)
        .map(|index| Open {
            queue: Queue::new(vec![Stamped::default(); 4 << index]).expect("a power of two"),
            delivered: Vec::new(),
            stream: Vec::new(),
            flushed: false,
        })
        .collect();
    let mut now = 0u64;

    for step in bytes.chunks_exact(4) {
        let (op, a, b, c) = (step[0], step[1], step[2], step[3]);
        let which = usize::from(a) % OPENS;
        let id = OpenId(which as u64);
        now += u64::from(c);
        let was_broken = session.is_broken();
        match op % 8 {
            0..=2 => {
                let count = usize::from(b) % (MAX_EVENTS + 1);
                let list: Vec<RawEvent> = (0..count)
                    .map(|index| {
                        let at = index % 3;
                        event(step[1 + at].wrapping_add(index as u8), step[3 - at])
                    })
                    .collect();
                let message = Message::Events(Events::new(&list).expect("at most 64"));
                let grab = session.grabbed();
                let mut reports: Vec<Report> = Vec::new();
                let _ = session.receive(&message, now, |report| reports.push(*report));
                for report in &reports {
                    let events = report.events();
                    assert!((2..=MAX_REPORT).contains(&events.len()));
                    let (last, body) = events.split_last().expect("not empty");
                    assert!(last.is_report(), "a report ends in {last:?}");
                    for event in body {
                        assert!(
                            published.bits.has_code(event.kind, event.code)
                                || (event.kind == EV_REP && published.bits.has_type(EV_REP)),
                            "an unpublished event {event:?} was delivered"
                        );
                    }
                    assert_eq!(report.recipient(), grab);
                    assert_eq!(report.time(), now);
                    for (index, open) in opens.iter_mut().enumerate() {
                        if report.is_for(OpenId(index as u64)) {
                            open.queue.deliver(report);
                            open.delivered.push(events.to_vec());
                        }
                    }
                }
            }
            3 => {
                if b & 1 == 0 {
                    let _ = session.grab(id);
                } else {
                    let _ = session.ungrab(id);
                }
            }
            4 => {
                let width = if b & 1 == 0 {
                    Width::Bits64
                } else {
                    Width::Bits32
                };
                drain(&mut opens[which], width, usize::from(c % 5));
            }
            5 => {
                let clock =
                    [Clock::Realtime, Clock::Monotonic, Clock::Boottime][usize::from(b % 3)];
                opens[which].queue.set_clock(clock, now);
            }
            6 => {
                let kind = [EV_KEY, EV_REL, EV_ABS, EV_LED][usize::from(b % 4)];
                opens[which].queue.flush_type(kind);
                opens[which].flushed = true;
            }
            _ => {
                if b & 1 == 0 {
                    let _ = session.stop();
                } else {
                    let _ = session.receive(&Message::Stopped, now, |_| {});
                }
            }
        }
        if was_broken {
            assert!(session.is_broken(), "a broken session recovered");
        }
    }

    for open in &mut opens {
        drain(open, Width::Bits64, 64);
        check_stream(open);
    }
});
