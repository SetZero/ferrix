//! The driver on the model, with a function, and the host's side of an
//! enumeration as Linux's hub driver and `cdc_acm` do it.

use std::string::String;
use std::vec::Vec;

use ferrix_usb_device::acm::SerialPort;
use ferrix_usb_device::{Function, Setup};

use super::model::{HostError, Memory, Model, Regs, Shared, Time};
use crate::layout::AREA_BYTES;
use crate::{Controller, Notice, Parts};

pub(super) type TestController = Controller<Regs, Memory, Time>;

pub(super) fn parts(model: &Shared) -> Parts<Regs, Memory, Time> {
    Parts {
        registers: Regs(model.clone()),
        memory: Memory(model.clone(), AREA_BYTES),
        clock: Time(model.clone()),
    }
}

/// The address the host gives.
pub(super) const ADDRESS: u16 = 5;

/// The driver, its function, and what its interrupts have said so far.
pub(super) struct Rig<F: Function = SerialPort> {
    pub(super) model: Shared,
    pub(super) controller: TestController,
    pub(super) function: F,
    pub(super) notice: Notice,
}

impl Rig<SerialPort> {
    pub(super) fn new() -> Self {
        Self::with(SerialPort::new("P7-TEST"))
    }
}

impl<F: Function> Rig<F> {
    pub(super) fn with(function: F) -> Self {
        let model = Model::new();
        let controller = Controller::start(parts(&model))
            .map_err(|(error, _)| error)
            .expect("the controller starts");
        Rig {
            model,
            controller,
            function,
            notice: Notice::default(),
        }
    }

    pub(super) fn violations(&self) -> Vec<String> {
        self.model.borrow().violations.clone()
    }

    pub(super) fn assert_clean(&self) {
        assert_eq!(self.violations(), Vec::<String>::new(), "no violations");
    }

    /// Run the bus and the driver until neither has anything to do.
    pub(super) fn pump(&mut self) {
        for _ in 0..10_000 {
            let moved = self.model.borrow_mut().advance();
            let interrupt = self.model.borrow().interrupt();
            if interrupt {
                let notice = self
                    .controller
                    .on_interrupt(&mut self.function)
                    .expect("events handled");
                merge(&mut self.notice, notice);
            }
            if !moved && !interrupt {
                return;
            }
        }
        panic!("the bus never settled");
    }

    pub(super) fn attach(&mut self, high_speed: bool) {
        self.model.borrow_mut().attach(high_speed);
        self.pump();
    }

    pub(super) fn bus_reset(&mut self) {
        self.model.borrow_mut().bus_reset();
        self.pump();
    }

    /// A control transfer, start to end.
    pub(super) fn control(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
        out: &[u8],
    ) -> Result<Vec<u8>, HostError> {
        let setup = Setup {
            request_type,
            request,
            value,
            index,
            length,
        };
        self.model.borrow_mut().begin_control(setup, out);
        self.pump();
        self.model
            .borrow_mut()
            .take_result()
            .expect("the transfer ended")
    }

    /// `GET_DESCRIPTOR`.
    pub(super) fn descriptor(
        &mut self,
        kind: u8,
        index: u8,
        language: u16,
        length: u16,
    ) -> Vec<u8> {
        let value = (u16::from(kind) << 8) | u16::from(index);
        self.control(0x80, 6, value, language, length, &[])
            .expect("a descriptor")
    }

    /// Enumerate as Linux does at a hub port, then open the tty: the
    /// device descriptor's first 64 bytes at address 0, a second reset,
    /// the address, the device descriptor, the configuration's header and
    /// then all of it, the strings, the configuration, and `cdc_acm`'s line
    /// coding and DTR. The descriptors, as read.
    pub(super) fn enumerate(&mut self, high_speed: bool) -> Enumerated {
        self.attach(high_speed);
        let first = self.descriptor(1, 0, 0, 64);
        assert_eq!(first.len(), 18, "a whole device descriptor for 64 asked");
        self.bus_reset();
        let _ = self
            .control(0x00, 5, ADDRESS, 0, 0, &[])
            .expect("SET_ADDRESS");
        let device = self.descriptor(1, 0, 0, 18);
        let header = self.descriptor(2, 0, 0, 9);
        let total = u16::from_le_bytes([header[2], header[3]]);
        let configuration = self.descriptor(2, 0, 0, total);
        let languages = self.descriptor(3, 0, 0, 255);
        let product = self.descriptor(3, 2, 0x0409, 255);
        let manufacturer = self.descriptor(3, 1, 0x0409, 255);
        let serial = self.descriptor(3, 3, 0x0409, 255);
        let _ = self
            .control(0x00, 9, 1, 0, 0, &[])
            .expect("SET_CONFIGURATION");
        let _ = self
            .control(0x21, 0x20, 0, 0, 7, &[0x80, 0x25, 0, 0, 0, 0, 8])
            .expect("SET_LINE_CODING");
        let _ = self
            .control(0x21, 0x22, 3, 0, 0, &[])
            .expect("SET_CONTROL_LINE_STATE");
        Enumerated {
            device,
            configuration,
            languages,
            strings: [manufacturer, product, serial].map(|bytes| text(&bytes)),
        }
    }
}

/// What an enumeration read.
pub(super) struct Enumerated {
    pub(super) device: Vec<u8>,
    pub(super) configuration: Vec<u8>,
    pub(super) languages: Vec<u8>,
    pub(super) strings: [String; 3],
}

/// A string descriptor's text.
pub(super) fn text(descriptor: &[u8]) -> String {
    assert_eq!(usize::from(descriptor[0]), descriptor.len(), "bLength");
    assert_eq!(descriptor[1], 3, "a string");
    let units: Vec<u16> = descriptor[2..]
        .chunks(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).expect("UTF-16")
}

fn merge(into: &mut Notice, from: Notice) {
    into.reset |= from.reset;
    into.connected = from.connected.or(into.connected);
    into.disconnected |= from.disconnected;
    into.configuration |= from.configuration;
    into.received |= from.received;
    into.sent |= from.sent;
    into.suspended |= from.suspended;
}
