//! `ICMPv6` messages, RFC 4443.
//!
//! The same shape as `ICMPv4` — a type, a code, a checksum and a body — with the
//! checksum taken over the IPv6 pseudo-header as well, so a message cannot be
//! verified without the addresses it arrived with. Neighbor Discovery, which
//! rides in `ICMPv6`, is [`crate::ndp`].

use crate::Error;
use crate::checksum::Pseudo;
use crate::wire;

/// Bytes before the body: type, code and checksum.
pub const HEADER_LEN: usize = 4;

/// The IPv6 next header value of `ICMPv6`.
pub const PROTOCOL: u8 = 58;

/// The message types the net core sends or answers.
pub mod kind {
    /// Destination unreachable.
    pub const DESTINATION_UNREACHABLE: u8 = 1;
    /// Packet too big.
    pub const PACKET_TOO_BIG: u8 = 2;
    /// Time exceeded.
    pub const TIME_EXCEEDED: u8 = 3;
    /// Parameter problem.
    pub const PARAMETER_PROBLEM: u8 = 4;
    /// Echo request.
    pub const ECHO_REQUEST: u8 = 128;
    /// Echo reply.
    pub const ECHO_REPLY: u8 = 129;
    /// Router solicitation.
    pub const ROUTER_SOLICITATION: u8 = 133;
    /// Router advertisement.
    pub const ROUTER_ADVERTISEMENT: u8 = 134;
    /// Neighbor solicitation.
    pub const NEIGHBOR_SOLICITATION: u8 = 135;
    /// Neighbor advertisement.
    pub const NEIGHBOR_ADVERTISEMENT: u8 = 136;
    /// Redirect.
    pub const REDIRECT: u8 = 137;
}

/// An `ICMPv6` message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Message<'a> {
    /// The message type; see [`kind`].
    pub kind: u8,
    /// The code, whose meaning the type decides.
    pub code: u8,
    /// Everything after the checksum.
    pub body: &'a [u8],
}

impl<'a> Message<'a> {
    /// Read the message that is all of `bytes`, which arrived from `source` to
    /// `destination`.
    pub fn parse(bytes: &'a [u8], source: [u8; 16], destination: [u8; 16]) -> Result<Self, Error> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Truncated);
        }
        let mut sum = Pseudo::V6 {
            source,
            destination,
        }
        .sum(PROTOCOL, bytes.len())
        .ok_or(Error::Malformed("a message the pseudo-header cannot hold"))?;
        sum.add_bytes(bytes);
        if sum.finish() != 0 {
            return Err(Error::BadChecksum);
        }
        Ok(Message {
            kind: wire::byte(bytes, 0).ok_or(Error::Truncated)?,
            code: wire::byte(bytes, 1).ok_or(Error::Truncated)?,
            body: bytes.get(HEADER_LEN..).ok_or(Error::Truncated)?,
        })
    }

    /// Write the message, to be sent from `source` to `destination`, at the
    /// start of `out`, checksum included, returning its length.
    pub fn emit(
        &self,
        source: [u8; 16],
        destination: [u8; 16],
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let len = HEADER_LEN
            .checked_add(self.body.len())
            .ok_or(Error::NoSpace)?;
        let message = out.get_mut(..len).ok_or(Error::NoSpace)?;
        let fields: [(usize, &[u8]); 3] = [
            (0, &[self.kind, self.code]),
            (2, &[0, 0]),
            (HEADER_LEN, self.body),
        ];
        for (at, field) in fields {
            wire::put(message, at, field).ok_or(Error::NoSpace)?;
        }
        let mut sum = Pseudo::V6 {
            source,
            destination,
        }
        .sum(PROTOCOL, len)
        .ok_or(Error::Malformed("a message the pseudo-header cannot hold"))?;
        sum.add_bytes(message);
        wire::put(message, 2, &sum.finish().to_be_bytes()).ok_or(Error::NoSpace)?;
        Ok(len)
    }
}
