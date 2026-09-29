//! The event buffer's entries (`union dwc3_event` in Linux's `core.h`).
//!
//! Each is a 32-bit word. Bit 0 clear is an endpoint's event: the physical
//! endpoint in bits 1-5, the event in 6-9, a status in 12-15 and a
//! parameter in 16-31. Bit 0 set with bits 1-7 zero is a device event: its
//! type in 8-11 and information in 16-24. Anything else -- a carkit or I²C
//! event on cores that have them -- is kept as it came.

/// The link states `DSTS` and a link state change or suspend event carry
/// that matter here: U3 is a suspended high-speed link.
pub const LINK_U3: u8 = 3;

/// An endpoint event's kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EndpointEvent {
    /// A transfer ended: its last TRB, or a short packet, completed.
    XferComplete,
    /// A TRB with IOC completed, and the transfer goes on.
    XferInProgress,
    /// The host wants to move data and no transfer is ready. On a control
    /// endpoint the status says which stage the host is in.
    XferNotReady,
    /// A FIFO under- or overrun.
    FifoError,
    /// A stream event.
    Stream,
    /// An endpoint command asked for IOC ended; the parameter's bits 8-11
    /// say which.
    CommandComplete,
    /// A reserved code.
    Other(u8),
}

/// A device event's kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceEvent {
    /// The host is gone: VBUS dropped, or the link went quiet.
    Disconnect,
    /// The host reset the bus.
    Reset,
    /// The reset ended and the link runs; `DSTS` says at what speed.
    ConnectDone,
    /// The link changed state, to the one given.
    LinkStateChange(u8),
    /// The host woke the link.
    Wakeup,
    /// The link was suspended, to the state given.
    Suspend(u8),
    /// A start of frame.
    StartOfFrame,
    /// An erratic error.
    Erratic,
    /// A device command ended.
    CommandComplete,
    /// The event buffer overflowed and events were lost.
    Overflow,
    /// A reserved or vendor code.
    Other(u8),
}

/// One entry of the event buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// An endpoint's.
    Endpoint {
        /// The physical endpoint: the number times two, plus one for IN.
        physical: u8,
        /// What happened.
        kind: EndpointEvent,
        /// The status bits.
        status: u8,
        /// The parameter.
        parameter: u16,
    },
    /// The device's.
    Device(DeviceEvent),
    /// Something else, as it came.
    Other(u32),
}

/// A control endpoint's transfer-not-ready status: the host is in the data
/// stage.
pub const CONTROL_DATA: u8 = 1;
/// The host is in the status stage.
pub const CONTROL_STATUS: u8 = 2;

/// An endpoint event's status: the transfer ended on a short packet.
pub const STATUS_SHORT: u8 = 1 << 1;

impl Event {
    /// Decode a raw entry.
    #[must_use]
    pub const fn decode(raw: u32) -> Self {
        if raw & 1 == 0 {
            return Event::Endpoint {
                physical: ((raw >> 1) & 0x1F) as u8,
                kind: endpoint_event(((raw >> 6) & 0xF) as u8),
                status: ((raw >> 12) & 0xF) as u8,
                parameter: (raw >> 16) as u16,
            };
        }
        if (raw >> 1) & 0x7F != 0 {
            return Event::Other(raw);
        }
        let information = ((raw >> 16) & 0x1FF) as u8;
        let state = information & 0xF;
        Event::Device(match (raw >> 8) & 0xF {
            0 => DeviceEvent::Disconnect,
            1 => DeviceEvent::Reset,
            2 => DeviceEvent::ConnectDone,
            3 => DeviceEvent::LinkStateChange(state),
            4 => DeviceEvent::Wakeup,
            6 => DeviceEvent::Suspend(state),
            7 => DeviceEvent::StartOfFrame,
            9 => DeviceEvent::Erratic,
            10 => DeviceEvent::CommandComplete,
            11 => DeviceEvent::Overflow,
            other => DeviceEvent::Other(other as u8),
        })
    }
}

const fn endpoint_event(code: u8) -> EndpointEvent {
    match code {
        1 => EndpointEvent::XferComplete,
        2 => EndpointEvent::XferInProgress,
        3 => EndpointEvent::XferNotReady,
        4 => EndpointEvent::FifoError,
        6 => EndpointEvent::Stream,
        7 => EndpointEvent::CommandComplete,
        other => EndpointEvent::Other(other),
    }
}
