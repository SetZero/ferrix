//! Fuzz the virtio-input driver's batch: from the events a device wrote to
//! the EVENTS messages the core reads.
//!
//! `libs/virtio-input`'s [`Batch`] is the driver's whole judgement about
//! where one message ends and the next begins, and the core's session is what
//! judges the result. A device chooses the events and when the glue takes
//! messages; the fuzzer plays both, and the session on the other side is a
//! real one, so a batch that builds a message the core refuses fails here
//! rather than in a boot.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **The core never refuses a message the batch made.** The driver pushes
//!    only events the device declared, so the one refusal left to the batch
//!    is `ReportTooLong`, which its cut at `REPORT_EVENTS` exists to prevent.
//! 2. **Every message fits**, at most `MAX_EVENTS` events and never empty.
//! 3. **Nothing is lost or reordered.** The events out are the events in, in
//!    order, less the `SYN_REPORT`s that closed nothing and plus exactly
//!    `Batch::split_reports` of the batch's own. The core delivers at most one
//!    report per `SYN_REPORT` it reads -- not always one, since it drops a
//!    report whose every event it filtered.
//! 4. **A message ends at a report boundary unless it is full.** A message
//!    shorter than `MAX_EVENTS` ends in a `SYN_REPORT`.
//! 5. **The batch never overruns.** `push` refuses rather than dropping, and
//!    a batch holding `MAX_EVENTS` or more always has a message to give, so
//!    taking messages always makes room.

#![no_main]

use ferrix_inputctl::message::{
    AXES, AxisRange, Bitmaps, DeviceId, Hello, MAX_EVENTS, Message, RawEvent, Text, VERSION,
};
use ferrix_inputctl::session::{Received, Session};
use ferrix_linux_abi::input::{
    ABS_X, BTN_LEFT, EV_ABS, EV_KEY, EV_REL, EV_SYN, KEY_A, REL_X, SYN_REPORT,
};
use ferrix_virtio_input::batch::{Batch, CAPACITY, PUSH_ROOM, REPORT_EVENTS};
use libfuzzer_sys::fuzz_target;

const SYN: RawEvent = RawEvent::new(EV_SYN, SYN_REPORT, 0);

fn set(bits: &mut [u8], index: u16) {
    bits[usize::from(index / 8)] |= 1 << (index % 8);
}

/// A device declaring what [`event`] sends, so the session's only quarrel
/// with a message can be its shape.
fn hello() -> Hello {
    let mut bits = Bitmaps::EMPTY;
    for kind in [EV_KEY, EV_REL, EV_ABS] {
        set(&mut bits.types, kind);
    }
    set(&mut bits.keys, KEY_A);
    set(&mut bits.keys, BTN_LEFT);
    set(&mut bits.rels, REL_X);
    set(&mut bits.abs, ABS_X);
    let mut axes = [AxisRange::default(); AXES];
    axes[usize::from(ABS_X)] = AxisRange {
        minimum: 0,
        maximum: 255,
        ..AxisRange::default()
    };
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

/// An event the device declared. `SYN_REPORT` is common, so reports are
/// mostly short, and every other choice is one the core publishes.
fn event(byte: u8) -> RawEvent {
    let value = i32::from(byte);
    match byte % 8 {
        0 | 1 | 2 => SYN,
        3 | 4 => RawEvent::new(EV_KEY, KEY_A, value & 1),
        5 => RawEvent::new(EV_KEY, BTN_LEFT, value & 1),
        6 => RawEvent::new(EV_REL, REL_X, value - 128),
        _ => RawEvent::new(EV_ABS, ABS_X, value),
    }
}

fuzz_target!(|bytes: &[u8]| {
    let described = hello();
    let mut session = Session::accept(&described, &Hello::HANDLE_RIGHTS).expect("a session");
    let mut batch = Batch::new();

    // Everything the device wrote, and everything that came back out.
    let mut pushed: Vec<RawEvent> = Vec::new();
    let mut out: Vec<RawEvent> = Vec::new();
    let mut delivered = 0usize;
    let mut now = 0u64;

    let take = |batch: &mut Batch,
                    session: &mut Session,
                    out: &mut Vec<RawEvent>,
                    delivered: &mut usize,
                    now: &mut u64| {
        while let Some(events) = batch.pop() {
            let carried = events.as_slice();
            // 2.
            assert!(
                !carried.is_empty() && carried.len() <= MAX_EVENTS,
                "a message of {} events",
                carried.len()
            );
            // 4.
            if carried.len() < MAX_EVENTS {
                assert!(
                    carried.last().is_some_and(RawEvent::is_report),
                    "a short message that does not end a report"
                );
            }
            out.extend_from_slice(carried);
            *now += 1;
            // 1.
            let received = session
                .receive(&Message::Events(events), *now, |report| {
                    assert!(!report.events().is_empty());
                    assert!(
                        report.events().last().is_some_and(RawEvent::is_report),
                        "a report the core delivered without its SYN_REPORT"
                    );
                    *delivered += 1;
                })
                .expect("the core never refuses a message the batch made");
            assert!(matches!(received, Received::Events { .. }));
        }
    };

    for &byte in bytes {
        // The high bit asks for the messages so far; a run without one fills
        // the batch until `push` refuses.
        if byte & 0x80 != 0 {
            take(
                &mut batch,
                &mut session,
                &mut out,
                &mut delivered,
                &mut now,
            );
            continue;
        }
        let event = event(byte);
        let room = batch.room();
        if batch.push(event) {
            pushed.push(event);
        } else {
            // 5. A push is refused only for want of room, and taking
            // messages always makes room again.
            assert!(room < PUSH_ROOM, "a push refused with {room} events free");
            assert!(batch.len() >= MAX_EVENTS);
            let message = batch.pop().expect("a full batch always has a message");
            out.extend_from_slice(message.as_slice());
            now += 1;
            let _ = session
                .receive(&Message::Events(message), now, |report| {
                    delivered += 1;
                    let _ = report;
                })
                .expect("the core never refuses a message the batch made");
            assert!(batch.room() >= PUSH_ROOM);
            assert!(batch.push(event), "room was made");
            pushed.push(event);
        }
        assert!(batch.len() <= CAPACITY);
        assert!(batch.open_report() <= REPORT_EVENTS);
    }

    // Close whatever is open, so everything the device wrote can come out,
    // and drain.
    let _ = batch.push(SYN);
    pushed.push(SYN);
    take(
        &mut batch,
        &mut session,
        &mut out,
        &mut delivered,
        &mut now,
    );
    assert!(batch.is_empty() || batch.open_report() != 0);

    // 3. Take the `SYN_REPORT`s out of both sides: what is left must match
    // event for event, and the reports must only differ by the cuts the
    // batch made and the empty ones it dropped.
    let (kept_in, syn_in): (Vec<RawEvent>, Vec<RawEvent>) =
        pushed.iter().partition(|event| !event.is_report());
    let (kept_out, syn_out): (Vec<RawEvent>, Vec<RawEvent>) =
        out.iter().partition(|event| !event.is_report());
    let waiting = batch.len();
    assert_eq!(
        kept_out.len() + waiting,
        kept_in.len(),
        "events were lost or invented"
    );
    assert_eq!(
        kept_out,
        kept_in[..kept_out.len()],
        "events came out in another order"
    );
    assert!(syn_out.len() <= syn_in.len() + usize::try_from(batch.split_reports()).unwrap_or(0));
    // At most one report per `SYN_REPORT`, and not always one: the core drops
    // a report whose every event it filtered, which an absolute axis that did
    // not move is.
    assert!(
        delivered <= syn_out.len(),
        "{delivered} reports from {} SYN_REPORTs",
        syn_out.len()
    );
});
