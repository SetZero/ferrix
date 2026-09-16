//! The driver's end of the ring: it takes submissions and it completes them.
//!
//! The driver never chooses a slot and never frees one. It reads what the
//! kernel submitted, does it, and answers. That is the whole of its side, and
//! it is deliberately the whole: a driver is the untrusted half, and the less
//! of the protocol it decides the less there is for a broken one to get wrong.

use crate::bell::{BELL_COMPLETE, Doorbell, Wait};
use crate::layout::{Completion, HeaderError, RingLayout, Status, Submission};
use crate::ring::{Consumer, Producer};
use crate::{Corruption, RingMemory};

/// Why a completion was not made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriverError {
    /// The completion ring is full, which means the kernel had more
    /// submissions outstanding than the ring holds.
    Full,
    /// The completion names a slot the ring does not have.
    SlotOutOfRange,
    /// The completion reports more bytes than a slot holds.
    LengthTooLarge,
    /// The ring said something impossible; this side is finished.
    Corrupt(Corruption),
}

/// What one take took.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Consumed {
    /// How many submissions were written into the caller's buffer.
    pub taken: usize,
    /// Whether more are waiting than the buffer held.
    pub more: bool,
}

/// The driver's end.
#[derive(Debug)]
pub struct DriverSide {
    /// Where everything is.
    layout: RingLayout,
    /// This side's end of the submission ring.
    submissions: Consumer,
    /// This side's end of the completion ring.
    completions: Producer,
    /// The corruption this side latched, if it saw one.
    corrupt: Option<Corruption>,
}

impl DriverSide {
    /// Write a fresh ring's header and take the driver's end of it.
    ///
    /// # Errors
    ///
    /// [`HeaderError`] if the ring is too small for what was asked, or the
    /// sizes are out of range.
    pub fn create<M: RingMemory>(
        memory: &mut M,
        ring_bytes: usize,
        entries: u32,
        slot_bytes: u32,
    ) -> Result<DriverSide, HeaderError> {
        let layout = RingLayout::write(memory, ring_bytes, entries, slot_bytes)?;
        Ok(DriverSide {
            layout,
            submissions: Consumer::default(),
            completions: Producer::default(),
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

    /// Take what the kernel has submitted.
    ///
    /// # Errors
    ///
    /// [`Corruption`], which is terminal for this side.
    pub fn take<M: RingMemory>(
        &mut self,
        memory: &mut M,
        out: &mut [Submission],
    ) -> Result<Consumed, Corruption> {
        if let Some(corruption) = self.corrupt {
            return Err(corruption);
        }
        let entries = self.layout.entries();
        let pending = self
            .submissions
            .pending(memory, self.layout.sub_tail(), entries)
            .inspect_err(|error| self.corrupt = Some(*error))?;
        let mut taken = 0;
        while taken < out.len() && (taken as u32) < pending {
            let index = self.submissions.head();
            let entry = self
                .layout
                .get_submission(memory, index)
                .inspect_err(|error| self.corrupt = Some(*error))?;
            if let Some(slot) = out.get_mut(taken) {
                *slot = entry;
            }
            self.submissions.advance(memory, self.layout.sub_head());
            taken += 1;
        }
        Ok(Consumed {
            taken,
            more: (taken as u32) < pending,
        })
    }

    /// Stage a completion. It is not visible to the kernel until
    /// [`DriverSide::publish`].
    ///
    /// # Errors
    ///
    /// [`DriverError`], naming why not.
    pub fn complete<M: RingMemory>(
        &mut self,
        memory: &mut M,
        slot: u32,
        length: u32,
        status: Status,
    ) -> Result<(), DriverError> {
        if let Some(corruption) = self.corrupt {
            return Err(DriverError::Corrupt(corruption));
        }
        if slot >= self.layout.entries() {
            return Err(DriverError::SlotOutOfRange);
        }
        if length > self.layout.slot_bytes() {
            return Err(DriverError::LengthTooLarge);
        }
        let head = memory.read_u32(self.layout.comp_head());
        let room = self.completions.observe_head(head).map_err(|error| {
            self.corrupt = Some(error);
            DriverError::Corrupt(error)
        })?;
        if room >= self.layout.entries() {
            return Err(DriverError::Full);
        }
        self.layout.put_completion(
            memory,
            self.completions.tail(),
            Completion {
                slot,
                length,
                status,
            },
        );
        self.completions.stage();
        Ok(())
    }

    /// Publish everything staged, and answer the bell to ring if the kernel
    /// asked for one.
    pub fn publish<M: RingMemory>(&mut self, memory: &mut M) -> Option<Doorbell> {
        self.completions.publish(
            memory,
            self.layout.comp_tail(),
            self.layout.comp_want_bell(),
            BELL_COMPLETE,
        )
    }

    /// Ask to be rung when submissions arrive, and look once more before
    /// sleeping.
    ///
    /// # Errors
    ///
    /// [`Corruption`], which is terminal for this side.
    pub fn prepare_to_sleep<M: RingMemory>(&mut self, memory: &mut M) -> Result<Wait, Corruption> {
        if let Some(corruption) = self.corrupt {
            return Err(corruption);
        }
        self.submissions
            .prepare_to_sleep(
                memory,
                self.layout.sub_want_bell(),
                self.layout.sub_tail(),
                self.layout.entries(),
            )
            .inspect_err(|error| self.corrupt = Some(*error))
    }

    /// Stop asking to be rung, which is the first thing to do on waking.
    pub fn woke<M: RingMemory>(&mut self, memory: &mut M) {
        self.submissions.woke(memory, self.layout.sub_want_bell());
    }
}
