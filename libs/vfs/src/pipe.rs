//! The buffer behind a pipe, and the rules that make it one.
//!
//! A pipe is a bounded byte queue with two kinds of end, and almost all of
//! what a program relies on is in what happens at the edges rather than in
//! the queue: a read of an empty pipe with a writer still open waits, and with
//! none is end of file; a write with no reader left is `EPIPE`; and a write of
//! at most [`PIPE_BUF`] bytes is never split, so lines from two processes
//! writing to one pipe do not interleave mid-line.
//!
//! This is the part of that which is a pure function of the buffer and the
//! count of ends. Waiting is not here. The kernel wraps a [`PipeBuffer`] in a
//! lock and two wait queues, and turns [`ReadOutcome::WouldBlock`] into a
//! sleep — or into `EAGAIN` under `O_NONBLOCK` — which is why every outcome
//! that would block is a value rather than a wait: the same buffer serves both
//! and the host tests can reach every edge.

use alloc::collections::VecDeque;

use crate::node::Readiness;

/// Linux's default pipe capacity: sixteen pages.
pub const PIPE_CAPACITY: usize = 65536;

/// The largest write POSIX promises is never split: one page.
pub const PIPE_BUF: usize = 4096;

/// `PIPEFS_MAGIC`, from `include/uapi/linux/magic.h`: what `fstatfs` on a pipe
/// reports as the filesystem it is on.
pub const PIPEFS_MAGIC: u64 = 0x5049_5045;

/// What a read did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Bytes were copied out. Zero only for a zero-length read.
    Read(usize),
    /// Nothing to read, and a writer could still add something.
    WouldBlock,
    /// Nothing to read, and no writer is left to add anything.
    EndOfFile,
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Bytes were queued. May be fewer than asked for a write larger than
    /// [`PIPE_BUF`]; the caller waits and writes the rest.
    Wrote(usize),
    /// Not enough room: none at all, or too little for a write that must not
    /// be split.
    WouldBlock,
    /// No reader is left. The caller reports `EPIPE`, and raises `SIGPIPE`.
    Broken,
}

/// The queue, and how many of each end are open.
#[derive(Debug)]
pub struct PipeBuffer {
    data: VecDeque<u8>,
    capacity: usize,
    readers: usize,
    writers: usize,
}

impl PipeBuffer {
    /// An empty pipe holding at most `capacity` bytes, with no ends open.
    #[must_use]
    pub fn new(capacity: usize) -> PipeBuffer {
        PipeBuffer {
            data: VecDeque::new(),
            capacity: capacity.max(PIPE_BUF),
            readers: 0,
            writers: 0,
        }
    }

    /// A read end was opened.
    pub fn open_reader(&mut self) {
        self.readers = self.readers.saturating_add(1);
    }

    /// A read end was closed.
    pub fn close_reader(&mut self) {
        self.readers = self.readers.saturating_sub(1);
    }

    /// A write end was opened.
    pub fn open_writer(&mut self) {
        self.writers = self.writers.saturating_add(1);
    }

    /// A write end was closed.
    pub fn close_writer(&mut self) {
        self.writers = self.writers.saturating_sub(1);
    }

    /// Open read ends.
    #[must_use]
    pub fn readers(&self) -> usize {
        self.readers
    }

    /// Open write ends.
    #[must_use]
    pub fn writers(&self) -> usize {
        self.writers
    }

    /// Bytes waiting to be read: `FIONREAD`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn free(&self) -> usize {
        self.capacity.saturating_sub(self.data.len())
    }

    /// Bytes a write could queue now: what `splice` into the pipe may take
    /// from its source without taking more than it can put down.
    #[must_use]
    pub fn room(&self) -> usize {
        self.free()
    }

    /// Whether [`PipeBuffer::read`] into a non-empty buffer would not answer
    /// [`ReadOutcome::WouldBlock`]: the condition a blocked reader waits for.
    #[must_use]
    pub fn can_read(&self) -> bool {
        !self.data.is_empty() || self.writers == 0
    }

    /// Whether [`PipeBuffer::write`] of `len` bytes would not answer
    /// [`WriteOutcome::WouldBlock`]: the condition a blocked writer waits for.
    ///
    /// The same rule as the write itself -- a write of at most [`PIPE_BUF`]
    /// needs room for all of it, a larger one room for anything -- so a writer
    /// woken by this is never woken to find it still cannot write.
    #[must_use]
    pub fn can_write(&self, len: usize) -> bool {
        if self.readers == 0 || len == 0 {
            return true;
        }
        if len <= PIPE_BUF {
            self.free() >= len
        } else {
            self.free() > 0
        }
    }

    /// Take up to `buf.len()` bytes.
    ///
    /// What is in the pipe is delivered even after the last writer has gone;
    /// end of file comes only once it is drained.
    pub fn read(&mut self, buf: &mut [u8]) -> ReadOutcome {
        if buf.is_empty() {
            return ReadOutcome::Read(0);
        }
        if self.data.is_empty() {
            return if self.writers == 0 {
                ReadOutcome::EndOfFile
            } else {
                ReadOutcome::WouldBlock
            };
        }
        let count = buf.len().min(self.data.len());
        for (slot, byte) in buf.iter_mut().zip(self.data.drain(..count)) {
            *slot = byte;
        }
        ReadOutcome::Read(count)
    }

    /// Put `bytes` back at the front, to be read next: the start of what
    /// [`PipeBuffer::read`] took and the reader could not be given, because
    /// the memory it was to be copied to was bad. Linux copies straight from
    /// the pipe's pages into the reader's and consumes only what arrived, so
    /// a read into a bad buffer leaves the pipe as it was; this is the same
    /// done after the fact.
    ///
    /// Taken whatever room is left: the bytes were in the pipe a moment ago,
    /// and a writer that filled the room since has only pushed the pipe past
    /// its capacity until they are read, as `free` saturates at none.
    pub fn unread(&mut self, bytes: &[u8]) {
        for &byte in bytes.iter().rev() {
            self.data.push_front(byte);
        }
    }

    /// Queue as much of `data` as the rules allow.
    ///
    /// A write of at most [`PIPE_BUF`] bytes goes in whole or not at all. A
    /// larger one takes whatever room there is, and the caller comes back for
    /// the rest — which is exactly the case in which POSIX lets writers
    /// interleave.
    pub fn write(&mut self, data: &[u8]) -> WriteOutcome {
        if self.readers == 0 {
            return WriteOutcome::Broken;
        }
        if data.is_empty() {
            return WriteOutcome::Wrote(0);
        }
        let free = self.free();
        let count = if data.len() <= PIPE_BUF {
            if free < data.len() {
                return WriteOutcome::WouldBlock;
            }
            data.len()
        } else {
            if free == 0 {
                return WriteOutcome::WouldBlock;
            }
            data.len().min(free)
        };
        self.data
            .extend(data.get(..count).unwrap_or_default().iter().copied());
        WriteOutcome::Wrote(count)
    }

    /// What `poll` reports for a read end.
    ///
    /// Readable when a read would not block — including at end of file, which
    /// is the one way a program polling a pipe learns the writer is gone — and
    /// hung up once no writer is left.
    #[must_use]
    pub fn read_readiness(&self) -> Readiness {
        Readiness {
            readable: !self.data.is_empty() || self.writers == 0,
            writable: false,
            hangup: self.writers == 0,
            error: false,
            priority: false,
        }
    }

    /// What `poll` reports for a write end.
    ///
    /// Writable when a write of [`PIPE_BUF`] bytes would go in whole, and in
    /// error once no reader is left — so a poll loop wakes to discover
    /// `EPIPE` rather than waiting for room nobody will make.
    #[must_use]
    pub fn write_readiness(&self) -> Readiness {
        Readiness {
            readable: false,
            writable: self.readers == 0 || self.free() >= PIPE_BUF,
            hangup: false,
            error: self.readers == 0,
            priority: false,
        }
    }
}
