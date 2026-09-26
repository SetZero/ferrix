//! Transfer request blocks: the sixteen bytes that describe a buffer to the
//! controller (`struct dwc3_trb` in Linux's `core.h`).
//!
//! Word 0 and 1 are the buffer's address, low and high; word 2 is its
//! length, which the controller rewrites as what it did not move, and a
//! status; word 3 is the control word, whose HWO bit says the controller
//! owns the TRB. The controller clears HWO when it is done with a TRB and
//! writes the transfer's event after.

use crate::Dma;

/// A TRB's size in memory, and its alignment.
pub const TRB_BYTES: usize = 16;

/// Word 2: the buffer's length, or what is left of it.
pub const SIZE_MASK: u32 = 0x00FF_FFFF;
/// Word 2: the status field's shift.
pub const STATUS_SHIFT: u32 = 28;
/// A TRB's status: a SETUP arrived before this control TRB could run.
pub const STATUS_SETUP_PENDING: u32 = 2;

/// The controller owns it.
pub const HWO: u32 = 1 << 0;
/// The last TRB of a transfer.
pub const LST: u32 = 1 << 1;
/// Chained to the next TRB: one packet stream across both buffers.
pub const CHN: u32 = 1 << 2;
/// Continue on short packet.
pub const CSP: u32 = 1 << 3;
/// The TRB control field's shift.
pub const TRBCTL_SHIFT: u32 = 4;
/// The TRB control field.
pub const TRBCTL_MASK: u32 = 0x3F << TRBCTL_SHIFT;
/// An OUT TRB ends at a short packet; an IN TRB's missed interval is
/// reported.
pub const ISP_IMI: u32 = 1 << 10;
/// Write the transfer's event when this TRB completes.
pub const IOC: u32 = 1 << 11;

/// A normal TRB: bulk and interrupt data.
pub const NORMAL: u32 = 1 << TRBCTL_SHIFT;
/// Control: the SETUP packet.
pub const CONTROL_SETUP: u32 = 2 << TRBCTL_SHIFT;
/// Control: the status stage of a transfer with no data stage.
pub const CONTROL_STATUS2: u32 = 3 << TRBCTL_SHIFT;
/// Control: the status stage of a transfer with one.
pub const CONTROL_STATUS3: u32 = 4 << TRBCTL_SHIFT;
/// Control: the data stage.
pub const CONTROL_DATA: u32 = 5 << TRBCTL_SHIFT;

/// A TRB, as the processor fills it or reads it back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Trb {
    /// The buffer's address, as the controller reaches it.
    pub buffer: u64,
    /// Word 2: length, and status once done.
    pub size: u32,
    /// Word 3: the control word.
    pub control: u32,
}

impl Trb {
    /// A TRB for `length` bytes at `buffer`, of type `kind` with `flags`,
    /// given to the controller.
    #[must_use]
    pub const fn new(buffer: u64, length: usize, kind: u32, flags: u32) -> Self {
        Trb {
            buffer,
            size: length as u32 & SIZE_MASK,
            control: kind | flags | HWO,
        }
    }

    /// The bytes it did not move.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        (self.size & SIZE_MASK) as usize
    }

    /// Its status.
    #[must_use]
    pub const fn status(&self) -> u32 {
        self.size >> STATUS_SHIFT
    }

    /// Whether the controller still owns it.
    #[must_use]
    pub const fn is_owned(&self) -> bool {
        self.control & HWO != 0
    }

    /// Write it at `offset`, the control word last.
    pub fn write<D: Dma>(&self, memory: &mut D, offset: usize) {
        memory.write32(offset, self.buffer as u32);
        memory.write32(offset + 4, (self.buffer >> 32) as u32);
        memory.write32(offset + 8, self.size);
        memory.write32(offset + 12, self.control);
    }

    /// Read the one at `offset`.
    #[must_use]
    pub fn read<D: Dma>(memory: &D, offset: usize) -> Self {
        let low = u64::from(memory.read32(offset));
        let high = u64::from(memory.read32(offset + 4));
        Trb {
            buffer: low | (high << 32),
            size: memory.read32(offset + 8),
            control: memory.read32(offset + 12),
        }
    }
}
