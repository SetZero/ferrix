//! Bring-up, a Linux host enumerating the serial port, and control
//! transfers that end otherwise.

use std::vec;

use ferrix_usb_device::acm::{
    DEVICE, FULL_SPEED_CONFIGURATION, HIGH_SPEED_CONFIGURATION, LineCoding, SerialPort,
};
use ferrix_usb_device::{EndpointInfo, Function, Reply, Setup, Speed, Status};

use super::model::{HostError, Regs};
use super::rig::{ADDRESS, Rig};
use crate::Registers;
use crate::regs::{DALEPENA, DCFG, DEPCMD_DEPSTARTCFG, DEPCMD_STARTTRANSFER};

#[test]
fn starts_as_the_tree_says() {
    let rig = Rig::new();
    // The model checks the registers against the tree when the run bit
    // goes on.
    rig.assert_clean();
    let model = rig.model.borrow();
    assert!(!model.is_halted(), "running");
    assert_eq!(model.dcfg_address(), 0, "address 0");
    let codes: vec::Vec<(u8, u32)> = model
        .commands
        .iter()
        .map(|&(physical, command)| (physical, command & 0xF))
        .collect();
    assert_eq!(codes[0], (0, DEPCMD_DEPSTARTCFG), "DEPSTARTCFG first");
    assert_eq!(
        codes.last(),
        Some(&(0, DEPCMD_STARTTRANSFER)),
        "a SETUP TRB last"
    );
}

#[test]
fn enumerates_like_linux() {
    let mut rig = Rig::new();
    let found = rig.enumerate(true);
    rig.assert_clean();
    assert_eq!(rig.notice.connected, Some(Speed::High), "high speed");
    assert_eq!(found.device, DEVICE, "the device descriptor");
    assert_eq!(
        found.configuration, HIGH_SPEED_CONFIGURATION,
        "the configuration"
    );
    assert_eq!(found.languages, [4, 3, 0x09, 0x04], "US English");
    assert_eq!(
        found.strings,
        ["Ferrix", "Ferrix console", "P7-TEST"],
        "the strings"
    );
    assert_eq!(rig.model.borrow().host_address, ADDRESS as u8, "addressed");
    assert!(rig.function.is_configured(), "configured");
    assert!(rig.controller.is_configured(), "the controller too");
    assert!(rig.notice.configuration, "and it said so");
    assert!(rig.function.dtr(), "DTR up");
    assert_eq!(
        rig.function.line_coding(),
        LineCoding {
            rate: 9600,
            stop_bits: 0,
            parity: 0,
            data_bits: 8
        },
        "9600 8N1"
    );
    let active = Regs(rig.model.clone()).read32(DALEPENA);
    assert_eq!(
        active, 0b11_1011,
        "endpoint 0 both ways, 0x81, 0x02 and 0x82"
    );
}

#[test]
fn enumerates_at_full_speed() {
    let mut rig = Rig::new();
    let found = rig.enumerate(false);
    rig.assert_clean();
    assert_eq!(rig.controller.speed(), Some(Speed::Full), "full speed");
    assert_eq!(
        found.configuration, FULL_SPEED_CONFIGURATION,
        "64-byte bulk"
    );
    let other = rig.descriptor(7, 0, 0, 255);
    assert_eq!(
        other[2..],
        HIGH_SPEED_CONFIGURATION[2..],
        "the other speed is high"
    );
}

#[test]
fn qualifier_and_status() {
    let mut rig = Rig::new();
    let _ = rig.enumerate(true);
    assert_eq!(rig.descriptor(6, 0, 0, 10).len(), 10, "DEVICE_QUALIFIER");
    assert_eq!(
        rig.control(0x80, 0, 0, 0, 2, &[]),
        Ok(vec![1, 0]),
        "self-powered"
    );
    assert_eq!(
        rig.control(0x80, 8, 0, 0, 1, &[]),
        Ok(vec![1]),
        "GET_CONFIGURATION"
    );
    assert_eq!(
        rig.control(0xA1, 0x21, 0, 0, 7, &[]),
        Ok(vec![0x80, 0x25, 0, 0, 0, 0, 8]),
        "GET_LINE_CODING"
    );
    rig.assert_clean();
}

#[test]
fn unknown_request_stalls_and_the_next_is_answered() {
    let mut rig = Rig::new();
    let _ = rig.enumerate(true);
    assert_eq!(
        rig.control(0xC0, 0x42, 0, 0, 4, &[]),
        Err(HostError::Stall),
        "vendor IN"
    );
    assert_eq!(
        rig.control(0x40, 0x42, 0, 0, 0, &[]),
        Err(HostError::Stall),
        "vendor, no data"
    );
    assert!(rig.model.borrow().stalled(0), "Set Stall on physical 0");
    assert_eq!(
        rig.control(0x80, 0, 0, 0, 2, &[]),
        Ok(vec![1, 0]),
        "the next SETUP clears it"
    );
    assert!(!rig.model.borrow().stalled(0), "cleared");
    rig.assert_clean();
}

#[test]
fn a_refused_out_stage_stalls_the_status_stage() {
    let mut rig = Rig::new();
    let _ = rig.enumerate(true);
    assert_eq!(
        rig.control(0x21, 0x20, 0, 0, 7, &[1, 2, 3, 4, 5]),
        Err(HostError::Stall),
        "five bytes of seven"
    );
    assert_eq!(
        rig.control(0x21, 0x20, 0, 0, 6, &[0; 6]),
        Err(HostError::Stall),
        "wLength 6"
    );
    assert_eq!(
        rig.control(0x80, 8, 0, 0, 1, &[]),
        Ok(vec![1]),
        "still answering"
    );
    rig.assert_clean();
}

#[test]
fn set_address_is_in_dcfg_before_the_status_stage() {
    let mut rig = Rig::new();
    rig.attach(true);
    assert_eq!(
        rig.control(0x00, 5, 42, 0, 0, &[]),
        Ok(vec![]),
        "SET_ADDRESS 42"
    );
    // The model checks DCFG at the status stage and then talks to 42.
    let dcfg = Regs(rig.model.clone()).read32(DCFG);
    assert_eq!((dcfg >> 3) & 0x7F, 42, "DCFG.DEVADDR");
    assert_eq!(rig.descriptor(1, 0, 0, 18).len(), 18, "answers at 42");
    rig.assert_clean();
}

#[test]
fn a_reset_goes_back_to_address_0() {
    let mut rig = Rig::new();
    let _ = rig.enumerate(true);
    rig.bus_reset();
    assert!(rig.notice.reset, "reported");
    assert!(
        !rig.function.is_configured(),
        "the function is back to default"
    );
    assert!(!rig.controller.is_configured(), "and the controller");
    assert_eq!(rig.model.borrow().dcfg_address(), 0, "address 0");
    let _ = rig.enumerate(true);
    rig.assert_clean();
}

#[test]
fn a_reset_in_the_middle_of_a_data_stage() {
    let mut rig = Rig::new();
    rig.attach(true);
    let setup = Setup {
        request_type: 0x80,
        request: 6,
        value: 0x0200,
        index: 0,
        length: 255,
    };
    rig.model.borrow_mut().begin_control(setup, &[]);
    // The SETUP arrives and the driver starts the data stage; then the
    // host resets the bus instead of reading it.
    assert!(rig.model.borrow_mut().advance(), "the SETUP");
    let _ = rig
        .controller
        .on_interrupt(&mut rig.function)
        .expect("the SETUP handled");
    rig.bus_reset();
    let _ = rig.enumerate(true);
    rig.assert_clean();
}

/// The serial port, and a vendor request answered with exactly one
/// control packet of bytes, which needs a zero-length packet after it when
/// the host asked for more.
struct OnePacket(SerialPort, [u8; 64]);

impl Function for OnePacket {
    fn reset(&mut self) {
        self.0.reset();
    }

    fn set_speed(&mut self, speed: Speed) {
        self.0.set_speed(speed);
    }

    fn setup(&mut self, setup: &Setup) -> Reply<'_> {
        if setup.request_type == 0xC0 && setup.request == 1 {
            return Reply::In(&self.1);
        }
        self.0.setup(setup)
    }

    fn out_data(&mut self, data: &[u8]) -> Status {
        self.0.out_data(data)
    }

    fn endpoints(&self) -> &[EndpointInfo] {
        self.0.endpoints()
    }
}

#[test]
fn a_full_packet_short_of_what_was_asked_ends_with_a_zero_length_packet() {
    let mut rig = Rig::with(OnePacket(SerialPort::new("x"), [7; 64]));
    rig.attach(true);
    assert_eq!(
        rig.control(0xC0, 1, 0, 0, 255, &[]),
        Ok(vec![7; 64]),
        "64 of 255"
    );
    assert_eq!(
        rig.control(0xC0, 1, 0, 0, 64, &[]),
        Ok(vec![7; 64]),
        "64 of 64"
    );
    assert_eq!(
        rig.control(0xC0, 1, 0, 0, 10, &[]),
        Ok(vec![7; 10]),
        "cut to 10"
    );
    rig.assert_clean();
}
