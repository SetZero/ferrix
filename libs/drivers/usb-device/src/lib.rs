//! The device side of USB, for whatever controller carries it: chapter 9's
//! standard requests, and a CDC-ACM serial port as the one function.
//!
//! The Pixel 7 is to show up on a PC as `/dev/ttyACM*` while it runs Ferrix
//! natively (`docs/PIXEL7-USB-HANDOVER.md`). Its controller, a DWC3, is
//! driven by `ferrix-dwc3`; nothing here knows it. A controller driver
//! needs only three things from the function it carries, and [`Function`]
//! is those three:
//!
//! * what to answer a SETUP packet with -- data in, data out of an expected
//!   length, a bare acknowledgement, or a stall ([`Reply`]) -- and what the
//!   controller itself must do before the status stage ([`Effect`]): take a
//!   new address, enable the configuration's endpoints, halt one;
//! * what an OUT data stage's bytes were worth ([`Status`]);
//! * which endpoints a configuration enables ([`EndpointInfo`]), at the
//!   speed the bus came up at.
//!
//! [`standard`] is chapter 9: the state a device moves through (default,
//! addressed, configured) and the requests every device answers.
//! [`acm`] is the serial port: its descriptors and the three class requests
//! Linux's `cdc_acm` sends. Everything is a pure function of the packets, so
//! it is tested here on its own, byte by byte against the specifications,
//! and again under `ferrix-dwc3`'s model of the controller.
//!
//! # What is not here
//!
//! Super speed (no BOS descriptor; `bcdUSB` says 2.0), remote wakeup, test
//! modes, alternate settings other than 0, and any function but the one.

#![no_std]
#![forbid(unsafe_code)]

pub mod acm;
pub mod setup;
pub mod standard;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

pub use setup::Setup;

/// The speed the bus came up at, which decides the descriptors' packet
/// sizes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    /// 12 Mbit/s.
    Full,
    /// 480 Mbit/s.
    High,
}

/// What a controller answers a SETUP packet with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reply<'a> {
    /// A data stage to the host with these bytes, already cut to the
    /// request's `wLength`. It may be shorter; the controller ends it with
    /// a zero-length packet when it is a multiple of the packet size.
    In(&'a [u8]),
    /// A data stage from the host of this many bytes, at most `wLength`;
    /// the controller hands them to [`Function::out_data`].
    Out(usize),
    /// No data stage: acknowledge in the status stage, having done the
    /// effect first.
    Ack(Effect),
    /// Refuse the request: a STALL handshake.
    Stall,
}

/// What the controller does before it acknowledges a request with no data
/// stage. Each of these is a register write or an endpoint command, which
/// is why the function cannot do it itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Nothing.
    None,
    /// Answer to this address from the status stage on. USB 2.0 §9.4.6
    /// has the device take the address once the status stage completes;
    /// the DWC3 databook has the driver write it before, and the core
    /// switches when the stage is over.
    Address(u8),
    /// Enable [`Function::endpoints`] for configuration `n`, or disable
    /// every endpoint but the control one for 0.
    Configure(u8),
    /// Halt the endpoint with this address, or clear its halt and reset
    /// its data toggle.
    Halt {
        /// The endpoint's address, direction bit included.
        endpoint: u8,
        /// Halt it, rather than clear the halt.
        halted: bool,
    },
}

/// What an OUT data stage's bytes were worth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Acknowledge them in the status stage.
    Ack,
    /// Stall the status stage.
    Stall,
}

/// An endpoint's transfer type, for the two a configuration here enables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransferKind {
    /// Bulk.
    Bulk,
    /// Interrupt.
    Interrupt,
}

/// An endpoint a configuration enables, as its descriptor gives it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EndpointInfo {
    /// `bEndpointAddress`: the number, and bit 7 for IN.
    pub address: u8,
    /// Its transfer type.
    pub kind: TransferKind,
    /// `wMaxPacketSize`.
    pub max_packet: u16,
    /// `bInterval`, as the descriptor has it: frames at full speed, a power
    /// of two of microframes at high speed; 0 for bulk.
    pub interval: u8,
}

impl EndpointInfo {
    /// Whether it sends to the host.
    #[must_use]
    pub const fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
}

/// What a controller driver needs of the function it carries.
///
/// The driver calls these from its interrupt handling, one at a time, in
/// the order the bus makes them happen.
pub trait Function {
    /// The bus was reset, or the cable pulled: back to the default state,
    /// no address, no configuration, nothing halted.
    fn reset(&mut self);

    /// The bus came up after a reset, at `speed`.
    fn set_speed(&mut self, speed: Speed);

    /// Answer a SETUP packet.
    fn setup(&mut self, setup: &Setup) -> Reply<'_>;

    /// The bytes of the OUT data stage the last [`Reply::Out`] asked for,
    /// which may be fewer if the host sent fewer.
    fn out_data(&mut self, data: &[u8]) -> Status;

    /// The endpoints the current configuration enables, at the current
    /// speed. Empty when not configured.
    fn endpoints(&self) -> &[EndpointInfo];
}
