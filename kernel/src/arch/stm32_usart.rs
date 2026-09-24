//! The USART on an STM32MP1, which is the console on every board in that
//! family.
//!
//! The second port this architecture can put a boot console on, and the reason
//! there is a choice at all: the STM32MP157's UART4 is ST's own design rather
//! than an Arm primecell. The same three things happen as in the PL011 driver
//! beside it — wait for room, write the byte, leave the baud rate alone — at
//! different offsets and with the flag the other way up. Receiving is its
//! mirror: look for a byte, take it, and clear an overrun, which on this port
//! stays set until it is cleared.
//!
//! The layout is the one Linux calls `stm32h7`, which is what every UART on an
//! STM32MP15 declares itself to be. As with the PL011, these are `MMIO`
//! registers and have to be mapped as device memory before a write means
//! anything.

use core::cell::UnsafeCell;

use ferrix_bootinfo::{KERNEL_VMAP_BASE, PAGE_SIZE};

use crate::early::{EarlyError, EarlyMemory};

/// Where the register window is mapped: the bottom of the kernel's dynamic
/// mapping area, which is where the PL011 would have gone — exactly one of the
/// two is ever brought up.
const WINDOW: u64 = KERNEL_VMAP_BASE;

/// Control register 1.
const CR1: u64 = 0x00;
/// `CR1`: interrupt when a received byte is waiting, and on an overrun. Named
/// `RXFNEIE` on a port with its FIFO enabled; the same bit either way.
const CR1_RXNEIE: u32 = 1 << 5;
/// `CR1`: interrupt while [`ISR_TXE`] is set. Named `TXFNFIE` on a port with
/// its FIFO enabled, where it interrupts whenever the FIFO has a free slot —
/// once per byte sent, which is why such a port uses [`CR1_TXFEIE`] instead.
const CR1_TXEIE: u32 = 1 << 7;
/// `CR1`: the port's FIFOs are enabled, sixteen bytes each way. Firmware
/// decides; RM0436 lets it be changed only with the port disabled, and this
/// driver does not disable a port a terminal is attached to.
const CR1_FIFOEN: u32 = 1 << 29;
/// `CR1`: interrupt while the transmit FIFO is empty (`ISR`'s `TXFE`, bit 23).
/// Only on a port with its FIFO enabled.
const CR1_TXFEIE: u32 = 1 << 30;
/// Interrupt and status register.
const ISR: u64 = 0x1C;
/// `ISR`: there is room for another byte, in the transmit register or in the
/// transmit FIFO on a port configured to have one. The same bit either way.
const ISR_TXE: u32 = 1 << 7;
/// `ISR`: a received byte is waiting, in the receive register or in the receive
/// FIFO on a port configured to have one. The same bit either way.
const ISR_RXNE: u32 = 1 << 5;
/// `ISR`: a byte arrived while the one before it was still unread, and was lost.
const ISR_ORE: u32 = 1 << 3;
/// Interrupt clear register: writing a bit clears the matching `ISR` flag.
const ICR: u64 = 0x20;
/// `ICR`: clear [`ISR_ORE`].
const ICR_ORECF: u32 = 1 << 3;
/// Receive data register: reading it takes the byte.
const RDR: u64 = 0x24;
/// Transmit data register: writing it sends.
const TDR: u64 = 0x28;

/// The mapped base address, once [`init`] has run.
struct Base(UnsafeCell<u64>);

// SAFETY: early boot is single-threaded — no other CPU has been started and
// interrupts are masked — so there is never a second accessor.
unsafe impl Sync for Base {}

static BASE: Base = Base(UnsafeCell::new(0));

/// Map the registers at physical address `phys` and record where they landed.
///
/// The offset within the page is kept, so a port whose registers do not start
/// on a page boundary still reads correctly — and on an STM32MP15 none of them
/// does: UART4 is at `0x4001_0000`, a kilobyte into its page.
pub(crate) fn init(memory: &mut EarlyMemory, phys: u64) -> Result<(), EarlyError> {
    let offset = phys % PAGE_SIZE;
    memory.map_device(WINDOW, phys - offset, PAGE_SIZE)?;
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    unsafe { *BASE.0.get() = WINDOW + offset };
    Ok(())
}

/// Read one of the port's registers.
fn read(offset: u64) -> u32 {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    let base = unsafe { *BASE.0.get() };
    // SAFETY: `base` is the device window `init` mapped — the only caller
    // checks it is not zero first — and `offset` is a register inside it.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

/// Write one of the port's registers.
fn write(offset: u64, value: u32) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    let base = unsafe { *BASE.0.get() };
    // SAFETY: as in `read`.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, value) };
}

/// Send one byte, waiting for room in the transmit register.
///
/// Firmware configured the baud rate and the line format and this driver does
/// not disturb them, for the reason the PL011 driver gives: reprogramming a
/// port that a terminal is already attached to turns a boot log into line noise
/// halfway through. On these boards firmware is U-Boot and the terminal is the
/// debugger's virtual serial port.
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
        if read(ISR) & ISR_TXE != 0 {
            break;
        }
        core::hint::spin_loop();
    }
    write(TDR, u32::from(byte));
}

/// Wait until everything written has left the port, for a caller about to
/// power off or stop.
///
/// [`write_byte`] waits only for room for the next byte, so the last line of a
/// report is still in the FIFO and the shift register when it returns. On a
/// board that is harmless until the next instruction removes power: PSCI
/// `SYSTEM_OFF` straight after the final line cut it off mid-word on a DK1,
/// which QEMU, whose port sends instantly, never shows. Bounded, like the
/// write: a port that never reports itself done costs a moment, not the stop.
pub(crate) fn drain() {
    /// `ISR`: the last byte has left the shift register and the FIFO is empty.
    const ISR_TC: u32 = 1 << 6;
    /// About half a second of polling on a Cortex-A7; the FIFO and shift
    /// register empty in under two milliseconds at 115200 baud.
    const DRAIN_LIMIT: u32 = 10_000_000;

    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    for _ in 0..DRAIN_LIMIT {
        if read(ISR) & ISR_TC != 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// One received byte, if the port has one waiting.
///
/// An overrun is cleared whenever it is seen. The byte it reports is already
/// gone, and the flag is sticky: left set, it would stay set through every
/// later byte, and with the receive interrupt enabled it would hold the line
/// asserted.
pub(crate) fn read_byte() -> Option<u8> {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return None;
    }

    let status = read(ISR);
    if status & ISR_ORE != 0 {
        write(ICR, ICR_ORECF);
    }
    if status & ISR_RXNE == 0 {
        return None;
    }
    Some(read(RDR).to_le_bytes()[0])
}

/// Let the port interrupt when it has received something.
///
/// The one enable covers both reasons to: a byte waiting, and an overrun.
/// [`read_byte`] ends both — reading `RDR` clears the first and it clears the
/// second — so the handler needs nothing else. Only this bit of `CR1` changes;
/// the ones firmware set for the line format may be written only with the port
/// disabled, and are left as they are.
pub(crate) fn enable_receive_interrupt() {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    write(CR1, read(CR1) | CR1_RXNEIE);
}

/// How many bytes the port can take now without anyone waiting: one while
/// [`ISR_TXE`] says there is room, in the transmit register or in the FIFO,
/// which the caller asks again after each.
pub(crate) fn transmit_room() -> usize {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return 0;
    }
    usize::from(read(ISR) & ISR_TXE != 0)
}

/// Hand the port one byte, for a caller [`transmit_room`] said it had room
/// for.
pub(crate) fn put(byte: u8) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    write(TDR, u32::from(byte));
}

/// Let the port interrupt when it can take more to send, or stop it.
///
/// Which interrupt depends on whether firmware enabled the FIFO, and is read
/// from the port each time rather than assumed: with it, when the FIFO has
/// emptied (`TXFEIE`), so that each interrupt is a FIFO's worth — sixteen
/// bytes, while the shift register is still sending the last one, so the line
/// never goes idle between them; without it, when the one transmit register
/// has (`TXEIE`), which is an interrupt a byte. Both are levels: the handler
/// turns them off when it has nothing more, or they would never stop.
pub(crate) fn transmit_interrupt(on: bool) {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return;
    }
    let control = read(CR1);
    let enable = if control & CR1_FIFOEN != 0 {
        CR1_TXFEIE
    } else {
        CR1_TXEIE
    };
    write(
        CR1,
        if on {
            control | enable
        } else {
            control & !enable
        },
    );
}

/// Whether the port's FIFO is enabled, for the boot line that says how the
/// console sends.
pub(crate) fn has_fifo() -> bool {
    // SAFETY: single-threaded, as documented on the `Sync` impl above.
    if unsafe { *BASE.0.get() } == 0 {
        return false;
    }
    read(CR1) & CR1_FIFOEN != 0
}
