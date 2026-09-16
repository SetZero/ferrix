//! The messages, as bytes.
//!
//! Every message starts with its type and its length, four bytes each, and has
//! exactly the length its type fixes. Handles ride in the channel message's
//! handle array; the glue reads their rights and [`Hello::validate`] and
//! [`Ready::HANDLE_RIGHTS`] say what they must be.
//!
//! ```text
//! HELLO     driver -> core, 208 bytes, handles [driver port]
//!   8 version u16   10 scanouts u16   12 location u32
//!   16 scanout 0: width u32, height u32, enabled u32   ... 16 of them, 12 bytes each
//! READY     core -> driver, 24 bytes, handles [card VMO, core port]
//!   8 card u32   12 reserved   16 card_bytes u64
//! REFUSED   core -> driver, 12 bytes: 8 reason u32
//! ATTACH    core -> driver, 48 bytes
//!   8 buffer u32   12 format u32   16 offset u64   24 length u64
//!   32 width u32   36 height u32   40 stride u32   44 reserved
//! ATTACHED  driver -> core, 16 bytes: 8 buffer u32   12 status u32
//! SCANOUT   core -> driver, 32 bytes
//!   8 scanout u32   12 buffer u32 (0: off)   16 rect x, y, width, height u32
//! FLUSH     core -> driver, 40 bytes
//!   8 buffer u32   12 reserved   16 sequence u64   24 rect x, y, width, height u32
//! FLIPPED   driver -> core, 24 bytes: 8 sequence u64   16 status u32   20 reserved
//! DETACH    core -> driver, 16 bytes: 8 buffer u32   12 reserved
//! DETACHED  driver -> core, 16 bytes: 8 buffer u32   12 status u32
//! STOP, STOPPED               8 bytes
//! ```
//!
//! Reserved bytes are written as zero and a message with any of them set is
//! malformed, so they can be given a meaning later without an old reader
//! misreading them.

use ::core::fmt;

use ferrix_linux_abi::drm::FORMAT_XRGB8888;
use ferrix_native_abi::rights::Rights;

/// The protocol version this crate speaks.
pub const VERSION: u16 = 1;

/// HELLO's type.
pub const HELLO: u32 = 1;
/// READY's type.
pub const READY: u32 = 2;
/// REFUSED's type.
pub const REFUSED: u32 = 3;
/// ATTACH's type.
pub const ATTACH: u32 = 4;
/// ATTACHED's type.
pub const ATTACHED: u32 = 5;
/// SCANOUT's type.
pub const SCANOUT: u32 = 6;
/// FLUSH's type.
pub const FLUSH: u32 = 7;
/// FLIPPED's type.
pub const FLIPPED: u32 = 8;
/// DETACH's type.
pub const DETACH: u32 = 9;
/// DETACHED's type.
pub const DETACHED: u32 = 10;
/// STOP's type.
pub const STOP: u32 = 11;
/// STOPPED's type.
pub const STOPPED: u32 = 12;

/// Bytes of the type and length, and all of STOP and STOPPED.
pub const HEADER_BYTES: usize = 8;
/// The most scanouts a HELLO describes: virtio-gpu's own limit.
pub const MAX_SCANOUTS: usize = 16;
/// Bytes of one scanout in HELLO.
pub const SCANOUT_BYTES: usize = 12;
/// Bytes of HELLO.
pub const HELLO_BYTES: usize = 16 + MAX_SCANOUTS * SCANOUT_BYTES;
/// Bytes of the longest message.
pub const MAX_BYTES: usize = HELLO_BYTES;

/// The largest width or height a mode may have: `docs/DISPLAY.md` §2.2.
pub const MAX_DIMENSION: u32 = 8192;

/// The only pixel format iteration 1 attaches: DRM's `XRGB8888`.
pub const FORMAT: u32 = FORMAT_XRGB8888;

/// Bytes per pixel of [`FORMAT`].
pub const BYTES_PER_PIXEL: u32 = 4;

/// The page size buffer ranges are aligned to.
pub const PAGE_SIZE: u64 = 4096;

/// Exactly the rights each side holds the other's port with.
pub const PORT_RIGHTS: Rights = Rights(Rights::WRITE.0 | Rights::TRANSFER.0);

/// Exactly the rights the driver holds the card VMO with: it may read and pin
/// it and was handed it, and may neither write, map nor copy it.
pub const CARD_VMO_RIGHTS: Rights = Rights(Rights::READ.0 | Rights::TRANSFER.0);

/// One scanout as HELLO describes it: its preferred mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScanoutMode {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Whether a display is attached.
    pub enabled: bool,
}

/// A rectangle in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Rect {
    /// Whether the rectangle is non-empty and lies inside `width` × `height`.
    #[must_use]
    pub fn inside(self, width: u32, height: u32) -> bool {
        self.width != 0
            && self.height != 0
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|end| end <= width)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|end| end <= height)
    }
}

/// HELLO: the driver introduces its device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hello {
    /// The protocol version.
    pub version: u16,
    /// How many of `modes` describe scanouts.
    pub scanouts: u16,
    /// The device's PCI location, as START gave it.
    pub location: u32,
    /// Each scanout's preferred mode; entries past `scanouts` are zero.
    pub modes: [ScanoutMode; MAX_SCANOUTS],
}

/// Why the core refuses a driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Refusal {
    /// The version is not [`VERSION`].
    Version = 1,
    /// `scanouts` is 0 or above [`MAX_SCANOUTS`].
    Scanouts = 2,
    /// A mode is zero-sized while enabled, above [`MAX_DIMENSION`], or set
    /// past `scanouts`.
    Mode = 3,
    /// A handle is missing, extra, or has rights other than exactly the
    /// specified ones.
    Rights = 4,
    /// The message is not a well-formed HELLO.
    Malformed = 5,
    /// `location` is not the device the channel was made for. Decided by the
    /// glue.
    WrongLocation = 6,
    /// The driver answered something the core did not ask. Decided by
    /// [`crate::session::Session`].
    Protocol = 7,
}

impl Refusal {
    /// The refusal a reason word names, if any.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => Self::Version,
            2 => Self::Scanouts,
            3 => Self::Mode,
            4 => Self::Rights,
            5 => Self::Malformed,
            6 => Self::WrongLocation,
            7 => Self::Protocol,
            _ => return None,
        })
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Version => "display protocol version mismatch",
            Self::Scanouts => "scanout count out of range",
            Self::Mode => "a scanout mode is not one a display can have",
            Self::Rights => "a handle has the wrong rights",
            Self::Malformed => "malformed HELLO",
            Self::WrongLocation => "HELLO names another device",
            Self::Protocol => "the driver broke the protocol",
        })
    }
}

impl Hello {
    /// HELLO's handles, in order, with exactly the rights each must carry.
    pub const HANDLE_RIGHTS: [Rights; 1] = [PORT_RIGHTS];

    /// Check a HELLO and the rights of the handles it came with.
    pub fn validate(&self, handle_rights: &[Rights]) -> Result<(), Refusal> {
        if self.version != VERSION {
            return Err(Refusal::Version);
        }
        let count = usize::from(self.scanouts);
        if count == 0 || count > MAX_SCANOUTS {
            return Err(Refusal::Scanouts);
        }
        for (index, mode) in self.modes.iter().enumerate() {
            let fine = if index >= count {
                *mode == ScanoutMode::default()
            } else if mode.enabled {
                (1..=MAX_DIMENSION).contains(&mode.width)
                    && (1..=MAX_DIMENSION).contains(&mode.height)
            } else {
                mode.width <= MAX_DIMENSION && mode.height <= MAX_DIMENSION
            };
            if !fine {
                return Err(Refusal::Mode);
            }
        }
        if handle_rights != Self::HANDLE_RIGHTS {
            return Err(Refusal::Rights);
        }
        Ok(())
    }
}

/// READY: the core accepts the driver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ready {
    /// The card's number, `card<N>`.
    pub card: u32,
    /// Bytes of the card VMO, which every ATTACH's range lies inside.
    pub card_bytes: u64,
}

impl Ready {
    /// READY's handles, in order: the card VMO and the core's port.
    pub const HANDLE_RIGHTS: [Rights; 2] = [CARD_VMO_RIGHTS, Rights::WRITE];
}

/// ATTACH: make a range of the card VMO a buffer the device can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attach {
    /// The buffer's id, not 0.
    pub buffer: u32,
    /// Its fourcc, [`FORMAT`].
    pub format: u32,
    /// Where its range starts in the card VMO, page-aligned.
    pub offset: u64,
    /// Its range's length, whole pages.
    pub length: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
}

/// Why an ATTACH is not one a driver can act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttachError {
    /// Buffer id 0.
    Id,
    /// A format other than [`FORMAT`].
    Format,
    /// A size of 0 or above [`MAX_DIMENSION`].
    Size,
    /// A stride under `width` × 4, or rows that do not fit the range.
    Stride,
    /// A range that is not whole pages, is empty, or runs past the card VMO.
    Range,
}

impl Attach {
    /// Check the buffer against itself and a card VMO of `card_bytes`.
    pub fn validate(&self, card_bytes: u64) -> Result<(), AttachError> {
        if self.buffer == 0 {
            return Err(AttachError::Id);
        }
        if self.format != FORMAT {
            return Err(AttachError::Format);
        }
        if !(1..=MAX_DIMENSION).contains(&self.width) || !(1..=MAX_DIMENSION).contains(&self.height)
        {
            return Err(AttachError::Size);
        }
        let row = u64::from(self.width) * u64::from(BYTES_PER_PIXEL);
        let pixels = u64::from(self.stride) * u64::from(self.height);
        if u64::from(self.stride) < row || pixels > self.length {
            return Err(AttachError::Stride);
        }
        let end = self.offset.checked_add(self.length);
        if self.length == 0
            || !self.offset.is_multiple_of(PAGE_SIZE)
            || !self.length.is_multiple_of(PAGE_SIZE)
            || end.is_none_or(|end| end > card_bytes)
        {
            return Err(AttachError::Range);
        }
        Ok(())
    }
}

/// The status a driver reports for ATTACH, FLUSH or DETACH.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Status {
    /// Done.
    Ok = 0,
    /// The device refused the command.
    DeviceRefused = 1,
    /// Pinning the range failed.
    PinFailed = 2,
    /// The driver or device ran out of memory.
    OutOfMemory = 3,
    /// The request was not one the driver could act on.
    Invalid = 4,
}

impl Status {
    /// The status a word names, if any.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0 => Self::Ok,
            1 => Self::DeviceRefused,
            2 => Self::PinFailed,
            3 => Self::OutOfMemory,
            4 => Self::Invalid,
            _ => return None,
        })
    }
}

/// Every message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Message {
    /// Driver to core.
    Hello(Hello),
    /// Core to driver.
    Ready(Ready),
    /// Core to driver.
    Refused(Refusal),
    /// Core to driver.
    Attach(Attach),
    /// Driver to core.
    Attached {
        /// The buffer.
        buffer: u32,
        /// How it went.
        status: Status,
    },
    /// Core to driver.
    Scanout {
        /// The scanout.
        scanout: u32,
        /// The buffer, or 0 to turn the scanout off.
        buffer: u32,
        /// The part of the buffer to show.
        rect: Rect,
    },
    /// Core to driver.
    Flush {
        /// The buffer.
        buffer: u32,
        /// The flush's number, counting from 1 per driver.
        sequence: u64,
        /// What changed.
        rect: Rect,
    },
    /// Driver to core.
    Flipped {
        /// The flush that finished.
        sequence: u64,
        /// How it went.
        status: Status,
    },
    /// Core to driver.
    Detach {
        /// The buffer.
        buffer: u32,
    },
    /// Driver to core.
    Detached {
        /// The buffer.
        buffer: u32,
        /// How it went.
        status: Status,
    },
    /// Core to driver.
    Stop,
    /// Driver to core.
    Stopped,
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
            Self::Attach(_) => ATTACH,
            Self::Attached { .. } => ATTACHED,
            Self::Scanout { .. } => SCANOUT,
            Self::Flush { .. } => FLUSH,
            Self::Flipped { .. } => FLIPPED,
            Self::Detach { .. } => DETACH,
            Self::Detached { .. } => DETACHED,
            Self::Stop => STOP,
            Self::Stopped => STOPPED,
        }
    }

    /// The fixed length of a message of type `kind`.
    #[must_use]
    pub const fn length_of(kind: u32) -> Option<usize> {
        Some(match kind {
            HELLO => HELLO_BYTES,
            READY => 24,
            REFUSED => 12,
            ATTACH => 48,
            ATTACHED | DETACH | DETACHED => 16,
            SCANOUT => 32,
            FLUSH => 40,
            FLIPPED => 24,
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
        match *self {
            Self::Hello(hello) => {
                put16(bytes, 8, hello.version);
                put16(bytes, 10, hello.scanouts);
                put32(bytes, 12, hello.location);
                for (index, mode) in hello.modes.iter().enumerate() {
                    let at = 16 + index * SCANOUT_BYTES;
                    put32(bytes, at, mode.width);
                    put32(bytes, at + 4, mode.height);
                    put32(bytes, at + 8, u32::from(mode.enabled));
                }
            }
            Self::Ready(ready) => {
                put32(bytes, 8, ready.card);
                put64(bytes, 16, ready.card_bytes);
            }
            Self::Refused(reason) => put32(bytes, 8, reason as u32),
            Self::Attach(attach) => {
                put32(bytes, 8, attach.buffer);
                put32(bytes, 12, attach.format);
                put64(bytes, 16, attach.offset);
                put64(bytes, 24, attach.length);
                put32(bytes, 32, attach.width);
                put32(bytes, 36, attach.height);
                put32(bytes, 40, attach.stride);
            }
            Self::Attached { buffer, status } | Self::Detached { buffer, status } => {
                put32(bytes, 8, buffer);
                put32(bytes, 12, status as u32);
            }
            Self::Scanout {
                scanout,
                buffer,
                rect,
            } => {
                put32(bytes, 8, scanout);
                put32(bytes, 12, buffer);
                put_rect(bytes, 16, rect);
            }
            Self::Flush {
                buffer,
                sequence,
                rect,
            } => {
                put32(bytes, 8, buffer);
                put64(bytes, 16, sequence);
                put_rect(bytes, 24, rect);
            }
            Self::Flipped { sequence, status } => {
                put64(bytes, 8, sequence);
                put32(bytes, 16, status as u32);
            }
            Self::Detach { buffer } => put32(bytes, 8, buffer),
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

fn decode_body(kind: u32, bytes: &[u8]) -> Option<Message> {
    let zero32 = |at: usize| get32(bytes, at).filter(|&value| value == 0).map(drop);
    let status = |at: usize| get32(bytes, at).and_then(Status::from_raw);
    Some(match kind {
        HELLO => {
            let mut modes = [ScanoutMode::default(); MAX_SCANOUTS];
            for (index, mode) in modes.iter_mut().enumerate() {
                let at = 16 + index * SCANOUT_BYTES;
                *mode = ScanoutMode {
                    width: get32(bytes, at)?,
                    height: get32(bytes, at + 4)?,
                    enabled: match get32(bytes, at + 8)? {
                        0 => false,
                        1 => true,
                        _ => return None,
                    },
                };
            }
            Message::Hello(Hello {
                version: get16(bytes, 8)?,
                scanouts: get16(bytes, 10)?,
                location: get32(bytes, 12)?,
                modes,
            })
        }
        READY => {
            zero32(12)?;
            Message::Ready(Ready {
                card: get32(bytes, 8)?,
                card_bytes: get64(bytes, 16)?,
            })
        }
        REFUSED => Message::Refused(Refusal::from_raw(get32(bytes, 8)?)?),
        ATTACH => {
            zero32(44)?;
            Message::Attach(Attach {
                buffer: get32(bytes, 8)?,
                format: get32(bytes, 12)?,
                offset: get64(bytes, 16)?,
                length: get64(bytes, 24)?,
                width: get32(bytes, 32)?,
                height: get32(bytes, 36)?,
                stride: get32(bytes, 40)?,
            })
        }
        ATTACHED => Message::Attached {
            buffer: get32(bytes, 8)?,
            status: status(12)?,
        },
        DETACHED => Message::Detached {
            buffer: get32(bytes, 8)?,
            status: status(12)?,
        },
        SCANOUT => Message::Scanout {
            scanout: get32(bytes, 8)?,
            buffer: get32(bytes, 12)?,
            rect: get_rect(bytes, 16)?,
        },
        FLUSH => {
            zero32(12)?;
            Message::Flush {
                buffer: get32(bytes, 8)?,
                sequence: get64(bytes, 16)?,
                rect: get_rect(bytes, 24)?,
            }
        }
        FLIPPED => {
            zero32(20)?;
            Message::Flipped {
                sequence: get64(bytes, 8)?,
                status: status(16)?,
            }
        }
        DETACH => {
            zero32(12)?;
            Message::Detach {
                buffer: get32(bytes, 8)?,
            }
        }
        STOP => Message::Stop,
        STOPPED => Message::Stopped,
        _ => return None,
    })
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

fn get64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn get_rect(bytes: &[u8], at: usize) -> Option<Rect> {
    Some(Rect {
        x: get32(bytes, at)?,
        y: get32(bytes, at + 4)?,
        width: get32(bytes, at + 8)?,
        height: get32(bytes, at + 12)?,
    })
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

fn put64(out: &mut [u8], at: usize, value: u64) {
    put(out, at, &value.to_le_bytes());
}

fn put_rect(out: &mut [u8], at: usize, rect: Rect) {
    put32(out, at, rect.x);
    put32(out, at + 4, rect.y);
    put32(out, at + 8, rect.width);
    put32(out, at + 12, rect.height);
}
