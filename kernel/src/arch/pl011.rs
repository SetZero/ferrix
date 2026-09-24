//! The PL011 UART, on both Arm architectures.
//!
//! One of the two device drivers inside the kernel — see `crate::console` for
//! why it is here at all rather than in userspace. The same Arm primecell sits
//! at the same register offsets on every machine that has one; what differs is
//! where it is, which the caller learns from the machine's description and
//! passes in.
//!
//! It is `MMIO`, so it has to be *mapped* before it can be written, and mapped
//! as device memory: through a normal cacheable mapping the writes may be
//! merged, reordered or held in a cache line, and the symptom is a console that
//! prints nothing at all.

use core::cell::UnsafeCell;

use ferrix_bootinfo::{KERNEL_VMAP_BASE, PAGE_SIZE};

use crate::early::{EarlyError, EarlyMemory};

/// Where the register window is mapped: the bottom of the kernel's dynamic
/// mapping area, below the arena, which keeps the low windows for early boot.
const WINDOW: u64 = KERNEL_VMAP_BASE;

/// Data register: writing transmits, reading takes a received byte.
const DR: u64 = 0x000;
/// `DR` as read: the framing, parity, break and overrun flags the byte arrived
/// with, above the byte itself.
const DR_ERRORS: u32 = 0xF00;
/// Receive status register; writing it, as the error clear register, clears
/// the flags.
const RSR_ECR: u64 = 0x004;
/// Flag register.
const FR: u64 = 0x018;
/// `FR`: the transmit FIFO is full.
const FR_TXFF: u32 = 1 << 5;
/// `FR`: the receive FIFO is empty.
const FR_RXFE: u32 = 1 << 4;
/// `FR`: the transmit FIFO is empty.
const FR_TXFE: u32 = 1 << 7;
/// `FR`: the port is still sending a byte, FIFO empty or not.
const FR_BUSY: u32 = 1 << 3;
/// Interrupt mask set/clear register: a set bit lets that interrupt out.
const IMSC: u64 = 0x038;
/// `IMSC`: the receive FIFO reached its trigger level.
const IMSC_RX: u32 = 1 << 4;
/// `IMSC`: bytes have sat in the receive FIFO, below the trigger level, for
/// longer than a few characters' time — the interrupt a single keystroke raises.
const IMSC_RT: u32 = 1 << 6;
/// `IMSC`: the transmit FIFO has drained to its trigger level.
const IMSC_TX: u32 = 1 << 5;

/// The mapped base address, once [`init`] has run.
struct Base(UnsafeCell<u64>);

// SAFETY: early boot is single-threaded — no other CPU has been started and
// interrupts are masked — so there is never a second accessor.
unsafe impl Sync for Base {}

static BASE: Base = Base(UnsafeCell::new(0));

/// Map the registers at physical address `phys` and record where they landed.
///
/// The offset within the page is kept, so a UART whose registers do not start
/// on a page boundary still reads correctly.
pub(crate) fn init(memory: &mut EarlyMemory, phys: u64) -> Result<(), EarlyError> {
    let offset = phys % PAGE_SIZE;
    memory.map_device(WINDOW, phys - offset, PAGE_SIZE)?;
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    unsafe { *BASE.0.get() = WINDOW + offset };
    Ok(())
}

/// Read one of the UART's registers.
fn read(offset: u64) -> u32 {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    let base = unsafe { *BASE.0.get() };
    // SAFETY: `base` is the device window `init` mapped — the only caller
    // checks it is not zero first — and `offset` is a register inside it.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

/// Write one of the UART's registers.
fn write(offset: u64, value: u32) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    let base = unsafe { *BASE.0.get() };
    // SAFETY: as in `read`.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, value) };
}

/// Send one byte, waiting for room in the transmit FIFO.
///
/// Firmware configured the baud rate and line format before handing over and
/// this driver does not disturb them: reprogramming a port a terminal is
/// already attached to is how a boot log turns into line noise halfway through.
///
/// The wait is bounded rather than a bare loop, because a panic that hangs
/// inside the console because nothing answered is worse than one nobody reads.
pub(crate) fn write_byte(byte: u8) {
    const SPIN_LIMIT: u32 = 100_000;

    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }

    for _ in 0..SPIN_LIMIT {
        if read(FR) & FR_TXFF == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    write(DR, u32::from(byte));
}

/// Wait until everything written has left the port, for a caller about to
/// power off or stop.
///
/// [`write_byte`] waits only for room in the FIFO, so the last line of a report
/// can still be sending when it returns, and a power-off straight after cuts it
/// off: seen on the STM32 USART of a DK1, and the same shape on any real
/// PL011. Bounded, like the write.
pub(crate) fn drain() {
    /// About half a second of polling; a sixteen-byte FIFO empties in under two
    /// milliseconds at 115200 baud.
    const DRAIN_LIMIT: u32 = 10_000_000;

    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    for _ in 0..DRAIN_LIMIT {
        if read(FR) & (FR_TXFE | FR_BUSY) == FR_TXFE {
            return;
        }
        core::hint::spin_loop();
    }
}

/// One received byte, if the receive FIFO holds one.
///
/// A byte that arrived with an error flag is still delivered, and the flags are
/// cleared: a character mangled by line noise is better seen than silently
/// dropped, and a break reads as the NUL the port puts in the FIFO for it.
pub(crate) fn read_byte() -> Option<u8> {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return None;
    }

    if read(FR) & FR_RXFE != 0 {
        return None;
    }
    let data = read(DR);
    if data & DR_ERRORS != 0 {
        write(RSR_ECR, 0);
    }
    Some(data.to_le_bytes()[0])
}

/// Let the port interrupt when it has received something.
///
/// Both receive interrupts, because each alone loses: the FIFO-level one never
/// fires for a byte or two typed by hand, and the timeout one waits out a gap
/// in a stream that has already filled the FIFO. Reading the FIFO empty clears
/// both, so the handler needs nothing but [`read_byte`].
pub(crate) fn enable_receive_interrupt() {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    write(IMSC, read(IMSC) | IMSC_RX | IMSC_RT);
}

/// How many bytes the port can take now without anyone waiting: one while the
/// transmit FIFO is not full, which the caller asks again after each.
pub(crate) fn transmit_room() -> usize {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return 0;
    }
    usize::from(read(FR) & FR_TXFF == 0)
}

/// Hand the port one byte, for a caller [`transmit_room`] said it had room
/// for.
pub(crate) fn put(byte: u8) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    write(DR, u32::from(byte));
}

/// Let the port interrupt when its transmit FIFO has drained to the trigger
/// level, or stop it.
///
/// The interrupt is raised as the FIFO drains *through* the level, and a real
/// PL011 turned on with its FIFO already below it says nothing: Linux's driver
/// fills the FIFO before it turns the interrupt on, and so does
/// `console::output`, which leaves it on only while it has more than the FIFO
/// took. QEMU's port sends at once and raises it on every write.
pub(crate) fn transmit_interrupt(on: bool) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    let mask = read(IMSC);
    write(IMSC, if on { mask | IMSC_TX } else { mask & !IMSC_TX });
}
