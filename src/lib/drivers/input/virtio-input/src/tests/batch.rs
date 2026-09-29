//! [`Batch`]'s rules: where a message ends, what is held, what is dropped and
//! what is cut.
//!
//! Each test names the rule from `crate::batch`'s documentation it holds to.
//! The events are a keyboard's and a mouse's, so a report reads the way one
//! the core would publish does.

use std::vec::Vec;

use ferrix_inputctl::message::{MAX_EVENTS, RawEvent};
use ferrix_inputctl::session::MAX_REPORT;
use ferrix_linux_abi::input::{EV_KEY, EV_REL, EV_SYN, KEY_A, KEY_ESC, REL_X, REL_Y, SYN_REPORT};

use crate::batch::{Batch, CAPACITY, PUSH_ROOM, REPORT_EVENTS};

const SYN: RawEvent = RawEvent::new(EV_SYN, SYN_REPORT, 0);

fn key(code: u16, value: i32) -> RawEvent {
    RawEvent::new(EV_KEY, code, value)
}

fn motion(code: u16, value: i32) -> RawEvent {
    RawEvent::new(EV_REL, code, value)
}

/// Push every event, requiring the batch to have had room for it.
fn push_all(batch: &mut Batch, events: &[RawEvent]) {
    for (index, &event) in events.iter().enumerate() {
        assert!(batch.push(event), "event {index} did not fit");
    }
}

/// Every message the batch has ready, in order.
fn messages(batch: &mut Batch) -> Vec<Vec<RawEvent>> {
    let mut out = Vec::new();
    while let Some(events) = batch.pop() {
        out.push(events.as_slice().to_vec());
    }
    out
}

#[test]
fn a_new_batch_is_empty_and_holds_a_whole_capacity() {
    let mut batch = Batch::new();
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
    assert_eq!(batch.room(), CAPACITY);
    assert_eq!(batch.open_report(), 0);
    assert_eq!(batch.split_reports(), 0);
    assert_eq!(batch.pop(), None, "an empty batch has no message");
}

#[test]
fn a_report_is_held_until_its_syn_report() {
    let mut batch = Batch::new();
    push_all(&mut batch, &[motion(REL_X, 3), motion(REL_Y, -1)]);
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch.open_report(),
        2,
        "the report is still being assembled"
    );
    assert_eq!(
        batch.pop(),
        None,
        "the core is not woken for half of a report"
    );

    push_all(&mut batch, &[SYN]);
    assert_eq!(batch.open_report(), 0, "the SYN_REPORT closed it");
    let sent = messages(&mut batch);
    assert_eq!(sent, [[motion(REL_X, 3), motion(REL_Y, -1), SYN]]);
    assert!(batch.is_empty());
}

#[test]
fn a_message_ends_at_the_last_syn_report_it_can_carry() {
    let mut batch = Batch::new();
    // Three whole reports of two events each: nine events, well inside one
    // message, so all three go in one.
    for value in 0..3 {
        push_all(
            &mut batch,
            &[motion(REL_X, value), motion(REL_Y, value), SYN],
        );
    }
    let sent = messages(&mut batch);
    assert_eq!(sent.len(), 1, "one message carried all three reports");
    assert_eq!(sent[0].len(), 9);
    assert_eq!(sent[0][8], SYN);
}

#[test]
fn a_message_stops_at_the_last_whole_report_and_leaves_the_rest() {
    let mut batch = Batch::new();
    // 63 events: 31 reports of `motion` + `SYN`, then one event of a report
    // that has not ended. The message takes the 62 that end in a SYN_REPORT.
    for value in 0..31 {
        push_all(&mut batch, &[motion(REL_X, value), SYN]);
    }
    push_all(&mut batch, &[motion(REL_Y, 9)]);
    assert_eq!(batch.len(), 63);

    let events = batch.pop().expect("31 whole reports are ready");
    assert_eq!(events.as_slice().len(), 62);
    assert_eq!(events.as_slice()[61], SYN, "a message ends at a SYN_REPORT");
    assert_eq!(batch.len(), 1, "the unfinished report stayed");
    assert_eq!(batch.pop(), None);
}

#[test]
fn a_report_longer_than_a_message_goes_in_whole_messages_without_its_syn() {
    let mut batch = Batch::new();
    // One report of MAX_EVENTS + 4 events. The first message is the first
    // MAX_EVENTS of it, with no SYN_REPORT; the rest waits for one.
    let long: Vec<RawEvent> = (0..MAX_EVENTS + 4)
        .map(|index| motion(REL_X, index as i32))
        .collect();
    push_all(&mut batch, &long);

    let first = batch.pop().expect("a full message of an open report");
    assert_eq!(first.as_slice().len(), MAX_EVENTS);
    assert!(
        !first.as_slice().iter().any(RawEvent::is_report),
        "the report has not ended, so no SYN_REPORT went out"
    );
    assert_eq!(first.as_slice()[0], motion(REL_X, 0));
    assert_eq!(batch.pop(), None, "the remaining four wait for the SYN");

    push_all(&mut batch, &[SYN]);
    let rest = batch.pop().expect("the SYN_REPORT finished the report");
    assert_eq!(rest.as_slice().len(), 5);
    assert_eq!(rest.as_slice()[0], motion(REL_X, MAX_EVENTS as i32));
    assert_eq!(rest.as_slice()[4], SYN);
    assert!(batch.is_empty());
}

#[test]
fn a_syn_report_with_nothing_before_it_is_dropped() {
    let mut batch = Batch::new();
    push_all(&mut batch, &[SYN, SYN, SYN]);
    assert!(
        batch.is_empty(),
        "the core delivers nothing for an empty report"
    );
    assert_eq!(batch.pop(), None);

    // And after a report has been closed, a second SYN_REPORT goes too.
    push_all(&mut batch, &[key(KEY_A, 1), SYN, SYN]);
    assert_eq!(batch.len(), 2);
    assert_eq!(messages(&mut batch), [[key(KEY_A, 1), SYN]]);
}

#[test]
fn a_report_reaching_the_core_s_limit_is_cut_and_counted() {
    let mut batch = Batch::new();
    // Fill a report to one event short of what the core refuses, taking
    // messages as they come so the ring never fills: `open_report` counts
    // events already handed out.
    let mut sent = Vec::new();
    for index in 0..REPORT_EVENTS {
        assert!(batch.push(motion(REL_X, index as i32)));
        sent.append(&mut messages(&mut batch));
    }
    assert_eq!(batch.open_report(), REPORT_EVENTS);
    assert_eq!(batch.split_reports(), 0, "nothing has been cut yet");

    // The next event of the same report cuts it: a SYN_REPORT of the batch's
    // own goes first, and the event starts a new report.
    assert!(batch.push(motion(REL_Y, 7)));
    assert_eq!(batch.split_reports(), 1);
    assert_eq!(batch.open_report(), 1, "the event began the next report");
    push_all(&mut batch, &[SYN]);
    sent.append(&mut messages(&mut batch));

    let all: Vec<RawEvent> = sent.concat();
    let ends: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, event)| event.is_report())
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        ends,
        [REPORT_EVENTS, REPORT_EVENTS + 2],
        "the cut SYN_REPORT sits after {REPORT_EVENTS} events, the real one after the next"
    );
    assert_eq!(
        REPORT_EVENTS + 1,
        MAX_REPORT,
        "a cut report is one event short of what the core refuses"
    );
    assert_eq!(all.len(), REPORT_EVENTS + 3);
}

#[test]
fn a_push_is_refused_rather_than_overrunning_the_ring() {
    let mut batch = Batch::new();
    // Every event but the SYN_REPORT reserves room for one, so the batch
    // fills at CAPACITY - 1 events without a report ever being taken.
    for index in 0..CAPACITY - PUSH_ROOM + 1 {
        assert!(batch.push(key(KEY_A, index as i32)), "event {index} fits");
    }
    assert_eq!(batch.len(), CAPACITY - PUSH_ROOM + 1);
    assert_eq!(batch.room(), PUSH_ROOM - 1);
    assert!(
        !batch.push(key(KEY_ESC, 1)),
        "an event with no room for its SYN_REPORT is refused"
    );
    assert_eq!(batch.len(), CAPACITY - PUSH_ROOM + 1, "nothing was added");

    // Taking a message always makes room, since the batch holds more than
    // MAX_EVENTS events.
    let events = batch.pop().expect("a full batch always has a message");
    assert_eq!(events.as_slice().len(), MAX_EVENTS);
    assert!(batch.room() >= PUSH_ROOM);
    assert!(batch.push(key(KEY_ESC, 1)));
}

#[test]
fn the_ring_wraps_and_keeps_the_order() {
    let mut batch = Batch::new();
    // Three times the capacity through a batch that never holds more than one
    // report, so `head` wraps twice and the order must still be the device's.
    let mut seen = Vec::new();
    for value in 0..(CAPACITY * 3) as i32 {
        push_all(&mut batch, &[motion(REL_X, value), SYN]);
        for message in messages(&mut batch) {
            seen.extend(message);
        }
    }
    let values: Vec<i32> = seen
        .iter()
        .filter(|event| !event.is_report())
        .map(|event| event.value)
        .collect();
    assert_eq!(values, (0..(CAPACITY * 3) as i32).collect::<Vec<_>>());
    assert_eq!(seen.len(), CAPACITY * 6, "each report kept its SYN_REPORT");
}

#[test]
fn clear_forgets_the_waiting_events_and_the_open_report() {
    let mut batch = Batch::new();
    push_all(&mut batch, &[key(KEY_A, 1), motion(REL_X, 2)]);
    batch.clear();
    assert!(batch.is_empty());
    assert_eq!(batch.room(), CAPACITY);
    assert_eq!(batch.open_report(), 0);
    assert_eq!(batch.pop(), None);

    // A SYN_REPORT after a clear is the empty one, and is dropped.
    push_all(&mut batch, &[SYN]);
    assert!(batch.is_empty());
}

#[test]
fn the_split_count_survives_a_clear_because_it_is_a_tally() {
    let mut batch = Batch::new();
    for index in 0..REPORT_EVENTS {
        assert!(batch.push(motion(REL_X, index as i32)));
        let _ = messages(&mut batch);
    }
    assert!(batch.push(motion(REL_Y, 1)));
    assert_eq!(batch.split_reports(), 1);
    batch.clear();
    assert_eq!(
        batch.split_reports(),
        1,
        "the driver reports what it cut over its life, not since the last drop"
    );
}
