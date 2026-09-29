//! A serial port: the Communications Device Class's Abstract Control Model
//! (CDC 1.2, and PSTN 1.2 for its requests), the function Linux's
//! `cdc_acm` binds as `/dev/ttyACM*`.
//!
//! # The device
//!
//! Class `0xEF`/`0x02`/`0x01` -- "miscellaneous, common class, interface
//! association" -- with an interface association descriptor grouping the
//! two interfaces, rather than class `0x02` on the device. Linux binds
//! `cdc_acm` to the communication interface either way: it matches the
//! interface's class 2, subclass 2 (ACM), and claims the data interface its
//! union descriptor names. The association is what makes every other host
//! -- Windows' composite driver, macOS -- hand both interfaces to one
//! driver, and it leaves room for a second function beside this one
//! without changing what the device says it is.
//!
//! `bcdUSB` is 2.0, not 2.01: with 2.01 a host asks for a BOS descriptor
//! to learn about link power management, which the controller has off.
//!
//! # The interfaces
//!
//! * interface 0, communication (class 2, subclass 2, protocol 1, AT
//!   commands V.250): the header, call management, ACM and union
//!   functional descriptors, and an interrupt IN endpoint, [`NOTIFY_IN`],
//!   for the class's notifications;
//! * interface 1, data (class `0x0A`): a bulk OUT endpoint, [`DATA_OUT`],
//!   for what the host writes, and a bulk IN, [`DATA_IN`], for what it
//!   reads.
//!
//! Bulk packets are 512 bytes at high speed and 64 at full; the control
//! endpoint's are 64 at both.
//!
//! # The class requests
//!
//! `SET_LINE_CODING` and `GET_LINE_CODING` keep the host's baud rate and
//! framing, which mean nothing over USB but must read back as set.
//! `SET_CONTROL_LINE_STATE` carries DTR, which a host raises when a program
//! opens the tty and drops when the last one closes it: [`SerialPort::dtr`]
//! is how the driver knows anyone is reading. Bytes written while it is low
//! go nowhere a program sees, so the driver drops them or keeps a bounded
//! backlog.

use crate::setup::{DIRECTION_IN, RECIPIENT_INTERFACE, TYPE_CLASS, TYPE_STANDARD};
use crate::standard::{Descriptors, Standard, State};
use crate::{EndpointInfo, Function, Reply, Setup, Speed, Status, TransferKind};

/// pid.codes' vendor ID.
pub const VENDOR: u16 = 0x1209;
/// pid.codes' product ID for private testing, which needs no allocation.
pub const PRODUCT: u16 = 0x0001;
/// `bcdDevice`: 1.00.
pub const RELEASE: u16 = 0x0100;

/// String 1.
pub const MANUFACTURER_STRING: &str = "Ferrix";
/// String 2.
pub const PRODUCT_STRING: &str = "Ferrix console";

/// The communication interface's number.
pub const COMMUNICATION_INTERFACE: u8 = 0;
/// The data interface's number.
pub const DATA_INTERFACE: u8 = 1;

/// The notification endpoint: interrupt IN 1.
pub const NOTIFY_IN: u8 = 0x81;
/// The data endpoint the host writes to: bulk OUT 2.
pub const DATA_OUT: u8 = 0x02;
/// The data endpoint the host reads from: bulk IN 2.
pub const DATA_IN: u8 = 0x82;
/// The adb interface: the third, beside the serial port's two
/// (`docs/ADB.md` §4, step 3). The host's `adb` finds it by its class,
/// whatever the vendor.
pub const ADB_INTERFACE: u8 = 2;
/// adb's bulk OUT endpoint: the host's messages.
pub const ADB_OUT: u8 = 0x03;
/// adb's bulk IN endpoint: the device's.
pub const ADB_IN: u8 = 0x83;

/// The control endpoint's packet size, at both speeds.
pub const CONTROL_PACKET: u8 = 64;
/// A notification's packet size: `SERIAL_STATE` is ten bytes.
pub const NOTIFY_PACKET: u16 = 16;
/// Bulk packets at high speed.
pub const HIGH_SPEED_BULK: u16 = 512;
/// Bulk packets at full speed.
pub const FULL_SPEED_BULK: u16 = 64;
/// The notification endpoint's `bInterval` at high speed: 2^(9-1)
/// microframes, 32 ms.
pub const HIGH_SPEED_NOTIFY_INTERVAL: u8 = 9;
/// The same at full speed, in frames: 32 ms.
pub const FULL_SPEED_NOTIFY_INTERVAL: u8 = 32;

/// The largest serial number string kept, in ASCII characters.
pub const SERIAL_BYTES: usize = 32;

/// `SET_LINE_CODING`.
pub const SET_LINE_CODING: u8 = 0x20;
/// `GET_LINE_CODING`.
pub const GET_LINE_CODING: u8 = 0x21;
/// `SET_CONTROL_LINE_STATE`.
pub const SET_CONTROL_LINE_STATE: u8 = 0x22;
/// The `SERIAL_STATE` notification.
pub const SERIAL_STATE: u8 = 0x20;

/// The class-specific interface descriptor type, `CS_INTERFACE`.
const CS_INTERFACE: u8 = 0x24;
/// The communications class.
const CLASS_COMMUNICATION: u8 = 0x02;
/// Its abstract control model subclass.
const SUBCLASS_ACM: u8 = 0x02;
/// AT commands, V.250: what `cdc_acm` matches, though none are sent.
const PROTOCOL_V250: u8 = 0x01;
/// The data interface class.
const CLASS_DATA: u8 = 0x0A;
/// adb's interface: vendor-specific class, subclass `0x42`, protocol 1, as
/// AOSP's `ADB_CLASS`, `ADB_SUBCLASS` and `ADB_PROTOCOL` are.
const CLASS_VENDOR: u8 = 0xFF;
const SUBCLASS_ADB: u8 = 0x42;
const PROTOCOL_ADB: u8 = 0x01;
/// ACM's capabilities: line coding and control line state requests, and the
/// serial state notification (PSTN 1.2 table 4).
const ACM_CAPABILITIES: u8 = 0x02;

/// The device descriptor's length.
pub const DEVICE_BYTES: usize = 18;
/// The device qualifier's.
pub const QUALIFIER_BYTES: usize = 10;
/// The configuration descriptor's, with everything under it.
pub const CONFIGURATION_BYTES: usize = 98;

/// The device descriptor.
pub const DEVICE: [u8; DEVICE_BYTES] = {
    let [vendor_low, vendor_high] = VENDOR.to_le_bytes();
    let [product_low, product_high] = PRODUCT.to_le_bytes();
    let [release_low, release_high] = RELEASE.to_le_bytes();
    [
        18,
        crate::setup::DEVICE,
        0x00,
        0x02, // bcdUSB 2.00
        0xEF,
        0x02,
        0x01, // interface association
        CONTROL_PACKET,
        vendor_low,
        vendor_high,
        product_low,
        product_high,
        release_low,
        release_high,
        1, // iManufacturer
        2, // iProduct
        3, // iSerialNumber
        1, // bNumConfigurations
    ]
};

/// The device qualifier: the same device at the other speed.
pub const QUALIFIER: [u8; QUALIFIER_BYTES] = [
    10,
    crate::setup::DEVICE_QUALIFIER,
    0x00,
    0x02,
    0xEF,
    0x02,
    0x01,
    CONTROL_PACKET,
    1, // bNumConfigurations
    0,
];

/// The configuration at high speed.
pub const HIGH_SPEED_CONFIGURATION: [u8; CONFIGURATION_BYTES] =
    configuration(HIGH_SPEED_BULK, HIGH_SPEED_NOTIFY_INTERVAL);
/// The configuration at full speed.
pub const FULL_SPEED_CONFIGURATION: [u8; CONFIGURATION_BYTES] =
    configuration(FULL_SPEED_BULK, FULL_SPEED_NOTIFY_INTERVAL);

/// The endpoints the configuration enables at high speed.
pub const HIGH_SPEED_ENDPOINTS: [EndpointInfo; 5] =
    endpoints(HIGH_SPEED_BULK, HIGH_SPEED_NOTIFY_INTERVAL);
/// The same at full speed.
pub const FULL_SPEED_ENDPOINTS: [EndpointInfo; 5] =
    endpoints(FULL_SPEED_BULK, FULL_SPEED_NOTIFY_INTERVAL);

/// The configuration, with bulk packets of `bulk` bytes and notifications
/// every `interval`: the serial port's descriptors, then adb's.
const fn configuration(bulk: u16, interval: u8) -> [u8; CONFIGURATION_BYTES] {
    let serial = serial_function(bulk, interval);
    let adb = adb_function(bulk);
    let mut whole = [0; CONFIGURATION_BYTES];
    let (head, tail) = whole.split_at_mut(SERIAL_FUNCTION_BYTES);
    head.copy_from_slice(&serial);
    tail.copy_from_slice(&adb);
    whole
}

/// The serial port's share of the configuration: the configuration
/// descriptor itself, and the ACM function under it.
const SERIAL_FUNCTION_BYTES: usize = 75;
/// adb's share: an interface and its two endpoints.
const ADB_FUNCTION_BYTES: usize = CONFIGURATION_BYTES - SERIAL_FUNCTION_BYTES;

/// adb's interface and endpoints, with bulk packets of `bulk` bytes.
const fn adb_function(bulk: u16) -> [u8; ADB_FUNCTION_BYTES] {
    let [bulk_low, bulk_high] = bulk.to_le_bytes();
    [
        // Interface 2: adb, two bulk endpoints.
        9,
        crate::setup::INTERFACE,
        ADB_INTERFACE,
        0,
        2,
        CLASS_VENDOR,
        SUBCLASS_ADB,
        PROTOCOL_ADB,
        0,
        7,
        crate::setup::ENDPOINT,
        ADB_OUT,
        0x02,
        bulk_low,
        bulk_high,
        0,
        7,
        crate::setup::ENDPOINT,
        ADB_IN,
        0x02,
        bulk_low,
        bulk_high,
        0,
    ]
}

/// The configuration descriptor and the serial port's function.
const fn serial_function(bulk: u16, interval: u8) -> [u8; SERIAL_FUNCTION_BYTES] {
    let [bulk_low, bulk_high] = bulk.to_le_bytes();
    let [notify_low, notify_high] = NOTIFY_PACKET.to_le_bytes();
    let [total_low, total_high] = (CONFIGURATION_BYTES as u16).to_le_bytes();
    [
        // Configuration 1: three interfaces, self-powered (the phone has
        // its battery), 2 mA from the bus.
        9,
        crate::setup::CONFIGURATION,
        total_low,
        total_high,
        3,
        1,
        0,
        0xC0,
        1,
        // Interface association: interfaces 0 and 1 are one ACM function.
        8,
        crate::setup::INTERFACE_ASSOCIATION,
        COMMUNICATION_INTERFACE,
        2,
        CLASS_COMMUNICATION,
        SUBCLASS_ACM,
        PROTOCOL_V250,
        0,
        // Interface 0: communication, one endpoint.
        9,
        crate::setup::INTERFACE,
        COMMUNICATION_INTERFACE,
        0,
        1,
        CLASS_COMMUNICATION,
        SUBCLASS_ACM,
        PROTOCOL_V250,
        0,
        // Header: CDC 1.10.
        5,
        CS_INTERFACE,
        0x00,
        0x10,
        0x01,
        // Call management: none by the device, data interface 1.
        5,
        CS_INTERFACE,
        0x01,
        0x00,
        DATA_INTERFACE,
        // Abstract control management.
        4,
        CS_INTERFACE,
        0x02,
        ACM_CAPABILITIES,
        // Union: interface 0 controls interface 1.
        5,
        CS_INTERFACE,
        0x06,
        COMMUNICATION_INTERFACE,
        DATA_INTERFACE,
        // Notifications: interrupt IN.
        7,
        crate::setup::ENDPOINT,
        NOTIFY_IN,
        0x03,
        notify_low,
        notify_high,
        interval,
        // Interface 1: data, two endpoints.
        9,
        crate::setup::INTERFACE,
        DATA_INTERFACE,
        0,
        2,
        CLASS_DATA,
        0,
        0,
        0,
        // Bulk OUT.
        7,
        crate::setup::ENDPOINT,
        DATA_OUT,
        0x02,
        bulk_low,
        bulk_high,
        0,
        // Bulk IN.
        7,
        crate::setup::ENDPOINT,
        DATA_IN,
        0x02,
        bulk_low,
        bulk_high,
        0,
    ]
}

const fn endpoints(bulk: u16, interval: u8) -> [EndpointInfo; 5] {
    [
        EndpointInfo {
            address: NOTIFY_IN,
            kind: TransferKind::Interrupt,
            max_packet: NOTIFY_PACKET,
            interval,
        },
        EndpointInfo {
            address: DATA_OUT,
            kind: TransferKind::Bulk,
            max_packet: bulk,
            interval: 0,
        },
        EndpointInfo {
            address: DATA_IN,
            kind: TransferKind::Bulk,
            max_packet: bulk,
            interval: 0,
        },
        EndpointInfo {
            address: ADB_OUT,
            kind: TransferKind::Bulk,
            max_packet: bulk,
            interval: 0,
        },
        EndpointInfo {
            address: ADB_IN,
            kind: TransferKind::Bulk,
            max_packet: bulk,
            interval: 0,
        },
    ]
}

/// The descriptors, with strings 1 to 3.
fn descriptors<'a>(strings: &'a [&'a str]) -> Descriptors<'a> {
    Descriptors {
        device: &DEVICE,
        qualifier: &QUALIFIER,
        full_speed: &FULL_SPEED_CONFIGURATION,
        high_speed: &HIGH_SPEED_CONFIGURATION,
        full_speed_endpoints: &FULL_SPEED_ENDPOINTS,
        high_speed_endpoints: &HIGH_SPEED_ENDPOINTS,
        strings,
    }
}

/// A line coding: PSTN 1.2 table 17.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineCoding {
    /// `dwDTERate`: bits per second.
    pub rate: u32,
    /// `bCharFormat`: 0 one stop bit, 1 one and a half, 2 two.
    pub stop_bits: u8,
    /// `bParityType`: 0 none, 1 odd, 2 even, 3 mark, 4 space.
    pub parity: u8,
    /// `bDataBits`: 5, 6, 7, 8 or 16.
    pub data_bits: u8,
}

impl LineCoding {
    /// 115200 baud, eight bits, no parity, one stop bit, until the host
    /// says otherwise.
    pub const DEFAULT: LineCoding = LineCoding {
        rate: 115_200,
        stop_bits: 0,
        parity: 0,
        data_bits: 8,
    };

    /// Its seven bytes.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 7] {
        let [r0, r1, r2, r3] = self.rate.to_le_bytes();
        [r0, r1, r2, r3, self.stop_bits, self.parity, self.data_bits]
    }

    /// From its seven bytes, if there are that many.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let [r0, r1, r2, r3, stop_bits, parity, data_bits, ..] = *bytes else {
            return None;
        };
        Some(LineCoding {
            rate: u32::from_le_bytes([r0, r1, r2, r3]),
            stop_bits,
            parity,
            data_bits,
        })
    }
}

/// A `SERIAL_STATE` notification for [`NOTIFY_IN`]: carrier detect and
/// data set ready as given, nothing else.
#[must_use]
pub const fn serial_state(carrier: bool, ready: bool) -> [u8; 10] {
    let state = (carrier as u8) | ((ready as u8) << 1);
    [
        DIRECTION_IN | TYPE_CLASS | RECIPIENT_INTERFACE,
        SERIAL_STATE,
        0,
        0,
        COMMUNICATION_INTERFACE,
        0,
        2,
        0,
        state,
        0,
    ]
}

/// The serial port: chapter 9's state and the class's.
#[derive(Clone, Debug)]
pub struct SerialPort {
    standard: Standard,
    serial: [u8; SERIAL_BYTES],
    serial_length: usize,
    line_coding: LineCoding,
    /// `SET_CONTROL_LINE_STATE`'s `wValue`: bit 0 DTR, bit 1 RTS.
    lines: u16,
    /// The class request whose OUT data stage is expected.
    pending: Option<u8>,
    /// Where `GET_LINE_CODING`'s answer is built.
    buffer: [u8; 7],
}

impl SerialPort {
    /// A port whose serial number string is `serial`: its first
    /// [`SERIAL_BYTES`] characters, anything but printable ASCII made `_`.
    #[must_use]
    pub fn new(serial: &str) -> Self {
        let mut kept = [0; SERIAL_BYTES];
        let mut length = 0;
        for (slot, character) in kept.iter_mut().zip(serial.chars()) {
            *slot = if character.is_ascii_graphic() {
                character as u8
            } else {
                b'_'
            };
            length += 1;
        }
        SerialPort {
            standard: Standard::new(),
            serial: kept,
            serial_length: length,
            line_coding: LineCoding::DEFAULT,
            lines: 0,
            pending: None,
            buffer: [0; 7],
        }
    }

    /// Chapter 9's state.
    #[must_use]
    pub const fn standard(&self) -> &Standard {
        &self.standard
    }

    /// Whether the host has configured the device, so its endpoints run.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.standard.state() == State::Configured
    }

    /// DTR: a program on the host has the port open.
    #[must_use]
    pub const fn dtr(&self) -> bool {
        self.lines & 1 != 0
    }

    /// RTS, as the host last set it.
    #[must_use]
    pub const fn rts(&self) -> bool {
        self.lines & 2 != 0
    }

    /// The line coding the host last set.
    #[must_use]
    pub const fn line_coding(&self) -> LineCoding {
        self.line_coding
    }

    /// The serial number string.
    #[must_use]
    pub fn serial(&self) -> &str {
        let (kept, _) = self.serial.split_at(self.serial_length.min(SERIAL_BYTES));
        core::str::from_utf8(kept).unwrap_or("")
    }

    /// Answer a class request: to the communication interface, once
    /// configured.
    fn class(&mut self, setup: &Setup) -> Reply<'_> {
        if !self.is_configured()
            || setup.recipient() != RECIPIENT_INTERFACE
            || setup.index != u16::from(COMMUNICATION_INTERFACE)
        {
            return Reply::Stall;
        }
        match (setup.request, setup.is_in(), setup.length) {
            (SET_LINE_CODING, false, 7) => {
                self.pending = Some(SET_LINE_CODING);
                Reply::Out(7)
            }
            (GET_LINE_CODING, true, length) if length > 0 => {
                self.buffer = self.line_coding.bytes();
                let (head, _) = self.buffer.split_at(usize::from(length).min(7));
                Reply::In(head)
            }
            (SET_CONTROL_LINE_STATE, false, 0) => {
                self.lines = setup.value & 3;
                Reply::Ack(crate::Effect::None)
            }
            _ => Reply::Stall,
        }
    }
}

impl Function for SerialPort {
    fn reset(&mut self) {
        self.standard.reset();
        self.lines = 0;
        self.pending = None;
    }

    fn set_speed(&mut self, speed: Speed) {
        self.standard.set_speed(speed);
    }

    fn setup(&mut self, setup: &Setup) -> Reply<'_> {
        self.pending = None;
        match setup.kind() {
            TYPE_STANDARD => {
                let (kept, _) = self.serial.split_at(self.serial_length.min(SERIAL_BYTES));
                let serial = core::str::from_utf8(kept).unwrap_or("");
                let strings = [MANUFACTURER_STRING, PRODUCT_STRING, serial];
                let reply = self.standard.answer(setup, &descriptors(&strings));
                if matches!(reply, Reply::Ack(crate::Effect::Configure(0))) {
                    self.lines = 0;
                }
                reply
            }
            TYPE_CLASS => self.class(setup),
            _ => Reply::Stall,
        }
    }

    fn out_data(&mut self, data: &[u8]) -> Status {
        match (self.pending.take(), LineCoding::parse(data)) {
            (Some(SET_LINE_CODING), Some(coding)) => {
                self.line_coding = coding;
                Status::Ack
            }
            _ => Status::Stall,
        }
    }

    fn endpoints(&self) -> &[EndpointInfo] {
        self.standard.endpoints(&descriptors(&[]))
    }
}
