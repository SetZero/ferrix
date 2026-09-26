//! IPv4 headers, RFC 791.
//!
//! A parse checks what makes the bytes an IPv4 header at all — the version, a
//! header length of at least five words that fits, a total length that covers
//! the header and fits the bytes, a verifying header checksum, the reserved
//! flag clear, and options that walk to their end — and hands back the options
//! and the payload bounded by the total length, so Ethernet's padding after a
//! short packet is not taken for payload. Reassembling fragments is the net
//! core's; a header only says whether it is one.

use crate::Error;
use crate::checksum;
use crate::wire;

/// Bytes in a header with no options.
pub const MIN_HEADER_LEN: usize = 20;

/// Bytes in a header with the most options its four-bit length allows.
pub const MAX_HEADER_LEN: usize = 60;

/// The protocol numbers the net core dispatches on.
pub mod protocol {
    /// ICMP, RFC 792.
    pub const ICMP: u8 = 1;
    /// TCP, RFC 9293.
    pub const TCP: u8 = 6;
    /// UDP, RFC 768.
    pub const UDP: u8 = 17;
}

/// The largest fragment offset, in eight-byte units.
const MAX_FRAGMENT_OFFSET: u16 = 0x1FFF;

/// An IPv4 header's fields, less the lengths and checksum a parse checks and an
/// emit computes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Differentiated services code point, 0 to 63.
    pub dscp: u8,
    /// Explicit congestion notification, 0 to 3.
    pub ecn: u8,
    /// Identification, shared by a datagram's fragments.
    pub identification: u16,
    /// Don't fragment.
    pub dont_fragment: bool,
    /// More fragments follow.
    pub more_fragments: bool,
    /// Where this fragment's data starts, in eight-byte units.
    pub fragment_offset: u16,
    /// Time to live.
    pub ttl: u8,
    /// The payload's protocol; see [`protocol`].
    pub protocol: u8,
    /// Source address.
    pub source: [u8; 4],
    /// Destination address.
    pub destination: [u8; 4],
}

/// A parsed packet: the header, its options, and its payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Packet<'a> {
    /// The header.
    pub header: Header,
    /// The option bytes, possibly ending in padding.
    pub options: &'a [u8],
    /// The payload, up to the total length.
    pub payload: &'a [u8],
}

impl Header {
    /// Read the packet at the start of `bytes`.
    pub fn parse(bytes: &[u8]) -> Result<Packet<'_>, Error> {
        let first = wire::byte(bytes, 0).ok_or(Error::Truncated)?;
        if first >> 4 != 4 {
            return Err(Error::Malformed("an IP version other than 4"));
        }
        let header_len = usize::from(first & 0x0F) * 4;
        if header_len < MIN_HEADER_LEN {
            return Err(Error::Malformed("a header length below five words"));
        }
        let header_bytes = bytes.get(..header_len).ok_or(Error::Truncated)?;
        let total_len = usize::from(wire::be16(bytes, 2).ok_or(Error::Truncated)?);
        if total_len < header_len {
            return Err(Error::Malformed("a total length shorter than the header"));
        }
        let payload = bytes.get(header_len..total_len).ok_or(Error::Truncated)?;
        if checksum::checksum(header_bytes) != 0 {
            return Err(Error::BadChecksum);
        }

        let service = wire::byte(bytes, 1).ok_or(Error::Truncated)?;
        let flags = wire::be16(bytes, 6).ok_or(Error::Truncated)?;
        if flags & 0x8000 != 0 {
            return Err(Error::Malformed("the reserved flag set"));
        }
        let options = header_bytes.get(MIN_HEADER_LEN..).ok_or(Error::Truncated)?;
        check_options(options)?;

        let header = Header {
            dscp: service >> 2,
            ecn: service & 0x03,
            identification: wire::be16(bytes, 4).ok_or(Error::Truncated)?,
            dont_fragment: flags & 0x4000 != 0,
            more_fragments: flags & 0x2000 != 0,
            fragment_offset: flags & MAX_FRAGMENT_OFFSET,
            ttl: wire::byte(bytes, 8).ok_or(Error::Truncated)?,
            protocol: wire::byte(bytes, 9).ok_or(Error::Truncated)?,
            source: wire::array(bytes, 12).ok_or(Error::Truncated)?,
            destination: wire::array(bytes, 16).ok_or(Error::Truncated)?,
        };
        Ok(Packet {
            header,
            options,
            payload,
        })
    }

    /// Whether this is one fragment of a larger datagram.
    #[must_use]
    pub const fn is_fragment(&self) -> bool {
        self.more_fragments || self.fragment_offset != 0
    }

    /// Write the header with `options` for a payload of `payload_len` bytes at
    /// the start of `out`, checksum included, returning the header's length.
    ///
    /// The payload itself is the caller's to write after it.
    pub fn emit(&self, options: &[u8], payload_len: usize, out: &mut [u8]) -> Result<usize, Error> {
        if !options.len().is_multiple_of(4) || options.len() > MAX_HEADER_LEN - MIN_HEADER_LEN {
            return Err(Error::Malformed(
                "options that are not whole words or over 40 bytes",
            ));
        }
        check_options(options)?;
        if self.dscp > 63 || self.ecn > 3 || self.fragment_offset > MAX_FRAGMENT_OFFSET {
            return Err(Error::Malformed(
                "a DSCP, ECN or fragment offset out of range",
            ));
        }
        let header_len = MIN_HEADER_LEN + options.len();
        let total_len = header_len
            .checked_add(payload_len)
            .and_then(|len| u16::try_from(len).ok())
            .ok_or(Error::Malformed("a packet over 65535 bytes"))?;
        let header = out.get_mut(..header_len).ok_or(Error::NoSpace)?;

        let words = u8::try_from(header_len / 4).map_err(|_| Error::NoSpace)?;
        let flags = if self.dont_fragment { 0x4000 } else { 0 }
            | if self.more_fragments { 0x2000 } else { 0 }
            | self.fragment_offset;
        let fields: [(usize, &[u8]); 10] = [
            (0, &[0x40 | words, (self.dscp << 2) | self.ecn]),
            (2, &total_len.to_be_bytes()),
            (4, &self.identification.to_be_bytes()),
            (6, &flags.to_be_bytes()),
            (8, &[self.ttl, self.protocol]),
            (10, &[0, 0]),
            (12, &self.source),
            (16, &self.destination),
            (MIN_HEADER_LEN, options),
            (header_len, &[]),
        ];
        for (at, field) in fields {
            wire::put(header, at, field).ok_or(Error::NoSpace)?;
        }
        let sum = checksum::checksum(header);
        wire::put(header, 10, &sum.to_be_bytes()).ok_or(Error::NoSpace)?;
        Ok(header_len)
    }
}

/// Walk the options to their end: a single-byte end-of-list or no-operation,
/// or a type, a length of at least two, and that many bytes in all.
fn check_options(options: &[u8]) -> Result<(), Error> {
    let mut rest = options;
    while let Some((&kind, tail)) = rest.split_first() {
        match kind {
            0 => return Ok(()),
            1 => rest = tail,
            _ => {
                let len = usize::from(
                    *tail
                        .first()
                        .ok_or(Error::Malformed("an option without its length"))?,
                );
                if len < 2 {
                    return Err(Error::Malformed("an option length below two"));
                }
                rest = rest
                    .get(len..)
                    .ok_or(Error::Malformed("an option running past the header"))?;
            }
        }
    }
    Ok(())
}
