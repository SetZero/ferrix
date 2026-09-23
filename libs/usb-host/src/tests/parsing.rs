//! The descriptors, the key table and the report translation on their own.

use ferrix_inputctl::message::{RawEvent, bit};
use ferrix_linux_abi::input::{BTN_EXTRA, BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_WHEEL, SYN_REPORT};

use crate::hid::{self, KEYBOARD_CODES, Kind, State};
use crate::hub::{PortStatus, get_port_status};
use crate::usb::{Configuration, DeviceDescriptor, Setup, Speed, first_language, string_ascii};

/// The key codes of `linux/input-event-codes.h` a keyboard's usages must
/// reach, by the HID usage tables' names.
#[test]
fn the_usage_table_gives_linuxs_codes() {
    let cases: [(usize, u8); 18] = [
        (0x04, 30),  // a: KEY_A
        (0x1D, 44),  // z: KEY_Z
        (0x1E, 2),   // 1: KEY_1
        (0x27, 11),  // 0: KEY_0
        (0x28, 28),  // Enter
        (0x29, 1),   // Escape
        (0x2A, 14),  // Backspace
        (0x2C, 57),  // Space
        (0x39, 58),  // Caps Lock
        (0x3A, 59),  // F1
        (0x45, 88),  // F12
        (0x4F, 106), // Right
        (0x52, 103), // Up
        (0x64, 86),  // Non-US backslash: KEY_102ND
        (0xE0, 29),  // Left Control
        (0xE1, 42),  // Left Shift
        (0xE6, 100), // Right Alt
        (0xE7, 126), // Right GUI: KEY_RIGHTMETA
    ];
    for (usage, code) in cases {
        assert_eq!(KEYBOARD_CODES[usage], code, "usage {usage:#04x}");
    }
    assert_eq!(KEYBOARD_CODES[0x00], 0, "no event");
    assert_eq!(KEYBOARD_CODES[0x01], 0, "ErrorRollOver");
}

#[test]
fn a_keyboard_declares_every_code_it_can_send_and_nothing_relative() {
    let bits = hid::bits(Kind::Keyboard);
    assert!(bits.has_type(EV_SYN) && bits.has_type(EV_KEY));
    assert!(!bits.has_type(EV_REL));
    for &code in KEYBOARD_CODES.iter().filter(|&&code| code != 0) {
        assert!(bit(&bits.keys, u16::from(code)), "code {code}");
    }
    assert!(!bit(&bits.keys, BTN_LEFT));
}

#[test]
fn a_mouse_declares_five_buttons_and_three_axes() {
    let bits = hid::bits(Kind::Mouse);
    assert!(bits.has_type(EV_REL));
    for code in BTN_LEFT..=BTN_EXTRA {
        assert!(bit(&bits.keys, code));
    }
    assert!(bit(&bits.rels, REL_WHEEL));
}

#[test]
fn a_short_or_empty_report_changes_nothing() {
    let mut keyboard = State::new(Kind::Keyboard);
    assert!(keyboard.report(&[0, 0, 4]).as_slice().is_empty());
    let mut mouse = State::new(Kind::Mouse);
    assert!(mouse.report(&[1, 2]).as_slice().is_empty());
    assert!(
        mouse.report(&[0, 0, 0]).as_slice().is_empty(),
        "nothing moved"
    );
}

#[test]
fn a_three_byte_mouse_report_has_no_wheel() {
    let mut mouse = State::new(Kind::Mouse);
    let events = mouse.report(&[0x10, 0, 0]);
    assert_eq!(
        events.as_slice(),
        [
            RawEvent::new(EV_KEY, BTN_EXTRA, 1),
            RawEvent::new(EV_SYN, SYN_REPORT, 0)
        ]
    );
}

#[test]
fn names_are_joined_as_linux_joins_them() {
    let joined = |m: &[u8], p: &[u8]| hid::name(m, p, b"USB mouse").as_bytes().to_vec();
    assert_eq!(
        joined(b"Logitech", b"G502 HERO Gaming Mouse"),
        b"Logitech G502 HERO Gaming Mouse"
    );
    assert_eq!(
        joined(b"Logitech", b"Logitech USB Receiver"),
        b"Logitech USB Receiver"
    );
    assert_eq!(joined(b"", b"Mouse"), b"Mouse");
    assert_eq!(joined(b"", b""), b"USB mouse");
}

/// The SETUP packet of `GET_DESCRIPTOR(DEVICE)`, byte for byte as USB 2.0's
/// table 9-2 lays it out.
#[test]
fn a_setup_packet_is_laid_out_little_endian() {
    assert_eq!(
        Setup::get_descriptor(1, 0, 0, 18).bytes(),
        [0x80, 6, 0, 1, 0, 0, 18, 0]
    );
    assert_eq!(get_port_status(3).bytes(), [0xA3, 0, 0, 0, 3, 0, 4, 0]);
    assert!(Setup::get_descriptor(1, 0, 0, 18).is_in());
    assert!(!Setup::set_address(5).is_in());
}

#[test]
fn a_device_descriptor_is_read() {
    let bytes = [
        18, 1, 0, 2, 0, 0, 0, 64, 0x6D, 0x04, 0x8B, 0xC0, 0x00, 0x74, 1, 2, 3, 1,
    ];
    let descriptor = DeviceDescriptor::parse(&bytes).unwrap();
    assert_eq!(
        (descriptor.vendor, descriptor.product, descriptor.release),
        (0x046D, 0xC08B, 0x7400)
    );
    assert_eq!(descriptor.max_packet, 64);
    assert_eq!((descriptor.manufacturer, descriptor.product_name), (1, 2));
    assert!(DeviceDescriptor::parse(&bytes[..17]).is_none());
}

/// A keyboard's configuration as such keyboards give it: a boot interface,
/// a HID descriptor between it and its endpoint, and a second interface
/// with an alternate setting that must not count.
#[test]
fn a_configuration_gives_its_interfaces_and_their_interrupt_endpoints() {
    let bytes = [
        9, 2, 75, 0, 2, 1, 0, 0xA0, 49, //
        9, 4, 0, 0, 1, 3, 1, 1, 0, //
        9, 0x21, 0x10, 0x01, 0, 1, 0x22, 65, 0, //
        7, 5, 0x81, 3, 8, 0, 10, //
        9, 4, 1, 0, 2, 3, 0, 0, 0, //
        9, 0x21, 0x10, 0x01, 0, 1, 0x22, 50, 0, //
        7, 5, 0x02, 3, 8, 0, 10, // OUT: not the one
        7, 5, 0x82, 3, 8, 0, 10, //
        9, 4, 1, 1, 1, 3, 0, 0, 0, // alternate setting 1: left out
        7, 5, 0x83, 3, 8, 0, 10,
    ];
    assert_eq!(Configuration::total_length(&bytes[..9]), Some(75));
    let config = Configuration::parse(&bytes).unwrap();
    assert_eq!(config.value, 1);
    let interfaces = config.interfaces();
    assert_eq!(interfaces.len(), 2);
    assert_eq!(
        (
            interfaces[0].class,
            interfaces[0].subclass,
            interfaces[0].protocol
        ),
        (3, 1, 1)
    );
    assert_eq!(interfaces[0].interrupt_in, Some((1, 8)));
    assert_eq!(interfaces[1].interrupt_in, Some((2, 8)));
}

#[test]
fn a_truncated_configuration_keeps_what_was_whole() {
    let bytes = [
        9, 2, 34, 0, 1, 1, 0, 0xA0, 49, 9, 4, 0, 0, 1, 3, 1, 2, 0, 7, 5, 0x81,
    ];
    let config = Configuration::parse(&bytes).unwrap();
    assert_eq!(config.interfaces().len(), 1);
    assert_eq!(config.interfaces()[0].interrupt_in, None);
}

#[test]
fn strings_become_ascii_without_trailing_spaces() {
    let descriptor = [14, 3, b'S', 0, b'E', 0, b'M', 0, 0xE9, 0, b' ', 0, b' ', 0];
    let mut out = [0_u8; 16];
    let len = string_ascii(&descriptor, &mut out);
    assert_eq!(&out[..len], b"SEM?");
    assert_eq!(first_language(&[4, 3, 0x09, 0x04]), Some(0x0409));
    assert_eq!(first_language(&[2, 3]), None);
}

#[test]
fn a_port_status_says_speed_and_changes() {
    let status = PortStatus::parse(&[0x03, 0x03, 0x11, 0x00]).unwrap();
    assert!(status.connected());
    assert_eq!(status.speed(), Speed::Low);
    assert_eq!(status.changes().collect::<std::vec::Vec<_>>(), [16, 20]);
    assert_eq!(
        PortStatus::parse(&[0x03, 0x05, 0, 0]).unwrap().speed(),
        Speed::High
    );
    assert_eq!(
        PortStatus::parse(&[0x03, 0x01, 0, 0]).unwrap().speed(),
        Speed::Full
    );
}
