//! The TCP state machine, as a pure function of segments and time.
//!
//! `libs/netwire` reads and writes a TCP header. This crate is what decides
//! which headers to write, in what order, and what to do with the ones that
//! arrive: the eleven states of RFC 9293, reassembly of what came out of
//! order, retransmission when nothing is acknowledged, and the congestion
//! arithmetic of RFC 5681 and RFC 6582.
//!
//! # Nothing here touches a network
//!
//! A [`Connection`] is driven by four calls and holds no clock, no socket and
//! no address:
//!
//! * [`Connection::on_segment`] takes a segment that arrived, with the time it
//!   arrived at.
//! * [`Connection::poll_transmit`] answers the next segment to send, copying
//!   its payload into a buffer the caller owns.
//! * [`Connection::poll_at`] says when the connection next has something to do.
//! * [`Connection::on_timer`] lets the clock reach a moment.
//!
//! Addresses belong to the layer above, which is why the header this crate
//! produces is checksummed by its caller against a pseudo-header the caller
//! knows and this one does not.
//!
//! That shape is the whole point. `cargo test` drives two connections against
//! each other over a channel it controls, dropping, delaying and reordering
//! segments as it likes, at a clock it advances by hand -- a loss recovery
//! test that would need a lossy network is a loop in this crate's tests
//! instead.
//!
//! # What it implements
//!
//! * The state machine of RFC 9293 section 3.10, in its order, including
//!   simultaneous open and simultaneous close.
//! * Reassembly of out-of-order data, bounded by the receive buffer.
//! * Window scaling (RFC 7323 section 2) and the maximum segment size option.
//! * Selective acknowledgment blocks (RFC 2018) for what arrived out of
//!   order. Blocks that arrive are not used to guide retransmission: this end
//!   retransmits from the oldest unacknowledged byte, which is always correct
//!   and sometimes sends more than it had to.
//! * Slow start, congestion avoidance, fast retransmit and fast recovery.
//! * Retransmission timing by RFC 6298, with Karn's algorithm and Linux's
//!   bounds.
//! * Nagle's algorithm, delayed acknowledgments, silly-window avoidance and
//!   the zero-window probe.
//! * `TIME-WAIT`, and a `FIN-WAIT-2` that does not wait for ever.
//!
//! # What it does not
//!
//! * Urgent data. The urgent pointer is parsed and ignored, as RFC 6093
//!   recommends every implementation now do.
//! * Timestamps and PAWS. Not offering the option means no peer uses it, which
//!   is legal; the cost is that a sequence number wrapping inside one segment
//!   lifetime is not detected, which needs a gigabit of the same connection.
//! * Explicit congestion notification, keepalives, and TCP fast open.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod congestion;
pub mod conn;
mod deliver;
mod input;
mod output;
pub mod ring;
pub mod rtt;
pub mod seq;
pub mod state;

pub use conn::{Config, Connection, Progress, Request};
pub use output::Transmit;
pub use rtt::Millis;
pub use seq::SeqNumber;
pub use state::{Failure, State};

#[cfg(test)]
mod tests;
