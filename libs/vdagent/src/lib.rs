//! SPICE's vdagent protocol, as bytes: the framing on the wire and the
//! clipboard messages inside it.
//!
//! `docs/CLIPBOARD.md` §4 is the specification. The numbers are from
//! `spice/vd_agent.h`, which is BSD-licensed and whose constants are cited
//! here rather than its code copied, and the behaviour is what QEMU 9.2.4
//! sends and accepts in `ui/vdagent.c` -- the host half this guest talks to,
//! and the only peer it has.
//!
//! # Two layers, and why they are two modules
//!
//! [`chunk`] is the wire: a message is cut into chunks of at most
//! [`chunk::MAX_PAYLOAD`] bytes, each with an eight-byte header, and the
//! receiver puts the payloads back together into a stream of messages. It
//! knows nothing of what a message means. [`message`] is a message: a header
//! and a body whose shape depends on what the two sides agreed to in their
//! capabilities.
//!
//! # The shape a capability changes
//!
//! `VD_AGENT_CAP_CLIPBOARD_SELECTION` puts four bytes -- a selection number
//! and three of padding -- at the front of every clipboard message, and
//! `VD_AGENT_CAP_CLIPBOARD_GRAB_SERIAL` puts four more into a grab. So the
//! same message type has four layouts, and which one a peer means is not in
//! the message: it was agreed earlier. There is deliberately no way here to
//! encode or decode a clipboard message without naming that agreement, which
//! is [`message::Shape`], because a decoder that guessed would read a
//! selection number out of a type field and be believed.
//!
//! # Trust
//!
//! Nothing that arrives is trusted. A message declaring a size larger than
//! the buffer assembling it is refused rather than truncated; a selection or
//! a clipboard type that is not one of the handful defined is refused; a grab
//! listing more types than [`message::MAX_TYPES`] is refused; and a message
//! whose body is shorter than its own layout needs is refused. A message type
//! this crate does not implement is *not* an error -- it is
//! [`message::Message::Other`], because a peer announcing capabilities this
//! agent never asked for is entitled to send what they describe, and the
//! answer to those is to ignore them.
//!
//! Nothing here sends, waits or allocates: buffers come from the caller.
//!
//! # Example
//!
//! ```
//! use ferrix_vdagent::message::{ClipboardType, Message, Selection, Shape};
//!
//! // The shape QEMU's `qemu-vdagent` with `clipboard=on` agrees to.
//! let shape = Shape::QEMU_CLIPBOARD;
//! let mut out = [0_u8; 64];
//!
//! let written = Message::ClipboardRequest {
//!     selection: Selection::Clipboard,
//!     kind: ClipboardType::Utf8Text,
//! }
//! .encode(shape, &mut out)?;
//!
//! assert!(matches!(
//!     Message::decode(&out[..written], shape)?,
//!     Message::ClipboardRequest { selection: Selection::Clipboard, .. }
//! ));
//! # Ok::<(), ferrix_vdagent::Error>(())
//! ```

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

pub mod chunk;
pub mod message;

#[cfg(test)]
mod tests;

/// Where `user/vport` offers the port, and where `compositor/vdagent` looks
/// for it.
///
/// The one thing the driver and the agent must agree on that is not the
/// protocol, so it lives in the crate they both already depend on rather
/// than in a third one holding a single string. It is an absolute path at
/// the root because a Ferrix guest has no `XDG_RUNTIME_DIR` and no `/run`:
/// the compositor's own Wayland socket is bound the same way
/// (`compositor/socket`), and a driver that cannot make a directory should
/// not need to.
pub const SOCKET_PATH: &[u8] = b"/vport";

/// Why a message or a chunk was refused.
///
/// Every variant carries what was seen, because the one thing a person
/// debugging an agent needs is the number that did not fit, and a log line
/// saying "malformed" costs an hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes than the layout needs: `want` at this point, `have` left.
    Short {
        /// Bytes the layout needs.
        want: usize,
        /// Bytes there are.
        have: usize,
    },
    /// The message header's `protocol` field was not [`message::PROTOCOL`].
    Protocol(u32),
    /// The declared body size is larger than the buffer assembling it.
    TooLong {
        /// Bytes the sender declared.
        declared: usize,
        /// Bytes there is room for.
        limit: usize,
    },
    /// A selection number that is neither clipboard nor primary.
    Selection(u8),
    /// A clipboard type number this crate does not carry.
    Type(u32),
    /// A grab listing more types than [`message::MAX_TYPES`].
    TooManyTypes(usize),
    /// A reassembler that refused something earlier: the stream's framing is
    /// no longer known to be at a boundary, so nothing after it is believed.
    Broken,
}

impl fmt::Display for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Short { want, have } => write!(out, "needs {want} bytes, has {have}"),
            Error::Protocol(saw) => write!(out, "protocol {saw}, not {}", message::PROTOCOL),
            Error::TooLong { declared, limit } => {
                write!(out, "a {declared}-byte message, with room for {limit}")
            }
            Error::Selection(saw) => write!(out, "selection {saw}"),
            Error::Type(saw) => write!(out, "clipboard type {saw}"),
            Error::TooManyTypes(saw) => {
                write!(out, "{saw} types, at most {}", message::MAX_TYPES)
            }
            Error::Broken => out.write_str("the stream was refused earlier"),
        }
    }
}

/// The four bytes at `offset`, little-endian, if they are there.
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let end = offset + 4;
    let field = bytes.get(offset..end).ok_or(Error::Short {
        want: end,
        have: bytes.len(),
    })?;
    let mut word = [0_u8; 4];
    word.copy_from_slice(field);
    Ok(u32::from_le_bytes(word))
}

/// Write `value` little-endian at `offset`.
///
/// Checked rather than indexed, because this tree denies a slice that may
/// panic: a caller that has already sized its buffer still writes through
/// this, and the `?` it costs is cheaper than the one site that turns out not
/// to have sized it.
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), Error> {
    let end = offset + 4;
    let have = bytes.len();
    let slot = bytes
        .get_mut(offset..end)
        .ok_or(Error::Short { want: end, have })?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// The bytes from `at` to `end`, or [`Error::Short`].
fn part(bytes: &[u8], at: usize, end: usize) -> Result<&[u8], Error> {
    bytes.get(at..end).ok_or(Error::Short {
        want: end,
        have: bytes.len(),
    })
}

/// The bytes from `at` to `end`, to be written into, or [`Error::Short`].
fn part_mut(bytes: &mut [u8], at: usize, end: usize) -> Result<&mut [u8], Error> {
    let have = bytes.len();
    bytes
        .get_mut(at..end)
        .ok_or(Error::Short { want: end, have })
}
