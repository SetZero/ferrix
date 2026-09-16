//! Where every field of the ring is, and what an entry holds.
//!
//! The offsets are constants rather than a `repr(C)` structure because the
//! ring is memory a stranger may also be writing: reading it as a structure
//! means casting bytes somebody else chose, and every field would then have to
//! be re-checked anyway. Reading it a field at a time, with the check at the
//! read, is shorter and needs no `unsafe`.

use crate::{Corruption, RingMemory};

/// The four bytes a net ring begins with.
pub const MAGIC: [u8; 4] = *b"FXNR";

/// The version this crate speaks.
pub const VERSION: u16 = 1;

/// How long the header is, and where the first array may start.
pub const HEADER_LEN: usize = 64;

/// How many bytes a submission takes.
pub const SUBMISSION_LEN: usize = 16;

/// How many bytes a completion takes.
pub const COMPLETION_LEN: usize = 16;

/// The fewest entries a ring may have.
pub const MIN_ENTRIES: u32 = 2;

/// The most.
///
/// Small enough that a [`crate::KernelSide`]'s bookkeeping -- a bit per slot
/// and a byte per slot -- is under three hundred bytes, so the value can live
/// on a kernel stack without an allocator. Two hundred and fifty-six slots of
/// the largest size is sixteen megabytes of data VMO, which is more ring than
/// any interface this will drive needs.
pub const MAX_ENTRIES: u32 = 256;

/// The smallest slot, which still holds a minimum Ethernet frame.
pub const MIN_SLOT_BYTES: u32 = 64;

/// The largest, which holds a jumbo frame and then some.
pub const MAX_SLOT_BYTES: u32 = 65_536;

/// A slot's size must be a multiple of this, so that a slot begins on a cache
/// line and two slots are never in one.
pub const SLOT_ALIGN: u32 = 64;

/// Offsets of the header's fields.
mod field {
    /// The magic.
    pub(super) const MAGIC: usize = 0;
    /// The version.
    pub(super) const VERSION: usize = 4;
    /// The flags, zero in version 1.
    pub(super) const FLAGS: usize = 6;
    /// How many entries each array has.
    pub(super) const ENTRIES: usize = 8;
    /// How many bytes a slot of the data VMO holds.
    pub(super) const SLOT_BYTES: usize = 12;
    /// Where the submissions start.
    pub(super) const SUB_OFFSET: usize = 16;
    /// Where the completions start.
    pub(super) const COMP_OFFSET: usize = 20;
    /// The submission tail, written by the kernel.
    pub(super) const SUB_TAIL: usize = 24;
    /// The submission head, written by the driver.
    pub(super) const SUB_HEAD: usize = 28;
    /// The completion tail, written by the driver.
    pub(super) const COMP_TAIL: usize = 32;
    /// The completion head, written by the kernel.
    pub(super) const COMP_HEAD: usize = 36;
    /// The driver's request to be rung when submissions arrive.
    pub(super) const SUB_WANT_BELL: usize = 40;
    /// The kernel's request to be rung when completions arrive.
    pub(super) const COMP_WANT_BELL: usize = 44;
}

/// What a submission asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// The slot holds a frame of `length` bytes; put it on the wire.
    Transmit,
    /// The slot is empty; fill it with the next frame that arrives.
    Receive,
}

impl Op {
    /// The byte this operation is written as.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Op::Transmit => 0,
            Op::Receive => 1,
        }
    }

    /// The operation a byte names, or `None` for one the protocol has not.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Op> {
        match code {
            0 => Some(Op::Transmit),
            1 => Some(Op::Receive),
            _ => None,
        }
    }
}

/// How a request ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// It was carried: a frame went out, or a frame arrived and its length is
    /// in the completion.
    Ok,
    /// The device refused it, or the link is down.
    Failed,
    /// The driver gave the slot back without carrying anything, which is what
    /// a reset does to what it held.
    Abandoned,
}

impl Status {
    /// The number this status is written as.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Status::Ok => 0,
            Status::Failed => -1,
            Status::Abandoned => -2,
        }
    }

    /// The status a number names, or `None` for one the protocol has not.
    #[must_use]
    pub const fn from_code(code: i32) -> Option<Status> {
        match code {
            0 => Some(Status::Ok),
            -1 => Some(Status::Failed),
            -2 => Some(Status::Abandoned),
            _ => None,
        }
    }
}

/// One entry of the submission ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Submission {
    /// Which slot of the data VMO it uses.
    pub slot: u32,
    /// How many bytes of that slot the frame takes, for a transmission; zero
    /// for a receive.
    pub length: u32,
    /// What to do with it.
    pub op: Op,
}

/// One entry of the completion ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completion {
    /// Which slot it answers.
    pub slot: u32,
    /// How many bytes the slot holds now: what was sent, or what arrived.
    pub length: u32,
    /// How it ended.
    pub status: Status,
}

/// Why a ring header could not be believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderError {
    /// The first four bytes are not [`MAGIC`].
    NotARing,
    /// The version is not one this crate speaks.
    Version(u16),
    /// A flag bit is set that version 1 does not define.
    Flags(u16),
    /// `entries` is not a power of two between [`MIN_ENTRIES`] and
    /// [`MAX_ENTRIES`].
    Entries(u32),
    /// `slot_bytes` is out of range or not a multiple of [`SLOT_ALIGN`].
    SlotBytes(u32),
    /// An array starts inside the header, is not eight-byte aligned, or runs
    /// past the ring.
    Arrays,
    /// The two arrays overlap.
    Overlap,
    /// The ring VMO is smaller than the header.
    TooSmall,
}

/// Where everything is, once a header has been believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RingLayout {
    /// How many entries each array has.
    entries: u32,
    /// How many bytes a slot holds.
    slot_bytes: u32,
    /// Where the submissions start.
    sub_offset: usize,
    /// Where the completions start.
    comp_offset: usize,
}

impl RingLayout {
    /// Write a header for `entries` entries of `slot_bytes` bytes, with the
    /// arrays after it, and answer the layout it describes.
    ///
    /// This is what a driver does to a fresh ring VMO before HELLO. The
    /// kernel never calls it: it only ever [`RingLayout::read`]s.
    ///
    /// # Errors
    ///
    /// The same refusals [`RingLayout::read`] makes, so a driver cannot
    /// publish a ring the kernel will refuse.
    pub fn write<M: RingMemory>(
        memory: &mut M,
        size: usize,
        entries: u32,
        slot_bytes: u32,
    ) -> Result<RingLayout, HeaderError> {
        let layout = RingLayout::describe(entries, slot_bytes, size)?;
        for (at, byte) in MAGIC.iter().enumerate() {
            memory.write_u8(field::MAGIC + at, *byte);
        }
        memory.write_u16(field::VERSION, VERSION);
        memory.write_u16(field::FLAGS, 0);
        memory.write_u32(field::ENTRIES, entries);
        memory.write_u32(field::SLOT_BYTES, slot_bytes);
        memory.write_u32(field::SUB_OFFSET, layout.sub_offset as u32);
        memory.write_u32(field::COMP_OFFSET, layout.comp_offset as u32);
        for at in [
            field::SUB_TAIL,
            field::SUB_HEAD,
            field::COMP_TAIL,
            field::COMP_HEAD,
            field::SUB_WANT_BELL,
            field::COMP_WANT_BELL,
        ] {
            memory.write_u32(at, 0);
        }
        for at in (48..HEADER_LEN).step_by(4) {
            memory.write_u32(at, 0);
        }
        Ok(layout)
    }

    /// The layout `entries` entries of `slot_bytes` bytes need in `size`
    /// bytes, with the arrays laid out one after the other behind the header.
    fn describe(entries: u32, slot_bytes: u32, size: usize) -> Result<RingLayout, HeaderError> {
        if size < HEADER_LEN {
            return Err(HeaderError::TooSmall);
        }
        if !(MIN_ENTRIES..=MAX_ENTRIES).contains(&entries) || !entries.is_power_of_two() {
            return Err(HeaderError::Entries(entries));
        }
        if !(MIN_SLOT_BYTES..=MAX_SLOT_BYTES).contains(&slot_bytes)
            || !slot_bytes.is_multiple_of(SLOT_ALIGN)
        {
            return Err(HeaderError::SlotBytes(slot_bytes));
        }
        let count = entries as usize;
        let sub_offset = HEADER_LEN;
        let sub_len = count
            .checked_mul(SUBMISSION_LEN)
            .ok_or(HeaderError::Arrays)?;
        let comp_offset = sub_offset.checked_add(sub_len).ok_or(HeaderError::Arrays)?;
        let comp_len = count
            .checked_mul(COMPLETION_LEN)
            .ok_or(HeaderError::Arrays)?;
        let end = comp_offset
            .checked_add(comp_len)
            .ok_or(HeaderError::Arrays)?;
        if end > size {
            return Err(HeaderError::Arrays);
        }
        Ok(RingLayout {
            entries,
            slot_bytes,
            sub_offset,
            comp_offset,
        })
    }

    /// Read a header the other side wrote, believing nothing.
    ///
    /// # Errors
    ///
    /// [`HeaderError`], naming the field that is impossible.
    pub fn read<M: RingMemory>(memory: &M, size: usize) -> Result<RingLayout, HeaderError> {
        if size < HEADER_LEN {
            return Err(HeaderError::TooSmall);
        }
        for (at, byte) in MAGIC.iter().enumerate() {
            if memory.read_u8(field::MAGIC + at) != *byte {
                return Err(HeaderError::NotARing);
            }
        }
        let version = memory.read_u16(field::VERSION);
        if version != VERSION {
            return Err(HeaderError::Version(version));
        }
        let flags = memory.read_u16(field::FLAGS);
        if flags != 0 {
            return Err(HeaderError::Flags(flags));
        }
        let entries = memory.read_u32(field::ENTRIES);
        let slot_bytes = memory.read_u32(field::SLOT_BYTES);
        let expected = RingLayout::describe(entries, slot_bytes, size)?;
        if memory.read_u32(field::SUB_OFFSET) as usize != expected.sub_offset
            || memory.read_u32(field::COMP_OFFSET) as usize != expected.comp_offset
        {
            // The arrays go where this crate puts them. Letting a peer choose
            // would mean checking two ranges for overlap with each other, with
            // the header and with the ring's end, on every attach; fixing them
            // makes the check one comparison and takes nothing away, since
            // both sides link this crate.
            return Err(HeaderError::Overlap);
        }
        Ok(expected)
    }

    /// How many entries each array has.
    #[must_use]
    pub const fn entries(&self) -> u32 {
        self.entries
    }

    /// How many bytes a slot holds.
    #[must_use]
    pub const fn slot_bytes(&self) -> u32 {
        self.slot_bytes
    }

    /// Where slot `slot` begins in the data VMO.
    ///
    /// # Errors
    ///
    /// [`Corruption::SlotOutOfRange`] for a slot the ring does not have.
    pub fn slot_offset(&self, slot: u32) -> Result<usize, Corruption> {
        if slot >= self.entries {
            return Err(Corruption::SlotOutOfRange);
        }
        Ok(slot as usize * self.slot_bytes as usize)
    }

    /// How many bytes the data VMO must hold.
    #[must_use]
    pub const fn data_bytes(&self) -> usize {
        self.entries as usize * self.slot_bytes as usize
    }

    /// Where the header's submission tail is.
    pub(crate) const fn sub_tail(&self) -> usize {
        field::SUB_TAIL
    }

    /// Where the header's submission head is.
    pub(crate) const fn sub_head(&self) -> usize {
        field::SUB_HEAD
    }

    /// Where the header's completion tail is.
    pub(crate) const fn comp_tail(&self) -> usize {
        field::COMP_TAIL
    }

    /// Where the header's completion head is.
    pub(crate) const fn comp_head(&self) -> usize {
        field::COMP_HEAD
    }

    /// Where the driver's want-bell flag is.
    pub(crate) const fn sub_want_bell(&self) -> usize {
        field::SUB_WANT_BELL
    }

    /// Where the kernel's want-bell flag is.
    pub(crate) const fn comp_want_bell(&self) -> usize {
        field::COMP_WANT_BELL
    }

    /// Where the submission for a free-running index is.
    pub(crate) fn submission_at(&self, index: u32) -> usize {
        let slot = (index & (self.entries - 1)) as usize;
        self.sub_offset + slot * SUBMISSION_LEN
    }

    /// Where the completion for a free-running index is.
    pub(crate) fn completion_at(&self, index: u32) -> usize {
        let slot = (index & (self.entries - 1)) as usize;
        self.comp_offset + slot * COMPLETION_LEN
    }

    /// Write a submission at a free-running index.
    pub(crate) fn put_submission<M: RingMemory>(
        &self,
        memory: &mut M,
        index: u32,
        entry: Submission,
    ) {
        let at = self.submission_at(index);
        memory.write_u32(at, entry.slot);
        memory.write_u32(at + 4, entry.length);
        memory.write_u8(at + 8, entry.op.code());
        memory.write_u8(at + 9, 0);
        memory.write_u16(at + 10, 0);
        memory.write_u32(at + 12, 0);
    }

    /// Read a submission at a free-running index, believing nothing.
    pub(crate) fn get_submission<M: RingMemory>(
        &self,
        memory: &M,
        index: u32,
    ) -> Result<Submission, Corruption> {
        let at = self.submission_at(index);
        let slot = memory.read_u32(at);
        let length = memory.read_u32(at + 4);
        let op = Op::from_code(memory.read_u8(at + 8)).ok_or(Corruption::UnknownOp)?;
        if slot >= self.entries {
            return Err(Corruption::SlotOutOfRange);
        }
        if length > self.slot_bytes {
            return Err(Corruption::LengthTooLarge);
        }
        Ok(Submission { slot, length, op })
    }

    /// Write a completion at a free-running index.
    pub(crate) fn put_completion<M: RingMemory>(
        &self,
        memory: &mut M,
        index: u32,
        entry: Completion,
    ) {
        let at = self.completion_at(index);
        memory.write_u32(at, entry.slot);
        memory.write_u32(at + 4, entry.length);
        memory.write_u32(at + 8, entry.status.code().cast_unsigned());
        memory.write_u32(at + 12, 0);
    }

    /// Read a completion at a free-running index, believing nothing.
    pub(crate) fn get_completion<M: RingMemory>(
        &self,
        memory: &M,
        index: u32,
    ) -> Result<Completion, Corruption> {
        let at = self.completion_at(index);
        let slot = memory.read_u32(at);
        let length = memory.read_u32(at + 4);
        let status = Status::from_code(memory.read_u32(at + 8).cast_signed())
            .ok_or(Corruption::UnknownStatus)?;
        if slot >= self.entries {
            return Err(Corruption::SlotOutOfRange);
        }
        if length > self.slot_bytes {
            return Err(Corruption::LengthTooLarge);
        }
        Ok(Completion {
            slot,
            length,
            status,
        })
    }
}
