//! The Enhanced Configuration Access Mechanism: configuration space as memory.
//!
//! PCI Express puts every function's 4 KiB of configuration space at a fixed
//! place in one physical window per segment, a megabyte per bus:
//!
//! ```text
//! offset = (bus - first_bus) << 20 | device << 15 | function << 12 | register
//! ```
//!
//! Firmware says where the window is and which buses it covers — the MCFG
//! table on an ACPI machine, `reg` and `bus-range` on a
//! `pci-host-ecam-generic` device tree node. [`Window`] is that description
//! and the arithmetic on it. [`Ecam`] turns a window and something that can
//! read and write bytes at an offset into a [`ConfigSpace`], answering for
//! everything outside the window the way hardware answers for a function that
//! is not there, so the kernel's side is four lines of volatile access.

use core::ops::RangeInclusive;

use crate::{Address, CONFIG_SPACE_SIZE, ConfigSpace};

/// Bytes of window each bus occupies.
pub const BYTES_PER_BUS: u64 = 1 << 20;

/// One segment's ECAM window, without its base address.
///
/// The base is the caller's business: it is a physical address in firmware's
/// description and a virtual one once the kernel has mapped it, and this type
/// is the same arithmetic either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    /// The segment group the window serves.
    segment: u16,
    /// The bus at offset zero.
    first_bus: u8,
    /// The last bus the window covers, inclusive.
    last_bus: u8,
}

impl Window {
    /// The window for `segment`, covering `first_bus..=last_bus`, or `None` if
    /// the range is empty.
    #[must_use]
    pub const fn new(segment: u16, first_bus: u8, last_bus: u8) -> Option<Self> {
        if first_bus > last_bus {
            return None;
        }
        Some(Window {
            segment,
            first_bus,
            last_bus,
        })
    }

    /// The segment group.
    #[must_use]
    pub const fn segment(self) -> u16 {
        self.segment
    }

    /// The buses the window covers.
    #[must_use]
    pub const fn buses(self) -> RangeInclusive<u8> {
        self.first_bus..=self.last_bus
    }

    /// How many bytes the window is: a megabyte per bus.
    #[must_use]
    pub const fn len(self) -> u64 {
        // At most 256 buses, so at most 256 MiB: no overflow is possible.
        (self.last_bus as u64 - self.first_bus as u64 + 1) * BYTES_PER_BUS
    }

    /// Whether the window is empty, which a constructed one never is.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// The offset into the window of `width` bytes at `register` in
    /// `function`'s space, or `None` if the function is on another segment or
    /// bus, or the access runs past the end of its space.
    #[must_use]
    pub const fn offset(self, function: Address, register: u16, width: u16) -> Option<u64> {
        if function.segment() != self.segment
            || function.bus() < self.first_bus
            || function.bus() > self.last_bus
        {
            return None;
        }
        // Both are below 4096 when they are summed, so the sum cannot wrap.
        if register >= CONFIG_SPACE_SIZE || width > CONFIG_SPACE_SIZE - register {
            return None;
        }
        let bus = (function.bus() - self.first_bus) as u64;
        Some(
            bus << 20
                | (function.device() as u64) << 15
                | (function.function() as u64) << 12
                | register as u64,
        )
    }
}

/// Byte-addressed access to a mapped ECAM window.
///
/// The offsets passed are always inside the window, and always leave room for
/// the width read or written, so an implementation over a mapping of
/// [`Window::len`] bytes needs no bounds check of its own.
pub trait Registers {
    /// Read a byte at `offset`.
    fn read8(&self, offset: u64) -> u8;
    /// Read a little-endian `u16` at `offset`.
    fn read16(&self, offset: u64) -> u16;
    /// Read a little-endian `u32` at `offset`.
    fn read32(&self, offset: u64) -> u32;
    /// Write a `u16` at `offset`.
    fn write16(&mut self, offset: u64, value: u16);
    /// Write a `u32` at `offset`.
    fn write32(&mut self, offset: u64, value: u32);
}

/// Configuration space through an ECAM window.
#[derive(Debug)]
pub struct Ecam<R> {
    /// Which functions the window reaches.
    window: Window,
    /// The window's bytes.
    registers: R,
}

impl<R: Registers> Ecam<R> {
    /// Configuration space for the functions `window` covers, read through
    /// `registers`.
    pub const fn new(window: Window, registers: R) -> Self {
        Ecam { window, registers }
    }

    /// The window.
    pub const fn window(&self) -> Window {
        self.window
    }
}

impl<R: Registers> ConfigSpace for Ecam<R> {
    fn read8(&self, function: Address, offset: u16) -> u8 {
        self.window
            .offset(function, offset, 1)
            .map_or(u8::MAX, |at| self.registers.read8(at))
    }

    fn read16(&self, function: Address, offset: u16) -> u16 {
        self.window
            .offset(function, offset, 2)
            .map_or(u16::MAX, |at| self.registers.read16(at))
    }

    fn read32(&self, function: Address, offset: u16) -> u32 {
        self.window
            .offset(function, offset, 4)
            .map_or(u32::MAX, |at| self.registers.read32(at))
    }

    fn write16(&mut self, function: Address, offset: u16, value: u16) {
        if let Some(at) = self.window.offset(function, offset, 2) {
            self.registers.write16(at, value);
        }
    }

    fn write32(&mut self, function: Address, offset: u16, value: u32) {
        if let Some(at) = self.window.offset(function, offset, 4) {
            self.registers.write32(at, value);
        }
    }
}
