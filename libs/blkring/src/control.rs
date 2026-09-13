//! The control plane: HELLO, READY, REFUSED, STOP and STOPPED.
//!
//! Every message is a fixed little-endian structure whose first four bytes are
//! its type and next four its length; handles ride in the channel message's
//! handle array, and the glue passes the rights it read off them to
//! [`Hello::validate`].
//!
//! ```text
//! HELLO    driver -> kernel, 40 bytes, handles [ring VMO, data VMO, driver port]
//!   0 type 1   4 length   8 version u16   10 queues u16   12 block_size u32
//!   16 capacity u64   24 max_sectors u32   28 device_flags u32
//!   32 data_vmo_size u64
//! READY    kernel -> driver, 8 bytes, handles [kernel completion port]
//! REFUSED  kernel -> driver, 12 bytes, no handles
//!   8 reason u32, a Refusal
//! STOP     kernel -> driver, 8 bytes
//! STOPPED  driver -> kernel, 8 bytes
//! ```

use core::fmt;

use ferrix_native_abi::rights::Rights;

use crate::geometry::{Device, DeviceFlags};
use crate::layout::VERSION;

/// HELLO's message type.
pub const HELLO: u32 = 1;
/// READY's message type.
pub const READY: u32 = 2;
/// REFUSED's message type.
pub const REFUSED: u32 = 3;
/// STOP's message type.
pub const STOP: u32 = 4;
/// STOPPED's message type.
pub const STOPPED: u32 = 5;

/// Bytes of the type and length every message starts with, and all of READY,
/// STOP and STOPPED.
pub const HEADER_BYTES: usize = 8;
/// Bytes of HELLO.
pub const HELLO_BYTES: usize = 40;
/// Bytes of REFUSED.
pub const REFUSED_BYTES: usize = 12;
/// Bytes of the longest message.
pub const MAX_BYTES: usize = HELLO_BYTES;

/// Ring pairs a v1 HELLO may announce.
pub const QUEUES: u16 = 1;

/// Exactly the rights the kernel holds a ring or data VMO with: no `DUPLICATE`,
/// no `TRANSFER`.
pub const VMO_RIGHTS: Rights = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

/// Exactly the rights each side holds the other's port with.
pub const PORT_RIGHTS: Rights = Rights::WRITE;

/// HELLO's handles, in order, with exactly the rights each must carry.
pub const HELLO_RIGHTS: [Rights; 3] = [VMO_RIGHTS, VMO_RIGHTS, PORT_RIGHTS];

/// READY's handle, with exactly the rights it must carry.
pub const READY_RIGHTS: [Rights; 1] = [PORT_RIGHTS];

/// Field offsets of HELLO.
pub mod hello {
    /// `type`, `u32`.
    pub const TYPE: usize = 0;
    /// `length`, `u32`.
    pub const LENGTH: usize = 4;
    /// Ring `version`, `u16`.
    pub const VERSION: usize = 8;
    /// `queues`, `u16`.
    pub const QUEUES: usize = 10;
    /// `block_size`, `u32`.
    pub const BLOCK_SIZE: usize = 12;
    /// `capacity`, `u64`, in `block_size` sectors.
    pub const CAPACITY: usize = 16;
    /// `max_sectors`, `u32`.
    pub const MAX_SECTORS: usize = 24;
    /// `device_flags`, `u32`.
    pub const DEVICE_FLAGS: usize = 28;
    /// `data_vmo_size`, `u64`.
    pub const DATA_VMO_SIZE: usize = 32;
}

/// Field offsets of REFUSED beyond the common header.
pub mod refused {
    /// `reason`, `u32`: a [`super::Refusal`].
    pub const REASON: usize = 8;
}

/// Why the kernel refused a HELLO, as REFUSED carries it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Refusal {
    /// The ring version is not 1.
    Version = 1,
    /// `queues` is not 1.
    Queues = 2,
    /// The ring header failed setup validation.
    Header = 3,
    /// A handle is missing, extra, or carries rights other than exactly the
    /// specified ones.
    Rights = 4,
    /// The message is not a well-formed HELLO.
    Malformed = 5,
    /// The device description is not one the kernel can use.
    Device = 6,
}

impl Refusal {
    /// The reason word this refusal is sent as.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self as u32
    }

    /// The refusal a reason word names, if it names one.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Refusal> {
        match raw {
            1 => Some(Refusal::Version),
            2 => Some(Refusal::Queues),
            3 => Some(Refusal::Header),
            4 => Some(Refusal::Rights),
            5 => Some(Refusal::Malformed),
            6 => Some(Refusal::Device),
            _ => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Refusal::Version => "ring version mismatch",
            Refusal::Queues => "queues is not 1",
            Refusal::Header => "the ring header is invalid",
            Refusal::Rights => "a handle has the wrong rights",
            Refusal::Malformed => "malformed HELLO",
            Refusal::Device => "unusable device description",
        })
    }
}

/// HELLO's fields, as sent and as received.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hello {
    /// The ring version.
    pub version: u16,
    /// Ring pairs.
    pub queues: u16,
    /// Bytes in a logical sector.
    pub block_size: u32,
    /// Sectors on the device.
    pub capacity: u64,
    /// The most sectors one request may carry.
    pub max_sectors: u32,
    /// [`DeviceFlags`], as a word.
    pub device_flags: u32,
    /// Bytes in the data VMO.
    pub data_vmo_size: u64,
}

impl Hello {
    /// The v1 HELLO for `device`.
    #[must_use]
    pub const fn for_device(device: &Device) -> Self {
        Self {
            version: VERSION,
            queues: QUEUES,
            block_size: device.block_size(),
            capacity: device.capacity(),
            max_sectors: device.max_sectors(),
            device_flags: device.flags().0,
            data_vmo_size: device.data_vmo_size(),
        }
    }

    /// Every check the kernel makes on a HELLO before it looks at the ring,
    /// given the rights of the handles that came with it, in order.
    ///
    /// Rights must match [`HELLO_RIGHTS`] exactly, and there must be exactly
    /// three: more rights than specified is refused as firmly as fewer. The
    /// glue must also check the handles' object types, which only it can see.
    /// The ring header is checked afterwards, by [`crate::KernelSide::attach`].
    ///
    /// # Errors
    ///
    /// The [`Refusal`] to send.
    pub fn validate(&self, handle_rights: &[Rights]) -> Result<Device, Refusal> {
        if self.version != VERSION {
            return Err(Refusal::Version);
        }
        if self.queues != QUEUES {
            return Err(Refusal::Queues);
        }
        if handle_rights != HELLO_RIGHTS {
            return Err(Refusal::Rights);
        }
        Device::new(
            self.block_size,
            self.capacity,
            self.max_sectors,
            DeviceFlags(self.device_flags),
            self.data_vmo_size,
        )
        .map_err(|_| Refusal::Device)
    }
}

/// One control message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Message {
    /// Driver to kernel: here is the device and the ring.
    Hello(Hello),
    /// Kernel to driver: accepted; the completion port rides along.
    Ready,
    /// Kernel to driver: refused, with the reason word. Carried raw so that a
    /// driver can log a reason newer than itself; [`Refusal::from_raw`] names
    /// the known ones.
    Refused(u32),
    /// Kernel to driver: finish up and reset.
    Stop,
    /// Driver to kernel: finished, device reset.
    Stopped,
}

/// Why bytes on the control channel are not a message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageError {
    /// Shorter than the type and length.
    Short,
    /// The type is not one v1 defines.
    UnknownType(u32),
    /// A HELLO for a ring version other than 1. Reported ahead of any length
    /// mismatch, since a newer version's HELLO may well be longer.
    Version(u16),
    /// The length field disagrees with the bytes received, or with the size
    /// this type has.
    Length,
}

impl MessageError {
    /// What the kernel sends back for a HELLO that failed to decode.
    #[must_use]
    pub const fn refusal(self) -> Refusal {
        match self {
            MessageError::Version(_) => Refusal::Version,
            MessageError::Short | MessageError::UnknownType(_) | MessageError::Length => {
                Refusal::Malformed
            }
        }
    }
}

impl fmt::Display for MessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageError::Short => formatter.write_str("shorter than a message header"),
            MessageError::UnknownType(kind) => write!(formatter, "unknown message type {kind}"),
            MessageError::Version(version) => write!(formatter, "ring version {version}"),
            MessageError::Length => formatter.write_str("wrong length"),
        }
    }
}

/// An encoded message: at most [`MAX_BYTES`], no allocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Encoded {
    bytes: [u8; MAX_BYTES],
    len: usize,
}

impl Encoded {
    /// The bytes to send.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}

impl Message {
    /// The handles this message carries.
    #[must_use]
    pub const fn handles(&self) -> usize {
        match self {
            Message::Hello(_) => HELLO_RIGHTS.len(),
            Message::Ready => READY_RIGHTS.len(),
            Message::Refused(_) | Message::Stop | Message::Stopped => 0,
        }
    }

    /// Encode the message.
    #[must_use]
    pub fn encode(&self) -> Encoded {
        let mut bytes = [0; MAX_BYTES];
        let (kind, len) = match self {
            Message::Hello(hello) => {
                put(&mut bytes, hello::VERSION, &hello.version.to_le_bytes());
                put(&mut bytes, hello::QUEUES, &hello.queues.to_le_bytes());
                put(
                    &mut bytes,
                    hello::BLOCK_SIZE,
                    &hello.block_size.to_le_bytes(),
                );
                put(&mut bytes, hello::CAPACITY, &hello.capacity.to_le_bytes());
                put(
                    &mut bytes,
                    hello::MAX_SECTORS,
                    &hello.max_sectors.to_le_bytes(),
                );
                put(
                    &mut bytes,
                    hello::DEVICE_FLAGS,
                    &hello.device_flags.to_le_bytes(),
                );
                put(
                    &mut bytes,
                    hello::DATA_VMO_SIZE,
                    &hello.data_vmo_size.to_le_bytes(),
                );
                (HELLO, HELLO_BYTES)
            }
            Message::Ready => (READY, HEADER_BYTES),
            Message::Refused(reason) => {
                put(&mut bytes, refused::REASON, &reason.to_le_bytes());
                (REFUSED, REFUSED_BYTES)
            }
            Message::Stop => (STOP, HEADER_BYTES),
            Message::Stopped => (STOPPED, HEADER_BYTES),
        };
        put(&mut bytes, hello::TYPE, &kind.to_le_bytes());
        // Every length is at most `MAX_BYTES`.
        put(&mut bytes, hello::LENGTH, &(len as u32).to_le_bytes());
        Encoded { bytes, len }
    }

    /// Decode a message received on the control channel.
    ///
    /// # Errors
    ///
    /// The [`MessageError`] saying why the bytes are not a message.
    /// [`MessageError::refusal`] names the reply to a bad HELLO.
    pub fn decode(bytes: &[u8]) -> Result<Message, MessageError> {
        let (Some(kind), Some(declared)) =
            (u32_at(bytes, hello::TYPE), u32_at(bytes, hello::LENGTH))
        else {
            return Err(MessageError::Short);
        };
        let size = match kind {
            HELLO => HELLO_BYTES,
            READY | STOP | STOPPED => HEADER_BYTES,
            REFUSED => REFUSED_BYTES,
            other => return Err(MessageError::UnknownType(other)),
        };
        if kind == HELLO
            && let Some(version) = u16_at(bytes, hello::VERSION)
            && version != VERSION
        {
            return Err(MessageError::Version(version));
        }
        if usize::try_from(declared) != Ok(bytes.len()) || bytes.len() != size {
            return Err(MessageError::Length);
        }
        decode_body(kind, bytes).ok_or(MessageError::Length)
    }
}

/// The body of a message whose type and length have been checked.
fn decode_body(kind: u32, bytes: &[u8]) -> Option<Message> {
    Some(match kind {
        HELLO => Message::Hello(Hello {
            version: u16_at(bytes, hello::VERSION)?,
            queues: u16_at(bytes, hello::QUEUES)?,
            block_size: u32_at(bytes, hello::BLOCK_SIZE)?,
            capacity: u64_at(bytes, hello::CAPACITY)?,
            max_sectors: u32_at(bytes, hello::MAX_SECTORS)?,
            device_flags: u32_at(bytes, hello::DEVICE_FLAGS)?,
            data_vmo_size: u64_at(bytes, hello::DATA_VMO_SIZE)?,
        }),
        READY => Message::Ready,
        REFUSED => Message::Refused(u32_at(bytes, refused::REASON)?),
        STOP => Message::Stop,
        STOPPED => Message::Stopped,
        _ => return None,
    })
}

/// Copy `source` into `out` at `at`, as far as `out` reaches.
fn put(out: &mut [u8], at: usize, source: &[u8]) {
    if let Some(destination) = out.get_mut(at..) {
        for (to, from) in destination.iter_mut().zip(source) {
            *to = *from;
        }
    }
}

fn array_at<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    array_at(bytes, at).map(u16::from_le_bytes)
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    array_at(bytes, at).map(u32::from_le_bytes)
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    array_at(bytes, at).map(u64::from_le_bytes)
}
