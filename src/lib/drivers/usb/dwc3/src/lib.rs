//! A Synopsys `DesignWare` USB 3 controller -- the DWC3, a `DWC_usb31` on the
//! Pixel 7's gs201 -- in device mode, as logic over registers and DMA
//! memory.
//!
//! `src/user/system/native/drivers/usb/usbdev` is to run this in a ring-3 process under devmgr, as
//! `src/user/system/native/drivers/usb/usbhid` runs `ferrix-usb-host` for the DK boards
//! (`docs/vendor/google/pixel7/USB-HANDOVER.md`, phase 3). The process holds an
//! `IoMapping` of the controller's 64 KiB window at `0x1121_0000`, its
//! interrupt (SPI 379), and pinned memory for what the controller reads
//! and writes. None of those exist in a unit test, so everything that
//! decides a register value, a TRB or an answer is here, written against
//! [`Registers`], [`Dma`] and [`Clock`], and tested against a model of the
//! controller and of a Linux host enumerating it (`src/tests/model.rs`):
//!
//! * [`controller`]: bring-up -- the core's soft reset, the quirks the
//!   phone's device tree names, the event buffer, endpoint 0 -- and the
//!   device events: reset, connect done, disconnect;
//! * [`event`]: the event buffer's entries, decoded;
//! * [`trb`]: the transfer request blocks the controller walks;
//! * [`layout`]: where each of those lives in the DMA area;
//! * endpoint 0's three-stage control transfers, answered by a
//!   [`ferrix_usb_device::Function`], and the bulk and interrupt
//!   endpoints' data, in two private modules.
//!
//! The register offsets, bits and command codes are the ones Linux's
//! driver uses (`drivers/usb/dwc3/core.h`, `gadget.h`, and the sequences
//! in `core.c`, `gadget.c` and `ep0.c`), read for their facts; this crate
//! shares no code with it.
//!
//! # The function
//!
//! The controller carries one function, and knows it only as a
//! [`Function`](ferrix_usb_device::Function): what to answer a SETUP with,
//! what an OUT data stage was worth, which endpoints a configuration
//! enables. That is a trait of `ferrix-usb-device`'s, and this crate
//! depends on that crate for it and for nothing else, so the controller
//! can carry the CDC-ACM port the phone needs today and another function
//! later without either knowing the other. The function is passed to each
//! call that can need it rather than owned, so the program keeps it, and
//! reads DTR from it, between interrupts.
//!
//! # Speed
//!
//! High speed at most: `DCFG` holds the core at USB 2.0, which keeps the
//! combo super-speed PHY out of the bring-up. The phone's tree says
//! `maximum-speed = "super-speed-plus"`; super speed can come later.
//!
//! # Memory the controller does not see through the caches
//!
//! The phone's tree gives the DWC3 no `dma-coherent`: its reads and writes
//! go to memory, not through the processor's caches. So every hand-off is
//! explicit in [`Dma`]: [`Dma::clean`] after the processor fills a TRB, a
//! data buffer or the event buffer's space and before the register write
//! that gives it to the controller, and [`Dma::invalidate`] after the
//! controller says it wrote something and before the processor reads it.
//! The model records a violation for either one missing; QEMU has no DWC3
//! and would hide both anyway.
//!
//! # Waiting
//!
//! Every wait is bounded through [`Clock`] and ends in an [`Error`]: the
//! core's soft reset, run and stop, and each endpoint command. Nothing
//! else waits: transfers finish in the event buffer, and
//! [`controller::Controller::on_interrupt`] reads what the controller
//! wrote there.

#![no_std]
#![forbid(unsafe_code)]

pub mod controller;
mod endpoint;
mod ep0;
pub mod event;
pub mod layout;
mod regs;
pub mod trb;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

pub use controller::{Controller, Notice};
pub use ferrix_usb_device as usb_device;

/// A window of 32-bit device registers, by byte offset from the core's
/// base: the globals at `0xC100`, the device registers at `0xC700`, the
/// endpoint command registers at `0xC800`.
pub trait Registers {
    /// Read the register at `offset`.
    fn read32(&self, offset: u32) -> u32;
    /// Write the register at `offset`.
    fn write32(&mut self, offset: u32, value: u32);
}

/// The memory the controller reads and writes: [`layout::AREA_BYTES`]
/// bytes, a page at a time wherever each page is.
///
/// The controller is not coherent with the processor's view of this
/// memory, so the two hand-offs are calls of their own. The ranges given
/// are exact; an implementation widens them to whatever granule its
/// maintenance works in (the layout keeps everything the two sides write
/// in [`layout::CACHE_LINE`]-aligned pieces of its own).
pub trait Dma {
    /// How many bytes there are.
    fn len(&self) -> usize;
    /// Whether there are none.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Read the aligned word at `offset`.
    fn read32(&self, offset: usize) -> u32;
    /// Write the aligned word at `offset`.
    fn write32(&mut self, offset: usize, value: u32);
    /// Read the byte at `offset`.
    fn read8(&self, offset: usize) -> u8;
    /// Write the byte at `offset`.
    fn write8(&mut self, offset: usize, value: u8);
    /// Read `bytes.len()` bytes from `offset`.
    fn read_bytes(&self, offset: usize, bytes: &mut [u8]) {
        for (at, byte) in (offset..).zip(bytes.iter_mut()) {
            *byte = self.read8(at);
        }
    }
    /// Write `bytes` at `offset`.
    fn write_bytes(&mut self, offset: usize, bytes: &[u8]) {
        for (at, &byte) in (offset..).zip(bytes) {
            self.write8(at, byte);
        }
    }
    /// Make the processor's writes to `len` bytes at `offset` visible to
    /// the controller before any later register write. On cached memory
    /// that is a clean to the point of coherency and a `dsb`; on memory
    /// mapped uncached it is the `dsb` alone, since Arm does not order a
    /// Normal-memory write before a later Device-memory write without one.
    fn clean(&mut self, offset: usize, len: usize);
    /// Make the controller's writes to `len` bytes at `offset` visible to
    /// the processor's later reads. On cached memory that is an invalidate
    /// by address, after a `dsb`; on memory mapped uncached, the barrier
    /// that orders the register read which said the writes were done
    /// before the reads of memory that follow.
    fn invalidate(&mut self, offset: usize, len: usize);
    /// The address the controller reaches the byte at `offset` by. The
    /// DWC3's pointers are 64 bits; `None` for a byte it cannot reach, as a
    /// stage-2 MPU in front of it may decide.
    fn device_address(&self, offset: usize) -> Option<u64>;
}

/// Time, for the waits a controller needs.
pub trait Clock {
    /// Monotonic nanoseconds.
    fn now_nanos(&self) -> u64;
    /// Wait at least `nanos`.
    fn sleep_nanos(&mut self, nanos: u64);
}

/// What the driver is built from.
#[derive(Debug)]
pub struct Parts<R, D, C> {
    /// The controller's registers.
    pub registers: R,
    /// The memory it reads and writes.
    pub memory: D,
    /// The time.
    pub clock: C,
}

/// Why the controller would not come up, or stopped answering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The area is smaller than [`layout::AREA_BYTES`], or a page of it has
    /// no address the controller reaches, or one not page-aligned.
    Memory,
    /// `GSNPSID` names no DWC3: the value read.
    NotDwc3(u32),
    /// The controller is not as a boot loader that ran fastboot on it and
    /// stopped leaves it -- in device mode, run bit clear, halted -- so
    /// nothing was written: the register, by offset, and what it read.
    Refused {
        /// The register's offset.
        register: u32,
        /// What it read.
        value: u32,
    },
    /// The core's soft reset did not finish.
    Reset,
    /// The controller did not start or halt when told to.
    RunStop,
    /// An endpoint command did not finish, or failed.
    Command {
        /// The physical endpoint: the number times two, plus one for IN.
        physical: u8,
        /// The command's code.
        command: u8,
        /// The status it ended with, or `None` if it never ended.
        status: Option<u8>,
    },
    /// `GEVNTCOUNT` counted more than the event buffer holds.
    Events(u32),
    /// The function listed more endpoints than there are places for.
    Endpoints,
}

/// A microsecond, in [`Clock`]'s units.
pub const MICROSECOND: u64 = 1_000;
/// A millisecond.
pub const MILLISECOND: u64 = 1_000_000;

/// Wait until `condition` holds, looking every `step` nanoseconds for at
/// most `limit`: whether it held.
fn wait_for<C: Clock>(
    clock: &mut C,
    limit: u64,
    step: u64,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let until = clock.now_nanos().saturating_add(limit);
    loop {
        if condition() {
            return true;
        }
        if clock.now_nanos() >= until {
            return condition();
        }
        clock.sleep_nanos(step);
    }
}
