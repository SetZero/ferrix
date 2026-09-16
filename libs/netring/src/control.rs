//! The control channel: how a ring is set up, described and ended.
//!
//! Every message is a kind byte and then its fields, little-endian, with the
//! handles travelling beside the bytes on the channel. Nothing here sends
//! anything: a message is encoded into the caller's buffer and decoded from
//! the caller's buffer, so both sides link one definition of the wire and the
//! glue keeps the channel.
//!
//! # The order
//!
//! 1. Whoever starts the driver -- `devmgr`, or the kernel's own check --
//!    sends **START**: which device it has, and where its register blocks are.
//! 2. The driver brings the device up, makes its ring and data VMOs and its
//!    port, and sends **HELLO** with them and with the interface's
//!    description.
//! 3. The kernel answers **READY** with its completion port, or **REFUSED**
//!    with the first reason in the order [`Refusal`] fixes. A refusal closes
//!    the kernel's end.
//! 4. The driver tells the kernel when the link comes up or goes down with
//!    **LINK**.
//! 5. **STOP** asks the driver to stop, reset its device and answer
//!    **STOPPED**. A driver that dies is an implicit STOPPED without the
//!    promise that its device was reset.

use ferrix_native_abi::rights::Rights;

use crate::layout::{
    MAX_ENTRIES, MAX_SLOT_BYTES, MIN_ENTRIES, MIN_SLOT_BYTES, SLOT_ALIGN, VERSION,
};

/// The longest interface name, matching Linux's `IFNAMSIZ` less its
/// terminator.
pub const MAX_NAME: usize = 15;

/// The longest message this protocol has.
pub const MAX_MESSAGE: usize = 64;

/// Exactly the rights each side holds the ring and data VMOs with: read,
/// write and map, because both sides look at the memory, and the `TRANSFER`
/// that carried them; never `DUPLICATE`, so the memory a HELLO describes
/// cannot grow a second owner behind the kernel's back.
pub const VMO_RIGHTS: Rights =
    Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0 | Rights::TRANSFER.0);

/// Exactly the rights each side holds the other's bell port with: it rings
/// it and was handed it, and it may not read it or wait on it.
pub const PORT_RIGHTS: Rights = Rights(Rights::WRITE.0 | Rights::TRANSFER.0);

/// HELLO's handles, in order, with exactly the rights each must carry: the
/// ring VMO, the data VMO, and the driver's bell.
pub const HELLO_RIGHTS: [Rights; 3] = [VMO_RIGHTS, VMO_RIGHTS, PORT_RIGHTS];

/// READY's handle, with exactly the rights it must carry: the kernel's bell.
pub const READY_RIGHTS: [Rights; 1] = [Rights::WRITE];

/// Exactly the rights a driver holds its end of the ring's control channel
/// with: it sends and receives on it, waits on it and was handed it, and
/// nobody copies it. The kernel inserts the end `net_ring_create` answers
/// with these, and `devmgr` passes it on unchanged.
pub const CONTROL_RIGHTS: Rights =
    Rights(Rights::TRANSFER.0 | Rights::READ.0 | Rights::WRITE.0 | Rights::WAIT.0);

/// Exactly the rights a driver holds its device with: `MANAGE`, for
/// `vmo_pin`, `interrupt_create` and `io_mapping_create`, and the `TRANSFER`
/// it arrived with.
pub const DEVICE_RIGHTS: Rights = Rights(Rights::TRANSFER.0 | Rights::MANAGE.0);

/// START's handles, in order, with exactly the rights each must carry.
pub const START_RIGHTS: [Rights; 2] = [DEVICE_RIGHTS, CONTROL_RIGHTS];

/// What an interface can do, as the driver reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct InterfaceFlags {
    /// The link is up: a frame put on it may reach somebody.
    pub carrier: bool,
    /// The device takes broadcast frames.
    pub broadcast: bool,
    /// The device takes multicast frames.
    pub multicast: bool,
}

impl InterfaceFlags {
    /// The bits these flags are written as.
    #[must_use]
    pub const fn bits(self) -> u32 {
        (self.carrier as u32) | ((self.broadcast as u32) << 1) | ((self.multicast as u32) << 2)
    }

    /// The flags those bits name, ignoring any this version does not define.
    #[must_use]
    pub const fn from_bits(bits: u32) -> InterfaceFlags {
        InterfaceFlags {
            carrier: bits & 1 != 0,
            broadcast: bits & 2 != 0,
            multicast: bits & 4 != 0,
        }
    }
}

/// What the driver says its interface is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interface {
    /// The name, without a terminator; at most [`MAX_NAME`] bytes.
    pub name: [u8; MAX_NAME],
    /// How many of those bytes are the name.
    pub name_len: u8,
    /// The hardware address.
    pub mac: [u8; 6],
    /// The largest payload a frame may carry.
    pub mtu: u32,
    /// What it can do.
    pub flags: InterfaceFlags,
}

impl Interface {
    /// An interface with that name, address and MTU.
    ///
    /// A name longer than [`MAX_NAME`] is cut, because a driver that offers
    /// one is a driver with a bug and refusing the whole ring over a long name
    /// helps nobody.
    #[must_use]
    pub fn new(name: &[u8], mac: [u8; 6], mtu: u32, flags: InterfaceFlags) -> Interface {
        let mut bytes = [0_u8; MAX_NAME];
        let mut len = 0_u8;
        for (slot, byte) in bytes.iter_mut().zip(name.iter().take(MAX_NAME)) {
            *slot = *byte;
            len += 1;
        }
        Interface {
            name: bytes,
            name_len: len,
            mac,
            mtu,
            flags,
        }
    }

    /// The name's bytes.
    #[must_use]
    pub fn name(&self) -> &[u8] {
        self.name.get(..usize::from(self.name_len)).unwrap_or(&[])
    }
}

/// What the driver offers the kernel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hello {
    /// The protocol version the driver speaks.
    pub version: u16,
    /// How many entries each ring has.
    pub entries: u32,
    /// How many bytes a slot of the data VMO holds.
    pub slot_bytes: u32,
    /// What the interface is.
    pub interface: Interface,
}

impl Hello {
    /// Whether this is an offer the kernel can take, and the first reason it
    /// cannot.
    ///
    /// The order is fixed so that a driver that is wrong in two ways is told
    /// the same thing every time.
    ///
    /// # Errors
    ///
    /// The first [`Refusal`] that applies.
    pub fn validate(&self) -> Result<(), Refusal> {
        if self.version != VERSION {
            return Err(Refusal::Version);
        }
        if !(MIN_ENTRIES..=MAX_ENTRIES).contains(&self.entries) || !self.entries.is_power_of_two() {
            return Err(Refusal::Entries);
        }
        if !(MIN_SLOT_BYTES..=MAX_SLOT_BYTES).contains(&self.slot_bytes)
            || !self.slot_bytes.is_multiple_of(SLOT_ALIGN)
        {
            return Err(Refusal::SlotBytes);
        }
        if self.interface.name_len == 0 || usize::from(self.interface.name_len) > MAX_NAME {
            return Err(Refusal::Name);
        }
        if self.interface.name().iter().any(|byte| {
            !byte.is_ascii_alphanumeric() && *byte != b'-' && *byte != b'_' && *byte != b'.'
        }) {
            return Err(Refusal::Name);
        }
        // An MTU below the smallest slot would let a frame the interface
        // promises to carry not fit the ring it is carried in.
        if self.interface.mtu < 68 || self.interface.mtu + 14 > self.slot_bytes {
            return Err(Refusal::Mtu);
        }
        Ok(())
    }
}

/// Why the kernel would not take a ring.
///
/// The numbers are part of the protocol: a driver logs the one it was given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The driver speaks another version.
    Version,
    /// `entries` is not a power of two in range.
    Entries,
    /// `slot_bytes` is out of range or misaligned.
    SlotBytes,
    /// A handle was missing, of the wrong kind, or carried the wrong rights.
    Handles,
    /// The ring VMO is too small for the header and the two arrays.
    RingTooSmall,
    /// The data VMO is too small for `entries` slots.
    DataTooSmall,
    /// The interface's name is empty, too long, or not a name.
    Name,
    /// The MTU is too small to be an interface's, or too large for a slot.
    Mtu,
    /// An interface of that name is already up.
    NameInUse,
    /// This device already has a ring.
    DeviceInUse,
    /// The message could not be read.
    Malformed,
}

impl Refusal {
    /// The number this refusal is sent as.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Refusal::Version => 1,
            Refusal::Entries => 2,
            Refusal::SlotBytes => 3,
            Refusal::Handles => 4,
            Refusal::Malformed => 5,
            Refusal::RingTooSmall => 6,
            Refusal::DataTooSmall => 7,
            Refusal::Name => 8,
            Refusal::Mtu => 9,
            Refusal::NameInUse => 10,
            Refusal::DeviceInUse => 11,
        }
    }

    /// The refusal a number names.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Refusal> {
        match code {
            1 => Some(Refusal::Version),
            2 => Some(Refusal::Entries),
            3 => Some(Refusal::SlotBytes),
            4 => Some(Refusal::Handles),
            5 => Some(Refusal::Malformed),
            6 => Some(Refusal::RingTooSmall),
            7 => Some(Refusal::DataTooSmall),
            8 => Some(Refusal::Name),
            9 => Some(Refusal::Mtu),
            10 => Some(Refusal::NameInUse),
            11 => Some(Refusal::DeviceInUse),
            _ => None,
        }
    }
}

/// What the driver is told to drive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Start {
    /// Which interface number the kernel would like it to be, or zero for
    /// "choose one".
    pub index: u32,
    /// Which PCI segment, bus, device and function it is, for the name.
    pub location: u32,
}

/// A message on the control channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Message {
    /// Kernel to driver: drive this device.
    Start(Start),
    /// Driver to kernel: here is my ring, my memory and my interface.
    Hello(Hello),
    /// Kernel to driver: taken; my completion port travels with this.
    Ready,
    /// Kernel to driver: not taken, and why.
    Refused(Refusal),
    /// Driver to kernel: the link came up, or went down.
    Link(bool),
    /// Kernel to driver: stop, reset the device, and say so.
    Stop,
    /// Driver to kernel: stopped, and the device was reset.
    Stopped,
}

/// Why a message could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageError {
    /// The buffer is empty, or shorter than the kind needs.
    Truncated,
    /// The kind byte is not one this version has.
    UnknownKind(u8),
    /// A field holds a value the protocol does not allow.
    Malformed,
}

/// The kind bytes.
mod kind {
    /// [`super::Message::Start`].
    pub(super) const START: u8 = 1;
    /// [`super::Message::Hello`].
    pub(super) const HELLO: u8 = 2;
    /// [`super::Message::Ready`].
    pub(super) const READY: u8 = 3;
    /// [`super::Message::Refused`].
    pub(super) const REFUSED: u8 = 4;
    /// [`super::Message::Link`].
    pub(super) const LINK: u8 = 5;
    /// [`super::Message::Stop`].
    pub(super) const STOP: u8 = 6;
    /// [`super::Message::Stopped`].
    pub(super) const STOPPED: u8 = 7;
}

impl Message {
    /// Write this message at the start of `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`MessageError::Truncated`] if `out` is shorter than the message.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, MessageError> {
        let mut writer = Writer { out, at: 0 };
        match self {
            Message::Start(start) => {
                writer.byte(kind::START)?;
                writer.word(start.index)?;
                writer.word(start.location)?;
            }
            Message::Hello(hello) => {
                writer.byte(kind::HELLO)?;
                writer.half(hello.version)?;
                writer.word(hello.entries)?;
                writer.word(hello.slot_bytes)?;
                writer.word(hello.interface.mtu)?;
                writer.word(hello.interface.flags.bits())?;
                writer.byte(hello.interface.name_len)?;
                writer.bytes(&hello.interface.name)?;
                writer.bytes(&hello.interface.mac)?;
            }
            Message::Ready => writer.byte(kind::READY)?,
            Message::Refused(refusal) => {
                writer.byte(kind::REFUSED)?;
                writer.byte(refusal.code())?;
            }
            Message::Link(up) => {
                writer.byte(kind::LINK)?;
                writer.byte(u8::from(*up))?;
            }
            Message::Stop => writer.byte(kind::STOP)?,
            Message::Stopped => writer.byte(kind::STOPPED)?,
        }
        Ok(writer.at)
    }

    /// Read the message at the start of `bytes`.
    ///
    /// # Errors
    ///
    /// [`MessageError`], naming what could not be read.
    pub fn decode(bytes: &[u8]) -> Result<Message, MessageError> {
        let mut reader = Reader { bytes, at: 0 };
        match reader.byte()? {
            kind::START => Ok(Message::Start(Start {
                index: reader.word()?,
                location: reader.word()?,
            })),
            kind::HELLO => Message::decode_hello(&mut reader),
            kind::READY => Ok(Message::Ready),
            kind::REFUSED => Refusal::from_code(reader.byte()?)
                .map(Message::Refused)
                .ok_or(MessageError::Malformed),
            kind::LINK => Ok(Message::Link(reader.byte()? != 0)),
            kind::STOP => Ok(Message::Stop),
            kind::STOPPED => Ok(Message::Stopped),
            other => Err(MessageError::UnknownKind(other)),
        }
    }

    /// The body of a HELLO.
    fn decode_hello(reader: &mut Reader<'_>) -> Result<Message, MessageError> {
        let version = reader.half()?;
        let entries = reader.word()?;
        let slot_bytes = reader.word()?;
        let mtu = reader.word()?;
        let flags = InterfaceFlags::from_bits(reader.word()?);
        let name_len = reader.byte()?;
        let mut name = [0_u8; MAX_NAME];
        reader.fill(&mut name)?;
        let mut mac = [0_u8; 6];
        reader.fill(&mut mac)?;
        if usize::from(name_len) > MAX_NAME {
            return Err(MessageError::Malformed);
        }
        Ok(Message::Hello(Hello {
            version,
            entries,
            slot_bytes,
            interface: Interface {
                name,
                name_len,
                mac,
                mtu,
                flags,
            },
        }))
    }
}

/// A cursor writing little-endian fields.
struct Writer<'a> {
    /// Where the message goes.
    out: &'a mut [u8],
    /// How much has been written.
    at: usize,
}

impl Writer<'_> {
    /// One byte.
    fn byte(&mut self, value: u8) -> Result<(), MessageError> {
        let slot = self.out.get_mut(self.at).ok_or(MessageError::Truncated)?;
        *slot = value;
        self.at += 1;
        Ok(())
    }

    /// Two bytes.
    fn half(&mut self, value: u16) -> Result<(), MessageError> {
        self.bytes(&value.to_le_bytes())
    }

    /// Four bytes.
    fn word(&mut self, value: u32) -> Result<(), MessageError> {
        self.bytes(&value.to_le_bytes())
    }

    /// A run of bytes.
    fn bytes(&mut self, value: &[u8]) -> Result<(), MessageError> {
        let end = self.at + value.len();
        let slot = self
            .out
            .get_mut(self.at..end)
            .ok_or(MessageError::Truncated)?;
        slot.copy_from_slice(value);
        self.at = end;
        Ok(())
    }
}

/// A cursor reading little-endian fields.
struct Reader<'a> {
    /// The message.
    bytes: &'a [u8],
    /// How much has been read.
    at: usize,
}

impl Reader<'_> {
    /// One byte.
    fn byte(&mut self) -> Result<u8, MessageError> {
        let value = *self.bytes.get(self.at).ok_or(MessageError::Truncated)?;
        self.at += 1;
        Ok(value)
    }

    /// Two bytes.
    fn half(&mut self) -> Result<u16, MessageError> {
        let mut bytes = [0_u8; 2];
        self.fill(&mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Four bytes.
    fn word(&mut self) -> Result<u32, MessageError> {
        let mut bytes = [0_u8; 4];
        self.fill(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// As many bytes as `out` holds.
    fn fill(&mut self, out: &mut [u8]) -> Result<(), MessageError> {
        let end = self.at + out.len();
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or(MessageError::Truncated)?;
        out.copy_from_slice(slice);
        self.at = end;
        Ok(())
    }
}
