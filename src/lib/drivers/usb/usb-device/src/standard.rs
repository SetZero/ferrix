//! Chapter 9: the states a device moves through, and the standard requests
//! every device answers (USB 2.0 §9.4).
//!
//! [`Standard`] keeps the state -- default, addressed or configured, the
//! configuration, which endpoints are halted -- and answers the requests
//! from a [`Descriptors`] set. What the requests do to the controller (an
//! address, endpoints to enable, a halt) comes back as an [`Effect`] for
//! the controller driver to carry out; the state here is only what the
//! answers to later requests depend on.
//!
//! A request the device does not support, or one that is not valid in the
//! state it is in, is stalled, as §9.4 says a device does with a request it
//! does not understand: `SET_DESCRIPTOR`, `SYNCH_FRAME`, remote wakeup and
//! test mode, an alternate setting other than 0, and any descriptor type
//! but the five a USB 2.0 device answers.

use crate::setup::{
    CLEAR_FEATURE, CONFIGURATION, DEVICE, DEVICE_QUALIFIER, DIRECTION_IN, ENDPOINT_HALT,
    GET_CONFIGURATION, GET_DESCRIPTOR, GET_INTERFACE, GET_STATUS, OTHER_SPEED_CONFIGURATION,
    RECIPIENT_DEVICE, RECIPIENT_ENDPOINT, RECIPIENT_INTERFACE, SET_ADDRESS, SET_CONFIGURATION,
    SET_FEATURE, SET_INTERFACE, STRING,
};
use crate::{Effect, EndpointInfo, Reply, Setup, Speed};

/// The largest descriptor answered from the scratch buffer: a string of
/// 126 UTF-16 units, or a configuration copied as the other speed's.
pub const BUFFER_BYTES: usize = 256;

/// The language ID string, string 0: US English alone.
const LANGUAGES: [u8; 4] = [4, STRING, 0x09, 0x04];

/// Everything a device describes itself with.
#[derive(Clone, Copy, Debug)]
pub struct Descriptors<'a> {
    /// The device descriptor.
    pub device: &'a [u8],
    /// The device qualifier: the device descriptor's fields at the other
    /// speed.
    pub qualifier: &'a [u8],
    /// The configuration, with its interfaces and endpoints, at full speed.
    pub full_speed: &'a [u8],
    /// The same at high speed.
    pub high_speed: &'a [u8],
    /// The endpoints the configuration enables at full speed.
    pub full_speed_endpoints: &'a [EndpointInfo],
    /// The same at high speed.
    pub high_speed_endpoints: &'a [EndpointInfo],
    /// Strings 1 and on, in US English.
    pub strings: &'a [&'a str],
}

impl<'a> Descriptors<'a> {
    /// The configuration descriptor at `speed`.
    #[must_use]
    pub const fn configuration(&self, speed: Speed) -> &'a [u8] {
        match speed {
            Speed::Full => self.full_speed,
            Speed::High => self.high_speed,
        }
    }

    /// The endpoints the configuration enables at `speed`.
    #[must_use]
    pub const fn endpoints(&self, speed: Speed) -> &'a [EndpointInfo] {
        match speed {
            Speed::Full => self.full_speed_endpoints,
            Speed::High => self.high_speed_endpoints,
        }
    }

    /// `bConfigurationValue`, the one value `SET_CONFIGURATION` takes
    /// besides 0.
    fn configuration_value(&self) -> u8 {
        self.high_speed.get(5).copied().unwrap_or(1)
    }

    /// `bNumInterfaces`.
    fn interfaces(&self) -> u16 {
        u16::from(self.high_speed.get(4).copied().unwrap_or(0))
    }

    /// `bmAttributes`' self-powered bit.
    fn self_powered(&self) -> bool {
        self.high_speed
            .get(7)
            .is_some_and(|attributes| attributes & 0x40 != 0)
    }
}

/// The state §9.1.1 names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// After a reset: address 0, no configuration.
    Default,
    /// An address, no configuration.
    Address,
    /// Configured: the function's endpoints are live.
    Configured,
}

/// Chapter 9's state, and the buffer answers are built in.
#[derive(Clone, Debug)]
pub struct Standard {
    state: State,
    speed: Speed,
    configuration: u8,
    /// A bit per endpoint: its number, plus 16 for IN.
    halted: u32,
    buffer: [u8; BUFFER_BYTES],
}

impl Default for Standard {
    fn default() -> Self {
        Self::new()
    }
}

impl Standard {
    /// A device just attached: default state, high speed until told.
    #[must_use]
    pub const fn new() -> Self {
        Standard {
            state: State::Default,
            speed: Speed::High,
            configuration: 0,
            halted: 0,
            buffer: [0; BUFFER_BYTES],
        }
    }

    /// Back to the default state, as a bus reset leaves a device.
    pub fn reset(&mut self) {
        self.state = State::Default;
        self.configuration = 0;
        self.halted = 0;
    }

    /// The bus runs at `speed`.
    pub fn set_speed(&mut self, speed: Speed) {
        self.speed = speed;
    }

    /// The state.
    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// The speed.
    #[must_use]
    pub const fn speed(&self) -> Speed {
        self.speed
    }

    /// The configuration, 0 for none.
    #[must_use]
    pub const fn configuration(&self) -> u8 {
        self.configuration
    }

    /// Whether the endpoint with this address is halted.
    #[must_use]
    pub const fn is_halted(&self, endpoint: u8) -> bool {
        self.halted & halt_bit(endpoint) != 0
    }

    /// The endpoints the current configuration enables, none when not
    /// configured.
    #[must_use]
    pub fn endpoints<'a>(&self, descriptors: &Descriptors<'a>) -> &'a [EndpointInfo] {
        if self.state == State::Configured {
            descriptors.endpoints(self.speed)
        } else {
            &[]
        }
    }

    /// Answer a standard request. What the answer carries is copied into a
    /// buffer of this state's, so the descriptors may be built for the call.
    pub fn answer(&mut self, setup: &Setup, descriptors: &Descriptors<'_>) -> Reply<'_> {
        match setup.request {
            GET_STATUS => self.get_status(setup, descriptors),
            CLEAR_FEATURE | SET_FEATURE => self.feature(setup, descriptors),
            SET_ADDRESS => self.set_address(setup),
            GET_DESCRIPTOR => self.get_descriptor(setup, descriptors),
            GET_CONFIGURATION => self.get_configuration(setup),
            SET_CONFIGURATION => self.set_configuration(setup, descriptors),
            GET_INTERFACE => self.get_interface(setup, descriptors),
            SET_INTERFACE => self.set_interface(setup, descriptors),
            _ => Reply::Stall,
        }
    }

    /// Answer with `bytes` from the buffer, cut to `wLength`.
    fn reply(&mut self, setup: &Setup, bytes: &[u8]) -> Reply<'_> {
        let length = bytes.len().min(usize::from(setup.length)).min(BUFFER_BYTES);
        let (head, _) = self.buffer.split_at_mut(length);
        let (source, _) = bytes.split_at(length);
        head.copy_from_slice(source);
        Reply::In(head)
    }

    fn get_status(&mut self, setup: &Setup, descriptors: &Descriptors<'_>) -> Reply<'_> {
        if setup.value != 0 || !setup.is_in() {
            return Reply::Stall;
        }
        let status = match setup.recipient() {
            RECIPIENT_DEVICE => Some(u8::from(descriptors.self_powered())),
            RECIPIENT_INTERFACE => self.interface_exists(setup, descriptors).then_some(0),
            RECIPIENT_ENDPOINT => self
                .endpoint_exists(setup.index, descriptors)
                .then(|| u8::from(self.is_halted(setup.index as u8))),
            _ => None,
        };
        match status {
            Some(status) => self.reply(setup, &[status, 0]),
            None => Reply::Stall,
        }
    }

    fn feature(&mut self, setup: &Setup, descriptors: &Descriptors<'_>) -> Reply<'_> {
        let halted = setup.request == SET_FEATURE;
        let endpoint = setup.index as u8;
        if setup.request_type != RECIPIENT_ENDPOINT
            || setup.value != ENDPOINT_HALT
            || setup.length != 0
        {
            // Remote wakeup and test mode are not offered, and there are
            // no interface features.
            return Reply::Stall;
        }
        if endpoint & 0x7F == 0 {
            // The control endpoint's halt is not implemented, which §9.4.5
            // allows; clearing it is harmless.
            return if halted {
                Reply::Stall
            } else {
                Reply::Ack(Effect::None)
            };
        }
        if !self.endpoint_exists(setup.index, descriptors) {
            return Reply::Stall;
        }
        if halted {
            self.halted |= halt_bit(endpoint);
        } else {
            self.halted &= !halt_bit(endpoint);
        }
        Reply::Ack(Effect::Halt { endpoint, halted })
    }

    fn set_address(&mut self, setup: &Setup) -> Reply<'_> {
        if setup.request_type != RECIPIENT_DEVICE
            || setup.value > 127
            || setup.index != 0
            || setup.length != 0
            || self.state == State::Configured
        {
            return Reply::Stall;
        }
        self.state = if setup.value == 0 {
            State::Default
        } else {
            State::Address
        };
        Reply::Ack(Effect::Address(setup.value as u8))
    }

    fn get_descriptor(&mut self, setup: &Setup, descriptors: &Descriptors<'_>) -> Reply<'_> {
        if setup.request_type != DIRECTION_IN | RECIPIENT_DEVICE {
            return Reply::Stall;
        }
        let other = match self.speed {
            Speed::Full => Speed::High,
            Speed::High => Speed::Full,
        };
        match (setup.descriptor_type(), setup.descriptor_index()) {
            (DEVICE, 0) => self.reply(setup, descriptors.device),
            (CONFIGURATION, 0) => self.reply(setup, descriptors.configuration(self.speed)),
            (DEVICE_QUALIFIER, 0) => self.reply(setup, descriptors.qualifier),
            (OTHER_SPEED_CONFIGURATION, 0) => {
                self.other_speed(setup, descriptors.configuration(other))
            }
            (STRING, 0) => self.reply(setup, &LANGUAGES),
            (STRING, index) => match descriptors.strings.get(usize::from(index) - 1) {
                Some(text) => self.string(setup, text),
                None => Reply::Stall,
            },
            _ => Reply::Stall,
        }
    }

    /// The configuration at the other speed, typed as the other speed's.
    fn other_speed(&mut self, setup: &Setup, configuration: &[u8]) -> Reply<'_> {
        let length = configuration.len().min(BUFFER_BYTES);
        let (head, _) = self.buffer.split_at_mut(length);
        let (source, _) = configuration.split_at(length);
        head.copy_from_slice(source);
        if let Some(kind) = head.get_mut(1) {
            *kind = OTHER_SPEED_CONFIGURATION;
        }
        let length = length.min(usize::from(setup.length));
        let (head, _) = self.buffer.split_at(length);
        Reply::In(head)
    }

    /// A string descriptor: `text` in UTF-16LE after the two header bytes.
    fn string(&mut self, setup: &Setup, text: &str) -> Reply<'_> {
        let mut length = 2;
        for unit in text.encode_utf16() {
            let Some(pair) = self.buffer.get_mut(length..length + 2) else {
                break;
            };
            pair.copy_from_slice(&unit.to_le_bytes());
            length += 2;
        }
        let length = length.min(254);
        if let Some(header) = self.buffer.get_mut(..2) {
            header.copy_from_slice(&[length as u8, STRING]);
        }
        let length = length.min(usize::from(setup.length));
        let (head, _) = self.buffer.split_at(length);
        Reply::In(head)
    }

    fn get_configuration(&mut self, setup: &Setup) -> Reply<'_> {
        if setup.request_type != DIRECTION_IN | RECIPIENT_DEVICE {
            return Reply::Stall;
        }
        let configuration = self.configuration;
        self.reply(setup, &[configuration])
    }

    fn set_configuration(&mut self, setup: &Setup, descriptors: &Descriptors<'_>) -> Reply<'_> {
        let value = setup.value;
        let valid = value == 0 || value == u16::from(descriptors.configuration_value());
        if setup.request_type != RECIPIENT_DEVICE
            || !valid
            || setup.length != 0
            || self.state == State::Default
        {
            return Reply::Stall;
        }
        self.configuration = value as u8;
        self.halted = 0;
        self.state = if value == 0 {
            State::Address
        } else {
            State::Configured
        };
        Reply::Ack(Effect::Configure(value as u8))
    }

    fn get_interface(&mut self, setup: &Setup, descriptors: &Descriptors<'_>) -> Reply<'_> {
        if setup.request_type != DIRECTION_IN | RECIPIENT_INTERFACE
            || !self.interface_exists(setup, descriptors)
        {
            return Reply::Stall;
        }
        self.reply(setup, &[0])
    }

    fn set_interface(&mut self, setup: &Setup, descriptors: &Descriptors<'_>) -> Reply<'_> {
        if setup.request_type != RECIPIENT_INTERFACE
            || setup.value != 0
            || setup.length != 0
            || !self.interface_exists(setup, descriptors)
        {
            return Reply::Stall;
        }
        Reply::Ack(Effect::None)
    }

    /// Whether `wIndex` names an interface of the configuration.
    fn interface_exists(&self, setup: &Setup, descriptors: &Descriptors<'_>) -> bool {
        self.state == State::Configured && setup.index < descriptors.interfaces()
    }

    /// Whether `index` names the control endpoint or one the
    /// configuration enables.
    fn endpoint_exists(&self, index: u16, descriptors: &Descriptors<'_>) -> bool {
        let Ok(address) = u8::try_from(index) else {
            return false;
        };
        address & 0x7F == 0
            || self
                .endpoints(descriptors)
                .iter()
                .any(|endpoint| endpoint.address == address)
    }
}

/// The bit of [`Standard`]'s halt set for the endpoint with `address`.
const fn halt_bit(address: u8) -> u32 {
    let direction = if address & 0x80 != 0 { 16 } else { 0 };
    1 << ((address & 0x0F) as u32 + direction)
}
