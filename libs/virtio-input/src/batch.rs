//! From the events the device wrote to the EVENTS messages the core reads.
//!
//! The core delivers a report only at its `SYN_REPORT` and accepts an EVENTS
//! that ends in the middle of one (`docs/INPUT.md` §3.2), so where a message
//! ends is the driver's choice. [`Batch`] makes it this way:
//!
//! * **A message ends at the last `SYN_REPORT` among its first
//!   [`MAX_EVENTS`] events.** A report is held until its `SYN_REPORT` arrives,
//!   so the core is not woken for half of one.
//! * **A report longer than a message is sent in whole messages**: once
//!   [`MAX_EVENTS`] events of one report are waiting, they go without their
//!   `SYN_REPORT`, and the next message continues the report.
//! * **No report is longer than the core holds.** The core refuses a report of
//!   [`MAX_REPORT`] events or more before its `SYN_REPORT`. When a report
//!   reaches one event short of that, the batch ends it with a `SYN_REPORT` of
//!   its own and starts a new report with the next event, counted in
//!   [`Batch::split_reports`]. QEMU's devices never do this: a report longer
//!   than its 64-entry event queue is one it can never deliver
//!   (`hw/input/virtio-input.c`, `virtio_input_send`). So only a device that
//!   breaks QEMU's shape meets the rule, and cutting its report keeps every
//!   event where dropping the rest would lose them.
//! * **A `SYN_REPORT` with nothing before it is dropped**, since the core
//!   delivers nothing for it (`inputctl::session`).
//!
//! The batch holds at most [`CAPACITY`] events. [`Batch::room`] says how many
//! more fit; the driver takes completions only while two do, one event and
//! the `SYN_REPORT` that may have to go before it, and leaves the rest in the
//! used ring until the glue has taken messages. A batch holding
//! [`MAX_EVENTS`] events or more always has a message to give, so taking
//! messages always makes room.

use ferrix_inputctl::message::{Events, MAX_EVENTS, RawEvent};
use ferrix_inputctl::session::MAX_REPORT;
use ferrix_linux_abi::input::{EV_SYN, SYN_REPORT};

/// Events a batch holds.
pub const CAPACITY: usize = 256;

/// Room [`Batch::push`] needs: the event and a `SYN_REPORT` before it.
pub const PUSH_ROOM: usize = 2;

/// The most events a report holds before its `SYN_REPORT`: one fewer than the
/// core refuses.
pub const REPORT_EVENTS: usize = MAX_REPORT - 1;

const SYN: RawEvent = RawEvent::new(EV_SYN, SYN_REPORT, 0);

/// Events on their way to the core, in order.
#[derive(Clone, Debug)]
pub struct Batch {
    ring: [RawEvent; CAPACITY],
    head: usize,
    len: usize,
    /// Events of the report being assembled, sent or not.
    open: usize,
    split_reports: u64,
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

impl Batch {
    /// An empty batch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ring: [SYN; CAPACITY],
            head: 0,
            len: 0,
            open: 0,
            split_reports: 0,
        }
    }

    /// Events waiting.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Events that still fit.
    #[must_use]
    pub const fn room(&self) -> usize {
        CAPACITY - self.len
    }

    /// Events of the report being assembled so far, including those already
    /// taken in a message.
    #[must_use]
    pub const fn open_report(&self) -> usize {
        self.open
    }

    /// Reports the batch ended itself because they reached
    /// [`REPORT_EVENTS`].
    #[must_use]
    pub const fn split_reports(&self) -> u64 {
        self.split_reports
    }

    fn append(&mut self, event: RawEvent) {
        if let Some(slot) = self.ring.get_mut((self.head + self.len) % CAPACITY) {
            *slot = event;
            self.len += 1;
        }
    }

    fn at(&self, index: usize) -> RawEvent {
        self.ring
            .get((self.head + index) % CAPACITY)
            .copied()
            .unwrap_or(SYN)
    }

    /// Add an event the driver accepted. `false`, with nothing added, when
    /// fewer than [`PUSH_ROOM`] events fit.
    pub fn push(&mut self, event: RawEvent) -> bool {
        if self.room() < PUSH_ROOM {
            return false;
        }
        if event.is_report() {
            if self.open != 0 {
                self.append(SYN);
                self.open = 0;
            }
            return true;
        }
        if self.open >= REPORT_EVENTS {
            self.append(SYN);
            self.open = 0;
            self.split_reports += 1;
        }
        self.append(event);
        self.open += 1;
        true
    }

    /// The next message, if one is ready: the waiting events up to the last
    /// `SYN_REPORT` among the first [`MAX_EVENTS`], or those [`MAX_EVENTS`]
    /// when none of them is one.
    pub fn pop(&mut self) -> Option<Events> {
        let window = self.len.min(MAX_EVENTS);
        let take = match (0..window).rev().find(|&index| self.at(index).is_report()) {
            Some(last) => last + 1,
            None if self.len >= MAX_EVENTS => MAX_EVENTS,
            None => return None,
        };
        let mut events = [SYN; MAX_EVENTS];
        for (index, slot) in events.iter_mut().take(take).enumerate() {
            *slot = self.at(index);
        }
        self.head = (self.head + take) % CAPACITY;
        self.len -= take;
        Events::new(events.get(..take)?)
    }

    /// Forget everything waiting and the report being assembled.
    pub const fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.open = 0;
    }
}
