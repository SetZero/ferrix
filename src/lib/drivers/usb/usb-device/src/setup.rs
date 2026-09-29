//! The eight-byte SETUP packet, and the names USB 2.0's chapter 9 gives its
//! fields' values (tables 9-2 to 9-6).

/// An eight-byte SETUP packet, as the host sent it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Setup {
    /// `bmRequestType`: direction, type and recipient.
    pub request_type: u8,
    /// `bRequest`.
    pub request: u8,
    /// `wValue`.
    pub value: u16,
    /// `wIndex`.
    pub index: u16,
    /// `wLength`: the most the data stage may carry, and whether there is
    /// one.
    pub length: u16,
}

/// `bmRequestType`: device to host.
pub const DIRECTION_IN: u8 = 0x80;
/// `bmRequestType`'s type field.
pub const TYPE_MASK: u8 = 0x60;
/// A standard request.
pub const TYPE_STANDARD: u8 = 0x00;
/// A class request.
pub const TYPE_CLASS: u8 = 0x20;
/// `bmRequestType`'s recipient field.
pub const RECIPIENT_MASK: u8 = 0x1F;
/// To the device.
pub const RECIPIENT_DEVICE: u8 = 0x00;
/// To an interface, named by `wIndex`.
pub const RECIPIENT_INTERFACE: u8 = 0x01;
/// To an endpoint, named by `wIndex`.
pub const RECIPIENT_ENDPOINT: u8 = 0x02;

/// `GET_STATUS`.
pub const GET_STATUS: u8 = 0;
/// `CLEAR_FEATURE`.
pub const CLEAR_FEATURE: u8 = 1;
/// `SET_FEATURE`.
pub const SET_FEATURE: u8 = 3;
/// `SET_ADDRESS`.
pub const SET_ADDRESS: u8 = 5;
/// `GET_DESCRIPTOR`.
pub const GET_DESCRIPTOR: u8 = 6;
/// `SET_DESCRIPTOR`, which nothing here accepts.
pub const SET_DESCRIPTOR: u8 = 7;
/// `GET_CONFIGURATION`.
pub const GET_CONFIGURATION: u8 = 8;
/// `SET_CONFIGURATION`.
pub const SET_CONFIGURATION: u8 = 9;
/// `GET_INTERFACE`.
pub const GET_INTERFACE: u8 = 10;
/// `SET_INTERFACE`.
pub const SET_INTERFACE: u8 = 11;
/// `SYNCH_FRAME`, which nothing here accepts.
pub const SYNCH_FRAME: u8 = 12;

/// The feature selector for an endpoint's halt.
pub const ENDPOINT_HALT: u16 = 0;
/// The feature selector for remote wakeup, which is not offered.
pub const DEVICE_REMOTE_WAKEUP: u16 = 1;
/// The feature selector for test mode, which is not offered.
pub const TEST_MODE: u16 = 2;

/// The device descriptor's type.
pub const DEVICE: u8 = 1;
/// The configuration descriptor's.
pub const CONFIGURATION: u8 = 2;
/// A string descriptor's.
pub const STRING: u8 = 3;
/// An interface descriptor's.
pub const INTERFACE: u8 = 4;
/// An endpoint descriptor's.
pub const ENDPOINT: u8 = 5;
/// The device qualifier's: what the device would be at the other speed.
pub const DEVICE_QUALIFIER: u8 = 6;
/// The other-speed configuration's: the configuration at the other speed.
pub const OTHER_SPEED_CONFIGURATION: u8 = 7;
/// An interface association descriptor's.
pub const INTERFACE_ASSOCIATION: u8 = 0x0B;

/// US English, the one language the strings are in.
pub const LANGUAGE_US: u16 = 0x0409;

impl Setup {
    /// The packet from its bytes, little-endian as the bus carries them.
    #[must_use]
    pub const fn parse(bytes: [u8; 8]) -> Self {
        let [request_type, request, v0, v1, i0, i1, l0, l1] = bytes;
        Setup {
            request_type,
            request,
            value: u16::from_le_bytes([v0, v1]),
            index: u16::from_le_bytes([i0, i1]),
            length: u16::from_le_bytes([l0, l1]),
        }
    }

    /// The packet's bytes.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 8] {
        let [v0, v1] = self.value.to_le_bytes();
        let [i0, i1] = self.index.to_le_bytes();
        let [l0, l1] = self.length.to_le_bytes();
        [self.request_type, self.request, v0, v1, i0, i1, l0, l1]
    }

    /// Whether the data stage, if any, goes to the host.
    #[must_use]
    pub const fn is_in(&self) -> bool {
        self.request_type & DIRECTION_IN != 0
    }

    /// The request's type: [`TYPE_STANDARD`], [`TYPE_CLASS`] or vendor.
    #[must_use]
    pub const fn kind(&self) -> u8 {
        self.request_type & TYPE_MASK
    }

    /// The request's recipient: [`RECIPIENT_DEVICE`] and the rest.
    #[must_use]
    pub const fn recipient(&self) -> u8 {
        self.request_type & RECIPIENT_MASK
    }

    /// The descriptor type a `GET_DESCRIPTOR` asks for, `wValue`'s high
    /// byte.
    #[must_use]
    pub const fn descriptor_type(&self) -> u8 {
        (self.value >> 8) as u8
    }

    /// The descriptor index, `wValue`'s low byte.
    #[must_use]
    pub const fn descriptor_index(&self) -> u8 {
        (self.value & 0xFF) as u8
    }
}
