//! The two byte queues a connection holds, and the out-of-order pieces it
//! cannot put in one yet.
//!
//! A TCP connection is two queues and a set of counters. [`ByteQueue`] is the
//! queue: bytes in at one end, bytes out at the other, with a fixed capacity
//! that is what the connection advertises as its window. It is a
//! [`VecDeque`] with a ceiling and an argument for why the ceiling is there --
//! a socket buffer that grows without bound is a machine a stranger can fill.
//!
//! [`Reassembly`] holds what arrived early. A segment that begins past
//! `RCV.NXT` cannot be given to the program, because the bytes before it have
//! not come; it is kept here, trimmed against what is already held, and drained
//! into the receive queue as soon as the gap closes.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::seq::SeqNumber;

/// A bounded first-in, first-out queue of bytes.
#[derive(Debug)]
pub struct ByteQueue {
    /// The bytes themselves, oldest first.
    bytes: VecDeque<u8>,
    /// The most bytes the queue will hold.
    capacity: usize,
}

impl ByteQueue {
    /// An empty queue that will hold `capacity` bytes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> ByteQueue {
        ByteQueue {
            bytes: VecDeque::new(),
            capacity,
        }
    }

    /// How many bytes are queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// How many more bytes the queue will take.
    #[must_use]
    pub fn free(&self) -> usize {
        self.capacity.saturating_sub(self.bytes.len())
    }

    /// The ceiling this queue was built with.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Change the ceiling, keeping whatever is already queued.
    ///
    /// Shrinking below what is held does not discard anything: the queue simply
    /// reports no free space until enough has been taken out.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
    }

    /// Append as much of `data` as there is room for, and say how much that
    /// was.
    pub fn write(&mut self, data: &[u8]) -> usize {
        let room = self.free();
        let taken = data.len().min(room);
        self.bytes.extend(data.iter().take(taken).copied());
        taken
    }

    /// Copy out of the front without removing, starting `offset` bytes in.
    ///
    /// Answers how many bytes were copied, which is bounded by what is queued
    /// past `offset` and by the length of `out`.
    pub fn peek(&self, offset: usize, out: &mut [u8]) -> usize {
        let mut copied = 0;
        for (slot, byte) in out.iter_mut().zip(self.bytes.iter().skip(offset)) {
            *slot = *byte;
            copied += 1;
        }
        copied
    }

    /// Remove `count` bytes from the front, or everything if fewer are held.
    pub fn discard(&mut self, count: usize) {
        let count = count.min(self.bytes.len());
        let _ = self.bytes.drain(..count);
    }

    /// Take bytes from the front into `out`, and say how many were taken.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let copied = self.peek(0, out);
        self.discard(copied);
        copied
    }

    /// Throw everything away.
    pub fn clear(&mut self) {
        self.bytes.clear();
    }
}

/// One run of bytes that arrived before the bytes in front of it.
#[derive(Debug)]
struct Hole {
    /// The sequence number of the run's first byte.
    start: SeqNumber,
    /// The bytes themselves.
    bytes: Vec<u8>,
}

impl Hole {
    /// One past the run's last byte.
    fn end(&self) -> SeqNumber {
        self.start.advance(self.bytes.len() as u32)
    }
}

/// Segments held because the bytes in front of them have not arrived.
///
/// The list is kept sorted by sequence number and non-overlapping, so draining
/// it is a walk from the front and the SACK blocks it reports are the list
/// itself.
#[derive(Debug)]
pub struct Reassembly {
    /// The runs, in sequence order, none of them touching or overlapping.
    holes: Vec<Hole>,
    /// The most bytes the whole list will hold.
    capacity: usize,
    /// How many bytes it holds now.
    held: usize,
}

impl Reassembly {
    /// An empty list that will hold `capacity` bytes across all its runs.
    #[must_use]
    pub const fn with_capacity(capacity: usize) -> Reassembly {
        Reassembly {
            holes: Vec::new(),
            capacity,
            held: 0,
        }
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.holes.is_empty()
    }

    /// How many bytes are held.
    #[must_use]
    pub const fn held(&self) -> usize {
        self.held
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.holes.clear();
        self.held = 0;
    }

    /// Change the ceiling on what may be held.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
    }

    /// Remember `bytes`, which begin at `start`.
    ///
    /// Anything already held, and anything past the capacity, is dropped rather
    /// than kept twice: a duplicate is not information, and a peer that sends
    /// nothing but holes must not be able to grow this list without end.
    pub fn insert(&mut self, start: SeqNumber, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let room = self.capacity.saturating_sub(self.held);
        let taken = bytes.len().min(room);
        if taken == 0 {
            return;
        }
        let hole = Hole {
            start,
            bytes: bytes.iter().take(taken).copied().collect(),
        };
        self.held += taken;
        let at = self
            .holes
            .iter()
            .position(|existing| hole.start.precedes(existing.start))
            .unwrap_or(self.holes.len());
        self.holes.insert(at, hole);
        self.coalesce();
    }

    /// Merge runs that touch or overlap, keeping the earlier copy of any byte
    /// held twice.
    fn coalesce(&mut self) {
        let mut merged: Vec<Hole> = Vec::with_capacity(self.holes.len());
        for hole in self.holes.drain(..) {
            match merged.last_mut() {
                Some(previous) if hole.start.precedes_or_equals(previous.end()) => {
                    let overlap = previous.end().distance_from(hole.start) as usize;
                    let extra = hole.bytes.len().saturating_sub(overlap);
                    previous
                        .bytes
                        .extend(hole.bytes.iter().skip(overlap).copied());
                    self.held -= hole.bytes.len() - extra;
                }
                _ => merged.push(hole),
            }
        }
        self.holes = merged;
    }

    /// Take the run that begins exactly at `next`, if there is one.
    ///
    /// The caller has just consumed everything before `next`, so this is how
    /// the gap closing turns held bytes into deliverable ones.
    pub fn take_contiguous(&mut self, next: SeqNumber) -> Option<Vec<u8>> {
        let first = self.holes.first()?;
        if first.start != next {
            return None;
        }
        let hole = self.holes.remove(0);
        self.held -= hole.bytes.len();
        Some(hole.bytes)
    }

    /// Drop everything before `next`, and trim a run that straddles it.
    pub fn trim_before(&mut self, next: SeqNumber) {
        for hole in &mut self.holes {
            if hole.start.follows_or_equals(next) {
                continue;
            }
            let drop = next.distance_from(hole.start) as usize;
            let drop = drop.min(hole.bytes.len());
            let _ = hole.bytes.drain(..drop);
            hole.start = next;
            self.held -= drop;
        }
        self.holes.retain(|hole| !hole.bytes.is_empty());
    }

    /// The runs held, newest-first, as the sequence ranges a SACK option
    /// reports.
    pub fn blocks(&self, out: &mut [(SeqNumber, SeqNumber)]) -> usize {
        let mut count = 0;
        for (slot, hole) in out.iter_mut().zip(self.holes.iter()) {
            *slot = (hole.start, hole.end());
            count += 1;
        }
        count
    }
}
