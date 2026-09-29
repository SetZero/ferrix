//! IPv6 headers, RFC 8200, and the walk over extension headers to the upper
//! layer.
//!
//! The fixed header is forty bytes and carries no checksum. What makes IPv6
//! parsing a matter of care is what follows it: a chain of extension headers,
//! each naming the next, before the transport header. [`upper_layer`] walks that
//! chain to its end with a bound on its length, checks each header fits, keeps
//! the fragment header's fields, and refuses a hop-by-hop header anywhere but
//! first, as RFC 8200 section 4.1 requires.

use crate::Error;
use crate::wire;

/// Bytes in the fixed header.
pub const HEADER_LEN: usize = 40;

/// The most extension headers [`upper_layer`] walks before giving up. A chain
/// longer than this is not one any stack sends, and bounding it bounds the work
/// a single packet can ask for.
pub const MAX_EXTENSION_HEADERS: usize = 8;

/// Next header values: the extension headers the walk knows, and the upper
/// layers the net core dispatches on.
pub mod next_header {
    /// Hop-by-hop options.
    pub const HOP_BY_HOP: u8 = 0;
    /// TCP.
    pub const TCP: u8 = 6;
    /// UDP.
    pub const UDP: u8 = 17;
    /// Routing header.
    pub const ROUTING: u8 = 43;
    /// Fragment header.
    pub const FRAGMENT: u8 = 44;
    /// Encapsulating security payload, opaque to the walk.
    pub const ESP: u8 = 50;
    /// Authentication header.
    pub const AUTHENTICATION: u8 = 51;
    /// `ICMPv6`.
    pub const ICMPV6: u8 = 58;
    /// No next header.
    pub const NO_NEXT_HEADER: u8 = 59;
    /// Destination options.
    pub const DESTINATION_OPTIONS: u8 = 60;
}

/// The largest flow label, twenty bits.
const MAX_FLOW_LABEL: u32 = 0x000F_FFFF;

/// An IPv6 fixed header's fields, less the payload length a parse checks and an
/// emit computes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Traffic class.
    pub traffic_class: u8,
    /// Flow label, twenty bits.
    pub flow_label: u32,
    /// What follows the fixed header; see [`next_header`].
    pub next_header: u8,
    /// Hop limit.
    pub hop_limit: u8,
    /// Source address.
    pub source: [u8; 16],
    /// Destination address.
    pub destination: [u8; 16],
}

/// A parsed packet: the fixed header and the payload its length bounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Packet<'a> {
    /// The fixed header.
    pub header: Header,
    /// Everything after it, extension headers included.
    pub payload: &'a [u8],
}

/// A fragment header's fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fragment {
    /// Where this fragment's data starts, in eight-byte units.
    pub offset: u16,
    /// More fragments follow.
    pub more: bool,
    /// Identification, shared by a datagram's fragments.
    pub identification: u32,
}

/// Where the extension-header chain ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UpperLayer<'a> {
    /// The next header value of what follows the last extension header.
    pub protocol: u8,
    /// Where it starts, counted from the start of the payload.
    pub offset: usize,
    /// Its bytes, to the end of the payload.
    pub bytes: &'a [u8],
    /// The fragment header on the way, if there was one.
    pub fragment: Option<Fragment>,
}

impl Header {
    /// Read the packet at the start of `bytes`. Bytes past its payload length,
    /// such as Ethernet padding, are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Packet<'_>, Error> {
        let first = wire::be32(bytes, 0).ok_or(Error::Truncated)?;
        if first >> 28 != 6 {
            return Err(Error::Malformed("an IP version other than 6"));
        }
        let payload_len = usize::from(wire::be16(bytes, 4).ok_or(Error::Truncated)?);
        let end = HEADER_LEN + payload_len;
        let payload = bytes.get(HEADER_LEN..end).ok_or(Error::Truncated)?;
        let header = Header {
            traffic_class: u8::try_from((first >> 20) & 0xFF).unwrap_or(0),
            flow_label: first & MAX_FLOW_LABEL,
            next_header: wire::byte(bytes, 6).ok_or(Error::Truncated)?,
            hop_limit: wire::byte(bytes, 7).ok_or(Error::Truncated)?,
            source: wire::array(bytes, 8).ok_or(Error::Truncated)?,
            destination: wire::array(bytes, 24).ok_or(Error::Truncated)?,
        };
        Ok(Packet { header, payload })
    }

    /// Write the fixed header for a payload of `payload_len` bytes at the start
    /// of `out`, returning [`HEADER_LEN`].
    pub fn emit(&self, payload_len: usize, out: &mut [u8]) -> Result<usize, Error> {
        if self.flow_label > MAX_FLOW_LABEL {
            return Err(Error::Malformed("a flow label over twenty bits"));
        }
        let length = u16::try_from(payload_len)
            .map_err(|_| Error::Malformed("a payload over 65535 bytes"))?;
        let header = out.get_mut(..HEADER_LEN).ok_or(Error::NoSpace)?;
        let first = (6u32 << 28) | (u32::from(self.traffic_class) << 20) | self.flow_label;
        let fields: [(usize, &[u8]); 5] = [
            (0, &first.to_be_bytes()),
            (4, &length.to_be_bytes()),
            (6, &[self.next_header, self.hop_limit]),
            (8, &self.source),
            (24, &self.destination),
        ];
        for (at, field) in fields {
            wire::put(header, at, field).ok_or(Error::NoSpace)?;
        }
        Ok(HEADER_LEN)
    }
}

/// Walk the extension headers of a payload whose first header is `first`, to
/// the upper layer.
pub fn upper_layer(first: u8, payload: &[u8]) -> Result<UpperLayer<'_>, Error> {
    let mut next = first;
    let mut at = 0usize;
    let mut fragment = None;
    for index in 0..=MAX_EXTENSION_HEADERS {
        let rest = payload.get(at..).ok_or(Error::Truncated)?;
        let len = match next {
            next_header::HOP_BY_HOP if index != 0 => {
                return Err(Error::Malformed("a hop-by-hop header that is not first"));
            }
            next_header::HOP_BY_HOP | next_header::ROUTING | next_header::DESTINATION_OPTIONS => {
                (usize::from(wire::byte(rest, 1).ok_or(Error::Truncated)?) + 1) * 8
            }
            next_header::FRAGMENT => {
                if fragment.is_some() {
                    return Err(Error::Malformed("two fragment headers"));
                }
                let field = wire::be16(rest, 2).ok_or(Error::Truncated)?;
                fragment = Some(Fragment {
                    offset: field >> 3,
                    more: field & 1 != 0,
                    identification: wire::be32(rest, 4).ok_or(Error::Truncated)?,
                });
                8
            }
            next_header::AUTHENTICATION => {
                (usize::from(wire::byte(rest, 1).ok_or(Error::Truncated)?) + 2) * 4
            }
            protocol => {
                return Ok(UpperLayer {
                    protocol,
                    offset: at,
                    bytes: rest,
                    fragment,
                });
            }
        };
        if rest.len() < len {
            return Err(Error::Truncated);
        }
        next = wire::byte(rest, 0).ok_or(Error::Truncated)?;
        at += len;
    }
    Err(Error::Malformed(
        "more extension headers than the walk allows",
    ))
}
