//! A console record in the phone's `ramoops` region.
//!
//! This loader has no serial port it can reach -- the phone's UART is behind a
//! debug accessory on the USB-C port -- and until it knows where the screen is,
//! no display either. What it does have is memory that survives a warm reset:
//! the region Android's kernel keeps its console in, and reads back after the
//! next boot. Writing a record there in the kernel's own format turns "what
//! happened" into a file on the phone.
//!
//! The format is Linux's `struct persistent_ram_buffer`: a signature, the
//! write offset, the number of valid bytes, then the bytes. With the offset
//! equal to the length the kernel reads it as one unwrapped run.
//!
//! Every access is volatile and at most word-sized and aligned, because until
//! the loader turns the MMU on, all of memory is Device memory, where an
//! unaligned access faults.

use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::board::{RAMOOPS_CONSOLE, RAMOOPS_CONSOLE_SIZE};

/// `PERSISTENT_RAM_SIG`, "DBGC".
const SIGNATURE: u32 = 0x4347_4244;

/// Bytes of header before the text.
const HEADER: u64 = 12;

/// Bytes of text the zone holds.
const CAPACITY: u64 = RAMOOPS_CONSOLE_SIZE - HEADER;

/// Bytes written so far. Only one processor runs, and a plain load and store
/// are used rather than a read-modify-write, because exclusive accesses to
/// Device memory are not defined to work.
static LENGTH: AtomicU64 = AtomicU64::new(0);

/// Write one aligned word of the header.
fn header_word(offset: u64, value: u32) {
    // SAFETY: the zone is RAM the device tree reserves for this record and
    // nothing else runs; the offset is 0, 4 or 8, so the word is aligned.
    unsafe {
        ptr::write_volatile((RAMOOPS_CONSOLE + offset) as *mut u32, value);
    }
}

/// Start an empty record, replacing whatever the zone held.
pub(crate) fn start() {
    LENGTH.store(0, Ordering::Relaxed);
    header_word(0, SIGNATURE);
    header_word(4, 0);
    header_word(8, 0);
}

/// Append one byte, dropping it once the zone is full.
pub(crate) fn byte(value: u8) {
    let length = LENGTH.load(Ordering::Relaxed);
    if length >= CAPACITY {
        return;
    }
    // SAFETY: the byte is inside the zone, below its capacity.
    unsafe {
        ptr::write_volatile((RAMOOPS_CONSOLE + HEADER + length) as *mut u8, value);
    }
    let length = length + 1;
    LENGTH.store(length, Ordering::Relaxed);
    // The capacity is under 4 GiB, so the length fits the header's word.
    let word = u32::try_from(length).unwrap_or(u32::MAX);
    header_word(4, word);
    header_word(8, word);
}

/// Append text.
pub(crate) fn text(value: &str) {
    value.bytes().for_each(byte);
}

/// Append a number as `0x` and sixteen hexadecimal digits.
pub(crate) fn hex(value: u64) {
    text("0x");
    for shift in (0..16).rev() {
        let digit = ((value >> (shift * 4)) & 0xf) as u8;
        byte(if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        });
    }
}

/// Append a number in decimal.
pub(crate) fn decimal(value: u64) {
    let mut divisor = 1;
    while value / divisor >= 10 {
        divisor *= 10;
    }
    while divisor > 0 {
        byte(b'0' + ((value / divisor) % 10) as u8);
        divisor /= 10;
    }
}

/// Append a labelled number and end the line.
pub(crate) fn field(label: &str, value: u64) {
    text(label);
    hex(value);
    byte(b'\n');
}
