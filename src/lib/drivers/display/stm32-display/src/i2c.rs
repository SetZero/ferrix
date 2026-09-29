//! The STM32 I2C controller (the STM32F7/MP1 design, Linux's
//! `i2c-stm32f7.c`), as a polled 7-bit master of at most 255 bytes a
//! transfer.
//!
//! A transfer is programmed whole in `CR2` — address, direction, byte count,
//! and whether the controller ends it with a STOP itself — and then fed a
//! byte each time `TXIS` asks, or emptied each time `RXNE` says. A write
//! followed by a read, which is how a register is read from the bridge, is a
//! write without AUTOEND, then a repeated START as a read with it.

use crate::sii9022::{Bus, BusError};
use crate::{Budget, Registers};

/// Control 1.
pub const CR1: u32 = 0x00;
/// Control 2.
pub const CR2: u32 = 0x04;
/// Timing.
pub const TIMINGR: u32 = 0x10;
/// Interrupt and status.
pub const ISR: u32 = 0x18;
/// Interrupt clear.
pub const ICR: u32 = 0x1C;
/// Received byte.
pub const RXDR: u32 = 0x24;
/// Byte to transmit.
pub const TXDR: u32 = 0x28;

/// `CR1`: peripheral enable. Clearing it resets the controller's state.
pub const CR1_PE: u32 = 1 << 0;
/// `CR2`: end the transfer with a STOP once its bytes are done.
pub const CR2_AUTOEND: u32 = 1 << 25;
/// `CR2`: generate a START (or repeated START).
pub const CR2_START: u32 = 1 << 13;
/// `CR2`: the transfer is a read.
pub const CR2_RD_WRN: u32 = 1 << 10;
/// `ISR`: the transmit register is empty and wants the next byte.
pub const ISR_TXIS: u32 = 1 << 1;
/// `ISR`: a byte was received.
pub const ISR_RXNE: u32 = 1 << 2;
/// `ISR`: the target did not acknowledge.
pub const ISR_NACKF: u32 = 1 << 4;
/// `ISR`: a STOP went out.
pub const ISR_STOPF: u32 = 1 << 5;
/// `ISR`: a transfer without AUTOEND has sent its bytes.
pub const ISR_TC: u32 = 1 << 6;
/// `ISR`: a misplaced START or STOP.
pub const ISR_BERR: u32 = 1 << 8;
/// `ISR`: another master took the bus.
pub const ISR_ARLO: u32 = 1 << 9;
/// `ISR`: the bus is in use.
pub const ISR_BUSY: u32 = 1 << 15;
/// Every flag `ICR` clears that this driver looks at.
pub const ICR_ALL: u32 = ISR_NACKF | ISR_STOPF | ISR_BERR | ISR_ARLO;

/// `TIMINGR` for standard mode, 100 kHz, from a 64 MHz kernel clock: a
/// prescaler of 16 makes a 250 ns tick, and SCL is 20 ticks low (5.0 µs)
/// and 16 high (4.0 µs), data held 2 ticks and set up 5, which meets I2C's
/// 4.7 µs low, 4.0 µs high and 250 ns set-up. The kernel puts the
/// controller on the 64 MHz HSI before it publishes the device, so this is
/// the one value there is.
pub const TIMING_100KHZ_AT_64MHZ: u32 = (15 << 28) | (4 << 20) | (2 << 16) | (0x0F << 8) | 0x13;

/// Polls one flag may take: some hundred microseconds of register reads,
/// against a byte that takes ninety.
pub const BYTE_BUDGET: Budget = Budget(200_000);

/// The controller.
#[derive(Debug)]
pub struct I2c<R: Registers> {
    registers: R,
}

impl<R: Registers> I2c<R> {
    /// Program the timing and turn the controller on.
    pub fn new(mut registers: R, timing: u32) -> Self {
        registers.write32(CR1, 0);
        registers.write32(TIMINGR, timing);
        registers.write32(ICR, ICR_ALL);
        registers.write32(CR1, CR1_PE);
        I2c { registers }
    }

    /// The registers, for a test to look at.
    pub const fn registers(&self) -> &R {
        &self.registers
    }

    /// Reset the controller's state machine, as `PE` going low does, after a
    /// transfer went wrong.
    fn recover(&mut self) {
        self.registers.write32(CR1, 0);
        // `PE` must stay low for three APB cycles; reading it back is more.
        for _ in 0..4 {
            let _ = self.registers.read32(CR1);
        }
        self.registers.write32(ICR, ICR_ALL);
        self.registers.write32(CR1, CR1_PE);
    }

    /// Wait for `flag`, failing at a NACK or a bus error.
    fn wait_for(&mut self, flag: u32) -> Result<(), BusError> {
        let mut status = 0;
        let seen = BYTE_BUDGET.wait(|| {
            status = self.registers.read32(ISR);
            status & (flag | ISR_NACKF | ISR_BERR | ISR_ARLO) != 0
        });
        if !seen {
            return Err(BusError::Timeout);
        }
        if status & ISR_NACKF != 0 {
            // A NACK makes the controller send a STOP of its own.
            let _ = BYTE_BUDGET.wait(|| self.registers.read32(ISR) & ISR_STOPF != 0);
            self.registers.write32(ICR, ISR_NACKF | ISR_STOPF);
            return Err(BusError::Nack);
        }
        if status & (ISR_BERR | ISR_ARLO) != 0 {
            return Err(BusError::Bus);
        }
        Ok(())
    }

    /// Program one transfer.
    fn begin(
        &mut self,
        address: u8,
        count: usize,
        read: bool,
        autoend: bool,
    ) -> Result<(), BusError> {
        let count = u32::try_from(count)
            .ok()
            .filter(|&count| (1..=255).contains(&count))
            .ok_or(BusError::Length)?;
        let mut cr2 = (u32::from(address & 0x7F) << 1) | (count << 16) | CR2_START;
        if read {
            cr2 |= CR2_RD_WRN;
        }
        if autoend {
            cr2 |= CR2_AUTOEND;
        }
        self.registers.write32(CR2, cr2);
        Ok(())
    }

    /// Wait for the STOP that ends an AUTOEND transfer, and clear it.
    fn end(&mut self) -> Result<(), BusError> {
        self.wait_for(ISR_STOPF)?;
        self.registers.write32(ICR, ISR_STOPF);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), BusError> {
        for &byte in bytes {
            self.wait_for(ISR_TXIS)?;
            self.registers.write32(TXDR, u32::from(byte));
        }
        Ok(())
    }

    fn transfer_write(&mut self, address: u8, bytes: &[u8]) -> Result<(), BusError> {
        if !BYTE_BUDGET.wait(|| self.registers.read32(ISR) & ISR_BUSY == 0) {
            return Err(BusError::Busy);
        }
        self.begin(address, bytes.len(), false, true)?;
        self.write_bytes(bytes)?;
        self.end()
    }

    fn transfer_write_read(
        &mut self,
        address: u8,
        out: &[u8],
        into: &mut [u8],
    ) -> Result<(), BusError> {
        if !BYTE_BUDGET.wait(|| self.registers.read32(ISR) & ISR_BUSY == 0) {
            return Err(BusError::Busy);
        }
        self.begin(address, out.len(), false, false)?;
        self.write_bytes(out)?;
        self.wait_for(ISR_TC)?;
        self.begin(address, into.len(), true, true)?;
        for slot in into.iter_mut() {
            self.wait_for(ISR_RXNE)?;
            *slot = (self.registers.read32(RXDR) & 0xFF) as u8;
        }
        self.end()
    }
}

impl<R: Registers> Bus for I2c<R> {
    fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), BusError> {
        let result = self.transfer_write(address, bytes);
        if matches!(
            result,
            Err(BusError::Timeout | BusError::Bus | BusError::Busy)
        ) {
            self.recover();
        }
        result
    }

    fn write_read(&mut self, address: u8, out: &[u8], into: &mut [u8]) -> Result<(), BusError> {
        let result = self.transfer_write_read(address, out, into);
        if matches!(
            result,
            Err(BusError::Timeout | BusError::Bus | BusError::Busy)
        ) {
            self.recover();
        }
        result
    }
}
