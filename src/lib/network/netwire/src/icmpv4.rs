//! `ICMPv4` messages, RFC 792.
//!
//! A message is a type, a code, a checksum over the whole message, four bytes
//! whose meaning the type decides, and a body. A parse verifies the checksum and
//! hands back the rest; echo's identifier and sequence are read out for the one
//! message the net core answers by itself.

use crate::Error;
use crate::checksum;
use crate::wire;

/// Bytes before the body: type, code, checksum and the four type-specific bytes.
pub const HEADER_LEN: usize = 8;

/// The message types the net core sends or answers.
pub mod kind {
    /// Echo reply.
    pub const ECHO_REPLY: u8 = 0;
    /// Destination unreachable.
    pub const DESTINATION_UNREACHABLE: u8 = 3;
    /// Echo request.
    pub const ECHO_REQUEST: u8 = 8;
    /// Time exceeded.
    pub const TIME_EXCEEDED: u8 = 11;
    /// Parameter problem.
    pub const PARAMETER_PROBLEM: u8 = 12;
}

/// An `ICMPv4` header, less the checksum a parse checks and an emit computes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// The message type; see [`kind`].
    pub kind: u8,
    /// The code, whose meaning the type decides.
    pub code: u8,
    /// The four bytes after the checksum, whose meaning the type decides.
    pub rest: [u8; 4],
}

/// A parsed message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Message<'a> {
    /// The header.
    pub header: Header,
    /// Everything after it.
    pub body: &'a [u8],
}

impl Header {
    /// Read the message that is all of `bytes`, as an IPv4 payload bounded by
    /// its total length.
    pub fn parse(bytes: &[u8]) -> Result<Message<'_>, Error> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Truncated);
        }
        if checksum::checksum(bytes) != 0 {
            return Err(Error::BadChecksum);
        }
        Ok(Message {
            header: Header {
                kind: wire::byte(bytes, 0).ok_or(Error::Truncated)?,
                code: wire::byte(bytes, 1).ok_or(Error::Truncated)?,
                rest: wire::array(bytes, 4).ok_or(Error::Truncated)?,
            },
            body: bytes.get(HEADER_LEN..).ok_or(Error::Truncated)?,
        })
    }

    /// An echo request or reply header, `kind` being one of the two.
    #[must_use]
    pub const fn echo(kind: u8, identifier: u16, sequence: u16) -> Header {
        let id = identifier.to_be_bytes();
        let seq = sequence.to_be_bytes();
        Header {
            kind,
            code: 0,
            rest: [id[0], id[1], seq[0], seq[1]],
        }
    }

    /// An echo message's identifier and sequence number; `None` for any other.
    #[must_use]
    pub const fn echo_fields(&self) -> Option<(u16, u16)> {
        match self.kind {
            kind::ECHO_REQUEST | kind::ECHO_REPLY if self.code == 0 => Some((
                u16::from_be_bytes([self.rest[0], self.rest[1]]),
                u16::from_be_bytes([self.rest[2], self.rest[3]]),
            )),
            _ => None,
        }
    }

    /// Write the message carrying `body` at the start of `out`, checksum
    /// included, returning its length.
    pub fn emit(&self, body: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        let len = HEADER_LEN.checked_add(body.len()).ok_or(Error::NoSpace)?;
        let message = out.get_mut(..len).ok_or(Error::NoSpace)?;
        let fields: [(usize, &[u8]); 4] = [
            (0, &[self.kind, self.code]),
            (2, &[0, 0]),
            (4, &self.rest),
            (HEADER_LEN, body),
        ];
        for (at, field) in fields {
            wire::put(message, at, field).ok_or(Error::NoSpace)?;
        }
        let sum = checksum::checksum(message);
        wire::put(message, 2, &sum.to_be_bytes()).ok_or(Error::NoSpace)?;
        Ok(len)
    }
}
