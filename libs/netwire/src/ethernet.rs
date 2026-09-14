//! Ethernet II frames, with at most one IEEE 802.1Q tag.
//!
//! A frame as virtio-net delivers it: destination, source, an optional VLAN
//! tag, and an `EtherType`, with no preamble and no frame check sequence. A type
//! field below `0x0600` is an IEEE 802.3 length instead, which nothing this
//! kernel runs over sends, and is refused rather than misread as a protocol.

use crate::Error;
use crate::wire;

/// Bytes in an untagged Ethernet II header.
pub const HEADER_LEN: usize = 14;

/// Bytes an 802.1Q tag adds.
pub const VLAN_TAG_LEN: usize = 4;

/// A 48-bit MAC address.
pub type Mac = [u8; 6];

/// The broadcast address.
pub const BROADCAST: Mac = [0xFF; 6];

/// The `EtherType` values the net core dispatches on.
pub mod ethertype {
    /// IPv4, RFC 894.
    pub const IPV4: u16 = 0x0800;
    /// ARP, RFC 826.
    pub const ARP: u16 = 0x0806;
    /// An IEEE 802.1Q tag follows.
    pub const VLAN: u16 = 0x8100;
    /// IPv6, RFC 2464.
    pub const IPV6: u16 = 0x86DD;
}

/// The smallest value an `EtherType` may take; below it, the field is a length.
const MIN_ETHERTYPE: u16 = 0x0600;

/// An IEEE 802.1Q tag's control information.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VlanTag {
    /// Priority code point, 0 to 7.
    pub priority: u8,
    /// Drop eligible indicator.
    pub drop_eligible: bool,
    /// VLAN identifier, 0 to 4095.
    pub id: u16,
}

/// An Ethernet II header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Where the frame is going.
    pub destination: Mac,
    /// Where it came from.
    pub source: Mac,
    /// Its VLAN tag, if it carries one.
    pub vlan: Option<VlanTag>,
    /// What the payload is.
    pub ethertype: u16,
}

impl Header {
    /// Read the header at the start of `frame`, and the payload after it.
    pub fn parse(frame: &[u8]) -> Result<(Header, &[u8]), Error> {
        let destination = wire::array(frame, 0).ok_or(Error::Truncated)?;
        let source = wire::array(frame, 6).ok_or(Error::Truncated)?;
        let first = wire::be16(frame, 12).ok_or(Error::Truncated)?;

        let (vlan, ethertype, len) = if first == ethertype::VLAN {
            let control = wire::be16(frame, 14).ok_or(Error::Truncated)?;
            let inner = wire::be16(frame, 16).ok_or(Error::Truncated)?;
            if inner == ethertype::VLAN {
                return Err(Error::Malformed("stacked 802.1Q tags"));
            }
            let tag = VlanTag {
                priority: u8::try_from(control >> 13).unwrap_or(0),
                drop_eligible: control & 0x1000 != 0,
                id: control & 0x0FFF,
            };
            (Some(tag), inner, HEADER_LEN + VLAN_TAG_LEN)
        } else {
            (None, first, HEADER_LEN)
        };

        if ethertype < MIN_ETHERTYPE {
            return Err(Error::Malformed(
                "an IEEE 802.3 length where an EtherType belongs",
            ));
        }
        let payload = frame.get(len..).ok_or(Error::Truncated)?;
        Ok((
            Header {
                destination,
                source,
                vlan,
                ethertype,
            },
            payload,
        ))
    }

    /// Bytes this header takes on the wire.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self.vlan {
            Some(_) => HEADER_LEN + VLAN_TAG_LEN,
            None => HEADER_LEN,
        }
    }

    /// Whether the header is empty; it never is, and this exists so `len` has
    /// its customary partner.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Write the header at the start of `out`, returning the bytes written.
    pub fn emit(&self, out: &mut [u8]) -> Result<usize, Error> {
        if self.ethertype < MIN_ETHERTYPE || self.ethertype == ethertype::VLAN {
            return Err(Error::Malformed("an EtherType that is a length or a tag"));
        }
        let len = self.len();
        if out.len() < len {
            return Err(Error::NoSpace);
        }
        wire::put(out, 0, &self.destination).ok_or(Error::NoSpace)?;
        wire::put(out, 6, &self.source).ok_or(Error::NoSpace)?;
        let type_at = match self.vlan {
            Some(tag) => {
                if tag.priority > 7 || tag.id > 0x0FFF {
                    return Err(Error::Malformed("a VLAN priority or id out of range"));
                }
                let control = (u16::from(tag.priority) << 13)
                    | if tag.drop_eligible { 0x1000 } else { 0 }
                    | tag.id;
                wire::put(out, 12, &ethertype::VLAN.to_be_bytes()).ok_or(Error::NoSpace)?;
                wire::put(out, 14, &control.to_be_bytes()).ok_or(Error::NoSpace)?;
                16
            }
            None => 12,
        };
        wire::put(out, type_at, &self.ethertype.to_be_bytes()).ok_or(Error::NoSpace)?;
        Ok(len)
    }
}
