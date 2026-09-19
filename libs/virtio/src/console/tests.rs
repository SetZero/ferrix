//! Tests for virtio-console's device protocol.
//!
//! Every number, offset and queue index below is written out by hand from
//! virtio 1.2 §5.3 and QEMU 9.2.4's `include/standard-headers/linux/
//! virtio_console.h` and `hw/char/virtio-serial-bus.c`, not taken from this
//! module's constants: a constant tested against itself proves nothing, and
//! the queue numbering is exactly the kind of arithmetic that is wrong in the
//! code and in the test at once.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

/// A configuration block of given bytes.
#[derive(Debug)]
struct Block(Vec<u8>);

impl DeviceConfig for Block {
    fn config_len(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap_or(u32::MAX)
    }

    fn config_read8(&self, offset: u32) -> u8 {
        // What QEMU returns for a byte past the block, which is what makes
        // reading past one dangerous rather than merely wrong.
        self.0
            .get(usize::try_from(offset).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(0xff)
    }

    fn config_read16(&self, offset: u32) -> u16 {
        u16::from(self.config_read8(offset)) | u16::from(self.config_read8(offset + 1)) << 8
    }

    fn config_read32(&self, offset: u32) -> u32 {
        u32::from(self.config_read16(offset)) | u32::from(self.config_read16(offset + 2)) << 16
    }
}

/// A block declaring `ports` ports, laid out by hand: `cols` and `rows` are
/// halfwords, `max_nr_ports` a word at offset 4.
fn block(ports: u32) -> Block {
    let mut bytes = vec![0_u8; 12];
    bytes[0] = 80; // cols, which is not read
    bytes[2] = 24; // rows, which is not read
    bytes[4..8].copy_from_slice(&ports.to_le_bytes());
    Block(bytes)
}

/// Virtio 1.2 §5.3.2: port 0 is queues 0 and 1, the control queues are 2 and
/// 3, and every port after the first is `2N + 2` and `2N + 3`. The one that
/// matters is port 1, the port a `virtserialport` adds, which is queues 4 and
/// 5 and not 2 and 3.
#[test]
fn the_port_queues_are_numbered_as_the_specification_says() {
    assert_eq!((receive_queue(0), transmit_queue(0)), (0, 1), "port 0");
    assert_eq!((receive_queue(1), transmit_queue(1)), (4, 5), "port 1");
    assert_eq!((receive_queue(2), transmit_queue(2)), (6, 7), "port 2");
    assert_eq!((receive_queue(30), transmit_queue(30)), (62, 63), "port 30");
    assert_eq!(
        (CONTROL_RECEIVE_QUEUE, CONTROL_TRANSMIT_QUEUE),
        (2, 3),
        "the control pair sits between port 0 and port 1"
    );
    // No port's queue is ever a control queue, which is the property the
    // hole between 1 and 4 exists for.
    for port in 0..MAX_PORTS {
        assert!(
            ![2, 3].contains(&receive_queue(port)) && ![2, 3].contains(&transmit_queue(port)),
            "port {port} collides with the control queues"
        );
    }
}

/// A device with one port still has four queues: port 0's pair, which nothing
/// uses, and the control pair.
#[test]
fn a_device_has_queues_for_the_ports_it_declares() {
    assert_eq!(queue_count(1), 4, "port 0 and the control pair");
    assert_eq!(queue_count(2), 6, "and port 1's pair");
    assert_eq!(queue_count(31), 64, "QEMU's default maximum");
}

/// The configuration block is read at the offsets the structure fixes.
#[test]
fn the_configuration_block_says_how_many_ports() {
    assert_eq!(Config::read(&block(2)), Ok(Config { ports: 2 }));
    assert_eq!(Config::read(&block(31)), Ok(Config { ports: 31 }));
}

/// A block too short to hold `max_nr_ports` is refused rather than read past.
/// QEMU answers `0xff` for a byte that is not there, so believing a short
/// block would mean believing in four billion ports.
#[test]
fn a_short_configuration_block_is_refused() {
    assert_eq!(Config::read(&Block(vec![0; 4])), Err(ConsoleError::Config));
    assert_eq!(Config::read(&Block(vec![0; 7])), Err(ConsoleError::Config));
}

/// A device with no ports has nothing to open, and one with more ports than
/// this crate drives is refused at the block rather than at a queue number.
#[test]
fn an_impossible_port_count_is_refused() {
    assert_eq!(Config::read(&block(0)), Err(ConsoleError::Ports(0)));
    assert_eq!(
        Config::read(&block(u32::MAX)),
        Err(ConsoleError::Ports(u32::MAX))
    );
    assert_eq!(Config::read(&block(32)), Err(ConsoleError::Ports(32)));
}

/// The control message is `id`, `event`, `value` -- four bytes and two and
/// two, little-endian -- and the events are numbered 0 to 7.
#[test]
fn the_device_control_messages_decode() {
    // port 1, PORT_ADD (1), value 0.
    assert_eq!(
        Control::read(&[1, 0, 0, 0, 1, 0, 0, 0], 2),
        Ok(Control::Add { port: 1 })
    );
    // port 1, PORT_REMOVE (2).
    assert_eq!(
        Control::read(&[1, 0, 0, 0, 2, 0, 0, 0], 2),
        Ok(Control::Remove { port: 1 })
    );
    // port 1, CONSOLE_PORT (4).
    assert_eq!(
        Control::read(&[1, 0, 0, 0, 4, 0, 0, 0], 2),
        Ok(Control::Console { port: 1 })
    );
    // port 0, RESIZE (5).
    assert_eq!(
        Control::read(&[0, 0, 0, 0, 5, 0, 0, 0], 2),
        Ok(Control::Resize { port: 0 })
    );
    // port 1, PORT_OPEN (6), value 1 and value 0.
    assert_eq!(
        Control::read(&[1, 0, 0, 0, 6, 0, 1, 0], 2),
        Ok(Control::Open {
            port: 1,
            open: true
        })
    );
    assert_eq!(
        Control::read(&[1, 0, 0, 0, 6, 0, 0, 0], 2),
        Ok(Control::Open {
            port: 1,
            open: false
        })
    );
}

/// The port's name is what follows the header, and QEMU sends it with a NUL
/// the specification does not require. A name read with the NUL left on would
/// never match the one the agent is looking for.
#[test]
fn a_name_is_trimmed_at_the_nul_if_there_is_one() {
    let mut with = vec![1, 0, 0, 0, 7, 0, 0, 0];
    with.extend_from_slice(b"com.redhat.spice.0\0");
    assert_eq!(
        Control::read(&with, 2),
        Ok(Control::Name {
            port: 1,
            name: b"com.redhat.spice.0"
        }),
        "QEMU's trailing NUL is not part of the name"
    );

    let mut without = vec![1, 0, 0, 0, 7, 0, 0, 0];
    without.extend_from_slice(b"com.redhat.spice.0");
    assert_eq!(
        Control::read(&without, 2),
        Ok(Control::Name {
            port: 1,
            name: SPICE_PORT_NAME
        }),
        "and a device that sends none says the same thing"
    );

    assert_eq!(
        Control::read(&[1, 0, 0, 0, 7, 0, 0, 0], 2),
        Ok(Control::Name { port: 1, name: b"" }),
        "a name with no bytes is empty, not short"
    );
}

/// An event number this crate does not know is carried, not refused: a later
/// device may send one and the answer is to ignore it.
#[test]
fn an_unknown_event_is_carried() {
    assert_eq!(
        Control::read(&[1, 0, 0, 0, 9, 0, 3, 0], 2),
        Ok(Control::Unknown {
            port: 1,
            event: 9,
            value: 3
        })
    );
}

/// A port id the device does not have names queues that do not exist, and is
/// refused. `VIRTIO_CONSOLE_BAD_ID` is refused by the same rule.
#[test]
fn a_port_the_device_does_not_have_is_refused() {
    assert_eq!(
        Control::read(&[2, 0, 0, 0, 1, 0, 0, 0], 2),
        Err(ConsoleError::Port(2)),
        "a device with two ports has no port 2"
    );
    assert_eq!(
        Control::read(&[0xff, 0xff, 0xff, 0xff, 1, 0, 0, 0], 2),
        Err(ConsoleError::Port(0xffff_ffff)),
        "and none of the events carried here is about no port"
    );
}

/// A message shorter than the header is refused rather than padded.
#[test]
fn a_short_control_message_is_refused() {
    assert_eq!(
        Control::read(&[1, 0, 0, 0, 6, 0, 1], 2),
        Err(ConsoleError::Short { want: 8, have: 7 })
    );
}

/// The driver's three messages, byte for byte. `DEVICE_READY` is about no
/// port, so it carries `VIRTIO_CONSOLE_BAD_ID`.
#[test]
fn the_drivers_control_messages_encode() {
    let mut out = [0_u8; 8];

    assert_eq!(
        Control::DeviceReady { ready: true }.write(&mut out),
        Ok(8),
        "the header is eight bytes"
    );
    assert_eq!(
        out,
        [0xff, 0xff, 0xff, 0xff, 0, 0, 1, 0],
        "BAD_ID, DEVICE_READY, 1"
    );

    assert_eq!(
        Control::Ready {
            port: 1,
            ready: true
        }
        .write(&mut out),
        Ok(8)
    );
    assert_eq!(out, [1, 0, 0, 0, 3, 0, 1, 0], "port 1, PORT_READY, 1");

    assert_eq!(
        Control::Open {
            port: 1,
            open: true
        }
        .write(&mut out),
        Ok(8)
    );
    assert_eq!(out, [1, 0, 0, 0, 6, 0, 1, 0], "port 1, PORT_OPEN, 1");

    assert_eq!(
        Control::Open {
            port: 1,
            open: false
        }
        .write(&mut out),
        Ok(8)
    );
    assert_eq!(out, [1, 0, 0, 0, 6, 0, 0, 0], "and closing it says 0");
}

/// A message only a device sends cannot be written by a driver, and a buffer
/// too small is refused rather than half filled.
#[test]
fn a_driver_cannot_write_a_devices_message() {
    let mut out = [0_u8; 8];
    assert_eq!(
        Control::Add { port: 1 }.write(&mut out),
        Err(ConsoleError::NotTheDrivers)
    );
    assert_eq!(out, [0; 8], "nothing was written");

    let mut small = [0_u8; 7];
    assert_eq!(
        Control::DeviceReady { ready: true }.write(&mut small),
        Err(ConsoleError::Short { want: 8, have: 7 })
    );
}

/// Multiport is required, not merely accepted: a device without it has one
/// anonymous stream, and a stream that cannot be named is not one an agent
/// may take for the host's clipboard.
#[test]
fn multiport_is_required() {
    assert_eq!(FEATURE_MULTIPORT, 1 << 1, "virtio 1.2 §5.3.3");
    // `VIRTIO_F_VERSION_1` is bit 32 and `VIRTIO_F_ACCESS_PLATFORM` bit 33,
    // so the two sets are these numbers and not any others: the device is
    // refused without version 1 and multiport, and the platform's address
    // translation is accepted but not required.
    assert_eq!(
        REQUIRED_FEATURES,
        (1 << 32) | (1 << 1),
        "version 1 and multiport, and nothing else, are required"
    );
    assert_eq!(
        DRIVER_FEATURES,
        (1 << 32) | (1 << 33) | (1 << 1),
        "the console's size and its emergency write are not asked for"
    );
    // `0x1040 + 3`, the modern id, and the transitional one.
    assert_eq!((PCI_DEVICE_ID, PCI_LEGACY_DEVICE_ID), (0x1043, 0x1003));
}
