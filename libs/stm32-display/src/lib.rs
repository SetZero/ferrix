//! The STM32MP15 display pipeline as logic over registers: the LTDC display
//! controller, the I2C controller, and the Silicon Image `SiI9022` HDMI bridge
//! an STM32MP157 DK board wires between them and its HDMI socket.
//!
//! `user/ltdc` runs this in a ring-3 process under devmgr, as `user/gpu` runs
//! `ferrix-virtio-gpu` (`docs/DISPLAY.md` §6). The process holds an
//! `IoMapping` of each controller's registers, the LTDC's interrupt, the card
//! VMO it pins buffers from, and the control channel to the kernel's display
//! core. None of those exist in a unit test, so everything that decides a
//! register value is here, written against [`Registers`] and
//! [`sii9022::Bus`], and tested against models of the hardware:
//!
//! * [`mode`]: a video mode, the one the board falls back to (CEA-861's
//!   1280x720 at 60 Hz), and the timing an EDID describes;
//! * [`ltdc`]: the controller's timing, its first layer, and its reload and
//!   interrupt bits;
//! * [`i2c`]: the STM32 I2C controller as a polled master;
//! * [`sii9022`]: the bridge's TPI registers, its DDC pass-through and its
//!   AVI infoframe;
//! * [`edid`]: the monitor's description, read through the bridge: every
//!   mode it offers and the timings it accepts;
//! * [`choice`]: which of those the board can make a pixel clock for, and
//!   the largest, which is the one it runs.
//!
//! # What the kernel has already done, and does on request
//!
//! Clocks and pins are not a driver's: the RCC and the GPIO banks are shared
//! by every peripheral on the chip. The kernel turns on the LTDC's and the
//! I2C controller's clocks, puts the I2C controller's kernel clock on the
//! 64 MHz HSI, muxes both controllers' pins, takes the bridge out of reset,
//! and checks that the LTDC's pixel clock is the one [`mode::Mode::CEA_720P60`]
//! needs, before it publishes the device. What reaches this crate is two
//! register windows that are ready to be programmed. A mode with another
//! pixel clock is the one thing that needs the RCC again, and the kernel
//! rounds and sets that clock for the driver (`device_clock`), changing
//! nothing of PLL4 but the divider of the output only the LTDC uses.
//!
//! # No clock
//!
//! A native program cannot read the time, so every wait here is a bounded
//! number of register reads ([`Budget`]). An I2C byte at 100 kHz takes about
//! 90 µs, which makes a poll of the bridge over I2C a clock of its own.

#![no_std]
#![forbid(unsafe_code)]

pub mod choice;
pub mod edid;
pub mod i2c;
pub mod ltdc;
pub mod mode;
pub mod sii9022;
mod timings;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod edid_tests;

/// A window of 32-bit device registers, by byte offset.
pub trait Registers {
    /// Read the register at `offset`.
    fn read32(&self, offset: u32) -> u32;
    /// Write the register at `offset`.
    fn write32(&mut self, offset: u32, value: u32);
}

/// How many times a wait may look before it gives up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Budget(pub u32);

impl Budget {
    /// Read `condition` until it holds or the budget is spent: whether it
    /// held.
    pub fn wait(self, mut condition: impl FnMut() -> bool) -> bool {
        for _ in 0..self.0 {
            if condition() {
                return true;
            }
        }
        condition()
    }
}
