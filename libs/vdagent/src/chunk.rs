//! The wire under the messages: chunks out, and a reassembler in.
//!
//! A message is cut into chunks of at most [`MAX_PAYLOAD`] bytes, each headed
//! by its port and its length:
//!
//! ```text
//! VDIChunkHeader   port u32   size u32
//! ```
//!
//! The receiver concatenates the payloads and reads messages out of the
//! result, so a chunk boundary is *not* a message boundary: one message may
//! span several chunks, and nothing says a chunk may not hold the end of one
//! message and the start of the next. QEMU's own sender does not do that --
//! it starts a chunk per message -- but a reader that assumed it would be
//! broken by a peer that is within its rights, so [`Reassembler`] treats the
//! chunks as a byte stream and the messages as framing on top of it.
//!
//! # The port field
//!
//! [`CLIENT_PORT`] on the way out of the guest, which is what SPICE's client
//! and QEMU's host half both write. QEMU ignores the field entirely when it
//! reads (`vdagent_chr_recv_chunk`, `ui/vdagent.c`), so nothing here depends
//! on it either -- it is written correctly and read loosely, and this
//! paragraph exists so that nobody sets it to [`SERVER_PORT`] in the belief
//! that something will change.

use crate::message::HEADER_BYTES;
use crate::{Error, part, part_mut, put_u32, u32_at};

/// Bytes of a chunk header.
pub const HEADER: usize = 8;

/// The most payload a chunk carries when this crate sends one.
///
/// QEMU cuts at exactly this (`vdagent_send_msg`), and a receiver that
/// allocates per chunk is a receiver a big chunk can hurt, so nothing sent
/// from here is larger. It is not enforced on the way in: a chunk larger than
/// this is carried as long as the message it belongs to fits the buffer.
pub const MAX_PAYLOAD: usize = 1024;

/// `VDP_CLIENT_PORT`: the port number a guest agent writes.
pub const CLIENT_PORT: u32 = 1;
/// `VDP_SERVER_PORT`: the other one, which nothing here sends.
pub const SERVER_PORT: u32 = 2;

/// Bytes that framing `message` takes, headers and all.
#[must_use]
pub fn framed_len(message: usize) -> usize {
    // A zero-length message still takes one chunk, so that a reader sees
    // something; nothing this crate encodes is zero-length, since every
    // message has a sixteen-byte header, but the arithmetic should not
    // depend on that.
    let chunks = message.div_ceil(MAX_PAYLOAD).max(1);
    chunks * HEADER + message
}

/// Frame `message` into `out` as chunks, and say how many bytes it took.
///
/// # Errors
///
/// [`Error::Short`] when `out` is smaller than [`framed_len`].
pub fn frame(message: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let want = framed_len(message.len());
    if out.len() < want {
        return Err(Error::Short {
            want,
            have: out.len(),
        });
    }
    let mut written = 0;
    let mut left = message;
    loop {
        let take = left.len().min(MAX_PAYLOAD);
        put_u32(out, written, CLIENT_PORT)?;
        put_u32(out, written + 4, u32::try_from(take).unwrap_or(u32::MAX))?;
        part_mut(out, written + HEADER, written + HEADER + take)?
            .copy_from_slice(part(left, 0, take)?);
        written += HEADER + take;
        left = part(left, take, left.len())?;
        if left.is_empty() {
            break;
        }
    }
    Ok(written)
}

/// Where the reassembler is in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Reading a chunk header, this much of it already in hand.
    Header(usize),
    /// Inside a chunk's payload, this many bytes of it still to come.
    Payload(usize),
    /// Something was refused, so where the framing is is no longer known.
    Broken,
}

/// Chunks in, whole messages out.
///
/// The buffer the caller lends it is the largest message it will accept: a
/// peer declaring a larger one is refused with [`Error::TooLong`] rather than
/// truncated, and the reassembler is broken from then on, because a message
/// that was not read to its end leaves the stream at an unknown place.
#[derive(Debug)]
pub struct Reassembler<'a> {
    /// Where the message being assembled goes.
    buffer: &'a mut [u8],
    /// How much of it is filled.
    filled: usize,
    /// The chunk header being read.
    header: [u8; HEADER],
    /// Where in the stream this is.
    state: State,
    /// Bytes of the message being assembled, once its header has arrived.
    message: Option<usize>,
    /// Whether [`Reassembler::message`] has one to give.
    complete: bool,
}

impl<'a> Reassembler<'a> {
    /// A reassembler that will accept messages up to `buffer`'s length.
    #[must_use]
    pub fn new(buffer: &'a mut [u8]) -> Reassembler<'a> {
        Reassembler {
            buffer,
            filled: 0,
            header: [0; HEADER],
            state: State::Header(0),
            message: None,
            complete: false,
        }
    }

    /// Take bytes from `bytes`, stopping at the end of a message, and say how
    /// many were taken.
    ///
    /// A caller feeds what it read and loops: when this returns, [`message`]
    /// may hold one, and [`take`] moves on to the next. It stops at each
    /// message boundary so that a caller never has to hold two.
    ///
    /// [`message`]: Reassembler::message
    /// [`take`]: Reassembler::take
    ///
    /// # Errors
    ///
    /// [`Error::TooLong`] for a message larger than the buffer,
    /// [`Error::Protocol`] for a header that is not this protocol, and
    /// [`Error::Broken`] for every call after either.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<usize, Error> {
        if self.state == State::Broken {
            return Err(Error::Broken);
        }
        if self.complete {
            // The caller has one in hand and has not taken it; giving it more
            // would overwrite what it is holding.
            return Ok(0);
        }
        let mut taken = 0;
        while taken < bytes.len() && !self.complete {
            match self.state {
                State::Broken => return Err(Error::Broken),
                State::Header(have) => {
                    let want = HEADER - have;
                    let copy = want.min(bytes.len() - taken);
                    part_mut(&mut self.header, have, have + copy)?.copy_from_slice(part(
                        bytes,
                        taken,
                        taken + copy,
                    )?);
                    taken += copy;
                    self.state = if have + copy == HEADER {
                        // The port is read and ignored; the size is the whole
                        // of what the header says.
                        let size = u32_at(&self.header, 4)? as usize;
                        State::Payload(size)
                    } else {
                        State::Header(have + copy)
                    };
                }
                State::Payload(0) => self.state = State::Header(0),
                State::Payload(left) => {
                    let copy = left.min(bytes.len() - taken);
                    // `absorb` stops at the end of a message, so it may take
                    // less than the chunk has left; what is still owed to the
                    // chunk is what it did not take.
                    let took = self.absorb(part(bytes, taken, taken + copy)?)?;
                    taken += took;
                    self.state = State::Payload(left - took);
                }
            }
        }
        Ok(taken)
    }

    /// Put a chunk's bytes into the message being assembled, stopping as soon
    /// as one is whole.
    fn absorb(&mut self, bytes: &[u8]) -> Result<usize, Error> {
        let mut taken = 0;
        while taken < bytes.len() && !self.complete {
            // Until the message header is in, take it a byte at a time: the
            // length is not known before it, and a chunk may end in the
            // middle of it.
            let want = match self.message {
                Some(size) => HEADER_BYTES + size,
                None => HEADER_BYTES,
            };
            let room = want.min(self.buffer.len());
            let copy = (room - self.filled).min(bytes.len() - taken);
            if copy == 0 {
                // No room for even a message header: a buffer too small to
                // hold anything, which is the caller's bug rather than the
                // peer's, but it is refused the same way so that it cannot
                // spin here.
                self.state = State::Broken;
                return Err(Error::TooLong {
                    declared: want,
                    limit: self.buffer.len(),
                });
            }
            part_mut(self.buffer, self.filled, self.filled + copy)?.copy_from_slice(part(
                bytes,
                taken,
                taken + copy,
            )?);
            self.filled += copy;
            taken += copy;

            if self.message.is_none() && self.filled >= HEADER_BYTES {
                let protocol = u32_at(self.buffer, 0)?;
                if protocol != crate::message::PROTOCOL {
                    self.state = State::Broken;
                    return Err(Error::Protocol(protocol));
                }
                let size = u32_at(self.buffer, 12)? as usize;
                if HEADER_BYTES + size > self.buffer.len() {
                    self.state = State::Broken;
                    return Err(Error::TooLong {
                        declared: HEADER_BYTES + size,
                        limit: self.buffer.len(),
                    });
                }
                self.message = Some(size);
                continue;
            }
            if let Some(size) = self.message
                && self.filled == HEADER_BYTES + size
            {
                self.complete = true;
            }
        }
        Ok(taken)
    }

    /// The whole message, header and body, once one is there.
    #[must_use]
    pub fn message(&self) -> Option<&[u8]> {
        self.complete
            .then(|| self.buffer.get(..self.filled))
            .flatten()
    }

    /// Forget the message [`Reassembler::message`] gave and make room for the
    /// next.
    pub fn take(&mut self) {
        if self.complete {
            self.complete = false;
            self.filled = 0;
            self.message = None;
        }
    }
}
