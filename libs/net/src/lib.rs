//! The net core: a host's networking, between a socket and an interface.
//!
//! `libs/netwire` reads and writes the headers. `libs/nettcp` decides which
//! TCP segments to send. This crate is everything between them and a program:
//! interfaces and the addresses on them, a routing table, a neighbour cache,
//! fragment reassembly, ICMP, UDP, and the socket table that says which packet
//! belongs to whom.
//!
//! # It is a value, not a subsystem
//!
//! A [`Stack`] holds no lock, no clock, no thread and no device. Four calls
//! drive it:
//!
//! * [`Stack::receive`] takes a frame that arrived on an interface.
//! * [`Stack::poll_transmit`] answers the next frame to put on one.
//! * [`Stack::poll_at`] says when there is next something to do.
//! * [`Stack::on_timer`] lets the clock reach a moment.
//!
//! and the socket calls -- bind, listen, accept, connect, send, recv -- are
//! ordinary methods on it. The kernel supplies the lock, the timer and the
//! driver; `cargo test` supplies a wire it controls and a clock it advances by
//! hand, which is how a routing decision or a lost fragment is tested without
//! a network.
//!
//! # What it does
//!
//! * IPv4 and IPv6, with fragmentation and reassembly on IPv4.
//! * ARP, and the Neighbor Discovery half of ICMPv6 that answers the same
//!   question.
//! * ICMP echo in both directions, including the unprivileged echo socket
//!   `ping` uses, and the unreachable messages a closed port earns.
//! * UDP, with the socket-matching rules Linux uses, so a connected socket
//!   still receives when somebody opens a wildcard one.
//! * TCP over [`ferrix_nettcp`]: connections, listeners with a backlog, and a
//!   reset for a segment with nowhere to go.
//! * `AF_INET6` sockets that carry IPv4 through `::ffff:0:0/96` unless
//!   `IPV6_V6ONLY` says otherwise.
//!
//! # What it does not
//!
//! * Forward. This is a host, not a router: a packet for somebody else is
//!   dropped and counted.
//! * Configure itself. Addresses and routes come from `ip`, which is the
//!   roadmap's exit criterion; router advertisements are parsed and ignored.
//! * Multicast groups beyond the ones a host must join: no IGMP, no MLD.
//! * IPv6 fragmentation, in either direction.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod addr;
mod demux;
pub mod iface;
mod input;
pub mod neighbor;
mod output;
mod poll;
pub mod ports;
pub mod rand;
pub mod reassembly;
pub mod route;
pub mod socket;
pub mod stack;
mod tcpin;
mod transfer;

pub use addr::{Endpoint, IpAddress, IpCidr, Ipv4, Ipv6};
pub use iface::{Interface, Medium};
pub use route::{Route, Routes};
pub use socket::{Error, Family, Readiness, Shutdown, Socket, SocketId};
pub use stack::{Config, Millis, Outgoing, Stack};
pub use transfer::{Received, to_v6};

#[cfg(test)]
mod tests;
