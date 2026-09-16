//! virtio-input's device protocol: its configuration queries, its queues and
//! the events it writes.
//!
//! Virtio 1.2 §5.8 defines the input device. It has almost no protocol of its
//! own: what it reports are evdev events, `type`, `code` and `value` numbered
//! exactly as Linux's `input-event-codes.h` numbers them, and what it
//! describes of itself through its configuration space is what evdev's
//! `EVIOCGNAME`, `EVIOCGBIT`, `EVIOCGABS` and `EVIOCGID` ask. So this module is
//! the configuration queries, the 8-byte event, and the handful of evdev
//! numbers a kernel needs to turn events into `struct input_event`s. What
//! drives a device — filling the event queue, when to query, what to do when
//! it misbehaves — is a driver's.
//!
//! # Where the numbers come from
//!
//! Virtio 1.2 §5.8, checked against Linux 6.8's
//! `include/uapi/linux/virtio_input.h` and `input-event-codes.h`, and against
//! QEMU 9.2.4's copy of the first, `include/standard-headers/linux/
//! virtio_input.h`, and the devices it builds from it in
//! `hw/input/virtio-input.c` and `virtio-input-hid.c`.
//!
//! # What QEMU's devices answer
//!
//! QEMU sizes the configuration block to its longest answer plus the 8-byte
//! header, not to the 136 bytes of the structure: a keyboard's block is 37
//! bytes, a mouse's or tablet's 51. So a block need only hold the header, and
//! an answer is refused when it would run past the block, where QEMU reads
//! back `0xff`. A name's `size` counts its NUL, since QEMU fills it in with
//! `sizeof` a string literal; a serial's does not, since it comes from
//! `snprintf`. A keyboard answers `EV_REP` with a `size` of 1 and no bits
//! set: an event type is present when its answer is not empty, whatever bits
//! it holds, which is how Linux's driver fills `evbit`.
//!
//! # A query is a write and a read
//!
//! The configuration space is not a block of fields but a question and its
//! answer. The driver writes `select` (what it asks) and `subsel` (about
//! which event type or axis), in that order; the device then puts the answer's
//! length in `size` and the answer in the 128-byte union. [`query`] does
//! that, over a [`ConfigSelect`], and the helpers [`name`], [`serial`],
//! [`devids`], [`prop_bits`], [`ev_bits`] and [`abs_info`] read each answer
//! the way its select defines. A `size` of 0 means the device has no answer:
//! an empty bitmap for the bit queries, and [`InputError::Absent`] for those
//! whose answer is a structure.
//!
//! # Events
//!
//! The device writes one [`Event`] into each buffer the driver posts on
//! [`EVENT_QUEUE`], and a report is the run of events up to an `EV_SYN` /
//! `SYN_REPORT`. The driver may send `EV_LED` and `EV_SND` events the other
//! way, on [`STATUS_QUEUE`].
//!
//! # Trust
//!
//! The answers and events are the device's word. A `size` over 128 is refused,
//! not cut to fit, and so is one that runs past the configuration block; a
//! structure answer shorter than its structure is refused; an
//! axis whose minimum lies above its maximum is refused; a completion on the
//! event queue that wrote anything but exactly one event is refused.

use core::fmt;

use crate::pci::{FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1};

/// The configuration-block reader, which every device class shares.
pub use crate::DeviceConfig;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// The device, virtio 1.2 §5.8.
// ---------------------------------------------------------------------------

/// `VIRTIO_ID_INPUT`: the virtio device id.
pub const DEVICE_ID: u16 = 18;
/// The modern PCI device id, `0x1040` plus [`DEVICE_ID`].
pub const PCI_DEVICE_ID: u16 = 0x1052;

/// The features a driver accepts: the transport's [`FEATURE_VERSION_1`],
/// required, and [`FEATURE_ACCESS_PLATFORM`], without which a device behind an
/// IOMMU refuses `FEATURES_OK`. virtio-input defines no feature bits of its
/// own.
pub const DRIVER_FEATURES: u64 = FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM;

/// The features without which the driver gives up.
pub const REQUIRED_FEATURES: u64 = FEATURE_VERSION_1;

/// The event queue's index: device-writable buffers, one event each.
pub const EVENT_QUEUE: u16 = 0;
/// The status queue's index: device-readable buffers the driver sends LED
/// and sound events in.
pub const STATUS_QUEUE: u16 = 1;

// ---------------------------------------------------------------------------
// Configuration space, virtio 1.2 §5.8.4.
// ---------------------------------------------------------------------------

/// Offset of `select`, which the driver writes.
pub const CONFIG_SELECT: u32 = 0;
/// Offset of `subsel`, which the driver writes.
pub const CONFIG_SUBSEL: u32 = 1;
/// Offset of `size`, the answer's length.
pub const CONFIG_SIZE: u32 = 2;
/// Offset of the union holding the answer, after five reserved bytes.
pub const CONFIG_UNION: u32 = 8;
/// Bytes of the union, and so the longest answer.
pub const ANSWER_MAX: usize = 128;
/// Bytes of `struct virtio_input_config`. A device's block may be shorter,
/// down to the [`CONFIG_UNION`] bytes of the header, as long as each answer
/// fits in it.
pub const CONFIG_LEN: u32 = 136;

/// `VIRTIO_INPUT_CFG_UNSET`: no question.
pub const CFG_UNSET: u8 = 0x00;
/// `VIRTIO_INPUT_CFG_ID_NAME`: the device's name, as a string.
pub const CFG_ID_NAME: u8 = 0x01;
/// `VIRTIO_INPUT_CFG_ID_SERIAL`: its serial number, as a string.
pub const CFG_ID_SERIAL: u8 = 0x02;
/// `VIRTIO_INPUT_CFG_ID_DEVIDS`: its bus, vendor, product and version.
pub const CFG_ID_DEVIDS: u8 = 0x03;
/// `VIRTIO_INPUT_CFG_PROP_BITS`: evdev's `INPUT_PROP_*` bitmap.
pub const CFG_PROP_BITS: u8 = 0x10;
/// `VIRTIO_INPUT_CFG_EV_BITS`: with `subsel` an event type, the bitmap of
/// codes of that type the device sends.
pub const CFG_EV_BITS: u8 = 0x11;
/// `VIRTIO_INPUT_CFG_ABS_INFO`: with `subsel` an `ABS_*` axis, its range.
pub const CFG_ABS_INFO: u8 = 0x12;

/// Bytes of `struct virtio_input_absinfo`.
pub const ABS_INFO_LEN: usize = 20;
/// Bytes of `struct virtio_input_devids`.
pub const DEVIDS_LEN: usize = 8;

/// A configuration block the driver can also write to: what a query needs.
pub trait ConfigSelect: DeviceConfig {
    /// Write `value` into the byte at `offset`.
    fn config_write8(&mut self, offset: u32, value: u8);
}

/// A device's answer to one query: up to [`ANSWER_MAX`] bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Answer {
    bytes: [u8; ANSWER_MAX],
    len: u8,
}

impl Answer {
    /// The empty answer, a `size` of 0.
    pub const EMPTY: Self = Self {
        bytes: [0; ANSWER_MAX],
        len: 0,
    };

    /// The answer's bytes, `size` of them.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// The answer's length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the device had no answer.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read as a bitmap, whether bit `bit` is set: byte `bit / 8`, bit
    /// `bit % 8`. A bit past the answer is clear.
    #[must_use]
    pub fn bit(&self, bit: u16) -> bool {
        self.as_bytes()
            .get(usize::from(bit / 8))
            .is_some_and(|byte| byte & (1 << (bit % 8)) != 0)
    }

    /// Read as a string: the bytes before the first NUL, or all of them. The
    /// device need not end its string with a NUL, and `size` may or may not
    /// count one: QEMU's names count it and its serials do not.
    #[must_use]
    pub fn as_str_bytes(&self) -> &[u8] {
        let bytes = self.as_bytes();
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        bytes.get(..end).unwrap_or(bytes)
    }
}

/// Ask the device `select` about `subsel` and return its answer: write
/// `select`, then `subsel`, then read `size` and that many bytes of the
/// union.
///
/// The block need only hold the header: an answer is refused when `size` is
/// over [`ANSWER_MAX`], or when that many bytes would run past the block.
pub fn query<D: ConfigSelect + ?Sized>(
    dev: &mut D,
    select: u8,
    subsel: u8,
) -> Result<Answer, InputError> {
    if dev.config_len() < CONFIG_UNION {
        return Err(InputError::ConfigTooShort(dev.config_len()));
    }
    dev.config_write8(CONFIG_SELECT, select);
    dev.config_write8(CONFIG_SUBSEL, subsel);
    let size = dev.config_read8(CONFIG_SIZE);
    if usize::from(size) > ANSWER_MAX {
        return Err(InputError::SizeTooLarge {
            select,
            subsel,
            size,
        });
    }
    if CONFIG_UNION + u32::from(size) > dev.config_len() {
        return Err(InputError::AnswerPastConfig {
            select,
            subsel,
            size,
            len: dev.config_len(),
        });
    }
    let mut answer = Answer {
        bytes: [0; ANSWER_MAX],
        len: size,
    };
    for (offset, slot) in (CONFIG_UNION..).zip(answer.bytes.iter_mut().take(size.into())) {
        *slot = dev.config_read8(offset);
    }
    Ok(answer)
}

/// The device's name: [`Answer::as_str_bytes`] is the string. A device with
/// no name is [`InputError::Absent`].
pub fn name<D: ConfigSelect + ?Sized>(dev: &mut D) -> Result<Answer, InputError> {
    present(query(dev, CFG_ID_NAME, 0)?, CFG_ID_NAME, 0)
}

/// The device's serial number, as [`name`] reads the name.
pub fn serial<D: ConfigSelect + ?Sized>(dev: &mut D) -> Result<Answer, InputError> {
    present(query(dev, CFG_ID_SERIAL, 0)?, CFG_ID_SERIAL, 0)
}

/// The device's `INPUT_PROP_*` bitmap; empty when it has none.
pub fn prop_bits<D: ConfigSelect + ?Sized>(dev: &mut D) -> Result<Answer, InputError> {
    query(dev, CFG_PROP_BITS, 0)
}

/// The bitmap of codes of event type `ev_type` the device sends; empty when
/// it sends none. The device sends events of `ev_type` exactly when the answer
/// is not empty, even with no bit set: QEMU's keyboard answers `EV_REP` with
/// one zero byte. With `ev_type` [`EV_SYN`], QEMU answers nothing.
pub fn ev_bits<D: ConfigSelect + ?Sized>(dev: &mut D, ev_type: u16) -> Result<Answer, InputError> {
    query(dev, CFG_EV_BITS, subsel(ev_type)?)
}

/// `struct virtio_input_absinfo`: one absolute axis's range.
///
/// The fields are `__le32` on the wire and `__s32` in evdev's `struct
/// input_absinfo`, which is what they mean: a minimum may be negative.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AbsInfo {
    /// The smallest value.
    pub min: i32,
    /// The largest value.
    pub max: i32,
    /// Noise to filter out.
    pub fuzz: i32,
    /// The dead zone around the centre.
    pub flat: i32,
    /// Units per millimetre, or per radian for a rotation.
    pub res: i32,
}

/// The range of `ABS_*` axis `axis`. An axis the device does not have is
/// [`InputError::Absent`].
pub fn abs_info<D: ConfigSelect + ?Sized>(dev: &mut D, axis: u16) -> Result<AbsInfo, InputError> {
    let axis = subsel(axis)?;
    let answer = sized(dev, CFG_ABS_INFO, axis, ABS_INFO_LEN)?;
    let bytes = answer.as_bytes();
    let short = InputError::AnswerTooShort {
        select: CFG_ABS_INFO,
        subsel: axis,
        size: answer.len,
    };
    let field = |at| get32(bytes, at).map(u32::cast_signed).ok_or(short);
    let info = AbsInfo {
        min: field(0)?,
        max: field(4)?,
        fuzz: field(8)?,
        flat: field(12)?,
        res: field(16)?,
    };
    if info.min > info.max {
        return Err(InputError::AbsRange {
            axis,
            min: info.min,
            max: info.max,
        });
    }
    Ok(info)
}

/// `struct virtio_input_devids`: what evdev's `EVIOCGID` returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DevIds {
    /// `BUS_*`.
    pub bustype: u16,
    /// The vendor.
    pub vendor: u16,
    /// The product.
    pub product: u16,
    /// The version.
    pub version: u16,
}

/// The device's ids. A device without them is [`InputError::Absent`].
pub fn devids<D: ConfigSelect + ?Sized>(dev: &mut D) -> Result<DevIds, InputError> {
    let answer = sized(dev, CFG_ID_DEVIDS, 0, DEVIDS_LEN)?;
    let bytes = answer.as_bytes();
    let short = InputError::AnswerTooShort {
        select: CFG_ID_DEVIDS,
        subsel: 0,
        size: answer.len,
    };
    let field = |at| get16(bytes, at).ok_or(short);
    Ok(DevIds {
        bustype: field(0)?,
        vendor: field(2)?,
        product: field(4)?,
        version: field(6)?,
    })
}

/// An event type or axis as `subsel`, which is a byte.
fn subsel(code: u16) -> Result<u8, InputError> {
    u8::try_from(code).map_err(|_| InputError::Subsel(code))
}

/// Refuse an empty answer as absent.
fn present(answer: Answer, select: u8, subsel: u8) -> Result<Answer, InputError> {
    if answer.is_empty() {
        return Err(InputError::Absent { select, subsel });
    }
    Ok(answer)
}

/// Query, refusing an empty answer as absent and one shorter than `needed`.
fn sized<D: ConfigSelect + ?Sized>(
    dev: &mut D,
    select: u8,
    subsel: u8,
    needed: usize,
) -> Result<Answer, InputError> {
    let answer = present(query(dev, select, subsel)?, select, subsel)?;
    if answer.len() < needed {
        return Err(InputError::AnswerTooShort {
            select,
            subsel,
            size: answer.len,
        });
    }
    Ok(answer)
}

// ---------------------------------------------------------------------------
// Events, virtio 1.2 §5.8.6.
// ---------------------------------------------------------------------------

/// Bytes of `struct virtio_input_event`.
pub const EVENT_LEN: usize = 8;

// The event types and codes are evdev's, from Linux's
// `include/uapi/linux/input-event-codes.h`: virtio-input does not renumber
// them, so a virtio event is an `input_event` without its timestamp.

/// `EV_SYN`: a marker between reports.
pub const EV_SYN: u16 = 0x00;
/// `EV_KEY`: a key or button, pressed (1), released (0) or repeated (2).
pub const EV_KEY: u16 = 0x01;
/// `EV_REL`: relative motion, such as a mouse's.
pub const EV_REL: u16 = 0x02;
/// `EV_ABS`: an absolute position, such as a tablet's.
pub const EV_ABS: u16 = 0x03;
/// `EV_MSC`: anything else, such as a key's scan code.
pub const EV_MSC: u16 = 0x04;
/// `EV_LED`: an LED's state, which the driver sends on [`STATUS_QUEUE`].
pub const EV_LED: u16 = 0x11;
/// `EV_SND`: a sound, which the driver sends on [`STATUS_QUEUE`].
pub const EV_SND: u16 = 0x12;
/// `EV_REP`: autorepeat's delay and period.
pub const EV_REP: u16 = 0x14;
/// `EV_MAX`: the largest event type.
pub const EV_MAX: u16 = 0x1f;
/// `SYN_REPORT`: the `EV_SYN` code that ends a report.
pub const SYN_REPORT: u16 = 0;

/// `struct virtio_input_event`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Event {
    /// `type`: an `EV_*`.
    pub kind: u16,
    /// `code`: which key, axis or LED, by `kind`.
    pub code: u16,
    /// `value`: `__le32` on the wire, `__s32` in evdev, where relative motion
    /// is negative.
    pub value: i32,
}

impl Event {
    /// Decode the event at the start of `bytes`, refusing fewer than
    /// [`EVENT_LEN`] bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, InputError> {
        let short = InputError::EventTooShort(bytes.len());
        Ok(Self {
            kind: get16(bytes, 0).ok_or(short)?,
            code: get16(bytes, 2).ok_or(short)?,
            value: get32(bytes, 4).ok_or(short)?.cast_signed(),
        })
    }

    /// Decode the event the device wrote into an event-queue `buffer`, of
    /// which it says it wrote `written` bytes: anything but exactly one event
    /// is refused.
    pub fn from_completion(buffer: &[u8], written: u32) -> Result<Self, InputError> {
        if written as usize != EVENT_LEN {
            return Err(InputError::EventWritten(written));
        }
        Self::decode(buffer)
    }

    /// The event's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; EVENT_LEN] {
        let [k0, k1] = self.kind.to_le_bytes();
        let [c0, c1] = self.code.to_le_bytes();
        let [v0, v1, v2, v3] = self.value.to_le_bytes();
        [k0, k1, c0, c1, v0, v1, v2, v3]
    }

    /// Encode the event into the start of `out`, returning its length.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, InputError> {
        let len = out.len();
        let slot = out
            .get_mut(..EVENT_LEN)
            .ok_or(InputError::BufferTooShort(len))?;
        slot.copy_from_slice(&self.to_bytes());
        Ok(EVENT_LEN)
    }

    /// Whether the event ends a report.
    #[must_use]
    pub const fn is_report(&self) -> bool {
        self.kind == EV_SYN && self.code == SYN_REPORT
    }
}

/// What can go wrong speaking to a virtio-input device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputError {
    /// The configuration block is shorter than its 8-byte header.
    ConfigTooShort(u32),
    /// The device answered with a `size` over [`ANSWER_MAX`].
    SizeTooLarge {
        /// The question.
        select: u8,
        /// About what.
        subsel: u8,
        /// The size it gave.
        size: u8,
    },
    /// The answer's `size` runs past the end of the configuration block.
    AnswerPastConfig {
        /// The question.
        select: u8,
        /// About what.
        subsel: u8,
        /// The size it gave.
        size: u8,
        /// The block's length.
        len: u32,
    },
    /// An event type or axis too large for `subsel`'s byte.
    Subsel(u16),
    /// The device had no answer to a question that needs one.
    Absent {
        /// The question.
        select: u8,
        /// About what.
        subsel: u8,
    },
    /// The answer is shorter than the structure it is.
    AnswerTooShort {
        /// The question.
        select: u8,
        /// About what.
        subsel: u8,
        /// The size it gave.
        size: u8,
    },
    /// An axis whose minimum is above its maximum.
    AbsRange {
        /// The axis.
        axis: u8,
        /// Its minimum.
        min: i32,
        /// Its maximum.
        max: i32,
    },
    /// Fewer bytes than an event.
    EventTooShort(usize),
    /// An event-queue completion that wrote anything but one event.
    EventWritten(u32),
    /// A buffer too short to encode an event into.
    BufferTooShort(usize),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigTooShort(len) => write!(
                f,
                "virtio-input configuration is {len} bytes, under its {CONFIG_UNION}-byte header"
            ),
            Self::SizeTooLarge {
                select,
                subsel,
                size,
            } => write!(
                f,
                "virtio-input answered select {select:#x}/{subsel:#x} with {size} bytes, over {ANSWER_MAX}"
            ),
            Self::AnswerPastConfig {
                select,
                subsel,
                size,
                len,
            } => write!(
                f,
                "virtio-input answered select {select:#x}/{subsel:#x} with {size} bytes, past its {len}-byte configuration"
            ),
            Self::Subsel(code) => write!(f, "{code:#x} does not fit virtio-input's subsel"),
            Self::Absent { select, subsel } => write!(
                f,
                "virtio-input has no answer to select {select:#x}/{subsel:#x}"
            ),
            Self::AnswerTooShort {
                select,
                subsel,
                size,
            } => write!(
                f,
                "virtio-input answered select {select:#x}/{subsel:#x} with only {size} bytes"
            ),
            Self::AbsRange { axis, min, max } => write!(
                f,
                "virtio-input axis {axis:#x} runs from {min} down to {max}"
            ),
            Self::EventTooShort(len) => write!(f, "a {len}-byte virtio-input event"),
            Self::EventWritten(written) => write!(
                f,
                "virtio-input wrote {written} bytes for one {EVENT_LEN}-byte event"
            ),
            Self::BufferTooShort(len) => write!(
                f,
                "a virtio-input event needs {EVENT_LEN} bytes, the buffer has {len}"
            ),
        }
    }
}

fn get16(bytes: &[u8], at: usize) -> Option<u16> {
    let field = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes(field.try_into().ok()?))
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}
