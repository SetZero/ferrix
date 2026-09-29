//! virtio-console's device protocol: its queue numbering, its configuration
//! block, and the control messages that open a port.
//!
//! Virtio 1.2 §5.3 defines the console device. Without
//! [`FEATURE_MULTIPORT`] it is one nameless byte stream and there is nothing
//! to say; with it, the device has a control queue over which ports are
//! added, named and opened, and that is what a `virtserialport` is and what
//! this module is for. Ferrix wants exactly one such port -- the one named
//! `com.redhat.spice.0`, which carries the clipboard (`docs/CLIPBOARD.md`
//! §3) -- but the numbering and the control conversation are the device's
//! whatever the port is for, so they are here and the clipboard is not.
//!
//! What drives a device -- posting receive buffers, when to notify, what to
//! do about a device that misbehaves -- is a driver's, as in every other
//! module here.
//!
//! # Where the numbers come from
//!
//! Virtio 1.2 §5.3, checked against QEMU 9.2.4's copy of Linux's header,
//! `include/standard-headers/linux/virtio_console.h`, and against the device
//! QEMU builds from it in `hw/char/virtio-serial-bus.c`.
//!
//! # The queue numbering is the trap
//!
//! Port 0 is queues 0 and 1. The control queues are 2 and 3. **Every port
//! after the first is queues `2N + 2` and `2N + 3`**, so the single port a
//! `virtserialport` adds -- port 1 -- is queues 4 and 5, and queues 0 and 1
//! belong to a port 0 that a console-less device never uses and that must be
//! set up anyway. A driver that assumes its port's queues are 0 and 1 gets a
//! device that accepts everything and delivers nothing, which is a bug with
//! no symptom other than silence. [`receive_queue`] and [`transmit_queue`]
//! are the arithmetic, written once.
//!
//! # Trust
//!
//! A control message is the device's word. A message shorter than
//! [`CONTROL_BYTES`] is refused rather than padded; an event number the
//! device invents is [`Control::Unknown`] rather than an error, since a
//! later device may send an event this driver has never heard of and the
//! answer to those is to ignore them; and a port id at or above the
//! `max_nr_ports` the configuration block declared is refused, since it
//! names a port whose queues do not exist.

use crate::pci::{FEATURE_ACCESS_PLATFORM, FEATURE_VERSION_1};

/// The configuration-block reader, which every device class shares.
pub use crate::DeviceConfig;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// The device, virtio 1.2 §5.3.
// ---------------------------------------------------------------------------

/// `VIRTIO_ID_CONSOLE`: the virtio device id.
pub const DEVICE_ID: u16 = 3;
/// The modern PCI device id, `0x1040` plus [`DEVICE_ID`].
pub const PCI_DEVICE_ID: u16 = 0x1043;
/// The transitional PCI device id, which `virtio-serial-pci` carries when it
/// is not told `disable-legacy=on`.
pub const PCI_LEGACY_DEVICE_ID: u16 = 0x1003;

/// `VIRTIO_CONSOLE_F_SIZE`: the device reports a console's size. Not asked
/// for: a serial port has no rows and no columns.
pub const FEATURE_SIZE: u64 = 1 << 0;
/// `VIRTIO_CONSOLE_F_MULTIPORT`: the device has a control queue, and ports
/// that can be named. Without it there is no port called
/// `com.redhat.spice.0` to find.
pub const FEATURE_MULTIPORT: u64 = 1 << 1;
/// `VIRTIO_CONSOLE_F_EMERG_WRITE`: a byte can be written through the
/// configuration block with no queue at all, for a panic that cannot wait.
/// Not asked for; `docs/CLIPBOARD.md` §7 has nowhere to put it.
pub const FEATURE_EMERG_WRITE: u64 = 1 << 2;

/// The features the driver accepts: multiport, the transport's
/// [`FEATURE_VERSION_1`], and [`FEATURE_ACCESS_PLATFORM`], without which a
/// device behind the machine's IOMMU refuses `FEATURES_OK`.
pub const DRIVER_FEATURES: u64 = FEATURE_VERSION_1 | FEATURE_ACCESS_PLATFORM | FEATURE_MULTIPORT;

/// The features without which the driver gives up. Multiport is among them,
/// unlike in every other module here: a device without it has one anonymous
/// stream, and a stream that cannot be identified by name is not one an agent
/// may assume is the host's clipboard.
pub const REQUIRED_FEATURES: u64 = FEATURE_VERSION_1 | FEATURE_MULTIPORT;

// ---------------------------------------------------------------------------
// The queues, virtio 1.2 §5.3.2.
// ---------------------------------------------------------------------------

/// The control queue the device writes to and the driver reads.
pub const CONTROL_RECEIVE_QUEUE: u16 = 2;
/// The control queue the driver writes to.
pub const CONTROL_TRANSMIT_QUEUE: u16 = 3;

/// The queue port `port` is received on: device-writable buffers.
#[must_use]
pub const fn receive_queue(port: u32) -> u16 {
    if port == 0 {
        0
    } else {
        // Ports are bounded by `max_nr_ports`, which `Config::ports` holds
        // to `MAX_PORTS`, so this cannot overflow a `u16` for any port a
        // driver reaches through this crate.
        (port as u16) * 2 + 2
    }
}

/// The queue port `port` is transmitted on: device-readable buffers.
#[must_use]
pub const fn transmit_queue(port: u32) -> u16 {
    receive_queue(port) + 1
}

/// The most ports this crate will drive.
///
/// Virtio fixes no maximum and QEMU's `virtio-serial` defaults to 31. The
/// bound is here so that a device declaring a `max_nr_ports` of four billion
/// is refused at the configuration block rather than believed all the way
/// down to a queue number that wraps.
pub const MAX_PORTS: u32 = 31;

/// How many queues a device declaring `ports` ports has, which is what a
/// driver sets up and what the transport's `num_queues` must be at least.
///
/// Two per port and the control pair, so a device with one port still has
/// four: `virtio_serial_device_realize` in QEMU's
/// `hw/char/virtio-serial-bus.c` adds port 0's pair, then the control pair,
/// then a pair for each port after the first, and `max_nr_ports` counts port
/// 0 among them.
#[must_use]
pub const fn queue_count(ports: u32) -> u16 {
    // `ports` is held to `MAX_PORTS` by `Config::read`, so this is far inside
    // a `u16`.
    (ports as u16) * 2 + 2
}

// ---------------------------------------------------------------------------
// Configuration space, virtio 1.2 §5.3.4.
// ---------------------------------------------------------------------------

/// Offset of `cols`.
pub const CONFIG_COLS: u32 = 0;
/// Offset of `rows`.
pub const CONFIG_ROWS: u32 = 2;
/// Offset of `max_nr_ports`.
pub const CONFIG_MAX_NR_PORTS: u32 = 4;
/// Offset of `emerg_wr`.
pub const CONFIG_EMERG_WR: u32 = 8;
/// Bytes of `struct virtio_console_config`.
pub const CONFIG_LEN: u32 = 12;

/// What the configuration block says about the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// `max_nr_ports`: how many ports the device can hold, and so how many
    /// queues it has.
    pub ports: u32,
}

impl Config {
    /// Read the block.
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Config`] for a block that does not reach
    /// `max_nr_ports`, and [`ConsoleError::Ports`] for a count of zero --
    /// which has no port to open -- or one above [`MAX_PORTS`].
    pub fn read<C: DeviceConfig + ?Sized>(config: &C) -> Result<Config, ConsoleError> {
        // A block that does not reach `max_nr_ports` is not this device's:
        // reading past it would be reading whatever the transport returns for
        // an absent byte, which on QEMU is `0xff` and would decode as four
        // billion ports.
        if config.config_len() < CONFIG_MAX_NR_PORTS + 4 {
            return Err(ConsoleError::Config);
        }
        // `F_SIZE` is not negotiated, so `cols` and `rows` are not read: the
        // device may leave them at anything.
        let ports = config.config_read32(CONFIG_MAX_NR_PORTS);
        if ports == 0 || ports > MAX_PORTS {
            return Err(ConsoleError::Ports(ports));
        }
        Ok(Config { ports })
    }
}

// ---------------------------------------------------------------------------
// The control protocol, virtio 1.2 §5.3.6.
// ---------------------------------------------------------------------------

/// Bytes of `struct virtio_console_control`: `id`, `event`, `value`.
pub const CONTROL_BYTES: usize = 8;

/// `VIRTIO_CONSOLE_BAD_ID`: a message about no port in particular.
pub const BAD_ID: u32 = u32::MAX;

/// `VIRTIO_CONSOLE_DEVICE_READY`.
pub const DEVICE_READY: u16 = 0;
/// `VIRTIO_CONSOLE_PORT_ADD`.
pub const PORT_ADD: u16 = 1;
/// `VIRTIO_CONSOLE_PORT_REMOVE`.
pub const PORT_REMOVE: u16 = 2;
/// `VIRTIO_CONSOLE_PORT_READY`.
pub const PORT_READY: u16 = 3;
/// `VIRTIO_CONSOLE_CONSOLE_PORT`.
pub const CONSOLE_PORT: u16 = 4;
/// `VIRTIO_CONSOLE_RESIZE`.
pub const RESIZE: u16 = 5;
/// `VIRTIO_CONSOLE_PORT_OPEN`.
pub const PORT_OPEN: u16 = 6;
/// `VIRTIO_CONSOLE_PORT_NAME`.
pub const PORT_NAME: u16 = 7;

/// A control message, in the direction it is read.
///
/// The name in [`Control::Name`] borrows the buffer it was read out of, which
/// is the receive buffer the driver posted: a driver that keeps the name
/// keeps its own copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control<'a> {
    /// The device has a new port. The driver answers [`Control::Ready`].
    Add {
        /// Which port.
        port: u32,
    },
    /// The port is gone: its queues carry nothing more.
    Remove {
        /// Which port.
        port: u32,
    },
    /// The port is a console rather than a serial port. Nothing here is.
    Console {
        /// Which port.
        port: u32,
    },
    /// The port's name, which is how a port is identified: its number is the
    /// device's to choose and may differ between one boot and the next.
    Name {
        /// Which port.
        port: u32,
        /// The name, without the NUL the device may or may not include.
        name: &'a [u8],
    },
    /// The host end opened, or closed when `open` is false. A port whose host
    /// end has closed carries nothing until it opens again, and whoever was
    /// using it must forget whatever state it had.
    Open {
        /// Which port.
        port: u32,
        /// Whether it is open.
        open: bool,
    },
    /// A console's size changed, which no port here has.
    Resize {
        /// Which port.
        port: u32,
    },
    /// An event this crate does not know, which is not an error: a later
    /// device may send one, and the answer is to ignore it.
    Unknown {
        /// Which port it claims to be about.
        port: u32,
        /// The event number.
        event: u16,
        /// Its value.
        value: u16,
    },
    /// The driver's answer to [`Control::Add`]: the port's queues are set up.
    Ready {
        /// Which port.
        port: u32,
        /// Whether the driver could set it up.
        ready: bool,
    },
    /// The driver saying its side of the whole device is set up, which must
    /// be sent before the device adds any port.
    DeviceReady {
        /// Whether the driver could.
        ready: bool,
    },
}

impl<'a> Control<'a> {
    /// Read a control message the device sent.
    ///
    /// `ports` bounds the port id, as the configuration block declared it.
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Short`] for fewer than [`CONTROL_BYTES`] bytes, and
    /// [`ConsoleError::Port`] for a port id that names queues the device does
    /// not have. A message about [`BAD_ID`] is refused the same way: the
    /// device sends it only for events that are about no port, none of which
    /// this carries.
    pub fn read(bytes: &'a [u8], ports: u32) -> Result<Control<'a>, ConsoleError> {
        let head = bytes.get(..CONTROL_BYTES).ok_or(ConsoleError::Short {
            want: CONTROL_BYTES,
            have: bytes.len(),
        })?;
        let mut word = [0_u8; 4];
        let mut half = [0_u8; 2];
        word.copy_from_slice(head.get(0..4).ok_or(ConsoleError::Short {
            want: CONTROL_BYTES,
            have: bytes.len(),
        })?);
        let port = u32::from_le_bytes(word);
        half.copy_from_slice(head.get(4..6).ok_or(ConsoleError::Short {
            want: CONTROL_BYTES,
            have: bytes.len(),
        })?);
        let event = u16::from_le_bytes(half);
        half.copy_from_slice(head.get(6..8).ok_or(ConsoleError::Short {
            want: CONTROL_BYTES,
            have: bytes.len(),
        })?);
        let value = u16::from_le_bytes(half);

        if port >= ports {
            return Err(ConsoleError::Port(port));
        }
        Ok(match event {
            PORT_ADD => Control::Add { port },
            PORT_REMOVE => Control::Remove { port },
            CONSOLE_PORT => Control::Console { port },
            RESIZE => Control::Resize { port },
            PORT_OPEN => Control::Open {
                port,
                open: value != 0,
            },
            PORT_NAME => Control::Name {
                port,
                // The name is whatever follows the header. QEMU writes it
                // NUL-terminated (`virtio_serial_port_name` in
                // `hw/char/virtio-serial-bus.c`) and the specification does
                // not require the NUL, so it is trimmed rather than trusted.
                name: trim_nul(bytes.get(CONTROL_BYTES..).unwrap_or(&[])),
            },
            other => Control::Unknown {
                port,
                event: other,
                value,
            },
        })
    }

    /// Write a message the driver sends into `out`, and say how many bytes it
    /// took, which is always [`CONTROL_BYTES`].
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Short`] for a buffer smaller than that, and
    /// [`ConsoleError::NotTheDrivers`] for a message only a device sends.
    pub fn write(&self, out: &mut [u8]) -> Result<usize, ConsoleError> {
        let (port, event, value) = match *self {
            Control::DeviceReady { ready } => (BAD_ID, DEVICE_READY, u16::from(ready)),
            Control::Ready { port, ready } => (port, PORT_READY, u16::from(ready)),
            Control::Open { port, open } => (port, PORT_OPEN, u16::from(open)),
            _ => return Err(ConsoleError::NotTheDrivers),
        };
        let have = out.len();
        let slot = out.get_mut(..CONTROL_BYTES).ok_or(ConsoleError::Short {
            want: CONTROL_BYTES,
            have,
        })?;
        let mut message = [0_u8; CONTROL_BYTES];
        let fields = port
            .to_le_bytes()
            .into_iter()
            .chain(event.to_le_bytes())
            .chain(value.to_le_bytes());
        for (cell, byte) in message.iter_mut().zip(fields) {
            *cell = byte;
        }
        slot.copy_from_slice(&message);
        Ok(CONTROL_BYTES)
    }
}

/// Everything after the first NUL, dropped.
fn trim_nul(name: &[u8]) -> &[u8] {
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    name.get(..end).unwrap_or(name)
}

/// The name the SPICE agent's port carries, and the only port `docs/CLIPBOARD.md`
/// looks for.
pub const SPICE_PORT_NAME: &[u8] = b"com.redhat.spice.0";

/// Why a console message or block was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleError {
    /// Fewer bytes than the message needs.
    Short {
        /// Bytes it needs.
        want: usize,
        /// Bytes there are.
        have: usize,
    },
    /// The configuration block does not reach `max_nr_ports`.
    Config,
    /// A `max_nr_ports` of zero, or above [`MAX_PORTS`].
    Ports(u32),
    /// A port id at or above the declared `max_nr_ports`, whose queues the
    /// device therefore does not have.
    Port(u32),
    /// A control message only a device sends, asked to be written by a
    /// driver.
    NotTheDrivers,
}
