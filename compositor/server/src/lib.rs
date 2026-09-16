//! The Wayland server: what a client's requests do.
//!
//! This crate is the protocol half of the compositor and holds no socket, no
//! descriptor and no pixel. A [`Client`] is handed the bytes that arrived and
//! the descriptors that came with them, and gives back the bytes to send; what
//! moves those bytes is the binary above it. So the whole of the protocol --
//! object lifetimes, versions, every way a client can break the rules and what
//! the server answers -- is host-tested, and running it on Ferrix tests the
//! socket rather than the protocol.
//!
//! # How a request is answered
//!
//! [`Client::read`] takes whole messages out of a buffer. For each one it
//! finds the object the message is addressed to, reads the signature of that
//! interface's request at that opcode, decodes the arguments and hands them to
//! the handler for the object's role. A handler may create objects, destroy
//! them, queue events, and tell the compositor above that something changed.
//!
//! # A protocol error is the end of the connection
//!
//! Wayland has no way to refuse one request and carry on: `wl_display.error`
//! names the object, a code its interface defines and a sentence, and the
//! connection is finished. So every refusal here goes through
//! [`Client::fail`], which queues that event once and stops reading. The
//! caller writes out what is queued and closes the socket. Nothing is answered
//! after the first error, which is what libwayland does and what a client
//! expects: the alternative, answering the rest of the buffer, hands a client
//! that already broke the rules more state to break.
//!
//! # What is here and what is not
//!
//! `wl_display` and `wl_registry` are here. The globals a client can bind are
//! declared in [`Globals`], and binding one is answered by the role table in
//! [`Role`]; the roles that draw -- surfaces, buffers, shells -- land after
//! this.

mod client;
mod globals;
mod role;

pub use client::{Client, Event, Fatal, Outgoing};
pub use globals::{Global, Globals};
pub use role::Role;

pub use compositor_protocol as protocol;
pub use compositor_wire as wire;

#[cfg(test)]
mod tests;
