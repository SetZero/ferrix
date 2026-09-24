//! A ring the front end idles in, and blocks of commands spliced into it:
//! the shape of etnaviv's kernel ring (`etnaviv_buffer_init`,
//! `etnaviv_buffer_queue`, `etnaviv_buffer_end`), cut down to one page and
//! to blocks that only raise events.
//!
//! # Why a `WAIT`/`LINK` loop rather than a stream that ends
//!
//! A stream that raises its event and `END`s proves the front end fetches
//! and the event arrives, and then the front end is stopped: the next work
//! needs `FE_COMMAND_CONTROL` written again, from wherever the driver is.
//! A GPU driver does not work that way, and G3 onwards will not. The front
//! end is started once, on a `WAIT` followed by a `LINK` back to that
//! `WAIT`, and spins there; work is handed to it by overwriting the `WAIT`
//! with a `LINK` to a block that does the work and ends in a `WAIT`/`LINK`
//! of its own. So the first run on the board exercises what everything
//! after it rests on: the loop, the splice, and the event at the end of a
//! block. [`Ring::stop`] ends the loop the same way, by overwriting the
//! last `WAIT` with an `END`.
//!
//! # The splice
//!
//! The front end may be executing the `WAIT` while it is replaced. A slot
//! is two words, and the core must never see the new header with the old
//! argument, which for a `LINK` would be a jump to address zero: so the
//! argument is written first, then a barrier, then the header (as
//! `etnaviv_buffer_replace_wait` does). A header alone is a single aligned
//! word, which the core reads whole.

use crate::stream::{self, Slot};

/// The memory the command buffer is in: words the program and the core see
/// alike, the core at the GPU address [`Ring::new`] is given for word 0.
pub trait CommandMemory {
    /// How many 32-bit words there are.
    fn words(&self) -> usize;
    /// Write the word at `index`.
    fn write32(&mut self, index: usize, value: u32);
    /// Read the word at `index`.
    fn read32(&self, index: usize) -> u32;
    /// Make every write before this visible to the core before any write
    /// after it, to memory or to a register.
    fn barrier(&self);
}

/// No room left for another block: the ring does not wrap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Full;

/// Where the front end's loop is and what has been queued.
#[derive(Debug)]
pub struct Ring<M> {
    memory: M,
    /// The GPU address of word 0.
    base: u32,
    /// How many core cycles a `WAIT` lasts.
    wait_cycles: u16,
    /// The slot whose `WAIT` the front end is looping on.
    waiting: usize,
    /// The first free slot.
    next: usize,
}

/// A slot is two words.
const SLOT_WORDS: usize = 2;
/// And eight bytes.
const SLOT_BYTES: usize = 8;
/// The slots in the loop a block ends in: a `WAIT` and its `LINK`.
const LOOP_SLOTS: usize = 2;

impl<M: CommandMemory> Ring<M> {
    /// Write the idle loop at the start of `memory`, whose first word the
    /// core reaches at GPU address `base`, with `WAIT`s of `wait_cycles`.
    /// The front end is to be started at [`Ring::start`].
    pub fn new(memory: M, base: u32, wait_cycles: u16) -> Result<Ring<M>, Full> {
        let mut ring = Ring {
            memory,
            base,
            wait_cycles,
            waiting: 0,
            next: 0,
        };
        let at = ring.reserve(LOOP_SLOTS)?;
        ring.write_loop(at);
        Ok(ring)
    }

    /// Where to start the front end and how many slots it is to prefetch:
    /// the idle loop's `WAIT` and `LINK` at the start of the ring, or the
    /// `LINK` a block queued before the start was spliced over that `WAIT`.
    #[must_use]
    pub fn start(&self) -> (u32, u16) {
        (self.address(0), LOOP_SLOTS as u16)
    }

    /// Queue a block that raises event `id` from the pixel engine, preceded
    /// by `before` (a pipe select, say), and ends in a loop of its own; and
    /// splice it in, so that the front end runs it the next time it looks
    /// at the `WAIT` it is spinning on.
    pub fn queue_event(&mut self, before: &[Slot], id: u32) -> Result<(), Full> {
        let slots = before.len() + 1 + LOOP_SLOTS;
        let at = self.reserve(slots)?;
        for (offset, slot) in before.iter().enumerate() {
            self.write_slot(at + offset, *slot);
        }
        self.write_slot(at + before.len(), stream::event(id));
        self.write_loop(at + before.len() + 1);
        let link = stream::link(u16::try_from(slots).map_err(|_| Full)?, self.address(at));
        self.splice(self.waiting, link);
        self.waiting = at + before.len() + 1;
        Ok(())
    }

    /// End the loop: the front end runs to the current `WAIT`, finds an
    /// `END` there, and goes idle.
    pub fn stop(&mut self) {
        self.splice(self.waiting, stream::end());
    }

    /// The GPU address of the `WAIT` the front end is looping on: where
    /// `FE_DMA_ADDRESS` stays while it idles.
    #[must_use]
    pub fn waiting_at(&self) -> u32 {
        self.address(self.waiting)
    }

    /// The memory, for reading back what was written.
    pub fn memory(&self) -> &M {
        &self.memory
    }

    /// The GPU address of slot `slot`.
    fn address(&self, slot: usize) -> u32 {
        // `reserve` keeps every slot inside the memory, which is a page: its
        // byte offset fits in 32 bits with room to spare.
        let offset = u32::try_from(slot.saturating_mul(SLOT_BYTES)).unwrap_or(u32::MAX);
        self.base.wrapping_add(offset)
    }

    /// `slots` slots at the end of what is used, if there is room.
    fn reserve(&mut self, slots: usize) -> Result<usize, Full> {
        let at = self.next;
        let end = at.checked_add(slots).ok_or(Full)?;
        if end.saturating_mul(SLOT_WORDS) > self.memory.words() {
            return Err(Full);
        }
        self.next = end;
        Ok(at)
    }

    /// A `WAIT` at `slot` and a `LINK` back to it after it: the front end
    /// spins here until the `WAIT` is replaced.
    fn write_loop(&mut self, slot: usize) {
        self.write_slot(slot, stream::wait(self.wait_cycles));
        self.write_slot(
            slot + 1,
            stream::link(LOOP_SLOTS as u16, self.address(slot)),
        );
    }

    /// Write a slot the front end is not looking at yet.
    fn write_slot(&mut self, slot: usize, [header, argument]: Slot) {
        self.memory.write32(slot * SLOT_WORDS, header);
        self.memory.write32(slot * SLOT_WORDS + 1, argument);
    }

    /// Replace the slot the front end may be executing: everything written
    /// so far first, then its argument, then its header.
    fn splice(&mut self, slot: usize, [header, argument]: Slot) {
        self.memory.barrier();
        self.memory.write32(slot * SLOT_WORDS + 1, argument);
        self.memory.barrier();
        self.memory.write32(slot * SLOT_WORDS, header);
        self.memory.barrier();
    }
}
