//! Network wire formats, read and written as bytes.
//!
//! The roadmap's networking section puts the byte-level half of the net core in
//! `libs/`, host-tested and fuzzed before the kernel calls it, for the reason
//! the continuous rule gives: a packet is bytes someone else chose. This crate
//! is the first of that half — the headers. The TCP state machine, netlink and
//! virtio-net's device protocol are crates of their own that build on it.
//!
//! # The shape of every header
//!
//! Each module has a header type that is a plain value, and two functions:
//!
//! * `parse` takes the bytes a lower layer handed up and returns the header and
//!   the bytes after it, borrowed from the input. Nothing is copied except the
//!   fixed-size fields, and nothing is allocated.
//! * `emit` writes the header into a buffer the caller owns and says how many
//!   bytes it took, so a packet is assembled front to back in one buffer.
//!
//! Every input is answered with a value or an [`Error`]: no length, offset or
//! field value makes a parse panic, index out of bounds, or read past the end
//! of the slice. The fuzz target holds the crate to that, and to parse and emit
//! agreeing.
//!
//! # Checksums are checked where they are defined
//!
//! A parse verifies the checksum its format carries — IPv4's header checksum,
//! and the ICMP, UDP and TCP ones, the last three over the pseudo-header of
//! [`checksum::Pseudo`] — and answers a mismatch with [`Error::BadChecksum`], so
//! a header that parses is one whose bytes arrived as they were sent. An emit
//! computes the same checksum and writes it.
//!
//! # No unsafe
//!
//! Every field is a bounds-checked slice and a big-endian decode, so the crate
//! carries `#![forbid(unsafe_code)]`. Casting bytes to `repr(C)` structures
//! would be shorter, and would put the first code that reads a stranger's
//! packet inside an `unsafe` block for nothing.
//!
//! ```
//! use ferrix_netwire::ethernet;
//!
//! # fn main() -> Result<(), ferrix_netwire::Error> {
//! let frame = [
//!     0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // destination: broadcast
//!     0x52, 0x54, 0x00, 0x12, 0x34, 0x56, // source
//!     0x08, 0x06, // EtherType: ARP
//!     0x00, 0x01, // ...the ARP packet follows
//! ];
//! let (header, payload) = ethernet::Header::parse(&frame)?;
//! assert_eq!(header.ethertype, ethernet::ethertype::ARP);
//! assert_eq!(payload, &[0x00, 0x01]);
//! # Ok(())
//! # }
//! ```

#![no_std]
#![forbid(unsafe_code)]

pub mod arp;
pub mod checksum;
pub mod ethernet;
pub mod icmpv4;
pub mod icmpv6;
pub mod ipv4;
pub mod ipv6;
pub mod ndp;
pub mod tcp;
pub mod udp;
mod wire;

#[cfg(test)]
mod tests;

/// Why bytes could not be read, or a header could not be written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Fewer bytes than the header, or the length it declares, needs.
    Truncated,
    /// A field holds a value the format does not allow; the text names it.
    Malformed(&'static str),
    /// A checksum the format carries does not verify.
    BadChecksum,
    /// The output buffer is smaller than what is being written.
    NoSpace,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Truncated => f.write_str("the packet is shorter than its header says"),
            Error::Malformed(what) => write!(f, "malformed header: {what}"),
            Error::BadChecksum => f.write_str("a checksum does not verify"),
            Error::NoSpace => f.write_str("the output buffer is too small"),
        }
    }
}
