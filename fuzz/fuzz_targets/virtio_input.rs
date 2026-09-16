//! Fuzz virtio-input's device protocol: the answers a device gives to
//! configuration queries and the events it writes.
//!
//! The driver runs in ring 3 against a device it does not control, so every
//! `size`, every answer and every event is the device's word.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A query asks, then reads what it was told**: `select` is written
//!    before `subsel`, both before `size` is read; an answer is refused
//!    exactly when `size` is over 128, and otherwise is `size` bytes of the
//!    union, byte for byte.
//! 2. **A parsed structure keeps its promises**: a name or serial is never
//!    empty, an axis's minimum is not above its maximum, and every field reads
//!    back from the union at its offset.
//! 3. **An event is eight bytes**: a completion is accepted only when it wrote
//!    exactly eight, decoding and encoding are inverse, and a short buffer is
//!    refused.

#![no_main]

use std::cell::RefCell;

use ferrix_virtio::input::{
    ConfigSelect, DeviceConfig, EVENT_LEN, Event, InputError, abs_info, devids, ev_bits, name,
    query, serial,
};
use libfuzzer_sys::fuzz_target;

/// A device whose answer to every query is the fuzzer's: `size` and the
/// union come from the input whatever was asked.
struct Device<'a> {
    size: u8,
    union: &'a [u8],
    select: Option<u8>,
    subsel: Option<u8>,
    order: RefCell<Vec<u32>>,
}

impl DeviceConfig for Device<'_> {
    fn config_len(&self) -> u32 {
        136
    }

    fn config_read8(&self, offset: u32) -> u8 {
        self.order.borrow_mut().push(offset);
        match offset {
            0 => self.select.unwrap_or(0),
            1 => self.subsel.unwrap_or(0),
            2 => self.size,
            3..=7 => 0,
            _ => self.union.get(offset as usize - 8).copied().unwrap_or(0),
        }
    }

    fn config_read16(&self, offset: u32) -> u16 {
        u16::from_le_bytes([self.config_read8(offset), self.config_read8(offset + 1)])
    }

    fn config_read32(&self, offset: u32) -> u32 {
        u32::from(self.config_read16(offset)) | u32::from(self.config_read16(offset + 2)) << 16
    }
}

impl ConfigSelect for Device<'_> {
    fn config_write8(&mut self, offset: u32, value: u8) {
        self.order.borrow_mut().push(offset);
        match offset {
            0 => self.select = Some(value),
            1 => self.subsel = Some(value),
            _ => panic!("a query wrote offset {offset}"),
        }
    }
}

fn union_byte(union: &[u8], at: usize) -> u8 {
    union.get(at).copied().unwrap_or(0)
}

fuzz_target!(|bytes: &[u8]| {
    let Some((&size, rest)) = bytes.split_first() else {
        return;
    };
    let Some((&asked, rest)) = rest.split_first() else {
        return;
    };
    let union = rest.get(..128.min(rest.len())).unwrap_or(&[]);
    let events = rest.get(union.len()..).unwrap_or(&[]);
    let device = || Device {
        size,
        union,
        select: None,
        subsel: None,
        order: RefCell::new(Vec::new()),
    };

    // 1.
    let mut dev = device();
    match query(&mut dev, 0x11, asked) {
        Ok(answer) => {
            assert!(size <= 128);
            assert_eq!(answer.len(), usize::from(size));
            for (at, &byte) in answer.as_bytes().iter().enumerate() {
                assert_eq!(byte, union_byte(union, at));
            }
            for bit in 0..(u16::from(size) * 8) {
                let byte = union_byte(union, usize::from(bit / 8));
                assert_eq!(answer.bit(bit), byte & (1 << (bit % 8)) != 0);
            }
        }
        Err(InputError::SizeTooLarge { size: refused, .. }) => {
            assert!(size > 128);
            assert_eq!(refused, size);
        }
        Err(other) => panic!("a query failed with {other:?}"),
    }
    assert_eq!((dev.select, dev.subsel), (Some(0x11), Some(asked)));
    assert_eq!(dev.order.borrow().get(..3), Some(&[0, 1, 2][..]));

    // 2.
    for result in [name(&mut device()), serial(&mut device())] {
        if let Ok(answer) = result {
            assert!(!answer.is_empty());
            assert!(answer.as_str_bytes().len() <= answer.len());
            assert!(!answer.as_str_bytes().contains(&0));
        }
    }
    let _ = ev_bits(&mut device(), u16::from(asked));
    let field32 =
        |at: usize| i32::from_le_bytes(std::array::from_fn(|index| union_byte(union, at + index)));
    if let Ok(info) = abs_info(&mut device(), u16::from(asked)) {
        assert!((20..=128).contains(&size));
        assert!(info.min <= info.max);
        assert_eq!(
            [info.min, info.max, info.fuzz, info.flat, info.res],
            [field32(0), field32(4), field32(8), field32(12), field32(16)]
        );
    }
    if let Ok(ids) = devids(&mut device()) {
        assert!((8..=128).contains(&size));
        let field16 =
            |at: usize| u16::from_le_bytes([union_byte(union, at), union_byte(union, at + 1)]);
        assert_eq!(
            [ids.bustype, ids.vendor, ids.product, ids.version],
            [field16(0), field16(2), field16(4), field16(6)]
        );
    }

    // 3.
    let written = u32::from(asked);
    match Event::from_completion(events, written) {
        Ok(event) => {
            assert_eq!(written as usize, EVENT_LEN);
            assert_eq!(event.to_bytes()[..], events[..EVENT_LEN]);
        }
        Err(InputError::EventWritten(_)) => assert_ne!(written as usize, EVENT_LEN),
        Err(InputError::EventTooShort(len)) => {
            assert_eq!(len, events.len());
            assert!(len < EVENT_LEN);
        }
        Err(other) => panic!("an event failed with {other:?}"),
    }
    for chunk in events.chunks(EVENT_LEN) {
        match Event::decode(chunk) {
            Ok(event) => {
                assert_eq!(event.to_bytes()[..], chunk[..]);
                let mut out = [0u8; EVENT_LEN];
                assert_eq!(event.encode(&mut out), Ok(EVENT_LEN));
                assert_eq!(Event::decode(&out), Ok(event));
                assert!(event.encode(&mut out[..EVENT_LEN - 1]).is_err());
            }
            Err(error) => {
                assert!(chunk.len() < EVENT_LEN);
                assert_eq!(error, InputError::EventTooShort(chunk.len()));
            }
        }
    }
});
