//! The input core's half of one device's conversation, as a state machine,
//! with the device's state and the reports it assembles.
//!
//! A [`Session`] starts from an accepted HELLO. From then on the driver sends
//! EVENTS until the core asks it to [`Session::stop`], and every message from
//! it goes through [`Session::receive`], which accepts only an EVENTS while
//! the device runs and a STOPPED after STOP. Anything else is
//! [`Refusal::Protocol`], after which the session is broken and refuses
//! everything, and the glue quiesces the driver the way it quiesces a block
//! driver that lies.
//!
//! # What the core publishes
//!
//! HELLO holds the device's own declaration. [`Capabilities`] is what the
//! core publishes of it (`docs/INPUT.md` §3.2): the event types in
//! [`SUPPORTED_TYPES`] and no other, with `EV_SYN` always, as
//! `input_register_device` sets it (`drivers/input/input.c`); no
//! `KEY_RESERVED`, which the same function clears; and no multi-touch axis,
//! `ABS_MT_SLOT` and every axis after it. [`Capabilities::left_out`] says what
//! was dropped, for the boot line.
//!
//! # What an EVENTS must hold
//!
//! Every event is checked against the HELLO as the driver sent it before any
//! event of the message has an effect, so a message that is refused changes
//! nothing. An `EV_SYN` event must be `SYN_REPORT`: `SYN_MT_REPORT` belongs to
//! multi-touch, which is left out, and neither `SYN_CONFIG` nor `SYN_DROPPED`
//! is a device's to send. An `EV_REP` event must name `REP_DELAY` or
//! `REP_PERIOD` of a device that declared `EV_REP`, since there is no code
//! bitmap for it. Any other event must be of a type and code HELLO declared.
//! And a report may hold at most [`MAX_REPORT`] events with its `SYN_REPORT`.
//! Anything else is refused: the driver drops what the device did not
//! declare (`docs/INPUT.md` §3.2), so one reaching the core means the driver
//! lied.
//!
//! # What an event does
//!
//! An accepted event goes through Linux's `input_get_disposition`
//! (`drivers/input/input.c`, at 3a2c4d55e32a): an event of a type or code the
//! core does not publish is ignored; a key, switch or LED whose value does
//! not change its state is ignored, and one that does flips it; a key's value
//! 2, a repeat the device made, passes without touching the state; an
//! absolute axis is defuzzed against its last value by
//! `input_defuzz_abs_event` and ignored if that leaves it unchanged; a
//! relative motion of 0 is ignored; `EV_MSC` passes; `EV_REP` stores a
//! non-negative value that differs. So the state changes as each event
//! arrives, as Linux's does, and not only at the end of its report as
//! `docs/INPUT.md` §3.1 words it: whether an event passes depends on the state
//! the events before it left.
//!
//! The events that pass are kept aside until `SYN_REPORT`. A report with no
//! event before its `SYN_REPORT` is not delivered, as `input_event_dispose`
//! passes nothing under two values. Otherwise the report is stamped with the
//! monotonic time the glue gave for the message, and handed to the glue's
//! `deliver` with its recipient: the grabbing open alone, or every open.
//!
//! # Grabs
//!
//! As `evdev_grab` and `evdev_ungrab` in `drivers/input/evdev.c` (at
//! 3abd29c61d2e): [`Session::grab`] fails with [`GrabError::Busy`] while any
//! open holds the grab, the grabbing open itself included;
//! [`Session::ungrab`] fails with [`GrabError::NotHolder`] for an open that
//! does not hold it; and closing or revoking an open releases its grab,
//! [`Session::release`]. Which open receives a report is decided when the
//! report is delivered.
//!
//! Capacity is fixed: the session allocates nothing per message.

use ferrix_linux_abi::input::{
    ABS_CNT, ABS_MT_SLOT, AbsInfo, EV_ABS, EV_KEY, EV_LED, EV_MSC, EV_REL, EV_REP, EV_SW, EV_SYN,
    KEY_RESERVED, REP_CNT, REP_DELAY, REP_MAX, REP_PERIOD, SYN_REPORT,
};
use ferrix_linux_abi::socket::Width;
use ferrix_native_abi::rights::Rights;

use crate::message::{
    AXES, AxisRange, Bitmaps, DeviceId, Events, Hello, KEY_BYTES, LED_BYTES, Message, RawEvent,
    Refusal, SUPPORTED_TYPES, SW_BYTES, TYPE_BYTES, Text, bit,
};

/// The most events a report holds, its `SYN_REPORT` included:
/// `docs/INPUT.md` §3.2.
pub const MAX_REPORT: usize = 256;

/// The delay and period, in milliseconds, a device starts with:
/// `input_register_device` calls `input_enable_softrepeat(dev, 250, 33)`
/// for a device whose driver set neither, which Linux's virtio-input driver
/// does not.
pub const DEFAULT_REPEAT: [i32; REP_CNT as usize] = [250, 33];

/// One open file of the device's node, named by the glue. The session only
/// compares them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct OpenId(pub u64);

fn set_bit(bits: &mut [u8], index: u16, value: bool) {
    if let Some(byte) = bits.get_mut(usize::from(index / 8)) {
        let mask = 1 << (index % 8);
        if value {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
}

/// What the core leaves out of a device's declaration.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LeftOut {
    /// The event types declared but not published, as a type bitmap.
    pub types: [u8; TYPE_BYTES],
    /// Whether any multi-touch axis was declared.
    pub mt_axes: bool,
}

impl LeftOut {
    /// Whether nothing was left out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.mt_axes && self.types.iter().all(|&byte| byte == 0)
    }
}

/// What the core publishes of a device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
    /// The published bitmaps.
    pub bits: Bitmaps,
    /// Each published axis's range; zero for the others.
    pub axes: [AxisRange; AXES],
    /// What the device declared and the core does not publish.
    pub left_out: LeftOut,
}

impl Capabilities {
    /// What the core publishes of `hello`'s declaration.
    #[must_use]
    pub fn from_hello(hello: &Hello) -> Self {
        let mut bits = hello.bits;
        let mut left_out = LeftOut::default();
        for (byte, declared) in left_out.types.iter_mut().zip(hello.bits.types) {
            *byte = declared;
        }
        bits.types = [0; TYPE_BYTES];
        for kind in SUPPORTED_TYPES {
            set_bit(&mut left_out.types, kind, false);
            set_bit(
                &mut bits.types,
                kind,
                kind == EV_SYN || hello.bits.has_type(kind),
            );
        }
        set_bit(&mut bits.keys, KEY_RESERVED, false);
        let mut axes = hello.axes;
        for (axis, range) in (0..ABS_CNT).zip(axes.iter_mut()) {
            if axis >= ABS_MT_SLOT {
                left_out.mt_axes |= bit(&bits.abs, axis);
                set_bit(&mut bits.abs, axis, false);
                *range = AxisRange::default();
            }
        }
        Self {
            bits,
            axes,
            left_out,
        }
    }

    /// The per-open queue size Linux gives this device:
    /// `evdev_compute_buffer_size` over `input_estimate_events_per_packet`.
    /// With no multi-touch axis published, the estimate is one `SYN_REPORT`,
    /// one event per absolute and per relative axis, and seven for keys and
    /// `EV_MSC`; the size is eight such packets, at least 64 events, rounded
    /// up to a power of two.
    #[must_use]
    pub fn queue_size(&self) -> usize {
        let count = |kind: u16, bits: &[u8]| {
            if self.bits.has_type(kind) {
                bits.iter().map(|byte| byte.count_ones() as usize).sum()
            } else {
                0
            }
        };
        let per_packet = 1 + count(EV_ABS, &self.bits.abs) + count(EV_REL, &self.bits.rels) + 7;
        (per_packet * crate::queue::BUFFER_PACKETS)
            .max(crate::queue::MIN_BUFFER)
            .next_power_of_two()
    }
}

/// The device's state: what `EVIOCGKEY`, `EVIOCGLED`, `EVIOCGSW`,
/// `EVIOCGABS` and `EVIOCGREP` read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
    /// Keys held down.
    pub keys: [u8; KEY_BYTES],
    /// LEDs lit.
    pub leds: [u8; LED_BYTES],
    /// Switches on.
    pub sw: [u8; SW_BYTES],
    /// Each axis's last value, 0 until it reports one.
    pub abs: [i32; AXES],
    /// The repeat delay and period.
    pub repeat: [i32; REP_CNT as usize],
}

impl Default for State {
    fn default() -> Self {
        Self {
            keys: [0; KEY_BYTES],
            leds: [0; LED_BYTES],
            sw: [0; SW_BYTES],
            abs: [0; AXES],
            repeat: DEFAULT_REPEAT,
        }
    }
}

/// A finished report: the events that passed, its `SYN_REPORT` last, one
/// monotonic time, and who may read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Report {
    time: u64,
    only: Option<OpenId>,
    len: usize,
    events: [RawEvent; MAX_REPORT],
}

impl Report {
    const EMPTY: Self = Self {
        time: 0,
        only: None,
        len: 0,
        events: [RawEvent::new(0, 0, 0); MAX_REPORT],
    };

    /// A report of `events`, stamped `time`, for `recipient` or every open:
    /// what a session delivers, for the glue's and the fuzzer's tests. `None`
    /// for more than [`MAX_REPORT`] events.
    #[must_use]
    pub fn new(time: u64, recipient: Option<OpenId>, events: &[RawEvent]) -> Option<Self> {
        let mut report = Self::EMPTY;
        report
            .events
            .get_mut(..events.len())?
            .copy_from_slice(events);
        report.len = events.len();
        report.time = time;
        report.only = recipient;
        Some(report)
    }

    /// The events, ending in `SYN_REPORT`.
    #[must_use]
    pub fn events(&self) -> &[RawEvent] {
        self.events.get(..self.len).unwrap_or(&[])
    }

    /// The monotonic time, in nanoseconds, the report was stamped with.
    #[must_use]
    pub const fn time(&self) -> u64 {
        self.time
    }

    /// The open holding the grab when the report was delivered, which alone
    /// receives it; `None` when every open does.
    #[must_use]
    pub const fn recipient(&self) -> Option<OpenId> {
        self.only
    }

    /// Whether `open`'s queue receives the report.
    #[must_use]
    pub fn is_for(&self, open: OpenId) -> bool {
        self.only.is_none_or(|only| only == open)
    }

    fn push(&mut self, event: RawEvent) {
        if let Some(slot) = self.events.get_mut(self.len) {
            *slot = event;
            self.len += 1;
        }
    }
}

/// A request the core may not make now. The conversation is unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestError {
    /// The session is broken, stopping or stopped.
    Closed,
}

/// Why a grab or its release is refused: `EBUSY` and `EINVAL` in
/// `evdev_grab` and `evdev_ungrab`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrabError {
    /// An open, perhaps this one, already holds the grab.
    Busy,
    /// This open does not hold the grab.
    NotHolder,
}

/// What a driver's message meant, once accepted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Received {
    /// An EVENTS, of which this many reports were delivered.
    Events {
        /// Reports handed to `deliver`.
        reports: usize,
    },
    /// The driver stopped.
    Stopped,
}

/// The core's side of one input driver's conversation.
#[derive(Clone, Debug)]
pub struct Session {
    id: DeviceId,
    name: Text,
    serial: Text,
    declared: Bitmaps,
    caps: Capabilities,
    state: State,
    report: Report,
    /// Events the driver sent since the last `SYN_REPORT`, passed or not.
    sent: usize,
    grab: Option<OpenId>,
    stopping: bool,
    stopped: bool,
    broken: bool,
}

impl Session {
    /// Accept a driver's HELLO, with the rights of the handles it came with.
    pub fn accept(hello: &Hello, handle_rights: &[Rights]) -> Result<Self, Refusal> {
        hello.validate(handle_rights)?;
        Ok(Self {
            id: hello.id,
            name: hello.name,
            serial: hello.serial,
            declared: hello.bits,
            caps: Capabilities::from_hello(hello),
            state: State::default(),
            report: Report::EMPTY,
            sent: 0,
            grab: None,
            stopping: false,
            stopped: false,
            broken: false,
        })
    }

    /// The device's ids, for `EVIOCGID`.
    #[must_use]
    pub const fn id(&self) -> DeviceId {
        self.id
    }

    /// The device's name, for `EVIOCGNAME`.
    #[must_use]
    pub const fn name(&self) -> &Text {
        &self.name
    }

    /// The device's serial, for `EVIOCGUNIQ`.
    #[must_use]
    pub const fn serial(&self) -> &Text {
        &self.serial
    }

    /// What the core publishes.
    #[must_use]
    pub const fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// The device's state.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// `EVIOCGABS(axis)`: the axis's range with its current value, or `None`
    /// past `ABS_MAX`. An axis the core does not publish answers zeros, as
    /// `evdev_do_ioctl` copies `dev->absinfo[t]` for any axis of a device
    /// with absolute axes; `docs/INPUT.md` §3.3 answers `EINVAL` for it, which
    /// the glue decides with [`Bitmaps::has_code`] on [`Self::capabilities`].
    #[must_use]
    pub fn abs_info(&self, axis: u16) -> Option<AbsInfo> {
        let index = usize::from(axis);
        let range = self.caps.axes.get(index)?;
        Some(AbsInfo {
            value: *self.state.abs.get(index)?,
            minimum: range.minimum,
            maximum: range.maximum,
            fuzz: range.fuzz,
            flat: range.flat,
            resolution: range.resolution,
        })
    }

    /// `EVIOCSREP`: store a new delay and period. Linux injects them as
    /// `EV_REP` events every open then reads; `docs/INPUT.md` §3.1 has the
    /// core store them, and so does this.
    pub const fn set_repeat(&mut self, delay: i32, period: i32) {
        self.state.repeat = [delay, period];
    }

    /// Whether the driver broke the protocol.
    #[must_use]
    pub const fn is_broken(&self) -> bool {
        self.broken
    }

    /// Whether the driver has stopped.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// STOP the driver. Every request after this is refused. EVENTS the driver
    /// sent before it read STOP are still accepted until its STOPPED, which
    /// discards a report still being assembled.
    pub fn stop(&mut self) -> Result<Message, RequestError> {
        if self.broken || self.stopping {
            return Err(RequestError::Closed);
        }
        self.stopping = true;
        Ok(Message::Stop)
    }

    /// `EVIOCGRAB(1)` from `open`.
    pub fn grab(&mut self, open: OpenId) -> Result<(), GrabError> {
        if self.grab.is_some() {
            return Err(GrabError::Busy);
        }
        self.grab = Some(open);
        Ok(())
    }

    /// `EVIOCGRAB(0)` from `open`.
    pub fn ungrab(&mut self, open: OpenId) -> Result<(), GrabError> {
        if self.grab != Some(open) {
            return Err(GrabError::NotHolder);
        }
        self.grab = None;
        Ok(())
    }

    /// `open` is closed or revoked: release its grab if it holds one.
    pub fn release(&mut self, open: OpenId) {
        if self.grab == Some(open) {
            self.grab = None;
        }
    }

    /// The open holding the grab.
    #[must_use]
    pub const fn grabbed(&self) -> Option<OpenId> {
        self.grab
    }

    fn violation(&mut self, refusal: Refusal) -> Refusal {
        self.broken = true;
        self.report.len = 0;
        refusal
    }

    /// Accept a message from the driver, if it is one the session waits for.
    ///
    /// For an EVENTS, `now` is the monotonic time in nanoseconds every report
    /// the message finishes is stamped with, and `deliver` is called with each
    /// in order.
    pub fn receive(
        &mut self,
        message: &Message,
        now: u64,
        deliver: impl FnMut(&Report),
    ) -> Result<Received, Refusal> {
        if self.broken {
            return Err(Refusal::Protocol);
        }
        match message {
            Message::Events(events) if !self.stopped => self.on_events(events, now, deliver),
            Message::Stopped if self.stopping && !self.stopped => {
                self.stopped = true;
                self.report.len = 0;
                Ok(Received::Stopped)
            }
            _ => Err(self.violation(Refusal::Protocol)),
        }
    }

    fn on_events(
        &mut self,
        events: &Events,
        now: u64,
        mut deliver: impl FnMut(&Report),
    ) -> Result<Received, Refusal> {
        let mut sent = self.sent;
        for event in events.as_slice() {
            if !self.is_declared(event) {
                return Err(self.violation(Refusal::Undeclared));
            }
            if event.is_report() {
                sent = 0;
            } else {
                sent += 1;
                if sent >= MAX_REPORT {
                    return Err(self.violation(Refusal::ReportTooLong));
                }
            }
        }
        let mut reports = 0;
        for &event in events.as_slice() {
            if event.is_report() {
                self.sent = 0;
                if self.report.len != 0 {
                    self.report.push(event);
                    self.report.time = now;
                    self.report.only = self.grab;
                    deliver(&self.report);
                    reports += 1;
                    self.report.len = 0;
                }
            } else {
                self.sent += 1;
                if let Some(passed) = self.dispose(event) {
                    self.report.push(passed);
                }
            }
        }
        Ok(Received::Events { reports })
    }

    /// Whether the driver may send `event`: the section "What an EVENTS must
    /// hold" above.
    fn is_declared(&self, event: &RawEvent) -> bool {
        match event.kind {
            EV_SYN => event.code == SYN_REPORT,
            EV_REP => self.declared.has_type(EV_REP) && event.code <= REP_MAX,
            kind => self.declared.has_code(kind, event.code),
        }
    }

    /// `input_get_disposition`: the event as it passes, or `None` when it is
    /// ignored.
    fn dispose(&mut self, event: RawEvent) -> Option<RawEvent> {
        let caps = &self.caps.bits;
        let state = &mut self.state;
        let RawEvent { kind, code, value } = event;
        if !caps.has_type(kind) {
            return None;
        }
        let toggle = |bits: &mut [u8]| {
            if bit(bits, code) == (value != 0) {
                return None;
            }
            set_bit(bits, code, value != 0);
            Some(event)
        };
        match kind {
            EV_KEY if caps.has_code(kind, code) => {
                if value == 2 {
                    Some(event)
                } else {
                    toggle(&mut state.keys)
                }
            }
            EV_SW if caps.has_code(kind, code) => toggle(&mut state.sw),
            EV_LED if caps.has_code(kind, code) => toggle(&mut state.leds),
            EV_ABS if caps.has_code(kind, code) => {
                let fuzz = self.caps.axes.get(usize::from(code))?.fuzz;
                let old = state.abs.get_mut(usize::from(code))?;
                let new = defuzz(value, *old, fuzz);
                if new == *old {
                    return None;
                }
                *old = new;
                Some(RawEvent {
                    value: new,
                    ..event
                })
            }
            EV_REL if caps.has_code(kind, code) && value != 0 => Some(event),
            EV_MSC if caps.has_code(kind, code) => Some(event),
            EV_REP if code == REP_DELAY || code == REP_PERIOD => {
                let stored = state.repeat.get_mut(usize::from(code))?;
                if value < 0 || *stored == value {
                    return None;
                }
                *stored = value;
                Some(event)
            }
            _ => None,
        }
    }
}

/// `input_defuzz_abs_event`: a value within half the fuzz of the last one is
/// the last one; within the fuzz, a quarter of the way to it; within twice
/// the fuzz, half way. Computed in 64 bits, where C's `int` sums could
/// overflow.
#[must_use]
pub fn defuzz(value: i32, old: i32, fuzz: i32) -> i32 {
    if fuzz == 0 {
        return value;
    }
    let (value64, old64, fuzz64) = (i64::from(value), i64::from(old), i64::from(fuzz));
    let within = |span: i64| value64 > old64 - span && value64 < old64 + span;
    let result = if within(fuzz64 / 2) {
        old64
    } else if within(fuzz64) {
        (old64 * 3 + value64) / 4
    } else if within(fuzz64 * 2) {
        (old64 + value64) / 2
    } else {
        value64
    };
    i32::try_from(result).unwrap_or(value)
}

/// The bytes `EVIOCGBIT`, `EVIOCGPROP`, `EVIOCGKEY`, `EVIOCGLED` and
/// `EVIOCGSW` copy: `bits_to_user` in `drivers/input/evdev.c`, which copies
/// `BITS_TO_LONGS(max)` longs of the caller's width, where `max` is the kind's
/// `*_MAX` as Linux passes it, cut to the request's `maxlen`. `bits` past its
/// own length reads as zero. Writes into `out` and returns the length, or
/// `None` if `out` is shorter.
#[must_use]
pub fn copy_bits(
    bits: &[u8],
    max: u16,
    width: Width,
    maxlen: usize,
    out: &mut [u8],
) -> Option<usize> {
    let word = width.bytes();
    let len = usize::from(max)
        .div_ceil(word * 8)
        .saturating_mul(word)
        .min(maxlen);
    let slot = out.get_mut(..len)?;
    for (index, byte) in slot.iter_mut().enumerate() {
        *byte = bits.get(index).copied().unwrap_or(0);
    }
    Some(len)
}

/// Why a string ioctl has no answer: `ENOENT` in `str_to_user`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NoEntry;

/// The bytes `EVIOCGNAME` and `EVIOCGUNIQ` copy: `str_to_user`, which copies
/// the string and its NUL cut to `maxlen`, and answers `ENOENT` for a device
/// that gave none. Returns the length, or `Ok(None)` if `out` is shorter.
pub fn copy_text(text: &Text, maxlen: usize, out: &mut [u8]) -> Result<Option<usize>, NoEntry> {
    let string = text.as_bytes();
    if string.is_empty() {
        return Err(NoEntry);
    }
    let len = (string.len() + 1).min(maxlen);
    let Some(slot) = out.get_mut(..len) else {
        return Ok(None);
    };
    for (index, byte) in slot.iter_mut().enumerate() {
        *byte = string.get(index).copied().unwrap_or(0);
    }
    Ok(Some(len))
}
