//! UDP datagrams, RFC 768, over IPv4 or IPv6.
//!
//! The length field bounds the datagram, so bytes after it in the IP payload
//! are not taken for data, and the checksum is verified over the pseudo-header
//! of the IP version it arrived in. A zero checksum means "none" over IPv4 and
//! is accepted there; over IPv6 the checksum is mandatory (RFC 8200 section
//! 8.1) and a zero is refused.

use crate::Error;
use crate::checksum::Pseudo;
use crate::wire;

/// Bytes in the header.
pub const HEADER_LEN: usize = 8;

/// The IP protocol number, and IPv6 next header value, of UDP.
pub const PROTOCOL: u8 = 17;

/// A UDP header's ports; the length and checksum are checked by a parse and
/// computed by an emit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Source port.
    pub source_port: u16,
    /// Destination port.
    pub destination_port: u16,
}

/// A parsed datagram.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Datagram<'a> {
    /// The ports.
    pub header: Header,
    /// The checksum field as it arrived, zero when an IPv4 sender sent none.
    pub checksum: u16,
    /// The data, up to the length field.
    pub payload: &'a [u8],
}

impl Header {
    /// Read the datagram at the start of `bytes`, which arrived with the
    /// addresses in `pseudo`.
    pub fn parse(bytes: &[u8], pseudo: Pseudo) -> Result<Datagram<'_>, Error> {
        let length = usize::from(wire::be16(bytes, 4).ok_or(Error::Truncated)?);
        if length < HEADER_LEN {
            return Err(Error::Malformed("a UDP length shorter than its header"));
        }
        let datagram = bytes.get(..length).ok_or(Error::Truncated)?;
        let checksum = wire::be16(bytes, 6).ok_or(Error::Truncated)?;
        match (checksum, pseudo) {
            (0, Pseudo::V4 { .. }) => {}
            (0, Pseudo::V6 { .. }) => {
                return Err(Error::Malformed("a zero UDP checksum over IPv6"));
            }
            _ => {
                let mut sum = pseudo.sum(PROTOCOL, length).ok_or(Error::Malformed(
                    "a UDP length the pseudo-header cannot hold",
                ))?;
                sum.add_bytes(datagram);
                if sum.finish() != 0 {
                    return Err(Error::BadChecksum);
                }
            }
        }
        Ok(Datagram {
            header: Header {
                source_port: wire::be16(bytes, 0).ok_or(Error::Truncated)?,
                destination_port: wire::be16(bytes, 2).ok_or(Error::Truncated)?,
            },
            checksum,
            payload: datagram.get(HEADER_LEN..).ok_or(Error::Truncated)?,
        })
    }

    /// Write the datagram carrying `payload`, to be sent with the addresses in
    /// `pseudo`, at the start of `out`, returning its length.
    ///
    /// The checksum is always computed, and a computed zero is sent as all
    /// ones, as RFC 768 says, so the datagram is never read as unchecksummed.
    pub fn emit(&self, payload: &[u8], pseudo: Pseudo, out: &mut [u8]) -> Result<usize, Error> {
        let length = HEADER_LEN
            .checked_add(payload.len())
            .ok_or(Error::Malformed("a datagram over 65535 bytes"))?;
        let wire_length =
            u16::try_from(length).map_err(|_| Error::Malformed("a datagram over 65535 bytes"))?;
        let datagram = out.get_mut(..length).ok_or(Error::NoSpace)?;
        let fields: [(usize, &[u8]); 5] = [
            (0, &self.source_port.to_be_bytes()),
            (2, &self.destination_port.to_be_bytes()),
            (4, &wire_length.to_be_bytes()),
            (6, &[0, 0]),
            (HEADER_LEN, payload),
        ];
        for (at, field) in fields {
            wire::put(datagram, at, field).ok_or(Error::NoSpace)?;
        }
        let mut sum = pseudo
            .sum(PROTOCOL, length)
            .ok_or(Error::Malformed("a datagram the pseudo-header cannot hold"))?;
        sum.add_bytes(datagram);
        let checksum = match sum.finish() {
            0 => 0xFFFF,
            value => value,
        };
        wire::put(datagram, 6, &checksum.to_be_bytes()).ok_or(Error::NoSpace)?;
        Ok(length)
    }
}
