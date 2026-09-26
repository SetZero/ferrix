//! Every standard and class request, answered or stalled.

use std::vec;
use std::vec::Vec;

use crate::acm::{
    DATA_IN, DATA_OUT, FULL_SPEED_ENDPOINTS, HIGH_SPEED_CONFIGURATION, HIGH_SPEED_ENDPOINTS,
    LineCoding, NOTIFY_IN, SerialPort, serial_state,
};
use crate::standard::State;
use crate::{Effect, Function, Reply, Setup, Speed, Status};

/// A reply with its bytes owned, to compare.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Got {
    In(Vec<u8>),
    Out(usize),
    Ack(Effect),
    Stall,
}

fn ask(
    port: &mut SerialPort,
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
) -> Got {
    let setup = Setup {
        request_type,
        request,
        value,
        index,
        length,
    };
    match port.setup(&setup) {
        Reply::In(bytes) => Got::In(bytes.to_vec()),
        Reply::Out(length) => Got::Out(length),
        Reply::Ack(effect) => Got::Ack(effect),
        Reply::Stall => Got::Stall,
    }
}

fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0, 3];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes[0] = bytes.len() as u8;
    bytes
}

fn addressed() -> SerialPort {
    let mut port = SerialPort::new("P7-0001");
    port.reset();
    port.set_speed(Speed::High);
    assert_eq!(
        ask(&mut port, 0x00, 5, 7, 0, 0),
        Got::Ack(Effect::Address(7)),
        "SET_ADDRESS"
    );
    port
}

fn configured() -> SerialPort {
    let mut port = addressed();
    assert_eq!(
        ask(&mut port, 0x00, 9, 1, 0, 0),
        Got::Ack(Effect::Configure(1)),
        "SET_CONFIGURATION"
    );
    port
}

#[test]
fn setup_packet_round_trips() {
    let bytes = [0x80, 6, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00];
    let setup = Setup::parse(bytes);
    assert_eq!(setup.request_type, 0x80, "bmRequestType");
    assert_eq!(setup.request, 6, "bRequest");
    assert_eq!(setup.value, 0x0100, "wValue");
    assert_eq!(setup.length, 64, "wLength");
    assert_eq!(setup.descriptor_type(), 1, "device descriptor");
    assert_eq!(setup.bytes(), bytes, "back to bytes");
}

#[test]
fn get_descriptor_device_cut_to_the_length_asked() {
    let mut port = SerialPort::new("x");
    port.reset();
    port.set_speed(Speed::High);
    // Linux's first read at address 0: 64 bytes asked, 18 given.
    let Got::In(bytes) = ask(&mut port, 0x80, 6, 0x0100, 0, 64) else {
        panic!("the device descriptor");
    };
    assert_eq!(bytes, crate::acm::DEVICE, "the whole descriptor");
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0100, 0, 8),
        Got::In(crate::acm::DEVICE[..8].to_vec()),
        "cut"
    );
}

#[test]
fn get_descriptor_configuration_short_then_whole() {
    let mut port = addressed();
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0200, 0, 9),
        Got::In(HIGH_SPEED_CONFIGURATION[..9].to_vec()),
        "the header first"
    );
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0200, 0, 255),
        Got::In(HIGH_SPEED_CONFIGURATION.to_vec()),
        "then the whole"
    );
}

#[test]
fn full_speed_gives_the_full_speed_configuration() {
    let mut port = SerialPort::new("x");
    port.reset();
    port.set_speed(Speed::Full);
    let Got::In(bytes) = ask(&mut port, 0x80, 6, 0x0200, 0, 255) else {
        panic!("a configuration");
    };
    // Bulk OUT is the tenth descriptor, at byte 61; its wMaxPacketSize at 65.
    assert_eq!(bytes[61 + 2], DATA_OUT, "bulk OUT");
    assert_eq!(bytes[65..67], [64, 0], "64 bytes");
    let Got::In(other) = ask(&mut port, 0x80, 6, 0x0700, 0, 255) else {
        panic!("the other speed's");
    };
    assert_eq!(other[1], 7, "typed OTHER_SPEED_CONFIGURATION");
    assert_eq!(
        other[2..],
        HIGH_SPEED_CONFIGURATION[2..],
        "the high-speed one"
    );
}

#[test]
fn other_speed_at_high_speed_is_full_speed() {
    let mut port = addressed();
    let Got::In(other) = ask(&mut port, 0x80, 6, 0x0700, 0, 255) else {
        panic!("the other speed's");
    };
    assert_eq!(other[1], 7, "typed OTHER_SPEED_CONFIGURATION");
    assert_eq!(
        other[2..],
        crate::acm::FULL_SPEED_CONFIGURATION[2..],
        "the full-speed one"
    );
}

#[test]
fn device_qualifier() {
    let mut port = addressed();
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0600, 0, 10),
        Got::In(crate::acm::QUALIFIER.to_vec()),
        "DEVICE_QUALIFIER"
    );
}

#[test]
fn strings() {
    let mut port = addressed();
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0300, 0, 255),
        Got::In(vec![4, 3, 0x09, 0x04]),
        "languages"
    );
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0301, 0x0409, 255),
        Got::In(utf16("Ferrix")),
        "manufacturer"
    );
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0302, 0x0409, 255),
        Got::In(utf16("Ferrix console")),
        "product"
    );
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0303, 0x0409, 255),
        Got::In(utf16("P7-0001")),
        "serial"
    );
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0302, 0x0409, 2),
        Got::In(vec![30, 3]),
        "just the length"
    );
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0304, 0x0409, 255),
        Got::Stall,
        "no string 4"
    );
}

#[test]
fn serial_is_kept_printable_and_bounded() {
    let port = SerialPort::new("a b\u{e9}0123456789012345678901234567890123456789");
    assert_eq!(
        port.serial(),
        "a_b_0123456789012345678901234567",
        "32 characters, spaces made _"
    );
}

#[test]
fn unknown_descriptors_and_requests_stall() {
    let mut port = configured();
    assert_eq!(ask(&mut port, 0x80, 6, 0x0F00, 0, 5), Got::Stall, "BOS");
    assert_eq!(
        ask(&mut port, 0x80, 6, 0x0201, 0, 9),
        Got::Stall,
        "configuration 1 of 1"
    );
    assert_eq!(
        ask(&mut port, 0x00, 7, 0x0100, 0, 18),
        Got::Stall,
        "SET_DESCRIPTOR"
    );
    assert_eq!(
        ask(&mut port, 0x82, 12, 0, 0x82, 2),
        Got::Stall,
        "SYNCH_FRAME"
    );
    assert_eq!(
        ask(&mut port, 0xC0, 0x42, 0, 0, 4),
        Got::Stall,
        "a vendor request"
    );
    assert_eq!(
        ask(&mut port, 0x00, 3, 1, 0, 0),
        Got::Stall,
        "remote wakeup"
    );
    assert_eq!(
        ask(&mut port, 0x00, 3, 2, 0x0400, 0),
        Got::Stall,
        "test mode"
    );
}

#[test]
fn set_address_rules() {
    let mut port = SerialPort::new("x");
    port.reset();
    assert_eq!(
        port.standard().state(),
        State::Default,
        "default after reset"
    );
    assert_eq!(ask(&mut port, 0x00, 5, 128, 0, 0), Got::Stall, "above 127");
    assert_eq!(
        ask(&mut port, 0x00, 5, 3, 0, 0),
        Got::Ack(Effect::Address(3)),
        "an address"
    );
    assert_eq!(port.standard().state(), State::Address, "addressed");
    assert_eq!(
        ask(&mut port, 0x00, 5, 0, 0, 0),
        Got::Ack(Effect::Address(0)),
        "back to 0"
    );
    assert_eq!(port.standard().state(), State::Default, "default again");
    let mut port = configured();
    assert_eq!(
        ask(&mut port, 0x00, 5, 4, 0, 0),
        Got::Stall,
        "not while configured"
    );
}

#[test]
fn configuration_requests() {
    let mut port = SerialPort::new("x");
    port.reset();
    assert_eq!(
        ask(&mut port, 0x00, 9, 1, 0, 0),
        Got::Stall,
        "not in the default state"
    );
    let mut port = addressed();
    assert!(port.endpoints().is_empty(), "no endpoints before");
    assert_eq!(
        ask(&mut port, 0x80, 8, 0, 0, 1),
        Got::In(vec![0]),
        "GET_CONFIGURATION 0"
    );
    assert_eq!(
        ask(&mut port, 0x00, 9, 2, 0, 0),
        Got::Stall,
        "no configuration 2"
    );
    assert_eq!(
        ask(&mut port, 0x00, 9, 1, 0, 0),
        Got::Ack(Effect::Configure(1)),
        "configure"
    );
    assert!(port.is_configured(), "configured");
    assert_eq!(
        port.endpoints(),
        HIGH_SPEED_ENDPOINTS,
        "high-speed endpoints"
    );
    assert_eq!(
        ask(&mut port, 0x80, 8, 0, 0, 1),
        Got::In(vec![1]),
        "GET_CONFIGURATION 1"
    );
    assert_eq!(
        ask(&mut port, 0x00, 9, 0, 0, 0),
        Got::Ack(Effect::Configure(0)),
        "deconfigure"
    );
    assert_eq!(port.standard().state(), State::Address, "addressed again");
    assert!(port.endpoints().is_empty(), "no endpoints after");
}

#[test]
fn full_speed_endpoints() {
    let mut port = SerialPort::new("x");
    port.reset();
    port.set_speed(Speed::Full);
    let _ = ask(&mut port, 0x00, 5, 2, 0, 0);
    let _ = ask(&mut port, 0x00, 9, 1, 0, 0);
    assert_eq!(port.endpoints(), FULL_SPEED_ENDPOINTS, "64-byte bulk");
}

#[test]
fn get_status() {
    let mut port = configured();
    assert_eq!(
        ask(&mut port, 0x80, 0, 0, 0, 2),
        Got::In(vec![1, 0]),
        "device: self-powered"
    );
    assert_eq!(
        ask(&mut port, 0x81, 0, 0, 1, 2),
        Got::In(vec![0, 0]),
        "interface 1"
    );
    assert_eq!(
        ask(&mut port, 0x81, 0, 0, 2, 2),
        Got::In(vec![0, 0]),
        "interface 2, adb's"
    );
    assert_eq!(
        ask(&mut port, 0x81, 0, 0, 3, 2),
        Got::Stall,
        "no interface 3"
    );
    assert_eq!(
        ask(&mut port, 0x82, 0, 0, 0x00, 2),
        Got::In(vec![0, 0]),
        "endpoint 0"
    );
    assert_eq!(
        ask(&mut port, 0x82, 0, 0, u16::from(DATA_IN), 2),
        Got::In(vec![0, 0]),
        "bulk IN"
    );
    assert_eq!(
        ask(&mut port, 0x82, 0, 0, 0x83, 2),
        Got::In(vec![0, 0]),
        "adb's bulk IN"
    );
    assert_eq!(
        ask(&mut port, 0x82, 0, 0, 0x84, 2),
        Got::Stall,
        "no endpoint 0x84"
    );
    let mut port = addressed();
    assert_eq!(
        ask(&mut port, 0x82, 0, 0, u16::from(DATA_IN), 2),
        Got::Stall,
        "not configured"
    );
}

#[test]
fn endpoint_halt() {
    let mut port = configured();
    let out = u16::from(DATA_OUT);
    assert_eq!(
        ask(&mut port, 0x02, 3, 0, out, 0),
        Got::Ack(Effect::Halt {
            endpoint: DATA_OUT,
            halted: true
        }),
        "SET_FEATURE halt"
    );
    assert!(port.standard().is_halted(DATA_OUT), "halted");
    assert!(!port.standard().is_halted(DATA_IN), "only that one");
    assert_eq!(
        ask(&mut port, 0x82, 0, 0, out, 2),
        Got::In(vec![1, 0]),
        "GET_STATUS says halted"
    );
    assert_eq!(
        ask(&mut port, 0x02, 1, 0, out, 0),
        Got::Ack(Effect::Halt {
            endpoint: DATA_OUT,
            halted: false
        }),
        "CLEAR_FEATURE halt"
    );
    assert_eq!(
        ask(&mut port, 0x82, 0, 0, out, 2),
        Got::In(vec![0, 0]),
        "cleared"
    );
    assert_eq!(
        ask(&mut port, 0x02, 1, 0, 0, 0),
        Got::Ack(Effect::None),
        "clear ep0"
    );
    assert_eq!(ask(&mut port, 0x02, 3, 0, 0x80, 0), Got::Stall, "halt ep0");
    assert_eq!(
        ask(&mut port, 0x02, 3, 0, 0x05, 0),
        Got::Stall,
        "halt a stranger"
    );
    let _ = ask(&mut port, 0x02, 3, 0, u16::from(NOTIFY_IN), 0);
    assert_eq!(
        ask(&mut port, 0x00, 9, 1, 0, 0),
        Got::Ack(Effect::Configure(1)),
        "reconfigure"
    );
    assert!(
        !port.standard().is_halted(NOTIFY_IN),
        "SET_CONFIGURATION clears halts"
    );
}

#[test]
fn interfaces() {
    let mut port = configured();
    assert_eq!(
        ask(&mut port, 0x81, 10, 0, 0, 1),
        Got::In(vec![0]),
        "GET_INTERFACE 0"
    );
    assert_eq!(
        ask(&mut port, 0x81, 10, 0, 1, 1),
        Got::In(vec![0]),
        "GET_INTERFACE 1"
    );
    assert_eq!(
        ask(&mut port, 0x81, 10, 0, 2, 1),
        Got::In(vec![0]),
        "GET_INTERFACE 2, adb's"
    );
    assert_eq!(
        ask(&mut port, 0x81, 10, 0, 3, 1),
        Got::Stall,
        "no interface 3"
    );
    assert_eq!(
        ask(&mut port, 0x01, 11, 0, 1, 0),
        Got::Ack(Effect::None),
        "SET_INTERFACE alt 0"
    );
    assert_eq!(ask(&mut port, 0x01, 11, 1, 1, 0), Got::Stall, "no alt 1");
    let mut port = addressed();
    assert_eq!(
        ask(&mut port, 0x81, 10, 0, 0, 1),
        Got::Stall,
        "not configured"
    );
}

#[test]
fn line_coding() {
    let mut port = configured();
    assert_eq!(
        ask(&mut port, 0xA1, 0x21, 0, 0, 7),
        Got::In(LineCoding::DEFAULT.bytes().to_vec()),
        "GET_LINE_CODING before any set"
    );
    assert_eq!(
        ask(&mut port, 0x21, 0x20, 0, 0, 7),
        Got::Out(7),
        "SET_LINE_CODING wants 7"
    );
    let coding = [0x80, 0x25, 0, 0, 2, 2, 7];
    assert_eq!(port.out_data(&coding), Status::Ack, "taken");
    assert_eq!(
        port.line_coding(),
        LineCoding {
            rate: 9600,
            stop_bits: 2,
            parity: 2,
            data_bits: 7
        },
        "9600 7E2"
    );
    assert_eq!(
        ask(&mut port, 0xA1, 0x21, 0, 0, 7),
        Got::In(coding.to_vec()),
        "reads back"
    );
    assert_eq!(ask(&mut port, 0x21, 0x20, 0, 0, 7), Got::Out(7), "again");
    assert_eq!(port.out_data(&coding[..5]), Status::Stall, "too short");
    assert_eq!(port.out_data(&coding), Status::Stall, "nothing pending");
    assert_eq!(
        ask(&mut port, 0x21, 0x20, 0, 0, 6),
        Got::Stall,
        "wLength must be 7"
    );
    assert_eq!(
        ask(&mut port, 0x21, 0x20, 0, 1, 7),
        Got::Stall,
        "to the data interface"
    );
}

#[test]
fn control_line_state() {
    let mut port = configured();
    assert!(!port.dtr(), "closed at first");
    assert_eq!(
        ask(&mut port, 0x21, 0x22, 3, 0, 0),
        Got::Ack(Effect::None),
        "DTR and RTS"
    );
    assert!(port.dtr() && port.rts(), "open");
    assert_eq!(
        ask(&mut port, 0x21, 0x22, 2, 0, 0),
        Got::Ack(Effect::None),
        "RTS alone"
    );
    assert!(!port.dtr(), "closed");
    let _ = ask(&mut port, 0x21, 0x22, 1, 0, 0);
    port.reset();
    assert!(!port.dtr(), "a bus reset closes it");
    let mut port = configured();
    let _ = ask(&mut port, 0x21, 0x22, 1, 0, 0);
    let _ = ask(&mut port, 0x00, 9, 0, 0, 0);
    assert!(!port.dtr(), "so does deconfiguring");
}

#[test]
fn class_requests_need_a_configuration() {
    let mut port = addressed();
    assert_eq!(
        ask(&mut port, 0x21, 0x22, 1, 0, 0),
        Got::Stall,
        "SET_CONTROL_LINE_STATE"
    );
    assert_eq!(
        ask(&mut port, 0xA1, 0x21, 0, 0, 7),
        Got::Stall,
        "GET_LINE_CODING"
    );
    let mut port = configured();
    assert_eq!(
        ask(&mut port, 0x21, 0x23, 100, 0, 0),
        Got::Stall,
        "SEND_BREAK: not offered"
    );
}

#[test]
fn serial_state_notification() {
    assert_eq!(
        serial_state(true, true),
        [0xA1, 0x20, 0, 0, 0, 0, 2, 0, 3, 0],
        "SERIAL_STATE, DCD and DSR"
    );
}
