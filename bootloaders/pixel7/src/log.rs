//! A console record in the phone's `ramoops` region, or in a guest of crosvm,
//! its 16550.
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

use crate::board::{self, GUEST_UART, RAMOOPS_CONSOLE, RAMOOPS_CONSOLE_SIZE};

/// The 16550's line status register, and its bit for "room to send".
const UART_LSR: u64 = 5;
const LSR_THR_EMPTY: u8 = 1 << 5;

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

/// Start an empty record, replacing whatever the zone held. A guest has no
/// zone: its bytes go straight out of the UART.
pub(crate) fn start() {
    if board::is_guest() {
        return;
    }
    LENGTH.store(0, Ordering::Relaxed);
    header_word(0, SIGNATURE);
    header_word(4, 0);
    header_word(8, 0);
}

/// Send one byte out of a guest's 16550, waiting a bounded time for room.
fn uart_byte(value: u8) {
    for _ in 0..100_000 {
        // SAFETY: the 16550 crosvm puts at `GUEST_UART`, which is MMIO, and
        // reached as Device memory with the MMU off; a byte register.
        if unsafe { ptr::read_volatile((GUEST_UART + UART_LSR) as *const u8) } & LSR_THR_EMPTY != 0
        {
            break;
        }
        core::hint::spin_loop();
    }
    // SAFETY: as above, its transmit holding register.
    unsafe { ptr::write_volatile(GUEST_UART as *mut u8, value) };
}

/// Append one byte, dropping it once the zone is full.
pub(crate) fn byte(value: u8) {
    if board::is_guest() {
        uart_byte(value);
        return;
    }
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

/// The record as a `core::fmt` sink, for [`say!`].
#[derive(Debug)]
pub(crate) struct Writer;

impl core::fmt::Write for Writer {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        text(value);
        Ok(())
    }
}

/// Append a formatted line to the record.
macro_rules! say {
    ($($argument:tt)*) => {{
        use core::fmt::Write as _;
        let _ = writeln!($crate::log::Writer, $($argument)*);
    }};
}
pub(crate) use say;
