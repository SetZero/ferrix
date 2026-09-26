//! The messages, as bytes.
//!
//! Every message starts with its type and its length, four bytes each, and has
//! exactly the length its type fixes. Handles ride in the channel message's
//! handle array; the glue reads their rights, and [`Hello::HANDLE_RIGHTS`] and
//! [`Ready::HANDLE_RIGHTS`] say what they must be.
//!
//! ```text
//! HELLO    driver -> core, 1688 bytes, handles [driver port]
//!   8 version u16   10 reserved u16   12 location u32
//!   16 bustype u16   18 vendor u16   20 product u16   22 id version u16
//!   24 name_len u16   26 serial_len u16   28 reserved u32
//!   32 name [u8; 128]   160 serial [u8; 128]
//!   288 props [u8; 4]   292 types [u8; 4]   296 keys [u8; 96]
//!   392 rels [u8; 2]   394 abs [u8; 8]   402 msc [u8; 1]   403 sw [u8; 3]
//!   406 leds [u8; 2]
//!   408 axis 0: minimum, maximum, fuzz, flat, resolution i32
//!       ... ABS_CNT (64) of them, 20 bytes each
//! READY    core -> driver, 16 bytes, handles [core port]
//!   8 node u32   12 reserved
//! REFUSED  core -> driver, 12 bytes: 8 reason u32
//! EVENTS   driver -> core, 528 bytes
//!   8 count u32   12 reserved
//!   16 event 0: type u16, code u16, value i32   ... MAX_EVENTS (64), 8 bytes each
//! STOP, STOPPED               8 bytes
//! STATUS   core -> driver, 528 bytes, laid out as EVENTS: what a program
//!          wrote to the device's node for the device itself, its LEDs
//! ```
//!
//! Each bitmap is as long as `input-event-codes.h`'s `*_CNT` for its kind,
//! rounded up to whole bytes, bit `n` in byte `n / 8` at `1 << (n % 8)`: the
//! order virtio-input answers in and Linux keeps its `unsigned long` bitmaps
//! in on a little-endian machine. The numbers are `ferrix-linux-abi`'s, which
//! its probe pins against the headers. `EV_SYN` and `EV_REP` have no code
//! bitmap: Linux keeps none for either (`handle_eviocgbit` in
//! `drivers/input/evdev.c` has no case for them), and virtio-input answers
//! `EV_REP` with an empty one.
//!
//! Reserved bytes are written as zero and a message with any of them set is
//! malformed, so they can be given a meaning later without an old reader
//! misreading them. So are the events past an EVENTS count. Name and serial
//! bytes past their lengths are checked by [`Hello::validate`], not by
//! decoding, because a length over its field is a refusal of its own.

use ::core::fmt;

use ferrix_linux_abi::input::{
    ABS_CNT, EV_ABS, EV_CNT, EV_KEY, EV_LED, EV_MSC, EV_REL, EV_REP, EV_SW, EV_SYN, INPUT_PROP_CNT,
    KEY_CNT, LED_CNT, MSC_CNT, REL_CNT, SW_CNT,
};
use ferrix_native_abi::rights::Rights;

/// The protocol version this crate speaks.
pub const VERSION: u16 = 1;

/// HELLO's type.
pub const HELLO: u32 = 1;
/// READY's type.
pub const READY: u32 = 2;
/// REFUSED's type.
pub const REFUSED: u32 = 3;
/// EVENTS's type.
pub const EVENTS: u32 = 4;
/// STOP's type.
pub const STOP: u32 = 5;
/// STOPPED's type.
pub const STOPPED: u32 = 6;
/// STATUS's type.
pub const STATUS: u32 = 7;

/// Bytes of the type and length, and all of STOP and STOPPED.
pub const HEADER_BYTES: usize = 8;

/// Bytes of a name or serial field: virtio-input's longest answer.
pub const TEXT_BYTES: usize = 128;

/// Bytes of a bitmap of `count` bits.
#[must_use]
pub const fn bitmap_bytes(count: u16) -> usize {
    (count as usize).div_ceil(8)
}

/// Bytes of the `INPUT_PROP_*` bitmap.
pub const PROP_BYTES: usize = bitmap_bytes(INPUT_PROP_CNT);
/// Bytes of the event type bitmap.
pub const TYPE_BYTES: usize = bitmap_bytes(EV_CNT);
/// Bytes of the `EV_KEY` code bitmap.
pub const KEY_BYTES: usize = bitmap_bytes(KEY_CNT);
/// Bytes of the `EV_REL` code bitmap.
pub const REL_BYTES: usize = bitmap_bytes(REL_CNT);
/// Bytes of the `EV_ABS` code bitmap.
pub const ABS_BYTES: usize = bitmap_bytes(ABS_CNT);
/// Bytes of the `EV_MSC` code bitmap.
pub const MSC_BYTES: usize = bitmap_bytes(MSC_CNT);
/// Bytes of the `EV_SW` code bitmap.
pub const SW_BYTES: usize = bitmap_bytes(SW_CNT);
/// Bytes of the `EV_LED` code bitmap.
pub const LED_BYTES: usize = bitmap_bytes(LED_CNT);

/// Absolute axes HELLO describes: every `ABS_*`.
pub const AXES: usize = ABS_CNT as usize;
/// Bytes of one axis in HELLO.
pub const AXIS_BYTES: usize = 20;

/// Where HELLO's name starts.
const NAME_AT: usize = 32;
/// Where HELLO's serial starts.
const SERIAL_AT: usize = NAME_AT + TEXT_BYTES;
/// Where HELLO's bitmaps start.
const BITS_AT: usize = SERIAL_AT + TEXT_BYTES;
/// Where HELLO's axes start.
const AXES_AT: usize = BITS_AT
    + PROP_BYTES
    + TYPE_BYTES
    + KEY_BYTES
    + REL_BYTES
    + ABS_BYTES
    + MSC_BYTES
    + SW_BYTES
    + LED_BYTES;

/// Bytes of HELLO.
pub const HELLO_BYTES: usize = AXES_AT + AXES * AXIS_BYTES;
/// Bytes of READY.
pub const READY_BYTES: usize = 16;
/// Bytes of REFUSED.
pub const REFUSED_BYTES: usize = 12;
/// The most events one EVENTS carries: `docs/INPUT.md` §3.2, which holds any
/// report QEMU's HID devices make.
pub const MAX_EVENTS: usize = 64;
/// Bytes of one event: virtio-input's own layout.
pub const EVENT_BYTES: usize = 8;
/// Bytes of EVENTS.
pub const EVENTS_BYTES: usize = 16 + MAX_EVENTS * EVENT_BYTES;
/// Bytes of the longest message.
pub const MAX_BYTES: usize = HELLO_BYTES;

/// The event types the core publishes in this iteration (`docs/INPUT.md`
/// §3.2). A device declaring any other is published without it.
pub const SUPPORTED_TYPES: [u16; 8] = [
    EV_SYN, EV_KEY, EV_REL, EV_ABS, EV_MSC, EV_SW, EV_LED, EV_REP,
];

/// Exactly the rights the core holds the driver's port with.
pub const PORT_RIGHTS: Rights = Rights(Rights::WRITE.0 | Rights::TRANSFER.0);

/// Whether bit `bit` of a little-endian bitmap is set; a bit past its end is
/// clear.
#[must_use]
pub fn bit(bits: &[u8], bit: u16) -> bool {
    bits.get(usize::from(bit / 8))
        .is_some_and(|byte| byte & (1 << (bit % 8)) != 0)
}

/// Whether any bit of `bits` at or past `count` is set.
#[must_use]
pub fn bits_past(bits: &[u8], count: u16) -> bool {
    let whole = usize::from(count / 8);
    let partial = count % 8;
    let (tail, rest) = if partial == 0 {
        (0, bits.get(whole..))
    } else {
        (
            bits.get(whole).map_or(0, |byte| byte >> partial),
            bits.get(whole + 1..),
        )
    };
    tail != 0 || rest.is_some_and(|rest| rest.iter().any(|&byte| byte != 0))
}

/// A name or serial as the device gave it: its length and bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Text {
    /// Bytes of the string, without a NUL. 0: the device gave none.
    pub len: u16,
    /// The string, zero past `len`.
    pub bytes: [u8; TEXT_BYTES],
}

impl Default for Text {
    fn default() -> Self {
        Self::NONE
    }
}

impl Text {
    /// No string.
    pub const NONE: Self = Self {
        len: 0,
        bytes: [0; TEXT_BYTES],
    };

    /// The text holding `string`, if it fits and holds no NUL.
    #[must_use]
    pub fn new(string: &[u8]) -> Option<Self> {
        let mut text = Self::NONE;
        text.bytes.get_mut(..string.len())?.copy_from_slice(string);
        text.len = u16::try_from(string.len()).ok()?;
        text.is_valid().then_some(text)
    }

    /// The string's bytes, without a NUL; empty for one the device did not
    /// give or a length over the field.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// Whether the length fits the field, no NUL lies inside the string and
    /// nothing is set past it.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let len = usize::from(self.len);
        len <= TEXT_BYTES
            && self.as_bytes().iter().all(|&byte| byte != 0)
            && self
                .bytes
                .get(len..)
                .is_some_and(|rest| rest.iter().all(|&byte| byte == 0))
    }
}

/// `struct input_id`: what `EVIOCGID` answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DeviceId {
    /// `BUS_*`.
    pub bustype: u16,
    /// The vendor.
    pub vendor: u16,
    /// The product.
    pub product: u16,
    /// The version.
    pub version: u16,
}

/// One absolute axis's range, as virtio-input gives it and
/// `struct input_absinfo` holds it without its value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AxisRange {
    /// The smallest value.
    pub minimum: i32,
    /// The largest value.
    pub maximum: i32,
    /// Noise to filter out.
    pub fuzz: i32,
    /// The dead zone around the centre.
    pub flat: i32,
    /// Units per millimetre, or per radian for a rotation.
    pub resolution: i32,
}

/// A device's bitmaps: its properties, its event types, and each type's codes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bitmaps {
    /// `INPUT_PROP_*`.
    pub props: [u8; PROP_BYTES],
    /// `EV_*`.
    pub types: [u8; TYPE_BYTES],
    /// `EV_KEY` codes.
    pub keys: [u8; KEY_BYTES],
    /// `EV_REL` codes.
    pub rels: [u8; REL_BYTES],
    /// `EV_ABS` codes.
    pub abs: [u8; ABS_BYTES],
    /// `EV_MSC` codes.
    pub msc: [u8; MSC_BYTES],
    /// `EV_SW` codes.
    pub sw: [u8; SW_BYTES],
    /// `EV_LED` codes.
    pub leds: [u8; LED_BYTES],
}

impl Default for Bitmaps {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Bitmaps {
    /// Nothing declared.
    pub const EMPTY: Self = Self {
        props: [0; PROP_BYTES],
        types: [0; TYPE_BYTES],
        keys: [0; KEY_BYTES],
        rels: [0; REL_BYTES],
        abs: [0; ABS_BYTES],
        msc: [0; MSC_BYTES],
        sw: [0; SW_BYTES],
        leds: [0; LED_BYTES],
    };

    /// The code bitmap of `kind` and its `*_CNT`, for the types that have
    /// one.
    #[must_use]
    pub fn codes(&self, kind: u16) -> Option<(&[u8], u16)> {
        Some(match kind {
            EV_KEY => (self.keys.as_slice(), KEY_CNT),
            EV_REL => (self.rels.as_slice(), REL_CNT),
            EV_ABS => (self.abs.as_slice(), ABS_CNT),
            EV_MSC => (self.msc.as_slice(), MSC_CNT),
            EV_SW => (self.sw.as_slice(), SW_CNT),
            EV_LED => (self.leds.as_slice(), LED_CNT),
            _ => return None,
        })
    }

    /// The mutable code bitmap of `kind`.
    pub fn codes_mut(&mut self, kind: u16) -> Option<&mut [u8]> {
        Some(match kind {
            EV_KEY => self.keys.as_mut_slice(),
            EV_REL => self.rels.as_mut_slice(),
            EV_ABS => self.abs.as_mut_slice(),
            EV_MSC => self.msc.as_mut_slice(),
            EV_SW => self.sw.as_mut_slice(),
            EV_LED => self.leds.as_mut_slice(),
            _ => return None,
        })
    }

    /// Whether the event type `kind` is declared.
    #[must_use]
    pub fn has_type(&self, kind: u16) -> bool {
        bit(&self.types, kind)
    }

    /// Whether code `code` of `kind` is declared, for a type with a code
    /// bitmap.
    #[must_use]
    pub fn has_code(&self, kind: u16, code: u16) -> bool {
        self.has_type(kind)
            && self
                .codes(kind)
                .is_some_and(|(bits, count)| code < count && bit(bits, code))
    }
}

/// HELLO: the driver introduces its device, as the device described itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hello {
    /// The protocol version.
    pub version: u16,
    /// The device's PCI location, as START gave it.
    pub location: u32,
    /// Bus, vendor, product and version.
    pub id: DeviceId,
    /// The device's name.
    pub name: Text,
    /// Its serial.
    pub serial: Text,
    /// What it declares.
    pub bits: Bitmaps,
    /// Each axis's range, zero for an axis it does not declare.
    pub axes: [AxisRange; AXES],
}

impl Default for Hello {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Hello {
    /// A hello that says nothing: no device, no name, nothing declared.
    ///
    /// Every field is zero, so building one costs a `memset` rather than a
    /// copy from a constant, which matters where a hello is put straight
    /// onto the heap.
    pub const EMPTY: Self = Self {
        version: 0,
        location: 0,
        id: DeviceId {
            bustype: 0,
            vendor: 0,
            product: 0,
            version: 0,
        },
        name: Text::NONE,
        serial: Text::NONE,
        bits: Bitmaps::EMPTY,
        axes: [AxisRange {
            minimum: 0,
            maximum: 0,
            fuzz: 0,
            flat: 0,
            resolution: 0,
        }; AXES],
    };

    /// Decode a HELLO into `place`, writing each field where it belongs
    /// instead of returning a hello.
    ///
    /// A `Hello` carries every bitmap a device can declare and every axis's
    /// range: 1672 bytes. [`Message::decode`] returns one inside a `Message`
    /// by value, and each step out of `decode_hello` adds another copy of
    /// that size to the frame -- around six kilobytes in all. A kernel task
    /// has four pages of stack, and running out of it is a double fault, so
    /// the one caller that has to decode a hello on a kernel stack uses this
    /// and keeps the hello itself on the heap.
    ///
    /// # Errors
    ///
    /// The same judgements [`Message::decode`] makes, refusing anything that
    /// is not exactly one well-formed HELLO.
    pub fn decode_into(bytes: &[u8], place: &mut Self) -> Result<(), MessageError> {
        let kind = get32(bytes, 0).ok_or(MessageError::Short)?;
        let length = get32(bytes, 4).ok_or(MessageError::Short)? as usize;
        if kind != HELLO {
            return Err(MessageError::Type(kind));
        }
        let expected = Message::length_of(HELLO).ok_or(MessageError::Type(kind))?;
        if length != expected || bytes.len() != expected {
            return Err(MessageError::Length);
        }
        if get16(bytes, 10) != Some(0) || get32(bytes, 28) != Some(0) {
            return Err(MessageError::Field);
        }
        decode_hello_into(bytes, place).ok_or(MessageError::Field)
    }
}

/// Why the core refuses a driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Refusal {
    /// The version is not [`VERSION`].
    Version = 1,
    /// A name or serial is longer than its field, holds a NUL, or has bytes
    /// set past its length.
    Text = 2,
    /// A bitmap has a bit set past its kind's `*_MAX`, or codes of a type the
    /// device does not declare.
    Bits = 3,
    /// A declared axis's minimum lies above its maximum, or an undeclared
    /// axis has a range.
    Axis = 4,
    /// A handle is missing, extra, or has rights other than exactly the
    /// specified ones.
    Rights = 5,
    /// The message is not a well-formed one of its type.
    Malformed = 6,
    /// `location` is not the device the channel was made for. Decided by the
    /// glue.
    WrongLocation = 7,
    /// The driver sent something the core did not wait for. Decided by
    /// [`crate::session::Session`].
    Protocol = 8,
    /// An event of a type or code the HELLO did not declare.
    Undeclared = 9,
    /// A report longer than the core holds,
    /// [`crate::session::MAX_REPORT`] events.
    ReportTooLong = 10,
    /// The core has no event node left to publish the device under. Decided
    /// by the core.
    NoNode = 11,
}

impl Refusal {
    /// The refusal a reason word names, if any.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => Self::Version,
            2 => Self::Text,
            3 => Self::Bits,
            4 => Self::Axis,
            5 => Self::Rights,
            6 => Self::Malformed,
            7 => Self::WrongLocation,
            8 => Self::Protocol,
            9 => Self::Undeclared,
            10 => Self::ReportTooLong,
            11 => Self::NoNode,
            _ => return None,
        })
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Version => "input protocol version mismatch",
            Self::Text => "a name or serial does not fit its field",
            Self::Bits => "a bitmap declares what its kind does not have",
            Self::Axis => "an axis range is not one an axis can have",
            Self::Rights => "a handle has the wrong rights",
            Self::Malformed => "malformed message",
            Self::WrongLocation => "HELLO names another device",
            Self::Protocol => "the driver broke the protocol",
            Self::Undeclared => "an event the device did not declare",
            Self::ReportTooLong => "a report longer than the core holds",
            Self::NoNode => "no event node is left to publish the device under",
        })
    }
}

impl Hello {
    /// HELLO's handles, in order, with exactly the rights each must carry.
    pub const HANDLE_RIGHTS: [Rights; 1] = [PORT_RIGHTS];

    /// Check a HELLO and the rights of the handles it came with, in the order
    /// its fields are read.
    pub fn validate(&self, handle_rights: &[Rights]) -> Result<(), Refusal> {
        if self.version != VERSION {
            return Err(Refusal::Version);
        }
        if !self.name.is_valid() || !self.serial.is_valid() {
            return Err(Refusal::Text);
        }
        let bits = &self.bits;
        if bits_past(&bits.props, INPUT_PROP_CNT) || bits_past(&bits.types, EV_CNT) {
            return Err(Refusal::Bits);
        }
        for kind in [EV_KEY, EV_REL, EV_ABS, EV_MSC, EV_SW, EV_LED] {
            let Some((codes, count)) = bits.codes(kind) else {
                continue;
            };
            let stray = !bits.has_type(kind) && codes.iter().any(|&byte| byte != 0);
            if bits_past(codes, count) || stray {
                return Err(Refusal::Bits);
            }
        }
        for (axis, range) in (0..ABS_CNT).zip(self.axes.iter()) {
            let fine = if bit(&bits.abs, axis) {
                range.minimum <= range.maximum
            } else {
                *range == AxisRange::default()
            };
            if !fine {
                return Err(Refusal::Axis);
            }
        }
        if handle_rights != Self::HANDLE_RIGHTS {
            return Err(Refusal::Rights);
        }
        Ok(())
    }
}

/// READY: the core accepts the driver and publishes its device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ready {
    /// The node's index, `event<N>`.
    pub node: u32,
}

impl Ready {
    /// READY's handles, in order: the core's port.
    pub const HANDLE_RIGHTS: [Rights; 1] = [Rights::WRITE];
}

/// One event as the driver forwards it: virtio-input's layout, an
/// `input_event` without its timestamp.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RawEvent {
    /// `EV_*`.
    pub kind: u16,
    /// The type's code.
    pub code: u16,
    /// The value.
    pub value: i32,
}

impl RawEvent {
    /// The event `kind`, `code`, `value`.
    #[must_use]
    pub const fn new(kind: u16, code: u16, value: i32) -> Self {
        Self { kind, code, value }
    }

    /// Whether the event ends a report.
    #[must_use]
    pub const fn is_report(&self) -> bool {
        self.kind == EV_SYN && self.code == ferrix_linux_abi::input::SYN_REPORT
    }
}

/// EVENTS: up to [`MAX_EVENTS`] events, which may end in the middle of a
/// report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Events {
    count: usize,
    events: [RawEvent; MAX_EVENTS],
}

impl Events {
    /// EVENTS carrying `events`, if there are at most [`MAX_EVENTS`].
    #[must_use]
    pub fn new(events: &[RawEvent]) -> Option<Self> {
        let mut out = Self {
            count: events.len(),
            events: [RawEvent::default(); MAX_EVENTS],
        };
        out.events.get_mut(..events.len())?.copy_from_slice(events);
        Some(out)
    }

    /// The events.
    #[must_use]
    pub fn as_slice(&self) -> &[RawEvent] {
        self.events.get(..self.count).unwrap_or(&[])
    }
}

/// Every message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "messages are fixed-size and nothing here allocates; HELLO comes once per device"
)]
pub enum Message {
    /// Driver to core.
    Hello(Hello),
    /// Core to driver.
    Ready(Ready),
    /// Core to driver.
    Refused(Refusal),
    /// Driver to core.
    Events(Events),
    /// Core to driver.
    Stop,
    /// Driver to core.
    Stopped,
    /// Core to driver: events a program wrote for the device, `EV_LED`
    /// ones, which the driver carries to it as virtio-input's status queue
    /// or a USB keyboard's output report does.
    Status(Events),
}

/// Why bytes are not a message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageError {
    /// Shorter than a header.
    Short,
    /// A type this crate does not know.
    Type(u32),
    /// A length other than the type's, or other than the bytes given.
    Length,
    /// A field outside its range, or a reserved byte set.
    Field,
}

/// An encoded message: bytes up to [`MAX_BYTES`] and how many are used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Encoded {
    bytes: [u8; MAX_BYTES],
    len: usize,
}

impl Encoded {
    /// The message's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}

impl Message {
    /// The message's type.
    #[must_use]
    pub const fn kind(&self) -> u32 {
        match self {
            Self::Hello(_) => HELLO,
            Self::Ready(_) => READY,
            Self::Refused(_) => REFUSED,
            Self::Events(_) => EVENTS,
            Self::Stop => STOP,
            Self::Stopped => STOPPED,
            Self::Status(_) => STATUS,
        }
    }

    /// The fixed length of a message of type `kind`.
    #[must_use]
    pub const fn length_of(kind: u32) -> Option<usize> {
        Some(match kind {
            HELLO => HELLO_BYTES,
            READY => READY_BYTES,
            REFUSED => REFUSED_BYTES,
            EVENTS | STATUS => EVENTS_BYTES,
            STOP | STOPPED => HEADER_BYTES,
            _ => return None,
        })
    }

    /// Encode the message.
    #[must_use]
    pub fn encode(&self) -> Encoded {
        let kind = self.kind();
        let len = Self::length_of(kind).unwrap_or(HEADER_BYTES);
        let mut out = Encoded {
            bytes: [0; MAX_BYTES],
            len,
        };
        let bytes = &mut out.bytes;
        put32(bytes, 0, kind);
        put32(bytes, 4, u32::try_from(len).unwrap_or(0));
        match self {
            Self::Hello(hello) => encode_hello(bytes, hello),
            Self::Ready(ready) => put32(bytes, 8, ready.node),
            Self::Refused(reason) => put32(bytes, 8, *reason as u32),
            Self::Events(events) | Self::Status(events) => {
                put32(bytes, 8, u32::try_from(events.count).unwrap_or(0));
                for (index, event) in events.events.iter().enumerate() {
                    let at = 16 + index * EVENT_BYTES;
                    put16(bytes, at, event.kind);
                    put16(bytes, at + 2, event.code);
                    put32(bytes, at + 4, event.value.cast_unsigned());
                }
            }
            Self::Stop | Self::Stopped => {}
        }
        out
    }

    /// Decode a message, refusing anything but exactly one well-formed one.
    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let kind = get32(bytes, 0).ok_or(MessageError::Short)?;
        let length = get32(bytes, 4).ok_or(MessageError::Short)? as usize;
        let expected = Self::length_of(kind).ok_or(MessageError::Type(kind))?;
        if length != expected || bytes.len() != expected {
            return Err(MessageError::Length);
        }
        decode_body(kind, bytes).ok_or(MessageError::Field)
    }
}

fn encode_hello(bytes: &mut [u8], hello: &Hello) {
    put16(bytes, 8, hello.version);
    put32(bytes, 12, hello.location);
    put16(bytes, 16, hello.id.bustype);
    put16(bytes, 18, hello.id.vendor);
    put16(bytes, 20, hello.id.product);
    put16(bytes, 22, hello.id.version);
    put16(bytes, 24, hello.name.len);
    put16(bytes, 26, hello.serial.len);
    put(bytes, NAME_AT, &hello.name.bytes);
    put(bytes, SERIAL_AT, &hello.serial.bytes);
    let bits = &hello.bits;
    let mut at = BITS_AT;
    for field in [
        bits.props.as_slice(),
        bits.types.as_slice(),
        bits.keys.as_slice(),
        bits.rels.as_slice(),
        bits.abs.as_slice(),
        bits.msc.as_slice(),
        bits.sw.as_slice(),
        bits.leds.as_slice(),
    ] {
        put(bytes, at, field);
        at += field.len();
    }
    for (index, range) in hello.axes.iter().enumerate() {
        let at = AXES_AT + index * AXIS_BYTES;
        for (offset, value) in [
            range.minimum,
            range.maximum,
            range.fuzz,
            range.flat,
            range.resolution,
        ]
        .into_iter()
        .enumerate()
        {
            put32(bytes, at + offset * 4, value.cast_unsigned());
        }
    }
}

fn decode_body(kind: u32, bytes: &[u8]) -> Option<Message> {
    let zero16 = |at: usize| get16(bytes, at).filter(|&value| value == 0).map(drop);
    let zero32 = |at: usize| get32(bytes, at).filter(|&value| value == 0).map(drop);
    Some(match kind {
        HELLO => {
            zero16(10)?;
            zero32(28)?;
            Message::Hello(decode_hello(bytes)?)
        }
        READY => {
            zero32(12)?;
            Message::Ready(Ready {
                node: get32(bytes, 8)?,
            })
        }
        REFUSED => Message::Refused(Refusal::from_raw(get32(bytes, 8)?)?),
        EVENTS | STATUS => {
            let count = get32(bytes, 8)? as usize;
            zero32(12)?;
            if count > MAX_EVENTS {
                return None;
            }
            let mut events = [RawEvent::default(); MAX_EVENTS];
            for (index, event) in events.iter_mut().enumerate() {
                let at = 16 + index * EVENT_BYTES;
                *event = RawEvent {
                    kind: get16(bytes, at)?,
                    code: get16(bytes, at + 2)?,
                    value: get32(bytes, at + 4)?.cast_signed(),
                };
                if index >= count && *event != RawEvent::default() {
                    return None;
                }
            }
            let events = Events { count, events };
            if kind == STATUS {
                Message::Status(events)
            } else {
                Message::Events(events)
            }
        }
        STOP => Message::Stop,
        STOPPED => Message::Stopped,
        _ => return None,
    })
}

fn decode_hello(bytes: &[u8]) -> Option<Hello> {
    let mut hello = Hello::EMPTY;
    decode_hello_into(bytes, &mut hello)?;
    Some(hello)
}

fn decode_hello_into(bytes: &[u8], place: &mut Hello) -> Option<()> {
    place.version = get16(bytes, 8)?;
    place.location = get32(bytes, 12)?;
    place.id = DeviceId {
        bustype: get16(bytes, 16)?,
        vendor: get16(bytes, 18)?,
        product: get16(bytes, 20)?,
        version: get16(bytes, 22)?,
    };
    place.name.len = get16(bytes, 24)?;
    place.name.bytes = bytes.get(NAME_AT..SERIAL_AT)?.try_into().ok()?;
    place.serial.len = get16(bytes, 26)?;
    place.serial.bytes = bytes.get(SERIAL_AT..BITS_AT)?.try_into().ok()?;

    let mut at = BITS_AT;
    let mut take = |len: usize| {
        let field = bytes.get(at..at.checked_add(len)?);
        at += len;
        field
    };
    place.bits.props = take(PROP_BYTES)?.try_into().ok()?;
    place.bits.types = take(TYPE_BYTES)?.try_into().ok()?;
    place.bits.keys = take(KEY_BYTES)?.try_into().ok()?;
    place.bits.rels = take(REL_BYTES)?.try_into().ok()?;
    place.bits.abs = take(ABS_BYTES)?.try_into().ok()?;
    place.bits.msc = take(MSC_BYTES)?.try_into().ok()?;
    place.bits.sw = take(SW_BYTES)?.try_into().ok()?;
    place.bits.leds = take(LED_BYTES)?.try_into().ok()?;

    for (index, range) in place.axes.iter_mut().enumerate() {
        let at = AXES_AT + index * AXIS_BYTES;
        let field = |offset: usize| get32(bytes, at + offset).map(u32::cast_signed);
        *range = AxisRange {
            minimum: field(0)?,
            maximum: field(4)?,
            fuzz: field(8)?,
            flat: field(12)?,
            resolution: field(16)?,
        };
    }
    Some(())
}

fn get16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn put(out: &mut [u8], at: usize, field: &[u8]) {
    if let Some(slot) = at
        .checked_add(field.len())
        .and_then(|end| out.get_mut(at..end))
    {
        slot.copy_from_slice(field);
    }
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    put(out, at, &value.to_le_bytes());
}

fn put32(out: &mut [u8], at: usize, value: u32) {
    put(out, at, &value.to_le_bytes());
}
