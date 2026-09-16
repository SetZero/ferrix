//! One open file's queue of events: Linux evdev's client buffer.
//!
//! Every rule here is `drivers/input/evdev.c`'s, read at 3abd29c61d2e, and
//! each function names the one it follows. The queue is a ring of a power of
//! two [`Stamped`] events, in storage the glue gives it, with three indices:
//! `head`, where the next event goes; `tail`, the next event to read; and
//! `packet_head`, the end of the last whole report. A reader sees only
//! `tail..packet_head`, so a report becomes readable at its `SYN_REPORT` and
//! not before.
//!
//! # A full queue
//!
//! `__pass_event`: when an event fills the ring, every unread event is
//! dropped. The queue is left holding a `SYN_DROPPED`, stamped with the
//! event's time, and the event itself, and `packet_head` is put back at the
//! `SYN_DROPPED`, so nothing is readable until the next `SYN_REPORT`. The
//! reader then reads `SYN_DROPPED`, the rest of the report that overflowed,
//! and its `SYN_REPORT`, and by evdev's contract discards everything up to and
//! including that `SYN_REPORT` and re-reads the state with `EVIOCGKEY` and
//! the others. `docs/INPUT.md` §3.1 words this as emptying the queue and
//! queuing a `SYN_DROPPED`; this is that, event by event, as Linux does it.
//!
//! # Reading the state flushes the queue
//!
//! `evdev_handle_get_val`, behind `EVIOCGKEY`, `EVIOCGLED`, `EVIOCGSND` and
//! `EVIOCGSW`, removes the queued events of that type
//! (`__evdev_flush_queue`, here [`Queue::flush_type`]) so a reader that just
//! read the state does not apply them again, and drops any report that is
//! left empty. If copying the state out then fails, it queues a
//! `SYN_DROPPED` ([`Queue::queue_syn_dropped`]).
//!
//! # Clocks
//!
//! Events are stored with the monotonic time the report was stamped with, and
//! a read converts them to the open's clock with the offsets the glue passes
//! ([`Clocks`]), as `docs/INPUT.md` §3.1 has it. Linux stamps each event in
//! the client's clock as it queues it instead; the two differ only when the
//! realtime clock is set between a report and its read. Changing the clock
//! (`evdev_set_clk_type`) empties a non-empty queue and queues a
//! `SYN_DROPPED`.
//!
//! # Reading
//!
//! [`Queue::read`] is `evdev_read` without the wait: a buffer shorter than one
//! `input_event` is `EINVAL` unless it is empty; a gone device or a revoked
//! open is `ENODEV` even with events queued, as Linux checks that first
//! (`docs/INPUT.md` §3.3 would drain the queue first; Linux does not); an
//! empty queue is `EAGAIN` under `O_NONBLOCK`; a zero-length read then
//! returns 0; and otherwise as many whole events as fit are copied, which may
//! split a report across reads. When nothing is readable and the open blocks,
//! [`ReadError::Empty`] tells the glue to wait until
//! [`Queue::poll`] changes, then read again.

use ferrix_linux_abi::input::{EV_SYN, Event, SYN_DROPPED};
use ferrix_linux_abi::socket::Width;

use crate::message::RawEvent;
use crate::session::Report;

/// `EVDEV_MIN_BUFFER_SIZE`: the fewest events a queue holds.
pub const MIN_BUFFER: usize = 64;
/// `EVDEV_BUF_PACKETS`: how many estimated packets a queue holds.
pub const BUFFER_PACKETS: usize = 8;
/// The smallest ring [`Queue::new`] takes: the drop rule keeps two events.
pub const MIN_SLOTS: usize = 4;

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_MICRO: u64 = 1_000;

/// The clock an open's timestamps are read in: `EVIOCSCLOCKID`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Clock {
    /// `CLOCK_REALTIME`, every open's clock until it asks for another.
    #[default]
    Realtime,
    /// `CLOCK_MONOTONIC`.
    Monotonic,
    /// `CLOCK_BOOTTIME`.
    Boottime,
}

impl Clock {
    /// The clock a `CLOCK_*` id names, if evdev takes it; `EINVAL` otherwise.
    #[must_use]
    pub const fn from_id(id: i32) -> Option<Self> {
        use ferrix_linux_abi::input::{CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_REALTIME};
        Some(match id {
            CLOCK_REALTIME => Self::Realtime,
            CLOCK_MONOTONIC => Self::Monotonic,
            CLOCK_BOOTTIME => Self::Boottime,
            _ => return None,
        })
    }
}

/// How the other clocks stood against the monotonic one when a read began,
/// in nanoseconds added to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Clocks {
    /// `CLOCK_REALTIME` minus `CLOCK_MONOTONIC`.
    pub realtime_offset: i64,
    /// `CLOCK_BOOTTIME` minus `CLOCK_MONOTONIC`.
    pub boottime_offset: i64,
}

impl Clocks {
    /// A monotonic time in `clock`, clamped to what a `u64` of nanoseconds
    /// holds.
    #[must_use]
    pub fn convert(&self, clock: Clock, monotonic: u64) -> u64 {
        let offset = match clock {
            Clock::Realtime => self.realtime_offset,
            Clock::Monotonic => 0,
            Clock::Boottime => self.boottime_offset,
        };
        let time = i128::from(monotonic) + i128::from(offset);
        u64::try_from(time.max(0)).unwrap_or(u64::MAX)
    }
}

/// An event with the monotonic time, in nanoseconds, it was stamped with.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stamped {
    /// Monotonic nanoseconds.
    pub time: u64,
    /// The event.
    pub event: RawEvent,
}

impl Stamped {
    /// `input_event` at `width`, in `clock`, into the start of `out`.
    ///
    /// On a 32-bit kernel the seconds are an `unsigned long`, so they are cut
    /// to 32 bits as Linux's assignment cuts them.
    pub fn write(&self, clock: Clock, clocks: &Clocks, width: Width, out: &mut [u8]) -> Option<()> {
        let time = clocks.convert(clock, self.time);
        let mut sec = time / NANOS_PER_SECOND;
        if matches!(width, Width::Bits32) {
            sec &= u64::from(u32::MAX);
        }
        Event {
            sec,
            usec: time % NANOS_PER_SECOND / NANOS_PER_MICRO,
            r#type: self.event.kind,
            code: self.event.code,
            value: self.event.value,
        }
        .write(width, out)
    }
}

/// The ring a queue was given is not a power of two of at least
/// [`MIN_SLOTS`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SizeError;

/// Why a read returned no events.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadError {
    /// `EINVAL`: a buffer shorter than one event, and not empty.
    TooSmall,
    /// `ENODEV`: the device is gone or the open was revoked.
    Gone,
    /// `EAGAIN`: nothing readable, and the open does not block.
    WouldBlock,
    /// Nothing readable: a blocking open waits, then reads again.
    Empty,
}

/// How a read is made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReadFlags {
    /// The caller's width.
    pub width: Width,
    /// `O_NONBLOCK`.
    pub nonblocking: bool,
    /// Whether the device is gone.
    pub gone: bool,
}

/// What `poll` reports: `evdev_poll`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Poll {
    /// A whole report is readable: `POLLIN | POLLRDNORM`.
    pub readable: bool,
    /// The device is gone or the open revoked: `POLLHUP | POLLERR`. Linux
    /// reports `POLLOUT | POLLWRNORM` otherwise, which is the glue's to add.
    pub hangup: bool,
}

/// One open's queue.
#[derive(Clone, Debug)]
pub struct Queue<S> {
    slots: S,
    head: usize,
    tail: usize,
    packet_head: usize,
    clock: Clock,
    revoked: bool,
}

impl<S: AsRef<[Stamped]> + AsMut<[Stamped]>> Queue<S> {
    /// A queue over `slots`, which must be a power of two of at least
    /// [`MIN_SLOTS`]. [`crate::session::Capabilities::queue_size`] is the
    /// size Linux gives a device's queues.
    pub fn new(slots: S) -> Result<Self, SizeError> {
        let len = slots.as_ref().len();
        if len < MIN_SLOTS || !len.is_power_of_two() {
            return Err(SizeError);
        }
        Ok(Self {
            slots,
            head: 0,
            tail: 0,
            packet_head: 0,
            clock: Clock::Realtime,
            revoked: false,
        })
    }

    fn mask(&self) -> usize {
        self.slots.as_ref().len() - 1
    }

    fn put(&mut self, at: usize, stamped: Stamped) {
        if let Some(slot) = self.slots.as_mut().get_mut(at) {
            *slot = stamped;
        }
    }

    fn get(&self, at: usize) -> Stamped {
        self.slots.as_ref().get(at).copied().unwrap_or_default()
    }

    /// The open's clock.
    #[must_use]
    pub const fn clock(&self) -> Clock {
        self.clock
    }

    /// Whether the open was revoked.
    #[must_use]
    pub const fn is_revoked(&self) -> bool {
        self.revoked
    }

    /// Events stored, readable or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.head.wrapping_sub(self.tail) & self.mask()
    }

    /// Whether nothing is stored.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// Whether a whole report is readable.
    #[must_use]
    pub const fn has_packet(&self) -> bool {
        self.packet_head != self.tail
    }

    /// `evdev_poll`, for a device that is `gone` or not.
    #[must_use]
    pub const fn poll(&self, gone: bool) -> Poll {
        Poll {
            readable: self.has_packet(),
            hangup: gone || self.revoked,
        }
    }

    /// `evdev_pass_values`: queue a report this open receives. A revoked open
    /// receives nothing, and a `SYN_REPORT` with nothing queued since the last
    /// one is dropped.
    pub fn deliver(&mut self, report: &Report) {
        if self.revoked {
            return;
        }
        for &event in report.events() {
            if event.is_report() && self.packet_head == self.head {
                continue;
            }
            self.pass(Stamped {
                time: report.time(),
                event,
            });
        }
    }

    /// `__pass_event`.
    fn pass(&mut self, stamped: Stamped) {
        let mask = self.mask();
        self.put(self.head, stamped);
        self.head = (self.head + 1) & mask;
        if self.head == self.tail {
            self.tail = self.head.wrapping_sub(2) & mask;
            self.put(self.tail, syn_dropped(stamped.time));
            self.packet_head = self.tail;
        }
        if stamped.event.is_report() {
            self.packet_head = self.head;
        }
    }

    /// `__evdev_queue_syn_dropped`: queue a `SYN_DROPPED` stamped `now`. If
    /// that fills the ring, only it is kept.
    pub fn queue_syn_dropped(&mut self, now: u64) {
        let mask = self.mask();
        self.put(self.head, syn_dropped(now));
        self.head = (self.head + 1) & mask;
        if self.head == self.tail {
            self.tail = self.head.wrapping_sub(1) & mask;
            self.packet_head = self.tail;
        }
    }

    /// `evdev_set_clk_type`: read timestamps in `clock` from now on. Changing
    /// it empties a non-empty queue and queues a `SYN_DROPPED` stamped `now`.
    pub fn set_clock(&mut self, clock: Clock, now: u64) {
        if self.clock == clock {
            return;
        }
        self.clock = clock;
        if self.head != self.tail {
            self.head = self.tail;
            self.packet_head = self.tail;
            self.queue_syn_dropped(now);
        }
    }

    /// `__evdev_flush_queue`: remove the queued events of type `kind`, and
    /// the reports left with nothing but their `SYN_REPORT`. `EV_SYN` is not
    /// a type Linux flushes (it is a `BUG_ON` there); here it does nothing.
    pub fn flush_type(&mut self, kind: u16) {
        if kind == EV_SYN {
            return;
        }
        let mask = self.mask();
        let mut head = self.tail;
        self.packet_head = self.tail;
        // Starts at 1 so a leading SYN_REPORT is kept.
        let mut kept = 1usize;
        let mut index = self.tail;
        while index != self.head {
            let stamped = self.get(index);
            index = (index + 1) & mask;
            let is_report = stamped.event.is_report();
            if stamped.event.kind == kind || (is_report && kept == 0) {
                continue;
            }
            self.put(head, stamped);
            kept += 1;
            head = (head + 1) & mask;
            if is_report {
                kept = 0;
                self.packet_head = head;
            }
        }
        self.head = head;
    }

    /// `EVIOCREVOKE`: the open reads `ENODEV`, polls hung up and receives
    /// nothing from now on. The glue also releases its grab with
    /// [`crate::session::Session::release`].
    pub const fn revoke(&mut self) {
        self.revoked = true;
    }

    /// `evdev_read` into `out`, whose length is the read's `count`, without
    /// waiting: the module's "Reading" section.
    pub fn read(
        &mut self,
        out: &mut [u8],
        flags: ReadFlags,
        clocks: &Clocks,
    ) -> Result<usize, ReadError> {
        let size = Event::size(flags.width);
        if !out.is_empty() && out.len() < size {
            return Err(ReadError::TooSmall);
        }
        if flags.gone || self.revoked {
            return Err(ReadError::Gone);
        }
        if !self.has_packet() && flags.nonblocking {
            return Err(ReadError::WouldBlock);
        }
        if out.is_empty() {
            return Ok(0);
        }
        let mask = self.mask();
        let mut read = 0;
        while read + size <= out.len() && self.has_packet() {
            let stamped = self.get(self.tail);
            let slot = out.get_mut(read..).ok_or(ReadError::TooSmall)?;
            if stamped
                .write(self.clock, clocks, flags.width, slot)
                .is_none()
            {
                break;
            }
            self.tail = (self.tail + 1) & mask;
            read += size;
        }
        if read == 0 {
            return Err(ReadError::Empty);
        }
        Ok(read)
    }
}

const fn syn_dropped(time: u64) -> Stamped {
    Stamped {
        time,
        event: RawEvent::new(EV_SYN, SYN_DROPPED, 0),
    }
}
