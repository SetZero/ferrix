//! The messages between the kernel and `devmgr` on its bootstrap channel,
//! as `docs/DEVMGR.md` §2 fixes them.
//!
//! Every message is a fixed little-endian structure whose first four bytes
//! are its type and next four its length. Handles ride in the channel
//! message's handle array. Pure functions over bytes, tested on the host, so
//! the kernel and the program share one definition.
//!
//! ```text
//! DEVICES    kernel -> devmgr, 24 + 32 x drivers bytes
//!   0 type 1   4 length   8 devices u32   12 drivers u32
//!   16 more u32 (devices still to come in later DEVICES messages)
//!   20 flags u32 (bit 0: the first message, which carries the job and
//!                 the images)
//!   24 name[0] [u8; 32] NUL-padded   56 name[1] ...
//!   handles: [job (first message only), device 0, device 0 again, ...,
//!             image 0 ... (first message only)]
//! REPORT     devmgr -> kernel, 16 bytes
//!   8 started u32   12 failed u32
//! PUBLISHED  kernel -> devmgr, 16 bytes
//!   8 location u32   12 reserved
//! DIED       devmgr -> kernel, 16 bytes
//!   8 location u32   12 status i32
//! RESTARTED  devmgr -> kernel, 16 bytes
//!   8 location u32   12 restarts u32 (this is the device's how-manieth)
//! ```
//!
//! A channel message carries at most 64 handles and a device takes two, so
//! a machine with many devices — ARMv7-A publishes its 32 virtio-mmio
//! transports — gets its devices over several DEVICES messages: the first
//! with the job and the images, the rest with devices only, each saying how
//! many are still to come.

#![no_std]
#![forbid(unsafe_code)]

/// DEVICES' message type.
pub const DEVICES: u32 = 1;
/// REPORT's message type.
pub const REPORT: u32 = 2;
/// PUBLISHED's message type.
pub const PUBLISHED: u32 = 3;
/// DIED's message type.
pub const DIED: u32 = 4;
/// RESTARTED's message type.
pub const RESTARTED: u32 = 5;

/// Bytes of the type and length every message starts with.
pub const HEADER_BYTES: usize = 8;
/// Bytes of DEVICES before its names.
pub const DEVICES_HEADER_BYTES: usize = 24;
/// Bytes of a driver's name in DEVICES: `PROCESS_NAME_MAX`, NUL-padded.
pub const NAME_BYTES: usize = 32;
/// Bytes of REPORT, PUBLISHED, DIED and RESTARTED.
pub const SHORT_BYTES: usize = 16;
/// The most drivers one DEVICES message names, and so the most a machine's
/// driver directory may hold.
pub const MAX_DRIVERS: usize = 16;
/// Bytes of the longest DEVICES message.
pub const DEVICES_MAX_BYTES: usize = DEVICES_HEADER_BYTES + MAX_DRIVERS * NAME_BYTES;
/// `flags` bit: this is the first DEVICES message, carrying the job and the
/// images.
pub const FLAG_FIRST: u32 = 1;

/// Why bytes on the channel are not a message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Malformed {
    /// Shorter than the type and length.
    Short,
    /// The type is not one this defines.
    UnknownType(u32),
    /// The length is not the type's, or not the bytes given.
    Length,
}

/// DEVICES as the kernel sends it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Devices<'a> {
    /// Devices in this message's handle array, two handles each.
    pub devices: u32,
    /// Devices still to come in later DEVICES messages.
    pub more: u32,
    /// Whether this is the first message: the job leads the handle array
    /// and the images follow the devices.
    pub first: bool,
    /// The drivers' names, one per image handle; empty unless `first`.
    pub names: &'a [[u8; NAME_BYTES]],
}

impl Devices<'_> {
    /// Bytes this message takes.
    #[must_use]
    pub const fn len(&self) -> usize {
        DEVICES_HEADER_BYTES + self.names.len() * NAME_BYTES
    }

    /// Whether the message carries nothing at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.devices == 0 && self.names.is_empty() && !self.first
    }

    /// Handles the message carries: the job if first, two per device, one
    /// per driver.
    #[must_use]
    pub const fn handles(&self) -> usize {
        (self.first as usize) + 2 * self.devices as usize + self.names.len()
    }

    /// Encode into `out`, answering the bytes written, or `None` if `out` is
    /// too small, the message names more drivers than [`MAX_DRIVERS`], or
    /// names drivers without being the first message.
    #[must_use]
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        if self.names.len() > MAX_DRIVERS || (!self.first && !self.names.is_empty()) {
            return None;
        }
        let len = self.len();
        let out = out.get_mut(..len)?;
        put(out, 0, &DEVICES.to_le_bytes());
        put(out, 4, &(len as u32).to_le_bytes());
        put(out, 8, &self.devices.to_le_bytes());
        put(out, 12, &(self.names.len() as u32).to_le_bytes());
        put(out, 16, &self.more.to_le_bytes());
        put(
            out,
            20,
            &(if self.first { FLAG_FIRST } else { 0 }).to_le_bytes(),
        );
        for (i, name) in self.names.iter().enumerate() {
            put(out, DEVICES_HEADER_BYTES + i * NAME_BYTES, name);
        }
        Some(len)
    }
}

/// DEVICES as received: the counts, and the names read back one at a time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DevicesView<'a> {
    /// Devices in this message's handle array, two handles each.
    pub devices: u32,
    /// Drivers named, one image handle each after the devices.
    pub drivers: u32,
    /// Devices still to come in later DEVICES messages.
    pub more: u32,
    /// Whether this is the first message, with the job and the images.
    pub first: bool,
    names: &'a [u8],
}

impl DevicesView<'_> {
    /// Driver `index`'s name, NUL-padded.
    #[must_use]
    pub fn name(&self, index: usize) -> Option<[u8; NAME_BYTES]> {
        let at = index.checked_mul(NAME_BYTES)?;
        self.names
            .get(at..at.checked_add(NAME_BYTES)?)
            .and_then(|name| name.try_into().ok())
    }

    /// Driver `index`'s name without its padding, for `process_create`.
    #[must_use]
    pub fn name_bytes(&self, index: usize) -> Option<&[u8]> {
        let at = index.checked_mul(NAME_BYTES)?;
        let name = self.names.get(at..at.checked_add(NAME_BYTES)?)?;
        let end = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(NAME_BYTES);
        name.get(..end)
    }

    /// Handles the message carries: the job if first, two per device, one
    /// per driver.
    #[must_use]
    pub const fn handles(&self) -> usize {
        (self.first as usize) + 2 * self.devices as usize + self.drivers as usize
    }

    /// Decode a DEVICES message.
    ///
    /// # Errors
    ///
    /// [`Malformed`].
    pub fn decode(bytes: &[u8]) -> Result<DevicesView<'_>, Malformed> {
        let (kind, declared) = header(bytes)?;
        if kind != DEVICES {
            return Err(Malformed::UnknownType(kind));
        }
        let devices = u32_at(bytes, 8).ok_or(Malformed::Length)?;
        let drivers = u32_at(bytes, 12).ok_or(Malformed::Length)?;
        let more = u32_at(bytes, 16).ok_or(Malformed::Length)?;
        let flags = u32_at(bytes, 20).ok_or(Malformed::Length)?;
        let expected = (drivers as usize)
            .checked_mul(NAME_BYTES)
            .and_then(|names| names.checked_add(DEVICES_HEADER_BYTES))
            .ok_or(Malformed::Length)?;
        if drivers as usize > MAX_DRIVERS || declared != expected || bytes.len() != expected {
            return Err(Malformed::Length);
        }
        Ok(DevicesView {
            devices,
            drivers,
            more,
            first: flags & FLAG_FIRST != 0,
            names: bytes.get(DEVICES_HEADER_BYTES..).unwrap_or(&[]),
        })
    }
}

/// The fixed-size messages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Message {
    /// `devmgr` to kernel: what it started.
    Report {
        /// Drivers started whose disks the kernel accepted.
        started: u32,
        /// Devices that matched a driver and got none.
        failed: u32,
    },
    /// Kernel to `devmgr`: the disk of the device at `location` is published.
    Published {
        /// The device's PCI address word.
        location: u32,
    },
    /// `devmgr` to kernel: the driver of the device at `location` ended.
    Died {
        /// The device's PCI address word.
        location: u32,
        /// The driver's exit status; 137 if killed.
        status: i32,
    },
    /// `devmgr` to kernel: the driver of the device at `location`, which
    /// died, was started again and has published.
    Restarted {
        /// The device's PCI address word.
        location: u32,
        /// How many times this device's driver has been started again,
        /// counting this one.
        restarts: u32,
    },
}

impl Message {
    /// Encode the message.
    #[must_use]
    pub fn encode(&self) -> [u8; SHORT_BYTES] {
        let mut bytes = [0; SHORT_BYTES];
        let (kind, a, b) = match *self {
            Message::Report { started, failed } => (REPORT, started, failed),
            Message::Published { location } => (PUBLISHED, location, 0),
            Message::Died { location, status } => (DIED, location, status as u32),
            Message::Restarted { location, restarts } => (RESTARTED, location, restarts),
        };
        put(&mut bytes, 0, &kind.to_le_bytes());
        put(&mut bytes, 4, &(SHORT_BYTES as u32).to_le_bytes());
        put(&mut bytes, 8, &a.to_le_bytes());
        put(&mut bytes, 12, &b.to_le_bytes());
        bytes
    }

    /// Decode a fixed-size message.
    ///
    /// # Errors
    ///
    /// [`Malformed`]; a DEVICES message is [`Malformed::UnknownType`] here,
    /// since it is decoded by [`DevicesView::decode`].
    pub fn decode(bytes: &[u8]) -> Result<Message, Malformed> {
        let (kind, declared) = header(bytes)?;
        if !matches!(kind, REPORT | PUBLISHED | DIED | RESTARTED) {
            return Err(Malformed::UnknownType(kind));
        }
        if declared != SHORT_BYTES || bytes.len() != SHORT_BYTES {
            return Err(Malformed::Length);
        }
        let a = u32_at(bytes, 8).ok_or(Malformed::Length)?;
        let b = u32_at(bytes, 12).ok_or(Malformed::Length)?;
        Ok(match kind {
            REPORT => Message::Report {
                started: a,
                failed: b,
            },
            PUBLISHED => Message::Published { location: a },
            RESTARTED => Message::Restarted {
                location: a,
                restarts: b,
            },
            _ => Message::Died {
                location: a,
                status: b as i32,
            },
        })
    }
}

/// The type and declared length, or [`Malformed::Short`].
fn header(bytes: &[u8]) -> Result<(u32, usize), Malformed> {
    match (u32_at(bytes, 0), u32_at(bytes, 4)) {
        (Some(kind), Some(len)) => Ok((kind, len as usize)),
        _ => Err(Malformed::Short),
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.checked_add(4)?)
        .and_then(|word| word.try_into().ok())
        .map(u32::from_le_bytes)
}

fn put(out: &mut [u8], at: usize, source: &[u8]) {
    if let Some(slot) = out.get_mut(at..) {
        for (to, from) in slot.iter_mut().zip(source) {
            *to = *from;
        }
    }
}

#[cfg(test)]
mod tests;
