//! Wayland's wire protocol, in Rust, with no libwayland.
//!
//! This is the bottom of the compositor's server: the bytes a client's
//! `AF_UNIX` socket carries, and the map from the object ids in them to the
//! things they name. It holds no socket and no descriptor of its own -- a
//! descriptor is an `i32` here and nothing more -- so it is host-tested and
//! fuzzed the way `libs/netwire` and `libs/inputctl` are, and the server
//! above it is the only part that has to run on Ferrix to be tried.
//!
//! Written from `/usr/share/wayland/wayland.xml` and the format
//! `wayland-util.c`, `connection.c` and `wire.c` implement, not from a
//! summary of them.
//!
//! # The format
//!
//! A message is a header and then its arguments, each padded to four bytes:
//!
//! * **The header is eight bytes:** the sender's object id, then a word
//!   holding the total size of the message in its high sixteen bits and the
//!   opcode in its low sixteen. The size counts the header, so the smallest
//!   message is eight bytes and the largest [`MAX_MESSAGE`].
//! * **`int` and `uint`** are one word. **`fixed`** is one word of
//!   [`Fixed`], Wayland's 24.8 signed fixed point.
//! * **`object`** is one word, an object id, and `0` where the argument is
//!   allowed to be null. **`new_id`** is one word too, except for the
//!   argument of `wl_registry.bind`, whose interface the protocol does not
//!   name: that one is a string, a version and an id.
//! * **`string`** is a word of length -- the bytes *with* their terminating
//!   NUL -- then those bytes, then zeros up to a multiple of four. Length
//!   zero is a null string, which is not the same as an empty one.
//! * **`array`** is a word of length without any NUL, then the bytes and the
//!   same padding.
//! * **`fd`** takes no room in the stream at all: it travels as a
//!   `SCM_RIGHTS` control message beside the bytes, and the reader takes the
//!   next one that arrived.
//!
//! The words are in the machine's own byte order, because both ends of the
//! socket are on one machine; every architecture Ferrix builds for is
//! little-endian, so this crate writes little-endian and says so, rather
//! than depending on the host that ran the test.
//!
//! # Nothing here knows what a message means
//!
//! The wire format is untyped: the same eight bytes are a different message
//! for a different object. So a reader is given the [`Signature`] of what it
//! is about to read, and the interface tables that say which signature goes
//! with which object and opcode are [`Interface`] values the crate above
//! builds. The one rule this crate keeps by itself is that ids stay in their
//! halves: a client names its objects from 1, a server from
//! [`ObjectId::SERVER_BASE`], and neither may make an object in the other's
//! range ([`Objects`]).
//!
//! # Where it is stricter than libwayland, and where it must not be
//!
//! A message whose arguments do not use every byte its header claimed is
//! refused. libwayland reads the arguments its signature names and consumes
//! the rest without looking, so a client could put anything after them. No
//! client does -- `serialize_closure` writes exactly the size it computed --
//! and the check catches a wrong signature in this crate's own tables loudly
//! rather than letting it misread the next message.
//!
//! The padding after a string or an array is *not* looked at, and must not
//! be. libwayland copies the bytes and steps its cursor on by the rounded-up
//! length, leaving the padding as whatever the buffer held; `wl_closure_send`
//! zeroes that buffer with `zalloc` but `wl_closure_queue`, the path a queued
//! request takes, uses `malloc`. A server that required zero padding would
//! drop real clients at random.

mod arg;
mod fixed;
mod interface;
mod message;
mod objects;

pub use arg::{Arg, ArgType, Fd, Signature};
pub use fixed::Fixed;
pub use interface::{Interface, Method};
pub use message::{Error, Header, Reader, Writer};
pub use objects::{Entry, ObjectError, ObjectId, Objects};

/// The largest message the wire format can carry: the size field is sixteen
/// bits, and libwayland's own buffer is 4096 bytes, so nothing larger is
/// ever sent. A message claiming more is refused.
pub const MAX_MESSAGE: usize = 4096;

/// The header's length in bytes: the sender and the size-and-opcode word.
pub const HEADER_BYTES: usize = 8;

/// The most descriptors one `sendmsg` carries, as `MAX_FDS_OUT` in
/// `connection.c`. A client that sends more in one message is misbehaving.
pub const MAX_FDS: usize = 28;

/// Round `len` up to the four-byte boundary the format pads every argument
/// to.
#[must_use]
pub const fn padded(len: usize) -> usize {
    len.next_multiple_of(4)
}

#[cfg(test)]
mod tests;
