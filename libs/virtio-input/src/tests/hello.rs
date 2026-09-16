//! HELLO from QEMU's devices, and from devices that stretch the description.
//!
//! The numbers on the device's side are written out by hand from QEMU 9.2.4's
//! `hw/input/virtio-input-hid.c`; the evdev names from `ferrix_linux_abi`
//! appear only on the other side of an assertion from them.

use core::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use ferrix_inputctl::message::{AxisRange, DeviceId, Hello, KEY_BYTES, bit};
use ferrix_inputctl::session::{Capabilities, Session};
use ferrix_linux_abi::input::{
    ABS_MT_POSITION_X, ABS_MT_SLOT, ABS_X, ABS_Y, BTN_LEFT, BUS_VIRTUAL, EV_ABS, EV_KEY, EV_LED,
    EV_REL, EV_REP, EV_SYN, INPUT_PROP_DIRECT, KEY_A, KEY_CNT, KEY_ESC, LED_CAPSL, REL_WHEEL,
    REL_X,
};
use ferrix_virtio::input::InputError;

use super::fake::{Answer, Bus, Device, Handle, keyboard, mouse, multitouch, qemu_abs, tablet};
use crate::read_hello;

fn hello_of(answers: Vec<Answer>) -> Result<(Hello, bool), InputError> {
    let bus = Bus::new();
    let device = Rc::new(RefCell::new(Device::new(bus, answers)));
    // The queries are asked after FEATURES_OK.
    device.borrow_mut().status = 0x0B;
    let mut handle = Handle {
        device: Rc::clone(&device),
        doorbells: Rc::new(RefCell::new(0)),
    };
    let result = read_hello(&mut handle);
    assert!(
        device.borrow().protocol_errors.is_empty(),
        "{:?}",
        device.borrow().protocol_errors
    );
    result
}

fn set_bits(bits: &[u8], count: u16) -> Vec<u16> {
    (0..count).filter(|&index| bit(bits, index)).collect()
}

#[test]
fn the_keyboard_is_described_as_qemu_declares_it() {
    let (hello, clipped) = hello_of(keyboard()).expect("described");
    assert!(!clipped);
    assert_eq!(hello.name.as_bytes(), b"QEMU Virtio Keyboard");
    assert_eq!(hello.name.len, 20, "the NUL QEMU counts is not the name's");
    assert_eq!(hello.serial.len, 0);
    assert_eq!(
        hello.id,
        DeviceId {
            bustype: 6,
            vendor: 0x0627,
            product: 1,
            version: 1
        }
    );
    assert_eq!(BUS_VIRTUAL, 6);
    // EV_KEY, EV_LED and EV_REP; QEMU answers nothing for EV_SYN.
    assert_eq!(set_bits(&hello.bits.types, 32), [0x01, 0x11, 0x14]);
    assert_eq!(set_bits(&hello.bits.types, 32), [EV_KEY, EV_LED, EV_REP]);
    assert_eq!(set_bits(&hello.bits.keys, KEY_CNT), [1, 30, 226]);
    assert_eq!(set_bits(&hello.bits.keys, 31), [KEY_ESC, KEY_A]);
    assert_eq!(set_bits(&hello.bits.leds, 16), [0, 1, 2]);
    assert!(bit(&hello.bits.leds, LED_CAPSL));
    assert!(hello.axes.iter().all(|range| *range == AxisRange::default()));

    hello
        .validate(&Hello::HANDLE_RIGHTS)
        .expect("the core accepts it");
    let caps = Capabilities::from_hello(&hello);
    assert!(caps.left_out.is_empty());
    assert!(caps.bits.has_type(EV_SYN), "the core publishes EV_SYN itself");
}

#[test]
fn the_mouse_and_tablet_are_described_with_their_axes() {
    let (mouse, _) = hello_of(mouse()).expect("described");
    assert_eq!(set_bits(&mouse.bits.types, 32), [EV_KEY, EV_REL]);
    assert_eq!(set_bits(&mouse.bits.rels, 16), [0, 1, 8]);
    assert!(bit(&mouse.bits.rels, REL_X) && bit(&mouse.bits.rels, REL_WHEEL));
    assert_eq!(
        set_bits(&mouse.bits.keys, KEY_CNT),
        [0x110, 0x111, 0x112, 0x113, 0x114, 0x14a, 0x150, 0x151]
    );
    assert_eq!((mouse.id.product, mouse.id.version), (2, 2));
    mouse.validate(&Hello::HANDLE_RIGHTS).expect("accepted");

    let (tablet, _) = hello_of(tablet()).expect("described");
    assert_eq!(set_bits(&tablet.bits.types, 32), [EV_KEY, EV_REL, EV_ABS]);
    assert_eq!(set_bits(&tablet.bits.abs, 64), [ABS_X, ABS_Y]);
    for axis in [0, 1] {
        assert_eq!(
            tablet.axes[axis],
            AxisRange {
                minimum: 0,
                maximum: 0x7fff,
                ..AxisRange::default()
            }
        );
    }
    assert!(tablet.axes[2..].iter().all(|range| *range == AxisRange::default()));
    assert!(bit(&tablet.bits.keys, BTN_LEFT));
    tablet.validate(&Hello::HANDLE_RIGHTS).expect("accepted");
    let _ = Session::accept(&tablet, &Hello::HANDLE_RIGHTS).expect("a session");
}

#[test]
fn multitouch_is_declared_and_the_core_leaves_it_out() {
    let (hello, _) = hello_of(multitouch()).expect("described");
    assert!(bit(&hello.bits.props, 1));
    assert!(bit(&hello.bits.props, INPUT_PROP_DIRECT));
    assert_eq!(set_bits(&hello.bits.abs, 64), [0x2f, 0x35, 0x36, 0x39]);
    assert_eq!(hello.axes[usize::from(ABS_MT_SLOT)].maximum, 10);
    assert_eq!(hello.axes[usize::from(ABS_MT_POSITION_X)].maximum, 0x7fff);
    hello.validate(&Hello::HANDLE_RIGHTS).expect("accepted");

    let caps = Capabilities::from_hello(&hello);
    assert!(caps.left_out.mt_axes, "the boot line can say so");
    assert!(caps.bits.abs.iter().all(|&byte| byte == 0));
}

#[test]
fn bits_past_a_kinds_max_are_left_out_and_said_to_be() {
    // A key bitmap of the whole union, every bit set: 1024 bits, KEY_CNT 768.
    let (hello, clipped) = hello_of(vec![(0x11, 0x01, vec![0xff; 128])]).expect("described");
    assert!(clipped);
    assert_eq!(hello.bits.keys, [0xff; KEY_BYTES]);
    assert_eq!(KEY_BYTES * 8, usize::from(KEY_CNT));
    hello.validate(&Hello::HANDLE_RIGHTS).expect("accepted");

    // Types past EV_MAX are never asked; a type with no code bitmap is only
    // a type bit. EV_FF (0x15) and EV_SND (0x12) here.
    let (hello, clipped) =
        hello_of(vec![(0x11, 0x15, vec![0xff; 16]), (0x11, 0x12, vec![0x0f])]).expect("described");
    assert!(!clipped);
    assert_eq!(set_bits(&hello.bits.types, 32), [0x12, 0x15]);
    hello.validate(&Hello::HANDLE_RIGHTS).expect("accepted");
    let caps = Capabilities::from_hello(&hello);
    assert_eq!(set_bits(&caps.left_out.types, 32), [0x12, 0x15]);
}

#[test]
fn a_device_without_names_or_ids_is_described_without_them() {
    let (hello, _) = hello_of(vec![(0x11, 0x01, vec![0, 0, 0, 0b0100_0000])]).expect("described");
    assert_eq!((hello.name.len, hello.serial.len), (0, 0));
    assert_eq!(hello.id, DeviceId::default());
    assert_eq!(set_bits(&hello.bits.keys, KEY_CNT), [KEY_A]);
    hello.validate(&Hello::HANDLE_RIGHTS).expect("accepted");

    // A serial as QEMU's serial= property gives it, without a NUL.
    let mut answers = keyboard();
    answers.push((0x02, 0, b"kbd0".to_vec()));
    let (hello, _) = hello_of(answers).expect("described");
    assert_eq!(hello.serial.as_bytes(), b"kbd0");
}

#[test]
fn a_declared_axis_without_a_usable_range_fails_the_description() {
    // Declares ABS_X and ABS_Y, describes only ABS_X.
    let answers = vec![(0x11, 0x03, vec![0b11]), qemu_abs(0x00, 0, 100)];
    assert_eq!(
        hello_of(answers).map(|_| ()),
        Err(InputError::Absent {
            select: 0x12,
            subsel: 1
        })
    );
    let answers = vec![(0x11, 0x03, vec![0b1]), qemu_abs(0x00, 5, 4)];
    assert_eq!(
        hello_of(answers).map(|_| ()),
        Err(InputError::AbsRange {
            axis: 0,
            min: 5,
            max: 4
        })
    );
    // A name whose size runs past the union.
    let mut answers = keyboard();
    answers[0] = (0x01, 0, vec![b'x'; 129]);
    assert!(matches!(
        hello_of(answers),
        Err(InputError::SizeTooLarge { .. })
    ));
}
