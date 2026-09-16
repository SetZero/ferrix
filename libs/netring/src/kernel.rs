//! The kernel's end of the ring: it submits and it drains.
//!
//! The kernel owns the slots. It takes a free one, puts a frame in it and
//! submits it, or submits it empty for the driver to fill; the driver answers
//! and the slot is free again. Nothing else allocates, so the one rule that
//! has to hold is that a slot is never submitted twice without a completion in
//! between, and [`KernelSide`] is what holds it.

use crate::bell::{BELL_SUBMIT, Doorbell, Wait};
use crate::layout::{Op, RingLayout, Status, Submission};
use crate::ring::{Consumer, Producer};
use crate::{Corruption, RingMemory};

/// How many `u64`s a bitmap of [`crate::layout::MAX_ENTRIES`] slots needs.
const BITMAP_WORDS: usize = (crate::layout::MAX_ENTRIES as usize).div_ceil(64);

/// Why the kernel would not take up a ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttachError {
    /// The header could not be believed.
    Header(crate::layout::HeaderError),
    /// The data VMO is smaller than `entries` slots.
    DataTooSmall,
}

/// Why a submission was not made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubmitError {
    /// Every slot is in the driver's hands.
    Full,
    /// The frame is longer than a slot holds.
    TooLong,
    /// That slot is already outstanding.
    SlotBusy,
    /// The ring said something impossible; this side is finished.
    Corrupt(Corruption),
}

/// One request the driver answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completed {
    /// Which slot, which is free again the moment this is read.
    pub slot: u32,
    /// What the slot was submitted for.
    pub op: Op,
    /// How many bytes it holds now.
    pub length: u32,
    /// How it ended.
    pub status: Status,
}

/// What one drain took.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Drain {
    /// How many completions were written into the caller's buffer.
    pub taken: usize,
    /// Whether more are waiting than the buffer held.
    pub more: bool,
}

/// The kernel's end.
#[derive(Debug)]
pub struct KernelSide {
    /// Where everything is.
    layout: RingLayout,
    /// This side's end of the submission ring.
    submissions: Producer,
    /// This side's end of the completion ring.
    completions: Consumer,
    /// Which slots the driver holds, one bit each.
    outstanding: [u64; BITMAP_WORDS],
    /// What each outstanding slot was submitted for.
    ///
    /// Both of these are arrays of the largest ring rather than of the ring
    /// attached, which is only affordable because [`crate::layout::MAX_ENTRIES`]
    /// is small: two hundred and fifty-six slots make this two hundred and
    /// eighty-eight bytes, and this value is built on a kernel stack. An
    /// earlier version allowed four thousand slots and overflowed that stack
    /// into a double fault.
    ops: [u8; crate::layout::MAX_ENTRIES as usize],
    /// How many slots are outstanding.
    held: u32,
    /// The corruption this side latched, if it saw one.
    corrupt: Option<Corruption>,
}

impl KernelSide {
    /// Take up a ring the driver wrote, believing nothing in it.
    ///
    /// # Errors
    ///
    /// [`AttachError`], naming what was impossible.
    pub fn attach<M: RingMemory>(
        memory: &M,
        ring_bytes: usize,
        data_bytes: usize,
    ) -> Result<KernelSide, AttachError> {
        let layout = RingLayout::read(memory, ring_bytes).map_err(AttachError::Header)?;
        if data_bytes < layout.data_bytes() {
            return Err(AttachError::DataTooSmall);
        }
        Ok(KernelSide {
            layout,
            submissions: Producer::default(),
            completions: Consumer::default(),
            outstanding: [0; BITMAP_WORDS],
            ops: [0; crate::layout::MAX_ENTRIES as usize],
            held: 0,
            corrupt: None,
        })
    }

    /// Where everything is.
    #[must_use]
    pub const fn layout(&self) -> &RingLayout {
        &self.layout
    }

    /// The corruption this side saw, if it saw one.
    #[must_use]
    pub const fn corruption(&self) -> Option<Corruption> {
        self.corrupt
    }

    /// How many slots the driver holds.
    #[must_use]
    pub const fn outstanding(&self) -> u32 {
        self.held
    }

    /// A slot nobody is using, or `None` when the driver holds them all.
    #[must_use]
    pub fn free_slot(&self) -> Option<u32> {
        (0..self.layout.entries()).find(|slot| !self.is_outstanding(*slot))
    }

    /// Whether the driver holds that slot.
    #[must_use]
    pub fn is_outstanding(&self, slot: u32) -> bool {
        let (word, bit) = (slot as usize / 64, slot as usize % 64);
        self.outstanding
            .get(word)
            .is_some_and(|bits| bits & (1 << bit) != 0)
    }

    /// Mark a slot as held, or not.
    fn set_outstanding(&mut self, slot: u32, held: bool) {
        let (word, bit) = (slot as usize / 64, slot as usize % 64);
        if let Some(bits) = self.outstanding.get_mut(word) {
            if held {
                *bits |= 1 << bit;
            } else {
                *bits &= !(1 << bit);
            }
        }
    }

    /// Stage a submission. It is not visible to the driver until
    /// [`KernelSide::publish`].
    ///
    /// # Errors
    ///
    /// [`SubmitError`], naming why not.
    pub fn submit<M: RingMemory>(
        &mut self,
        memory: &mut M,
        slot: u32,
        op: Op,
        length: u32,
    ) -> Result<(), SubmitError> {
        if let Some(corruption) = self.corrupt {
            return Err(SubmitError::Corrupt(corruption));
        }
        if slot >= self.layout.entries() {
            return Err(SubmitError::SlotBusy);
        }
        if self.is_outstanding(slot) {
            return Err(SubmitError::SlotBusy);
        }
        if length > self.layout.slot_bytes() {
            return Err(SubmitError::TooLong);
        }
        if self.held >= self.layout.entries() {
            return Err(SubmitError::Full);
        }
        let entry = Submission { slot, length, op };
        self.layout
            .put_submission(memory, self.submissions.tail(), entry);
        self.submissions.stage();
        self.set_outstanding(slot, true);
        if let Some(recorded) = self.ops.get_mut(slot as usize) {
            *recorded = op.code();
        }
        self.held += 1;
        Ok(())
    }

    /// Publish everything staged, and answer the bell to ring if the driver
    /// asked for one.
    pub fn publish<M: RingMemory>(&mut self, memory: &mut M) -> Option<Doorbell> {
        self.submissions.publish(
            memory,
            self.layout.sub_tail(),
            self.layout.sub_want_bell(),
            BELL_SUBMIT,
        )
    }

    /// Take what the driver has answered.
    ///
    /// # Errors
    ///
    /// [`Corruption`], which is terminal for this side.
    pub fn drain<M: RingMemory>(
        &mut self,
        memory: &mut M,
        out: &mut [Completed],
    ) -> Result<Drain, Corruption> {
        if let Some(corruption) = self.corrupt {
            return Err(corruption);
        }
        let entries = self.layout.entries();
        let pending = self
            .completions
            .pending(memory, self.layout.comp_tail(), entries)
            .inspect_err(|error| self.corrupt = Some(*error))?;
        let mut taken = 0;
        while taken < out.len() && (taken as u32) < pending {
            let index = self.completions.head();
            let entry = self
                .layout
                .get_completion(memory, index)
                .inspect_err(|error| self.corrupt = Some(*error))?;
            if !self.is_outstanding(entry.slot) {
                self.corrupt = Some(Corruption::UnknownSlot);
                return Err(Corruption::UnknownSlot);
            }
            let op = self
                .ops
                .get(entry.slot as usize)
                .and_then(|code| Op::from_code(*code))
                .ok_or(Corruption::UnknownOp)
                .inspect_err(|error| self.corrupt = Some(*error))?;
            self.set_outstanding(entry.slot, false);
            self.held = self.held.saturating_sub(1);
            if let Some(slot) = out.get_mut(taken) {
                *slot = Completed {
                    slot: entry.slot,
                    op,
                    length: entry.length,
                    status: entry.status,
                };
            }
            self.completions.advance(memory, self.layout.comp_head());
            taken += 1;
        }
        Ok(Drain {
            taken,
            more: (taken as u32) < pending,
        })
    }

    /// Ask to be rung when completions arrive, and look once more before
    /// sleeping.
    ///
    /// # Errors
    ///
    /// [`Corruption`], which is terminal for this side.
    pub fn prepare_to_sleep<M: RingMemory>(&mut self, memory: &mut M) -> Result<Wait, Corruption> {
        if let Some(corruption) = self.corrupt {
            return Err(corruption);
        }
        self.completions
            .prepare_to_sleep(
                memory,
                self.layout.comp_want_bell(),
                self.layout.comp_tail(),
                self.layout.entries(),
            )
            .inspect_err(|error| self.corrupt = Some(*error))
    }

    /// Stop asking to be rung, which is the first thing to do on waking.
    pub fn woke<M: RingMemory>(&mut self, memory: &mut M) {
        self.completions.woke(memory, self.layout.comp_want_bell());
    }

    /// Give up every outstanding slot, which is what the end of a ring does.
    ///
    /// Answers how many there were. Their memory is not free to reuse until
    /// the device has been reset, which is the glue's business and
    /// `docs/NET-RING.md` §7's rule.
    pub fn abandon(&mut self) -> u32 {
        let held = self.held;
        self.outstanding = [0; BITMAP_WORDS];
        self.held = 0;
        held
    }
}
