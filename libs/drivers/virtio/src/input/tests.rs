//! Tests for virtio-input's device protocol.
//!
//! Every number, length and offset below is written out by hand from Linux
//! 6.8's `include/uapi/linux/virtio_input.h` and `input-event-codes.h`, and
//! QEMU 9.2.4's `hw/input/virtio-input.c` and `virtio-input-hid.c`, not
//! taken from this module's constants, since a constant tested against itself
//! proves nothing. The device's side is played by [`Device`], a configuration
//! block that answers a query the way virtio 1.2 §5.8.4 says, from a table,
//! and records every access; some of its answers are wrong on purpose.
//!
//! The evdev names from `ferrix_linux_abi::input` appear only on the other
//! side of an assertion from those hand-written numbers: where QEMU's devices
//! set bit 30 for `KEY_A`, the test requires that bit to be the probe's
//! `KEY_A`, so a code QEMU sends and the probe's number for it cannot part.

extern crate std;

use std::vec;
use std::vec::Vec;

use ferrix_linux_abi::input::{
    ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, BTN_EXTRA,
    BTN_GEAR_DOWN, BTN_GEAR_UP, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, BTN_TOUCH,
    INPUT_PROP_DIRECT, KEY_A, KEY_ESC, LED_CAPSL, LED_NUML, LED_SCROLLL, REL_WHEEL, REL_X, REL_Y,
};

use super::*;

/// One configuration access.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    Write(u32, u8),
    Read(u32),
}

/// A virtio-input configuration block over a table of answers.
struct Device {
    block: Vec<u8>,
    /// `(select, subsel, size, payload)`; `size` is what the device claims,
    /// whatever the payload's length.
    answers: Vec<(u8, u8, u8, Vec<u8>)>,
    log: std::cell::RefCell<Vec<Access>>,
}

impl Device {
    fn new(answers: Vec<(u8, u8, u8, Vec<u8>)>) -> Self {
        Self::sized(136, answers)
    }

    /// A block of `len` bytes, as QEMU sizes it: its longest answer plus the
    /// 8-byte header.
    fn sized(len: usize, answers: Vec<(u8, u8, u8, Vec<u8>)>) -> Self {
        Self {
            block: vec![0; len],
            answers,
            log: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// Put the answer to the `select` and `subsel` now in the block into it.
    fn answer(&mut self) {
        let (select, subsel) = (self.block[0], self.block[1]);
        self.block[2] = 0;
        self.block[8..].fill(0);
        if let Some((_, _, size, payload)) = self
            .answers
            .iter()
            .find(|(s, sub, _, _)| (*s, *sub) == (select, subsel))
        {
            self.block[2] = *size;
            let len = payload.len().min(128).min(self.block.len() - 8);
            self.block[8..8 + len].copy_from_slice(&payload[..len]);
        }
    }
}

impl DeviceConfig for Device {
    fn config_len(&self) -> u32 {
        u32::try_from(self.block.len()).expect("small")
    }

    fn config_read8(&self, offset: u32) -> u8 {
        self.log.borrow_mut().push(Access::Read(offset));
        // Past the block, QEMU's virtio_config_modern_readb reads all ones.
        self.block.get(offset as usize).copied().unwrap_or(0xff)
    }

    fn config_read16(&self, offset: u32) -> u16 {
        u16::from_le_bytes([self.config_read8(offset), self.config_read8(offset + 1)])
    }

    fn config_read32(&self, offset: u32) -> u32 {
        u32::from_le_bytes([
            self.config_read8(offset),
            self.config_read8(offset + 1),
            self.config_read8(offset + 2),
            self.config_read8(offset + 3),
        ])
    }
}

impl ConfigSelect for Device {
    fn config_write8(&mut self, offset: u32, value: u8) {
        self.log.borrow_mut().push(Access::Write(offset, value));
        self.block[offset as usize] = value;
        self.answer();
    }
}

fn answer(select: u8, subsel: u8, payload: &[u8]) -> (u8, u8, u8, Vec<u8>) {
    let size = u8::try_from(payload.len()).expect("at most 128");
    (select, subsel, size, payload.to_vec())
}

// -- The device and its configuration block ----------------------------------------

#[test]
fn the_numbers_are_virtio_and_evdevs() {
    assert_eq!((DEVICE_ID, PCI_DEVICE_ID), (18, 0x1052));
    assert_eq!((EVENT_QUEUE, STATUS_QUEUE), (0, 1));
    assert_eq!(DRIVER_FEATURES, (1 << 32) | (1 << 33));
    assert_eq!(REQUIRED_FEATURES, 1 << 32);
    assert_eq!(
        [
            CFG_UNSET,
            CFG_ID_NAME,
            CFG_ID_SERIAL,
            CFG_ID_DEVIDS,
            CFG_PROP_BITS,
            CFG_EV_BITS,
            CFG_ABS_INFO
        ],
        [0x00, 0x01, 0x02, 0x03, 0x10, 0x11, 0x12]
    );
    assert_eq!(
        [
            EV_SYN, EV_KEY, EV_REL, EV_ABS, EV_MSC, EV_LED, EV_SND, EV_REP, EV_MAX
        ],
        [0, 1, 2, 3, 4, 0x11, 0x12, 0x14, 0x1f]
    );
    assert_eq!(SYN_REPORT, 0);
}

#[test]
fn config_offsets_are_the_structs() {
    // struct virtio_input_config: select, subsel, size, reserved[5], then the
    // 128-byte union.
    assert_eq!(
        (CONFIG_SELECT, CONFIG_SUBSEL, CONFIG_SIZE, CONFIG_UNION),
        (0, 1, 2, 8)
    );
    assert_eq!((ANSWER_MAX, CONFIG_LEN), (128, 136));
    // Five __le32 and four __le16.
    assert_eq!((ABS_INFO_LEN, DEVIDS_LEN), (20, 8));
}

#[test]
fn a_query_writes_select_then_subsel_then_reads_size_and_the_answer() {
    let mut dev = Device::new(vec![answer(0x11, 0x01, &[0xAA, 0xBB, 0xCC])]);
    let got = query(&mut dev, 0x11, 0x01).expect("it answers");
    assert_eq!(got.as_bytes(), [0xAA, 0xBB, 0xCC]);
    assert_eq!(got.len(), 3);
    assert_eq!(
        dev.log.borrow()[..],
        [
            Access::Write(0, 0x11),
            Access::Write(1, 0x01),
            Access::Read(2),
            Access::Read(8),
            Access::Read(9),
            Access::Read(10),
        ]
    );
}

#[test]
fn nothing_is_asked_of_a_block_without_its_header() {
    let mut dev = Device::sized(7, vec![]);
    assert_eq!(query(&mut dev, 0x01, 0), Err(InputError::ConfigTooShort(7)));
    assert!(dev.log.borrow().is_empty());
    // The header alone is a block: its only answer is the empty one.
    let mut dev = Device::sized(8, vec![]);
    assert_eq!(query(&mut dev, 0x01, 0), Ok(Answer::EMPTY));
}

#[test]
fn an_answer_past_the_block_is_refused_not_read() {
    // A 37-byte block, as QEMU's keyboard has, holds 29 bytes of answer.
    let mut dev = Device::sized(
        37,
        vec![
            answer(0x11, 0x01, &[0xFF; 29]),
            (0x01, 0, 30, vec![b'x'; 30]),
        ],
    );
    assert_eq!(query(&mut dev, 0x11, 0x01).expect("fits").len(), 29);
    dev.log.borrow_mut().clear();
    assert_eq!(
        name(&mut dev),
        Err(InputError::AnswerPastConfig {
            select: 0x01,
            subsel: 0,
            size: 30,
            len: 37
        })
    );
    // Asked and sized, but nothing of the union read.
    assert_eq!(
        dev.log.borrow()[..],
        [Access::Write(0, 0x01), Access::Write(1, 0), Access::Read(2)]
    );
}

// -- Answers ----------------------------------------------------------------------

#[test]
fn a_size_past_the_union_is_refused_not_cut() {
    for size in [129u8, 200, 255] {
        let mut dev = Device::new(vec![(0x01, 0, size, vec![b'x'; 128])]);
        assert_eq!(
            query(&mut dev, 0x01, 0),
            Err(InputError::SizeTooLarge {
                select: 0x01,
                subsel: 0,
                size
            })
        );
        assert_eq!(
            name(&mut dev),
            Err(InputError::SizeTooLarge {
                select: 0x01,
                subsel: 0,
                size
            })
        );
    }
    // Exactly 128 is the whole union.
    let mut dev = Device::new(vec![answer(0x01, 0, &[b'x'; 128])]);
    assert_eq!(name(&mut dev).expect("full").as_str_bytes(), [b'x'; 128]);
}

#[test]
fn names_need_no_nul_and_stop_at_one() {
    // A name of `size` bytes with no NUL after it.
    let mut dev = Device::new(vec![
        answer(0x01, 0, b"QEMU Virtio Keyboard"),
        answer(0x02, 0, b"ab\0cd"),
    ]);
    let got = name(&mut dev).expect("named");
    assert_eq!(got.as_str_bytes(), b"QEMU Virtio Keyboard");
    assert_eq!(got.len(), 20);
    let got = serial(&mut dev).expect("serial");
    assert_eq!(got.as_bytes(), b"ab\0cd");
    assert_eq!(got.as_str_bytes(), b"ab");
}

#[test]
fn a_structure_the_device_does_not_have_is_absent() {
    let mut dev = Device::new(vec![]);
    assert_eq!(
        name(&mut dev),
        Err(InputError::Absent {
            select: 0x01,
            subsel: 0
        })
    );
    assert_eq!(
        serial(&mut dev),
        Err(InputError::Absent {
            select: 0x02,
            subsel: 0
        })
    );
    assert_eq!(
        devids(&mut dev),
        Err(InputError::Absent {
            select: 0x03,
            subsel: 0
        })
    );
    assert_eq!(
        abs_info(&mut dev, 0),
        Err(InputError::Absent {
            select: 0x12,
            subsel: 0
        })
    );
    // Bitmaps are empty instead, which is an answer.
    assert_eq!(ev_bits(&mut dev, 1), Ok(Answer::EMPTY));
    assert_eq!(prop_bits(&mut dev), Ok(Answer::EMPTY));

    // A size of 0, whatever lies in the union behind it.
    let mut dev = Device::new(vec![(0x12, 0x00, 0, vec![1; 20])]);
    assert_eq!(
        abs_info(&mut dev, 0),
        Err(InputError::Absent {
            select: 0x12,
            subsel: 0
        })
    );
}

#[test]
fn bitmap_bit_n_is_byte_n_over_8_bit_n_mod_8() {
    // EV_KEY codes KEY_ESC (1), KEY_A (30) and BTN_LEFT (0x110).
    let mut bitmap = [0u8; 35];
    bitmap[0] = 0b0000_0010;
    bitmap[3] = 0b0100_0000;
    bitmap[34] = 0b0000_0001;
    let mut dev = Device::new(vec![answer(0x11, 1, &bitmap)]);
    let bits = ev_bits(&mut dev, 1).expect("keys");
    assert_eq!(
        dev.log.borrow()[..2],
        [Access::Write(0, 0x11), Access::Write(1, 1)]
    );
    let set: Vec<u16> = (0..1024).filter(|&bit| bits.bit(bit)).collect();
    assert_eq!(set, [1, 30, 0x110]);
    assert_eq!(set, [KEY_ESC, KEY_A, BTN_LEFT]);
    // Past the answer, and past the union, is clear.
    assert!(!bits.bit(35 * 8));
    assert!(!bits.bit(u16::MAX));
}

#[test]
fn a_type_or_axis_past_a_byte_is_not_asked() {
    let mut dev = Device::new(vec![]);
    assert_eq!(ev_bits(&mut dev, 0x100), Err(InputError::Subsel(0x100)));
    assert_eq!(abs_info(&mut dev, 0x100), Err(InputError::Subsel(0x100)));
    assert!(dev.log.borrow().is_empty());
}

#[test]
fn absinfo_is_min_max_fuzz_flat_res() {
    let mut payload = Vec::new();
    for field in [-5i32, 0x7fff, 2, 3, 4] {
        payload.extend_from_slice(&field.to_le_bytes());
    }
    let mut dev = Device::new(vec![answer(0x12, 0x01, &payload)]);
    assert_eq!(
        abs_info(&mut dev, 1),
        Ok(AbsInfo {
            min: -5,
            max: 0x7fff,
            fuzz: 2,
            flat: 3,
            res: 4
        })
    );

    // Too short for the structure, even with the bytes behind it.
    let mut dev = Device::new(vec![(0x12, 0x01, 19, payload.clone())]);
    assert_eq!(
        abs_info(&mut dev, 1),
        Err(InputError::AnswerTooShort {
            select: 0x12,
            subsel: 1,
            size: 19
        })
    );

    // A minimum above the maximum.
    payload[0..4].copy_from_slice(&0x8000i32.to_le_bytes());
    let mut dev = Device::new(vec![answer(0x12, 0x00, &payload)]);
    assert_eq!(
        abs_info(&mut dev, 0),
        Err(InputError::AbsRange {
            axis: 0,
            min: 0x8000,
            max: 0x7fff
        })
    );
}

#[test]
fn devids_are_bustype_vendor_product_version() {
    let payload = [0x06, 0x00, 0x27, 0x06, 0x01, 0x00, 0x01, 0x00];
    let mut dev = Device::new(vec![answer(0x03, 0, &payload)]);
    assert_eq!(
        devids(&mut dev),
        Ok(DevIds {
            bustype: 6,
            vendor: 0x0627,
            product: 1,
            version: 1
        })
    );
    let mut dev = Device::new(vec![answer(0x03, 0, &payload[..7])]);
    assert_eq!(
        devids(&mut dev),
        Err(InputError::AnswerTooShort {
            select: 0x03,
            subsel: 0,
            size: 7
        })
    );
}

// -- QEMU 9.2.4's devices ----------------------------------------------------------

/// A QEMU name answer: `sizeof` the literal, so the NUL is counted.
fn qemu_name(name: &[u8]) -> (u8, u8, u8, Vec<u8>) {
    let mut payload = name.to_vec();
    payload.push(0);
    answer(0x01, 0, &payload)
}

/// A QEMU devids answer: `BUS_VIRTUAL`, vendor 0x0627.
fn qemu_devids(product: u16, version: u16) -> (u8, u8, u8, Vec<u8>) {
    let mut payload = vec![0x06, 0x00, 0x27, 0x06];
    payload.extend_from_slice(&product.to_le_bytes());
    payload.extend_from_slice(&version.to_le_bytes());
    answer(0x03, 0, &payload)
}

/// A bitmap answer as `virtio_input_extend_config` builds one: as many bytes
/// as reach the highest bit.
fn qemu_bits(select: u8, subsel: u8, bits: &[u16]) -> (u8, u8, u8, Vec<u8>) {
    let top = bits.iter().copied().max().expect("bits");
    let mut payload = vec![0u8; usize::from(top / 8) + 1];
    for &bit in bits {
        payload[usize::from(bit / 8)] |= 1 << (bit % 8);
    }
    answer(select, subsel, &payload)
}

/// A QEMU absinfo answer: only `min` and `max` set.
fn qemu_abs(axis: u8, min: i32, max: i32) -> (u8, u8, u8, Vec<u8>) {
    let mut payload = Vec::new();
    for field in [min, max, 0, 0, 0] {
        payload.extend_from_slice(&field.to_le_bytes());
    }
    answer(0x12, axis, &payload)
}

/// QEMU's block: the longest answer plus the header.
fn qemu_device(answers: Vec<(u8, u8, u8, Vec<u8>)>) -> Device {
    let longest = answers.iter().map(|a| usize::from(a.2)).max().unwrap_or(0);
    Device::sized(longest + 8, answers)
}

#[test]
fn qemus_keyboard_reads_whole() {
    // The highest code QEMU's qcode map gives is KEY_MEDIA, 226.
    let mut dev = qemu_device(vec![
        qemu_name(b"QEMU Virtio Keyboard"),
        qemu_devids(1, 1),
        answer(0x11, 0x14, &[0]),
        answer(0x11, 0x11, &[0b111]),
        qemu_bits(0x11, 0x01, &[1, 30, 226]),
        // A serial= property, which QEMU writes with snprintf.
        answer(0x02, 0, b"kbd0"),
    ]);
    assert_eq!(dev.block.len(), 37);

    let got = name(&mut dev).expect("named");
    assert_eq!(got.len(), 21);
    assert_eq!(got.as_str_bytes(), b"QEMU Virtio Keyboard");
    let got = serial(&mut dev).expect("serial");
    assert_eq!((got.len(), got.as_str_bytes()), (4, &b"kbd0"[..]));
    assert_eq!(
        devids(&mut dev),
        Ok(DevIds {
            bustype: 6,
            vendor: 0x0627,
            product: 1,
            version: 1
        })
    );
    // EV_REP is present with no bit set: its answer is one zero byte.
    let rep = ev_bits(&mut dev, 0x14).expect("rep");
    assert!(!rep.is_empty());
    assert!((0..8).all(|bit| !rep.bit(bit)));
    // LED_NUML, LED_CAPSL, LED_SCROLLL.
    let leds = ev_bits(&mut dev, 0x11).expect("leds");
    assert_eq!(
        (0..8).filter(|&bit| leds.bit(bit)).collect::<Vec<_>>(),
        [LED_NUML, LED_CAPSL, LED_SCROLLL]
    );
    let keys = ev_bits(&mut dev, 0x01).expect("keys");
    assert_eq!(keys.len(), 29);
    assert!(keys.bit(226) && keys.bit(30) && !keys.bit(227));
    // EV_SYN, EV_ABS and the properties: nothing.
    assert_eq!(ev_bits(&mut dev, 0x00), Ok(Answer::EMPTY));
    assert_eq!(ev_bits(&mut dev, 0x03), Ok(Answer::EMPTY));
    assert_eq!(prop_bits(&mut dev), Ok(Answer::EMPTY));
    assert_eq!(
        abs_info(&mut dev, 0),
        Err(InputError::Absent {
            select: 0x12,
            subsel: 0
        })
    );
}

#[test]
fn qemus_mouse_and_tablet_read_whole() {
    // BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA, BTN_TOUCH,
    // BTN_GEAR_DOWN, BTN_GEAR_UP.
    let buttons = [0x110, 0x111, 0x112, 0x113, 0x114, 0x14a, 0x150, 0x151];
    assert_eq!(
        buttons,
        [
            BTN_LEFT,
            BTN_RIGHT,
            BTN_MIDDLE,
            BTN_SIDE,
            BTN_EXTRA,
            BTN_TOUCH,
            BTN_GEAR_DOWN,
            BTN_GEAR_UP
        ]
    );
    let mut mouse = qemu_device(vec![
        qemu_name(b"QEMU Virtio Mouse"),
        qemu_devids(2, 2),
        answer(0x11, 0x02, &[0b11, 1 << (0x08 - 8)]),
        qemu_bits(0x11, 0x01, &buttons),
    ]);
    assert_eq!(mouse.block.len(), 51);
    assert_eq!(name(&mut mouse).expect("named").len(), 18);
    let rel = ev_bits(&mut mouse, 0x02).expect("rel");
    // REL_X, REL_Y, REL_WHEEL.
    assert_eq!(
        (0..16).filter(|&bit| rel.bit(bit)).collect::<Vec<_>>(),
        [REL_X, REL_Y, REL_WHEEL]
    );
    let keys = ev_bits(&mut mouse, 0x01).expect("buttons");
    assert_eq!(keys.len(), 43);
    assert!(buttons.iter().all(|&bit| keys.bit(bit)));

    let mut tablet = qemu_device(vec![
        qemu_name(b"QEMU Virtio Tablet"),
        qemu_devids(3, 2),
        answer(0x11, 0x03, &[0b11]),
        answer(0x11, 0x02, &[0, 1 << (0x08 - 8)]),
        qemu_abs(0x00, 0, 0x7fff),
        qemu_abs(0x01, 0, 0x7fff),
        qemu_bits(0x11, 0x01, &buttons),
    ]);
    assert_eq!(tablet.block.len(), 51);
    for axis in [0, 1] {
        assert_eq!(
            abs_info(&mut tablet, axis),
            Ok(AbsInfo {
                min: 0,
                max: 0x7fff,
                ..AbsInfo::default()
            })
        );
    }
    assert!(matches!(
        abs_info(&mut tablet, 2),
        Err(InputError::Absent { .. })
    ));
    assert_eq!(devids(&mut tablet).expect("ids").product, 3);
}

#[test]
fn qemus_multitouch_reads_whole() {
    // ABS_MT_SLOT, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_TRACKING_ID.
    let mut dev = qemu_device(vec![
        qemu_name(b"QEMU Virtio MultiTouch"),
        qemu_devids(3, 1),
        qemu_abs(0x2f, 0, 10),
        qemu_abs(0x39, 0, 10),
        qemu_abs(0x35, 0, 0x7fff),
        qemu_abs(0x36, 0, 0x7fff),
        qemu_bits(0x11, 0x01, &[0x110, 0x14a, 0x151]),
        // INPUT_PROP_DIRECT.
        qemu_bits(0x10, 0, &[1]),
        qemu_bits(0x11, 0x03, &[0x2f, 0x35, 0x36, 0x39]),
    ]);
    assert_eq!(dev.block.len(), 51);
    assert!(prop_bits(&mut dev).expect("props").bit(1));
    assert!(prop_bits(&mut dev).expect("props").bit(INPUT_PROP_DIRECT));
    let abs = ev_bits(&mut dev, 0x03).expect("abs");
    assert_eq!(abs.len(), 8);
    assert_eq!(
        (0..64).filter(|&bit| abs.bit(bit)).collect::<Vec<_>>(),
        [
            ABS_MT_SLOT,
            ABS_MT_POSITION_X,
            ABS_MT_POSITION_Y,
            ABS_MT_TRACKING_ID
        ]
    );
    for (axis, max) in [(0x2f, 10), (0x39, 10), (0x35, 0x7fff), (0x36, 0x7fff)] {
        assert!(abs.bit(axis));
        assert_eq!(abs_info(&mut dev, axis).expect("axis").max, max);
    }
}

// -- Events -------------------------------------------------------------------------

#[test]
fn an_event_is_type_code_value_little_endian() {
    // struct virtio_input_event: __le16 type, __le16 code, __le32 value.
    let bytes = [0x02, 0x00, 0x01, 0x00, 0xFE, 0xFF, 0xFF, 0xFF];
    let event = Event::decode(&bytes).expect("eight bytes");
    assert_eq!(
        event,
        Event {
            kind: 2,
            code: 1,
            value: -2
        }
    );
    assert_eq!(event.to_bytes(), bytes);
    let mut out = [0xEEu8; 9];
    assert_eq!(event.encode(&mut out), Ok(8));
    assert_eq!(out[..8], bytes);
    assert_eq!(out[8], 0xEE);
}

#[test]
fn events_round_trip() {
    for event in [
        Event::default(),
        Event {
            kind: 1,
            code: 30,
            value: 1,
        },
        Event {
            kind: 0x11,
            code: 1,
            value: 0,
        },
        Event {
            kind: u16::MAX,
            code: u16::MAX,
            value: i32::MIN,
        },
    ] {
        assert_eq!(Event::decode(&event.to_bytes()), Ok(event));
        assert_eq!(Event::from_completion(&event.to_bytes(), 8), Ok(event));
    }
    assert!(Event::default().is_report());
    assert!(
        !Event {
            kind: 0,
            code: 3,
            value: 0
        }
        .is_report()
    );
}

#[test]
fn a_short_or_miscounted_event_is_refused() {
    let bytes = [1u8, 0, 30, 0, 1, 0, 0, 0];
    for len in 0..8 {
        assert_eq!(
            Event::decode(&bytes[..len]),
            Err(InputError::EventTooShort(len))
        );
        assert_eq!(
            Event::encode(&Event::default(), &mut [0u8; 8][..len]),
            Err(InputError::BufferTooShort(len))
        );
    }
    for written in [0u32, 4, 7, 9, 16, u32::MAX] {
        assert_eq!(
            Event::from_completion(&bytes, written),
            Err(InputError::EventWritten(written))
        );
    }
    // Written says 8, but the buffer handed over is shorter.
    assert_eq!(
        Event::from_completion(&bytes[..4], 8),
        Err(InputError::EventTooShort(4))
    );
}

#[test]
fn every_error_displays() {
    use std::string::ToString;
    for error in [
        InputError::ConfigTooShort(1),
        InputError::SizeTooLarge {
            select: 1,
            subsel: 0,
            size: 129,
        },
        InputError::AnswerPastConfig {
            select: 1,
            subsel: 0,
            size: 30,
            len: 37,
        },
        InputError::Subsel(0x100),
        InputError::Absent {
            select: 1,
            subsel: 0,
        },
        InputError::AnswerTooShort {
            select: 0x12,
            subsel: 0,
            size: 3,
        },
        InputError::AbsRange {
            axis: 0,
            min: 1,
            max: 0,
        },
        InputError::EventTooShort(3),
        InputError::EventWritten(9),
        InputError::BufferTooShort(3),
    ] {
        assert!(error.to_string().contains("virtio-input"), "{error:?}");
    }
}
