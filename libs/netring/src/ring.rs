//! The index discipline both sides share: private indices, checked reads of
//! the peer's, and the want-bell handshake.
//!
//! A side is a producer on one ring and a consumer on the other. What differs
//! between the kernel and the driver is which offsets those are and what is in
//! an entry; the arithmetic and the order of the handshake's reads and writes
//! are the same, so they are written once, here.
//!
//! This is the discipline `libs/blkring` keeps, written a second time; the
//! crate root says why.

use crate::bell::{Doorbell, WANT_BELL, Wait};
use crate::{Corruption, RingMemory};

/// This side's end of a ring it consumes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Consumer {
    /// The next entry to consume. Written to the ring, never read back.
    head: u32,
    /// The producer's tail, as last read and accepted.
    observed_tail: u32,
    /// What this side last wrote to its want-bell field.
    want_bell: bool,
}

impl Consumer {
    /// The next entry to consume.
    pub(crate) const fn head(&self) -> u32 {
        self.head
    }

    /// Accept a freshly read tail, or say why it is impossible. Returns how
    /// many entries are pending.
    fn observe(&mut self, tail: u32, entries: u32) -> Result<u32, Corruption> {
        let pending = tail.wrapping_sub(self.head);
        if pending > entries {
            return Err(Corruption::TailOverrun);
        }
        if pending < self.observed_tail.wrapping_sub(self.head) {
            return Err(Corruption::TailBackwards);
        }
        self.observed_tail = tail;
        Ok(pending)
    }

    /// Read the producer's tail once and say how many entries are pending.
    pub(crate) fn pending<M: RingMemory>(
        &mut self,
        memory: &M,
        tail_at: usize,
        entries: u32,
    ) -> Result<u32, Corruption> {
        self.observe(memory.read_u32(tail_at), entries)
    }

    /// Step past the entry at the head, and publish the new head.
    pub(crate) fn advance<M: RingMemory>(&mut self, memory: &mut M, head_at: usize) {
        self.head = self.head.wrapping_add(1);
        memory.write_u32(head_at, self.head);
    }

    /// The consumer's half of the handshake before sleeping: ask to be rung,
    /// then look once more.
    pub(crate) fn prepare_to_sleep<M: RingMemory>(
        &mut self,
        memory: &mut M,
        want_at: usize,
        tail_at: usize,
        entries: u32,
    ) -> Result<Wait, Corruption> {
        self.set_want_bell(memory, want_at, true);
        memory.barrier();
        let pending = self.pending(memory, tail_at, entries)?;
        if pending == 0 {
            return Ok(Wait::Sleep);
        }
        self.set_want_bell(memory, want_at, false);
        Ok(Wait::Pending(pending))
    }

    /// The consumer's half of the handshake on waking: stop asking, then the
    /// caller drains.
    pub(crate) fn woke<M: RingMemory>(&mut self, memory: &mut M, want_at: usize) {
        self.set_want_bell(memory, want_at, false);
    }

    fn set_want_bell<M: RingMemory>(&mut self, memory: &mut M, want_at: usize, on: bool) {
        self.want_bell = on;
        memory.write_u32(want_at, u32::from(on));
    }
}

/// This side's end of a ring it produces.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Producer {
    /// The next slot to fill, counting entries written but not yet published.
    tail: u32,
    /// The tail as last written to the ring.
    published: u32,
    /// The consumer's head, as last read and accepted.
    observed_head: u32,
}

impl Producer {
    /// The next slot to fill.
    pub(crate) const fn tail(&self) -> u32 {
        self.tail
    }

    /// Count one more entry written at the tail. It is not visible to the
    /// consumer until [`Producer::publish`].
    pub(crate) fn stage(&mut self) {
        self.tail = self.tail.wrapping_add(1);
    }

    /// Accept a freshly read consumer head, or say why it is impossible.
    /// Returns how many entries lie between it and this side's tail, staged
    /// ones included.
    pub(crate) fn observe_head(&mut self, head: u32) -> Result<u32, Corruption> {
        // The head may only move forward from where it was, and never past
        // what was published: measured back from the published tail, it may
        // only get closer.
        if self.published.wrapping_sub(head) > self.published.wrapping_sub(self.observed_head) {
            return Err(Corruption::HeadOutOfRange);
        }
        self.observed_head = head;
        Ok(self.tail.wrapping_sub(head))
    }

    /// The producer's half of the handshake: publish the tail, then look at the
    /// consumer's want-bell. `None` if there was nothing new to publish or the
    /// consumer did not ask.
    pub(crate) fn publish<M: RingMemory>(
        &mut self,
        memory: &mut M,
        tail_at: usize,
        want_at: usize,
        key: u64,
    ) -> Option<Doorbell> {
        if self.tail == self.published {
            return None;
        }
        self.published = self.tail;
        memory.write_u32(tail_at, self.tail);
        memory.barrier();
        (memory.read_u32(want_at) == WANT_BELL).then_some(Doorbell::new(key, self.tail))
    }
}
