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
    // Interrupts off: the kernel polls, because there is no interrupt
    // controller yet and a panic must be able to print without one.
    write_register(INTERRUPT_ENABLE, 0x00);
    write_register(LINE_CONTROL, DIVISOR_LATCH);
    write_register(DATA, DIVISOR as u8);
    write_register(INTERRUPT_ENABLE, (DIVISOR >> 8) as u8);
    write_register(LINE_CONTROL, EIGHT_N_ONE);
    write_register(FIFO_CONTROL, FIFO_ENABLE);
    write_register(MODEM_CONTROL, MODEM_READY);
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

/// One received byte, if the port has one.
///
/// Polled rather than interrupt-driven, for the same reason [`init`] leaves
/// the port's interrupts off: the console has to work before there is an
/// interrupt controller and during a panic, and a second path that only works
/// afterwards is a second thing to get wrong. A program waiting on its
/// keyboard costs a busy processor until the tty layer can put it to sleep.
pub(crate) fn read_byte() -> Option<u8> {
    if read_register(LINE_STATUS) & DATA_READY == 0 {
        return None;
    }
    Some(read_register(DATA))
}
