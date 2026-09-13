//! The 16550 UART behind COM1.
//!
//! One of the two device drivers inside the kernel — see `crate::console` for
//! why it is here at all rather than in userspace.

use super::cpu;

/// COM1. Fixed by convention since the IBM PC and still where firmware puts the
/// debug console, which is what `-serial stdio` connects to.
const COM1: u16 = 0x3F8;

/// Transmit and receive buffer, and the low half of the divisor when the
/// divisor latch is open.
const DATA: u16 = COM1;
/// Interrupt enable, and the high half of the divisor.
const INTERRUPT_ENABLE: u16 = COM1 + 1;
/// FIFO control.
const FIFO_CONTROL: u16 = COM1 + 2;
/// Line control, whose top bit is the divisor latch.
const LINE_CONTROL: u16 = COM1 + 3;
/// Modem control.
const MODEM_CONTROL: u16 = COM1 + 4;
/// Line status.
const LINE_STATUS: u16 = COM1 + 5;

/// `LINE_CONTROL`: open the divisor latch.
const DIVISOR_LATCH: u8 = 0x80;
/// `LINE_CONTROL`: eight data bits, no parity, one stop bit.
const EIGHT_N_ONE: u8 = 0x03;
/// `FIFO_CONTROL`: enable, clear both FIFOs, interrupt at 14 bytes.
const FIFO_ENABLE: u8 = 0xC7;
/// `MODEM_CONTROL`: data terminal ready, request to send, auxiliary output 2.
const MODEM_READY: u8 = 0x0B;
/// `LINE_STATUS`: a received byte is waiting in the data register.
const DATA_READY: u8 = 1;
/// `LINE_STATUS`: the transmit holding register is empty.
const TRANSMIT_EMPTY: u8 = 1 << 5;

/// Divisor for 115200 baud from the 1.8432 MHz clock.
const DIVISOR: u16 = 1;

/// Configure the port.
///
/// Firmware has usually done this already, but "usually" is not a property to
/// build a panic handler on.
pub(crate) fn init() {
    // Interrupts off: there is no interrupt controller yet, and a panic must be
    // able to print without one. `enable_receive_interrupt` turns receive on
    // once the I/O APIC routes the line.
    write_register(INTERRUPT_ENABLE, 0x00);
    write_register(LINE_CONTROL, DIVISOR_LATCH);
    write_register(DATA, DIVISOR as u8);
    write_register(INTERRUPT_ENABLE, (DIVISOR >> 8) as u8);
    write_register(LINE_CONTROL, EIGHT_N_ONE);
    write_register(FIFO_CONTROL, FIFO_ENABLE);
    write_register(MODEM_CONTROL, MODEM_READY);
}

/// The ISA interrupt COM1 raises.
pub(crate) const ISA_IRQ: u8 = 4;

/// Interrupt enable register: raise the port's line while received data waits.
const RECEIVE_DATA_AVAILABLE: u8 = 1;

/// Let the port raise its interrupt when a byte arrives.
///
/// Reading the byte clears it, which [`read_byte`] does. `MODEM_CONTROL`
/// already sets `OUT2`, the bit that on a PC connects the 16550's interrupt
/// output to the interrupt controller at all.
pub(crate) fn enable_receive_interrupt() {
    write_register(INTERRUPT_ENABLE, RECEIVE_DATA_AVAILABLE);
}

/// Write one of COM1's registers.
///
/// Safe, and the reason it is safe is worth stating once here rather than at
/// seven call sites: every `port` passed in is one of the constants above, all
/// of which name a register of the 16550 the platform fixes at `COM1`.
fn write_register(port: u16, value: u8) {
    // SAFETY: `port` is one of COM1's own registers and `value` is the setting
    // the device's documented initialisation sequence calls for.
    unsafe { cpu::outb(port, value) };
}

/// Read one of COM1's registers.
fn read_register(port: u16) -> u8 {
    // SAFETY: as `write_register`. The only register read is `LINE_STATUS`,
    // which has no side effects — unlike `DATA`, which would consume a
    // received character.
    unsafe { cpu::inb(port) }
}

/// Send one byte, waiting for room.
///
/// The wait is bounded rather than a bare loop: a panic that hangs forever
/// inside the console because no serial port answered is strictly worse than a
/// panic nobody can read.
pub(crate) fn write_byte(byte: u8) {
    const SPIN_LIMIT: u32 = 100_000;

    for _ in 0..SPIN_LIMIT {
        if read_register(LINE_STATUS) & TRANSMIT_EMPTY != 0 {
            break;
        }
        core::hint::spin_loop();
    }

    // If the wait above timed out the byte is dropped by the hardware, which is
    // the right outcome for a port nothing is listening to.
    write_register(DATA, byte);
}

/// Wait until everything written has left the port, for a caller about to
/// power off or stop.
///
/// [`write_byte`] waits for the holding register, which empties as soon as the
/// byte moves to the shift register, so the last byte can still be sending when
/// it returns. Harmless under QEMU, whose port sends instantly; on hardware a
/// power-off straight after would cut the line, as it did on a DK1's USART.
/// Bounded, like the write.
pub(crate) fn drain() {
    /// `LINE_STATUS`: the holding register and the shift register are both
    /// empty.
    const TRANSMITTER_IDLE: u8 = 1 << 6;
    /// Far longer than a sixteen-byte FIFO takes to empty at 115200 baud.
    const DRAIN_LIMIT: u32 = 10_000_000;

    for _ in 0..DRAIN_LIMIT {
        if read_register(LINE_STATUS) & TRANSMITTER_IDLE != 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// One received byte, if the port has one.
///
/// A plain read, for both paths: polled before the port's receive interrupt is
/// installed and during a panic, which is why [`init`] leaves the port's
/// interrupts off; and called from that interrupt once `console::input` has
/// routed it, to empty the port into the receive ring.
pub(crate) fn read_byte() -> Option<u8> {
    if read_register(LINE_STATUS) & DATA_READY == 0 {
        return None;
    }
    Some(read_register(DATA))
}
