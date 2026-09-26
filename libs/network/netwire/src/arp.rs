//! ARP for IPv4 over Ethernet, RFC 826.
//!
//! ARP is general over hardware and protocol types, and the only pairing an
//! IPv4 host on Ethernet sees is Ethernet and IPv4, with six- and four-byte
//! addresses. Anything else is refused rather than half-read, so a packet that
//! parses is one the net core can answer.

use crate::Error;
use crate::ethernet::Mac;
use crate::wire;

/// Bytes in an Ethernet/IPv4 ARP packet. A frame may be longer, padded to
/// Ethernet's minimum; the padding is not part of the packet.
pub const PACKET_LEN: usize = 28;

/// The hardware type for Ethernet.
pub const HARDWARE_ETHERNET: u16 = 1;

/// The protocol type for IPv4, which is its `EtherType`.
pub const PROTOCOL_IPV4: u16 = crate::ethernet::ethertype::IPV4;

/// What an ARP packet asks or answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operation {
    /// "Who has the target address?"
    Request,
    /// "The sender address is at the sender MAC."
    Reply,
}

impl Operation {
    /// The operation code on the wire.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Operation::Request => 1,
            Operation::Reply => 2,
        }
    }
}

/// An Ethernet/IPv4 ARP packet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Packet {
    /// Request or reply.
    pub operation: Operation,
    /// The sender's MAC address.
    pub sender_mac: Mac,
    /// The sender's IPv4 address.
    pub sender_ip: [u8; 4],
    /// The target's MAC address; zero in a request.
    pub target_mac: Mac,
    /// The target's IPv4 address.
    pub target_ip: [u8; 4],
}

impl Packet {
    /// Read the packet at the start of `bytes`. Trailing bytes, such as
    /// Ethernet padding, are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Packet, Error> {
        if bytes.len() < PACKET_LEN {
            return Err(Error::Truncated);
        }
        let hardware = wire::be16(bytes, 0).ok_or(Error::Truncated)?;
        let protocol = wire::be16(bytes, 2).ok_or(Error::Truncated)?;
        let hardware_len = wire::byte(bytes, 4).ok_or(Error::Truncated)?;
        let protocol_len = wire::byte(bytes, 5).ok_or(Error::Truncated)?;
        if hardware != HARDWARE_ETHERNET || hardware_len != 6 {
            return Err(Error::Malformed("an ARP hardware type other than Ethernet"));
        }
        if protocol != PROTOCOL_IPV4 || protocol_len != 4 {
            return Err(Error::Malformed("an ARP protocol type other than IPv4"));
        }
        let operation = match wire::be16(bytes, 6).ok_or(Error::Truncated)? {
            1 => Operation::Request,
            2 => Operation::Reply,
            _ => {
                return Err(Error::Malformed(
                    "an ARP operation other than request or reply",
                ));
            }
        };
        Ok(Packet {
            operation,
            sender_mac: wire::array(bytes, 8).ok_or(Error::Truncated)?,
            sender_ip: wire::array(bytes, 14).ok_or(Error::Truncated)?,
            target_mac: wire::array(bytes, 18).ok_or(Error::Truncated)?,
            target_ip: wire::array(bytes, 24).ok_or(Error::Truncated)?,
        })
    }

    /// Write the packet at the start of `out`, returning the bytes written.
    pub fn emit(&self, out: &mut [u8]) -> Result<usize, Error> {
        if out.len() < PACKET_LEN {
            return Err(Error::NoSpace);
        }
        let fixed = [
            HARDWARE_ETHERNET.to_be_bytes(),
            PROTOCOL_IPV4.to_be_bytes(),
            [6, 4],
            self.operation.code().to_be_bytes(),
        ];
        for (index, field) in fixed.iter().enumerate() {
            wire::put(out, index * 2, field).ok_or(Error::NoSpace)?;
        }
        wire::put(out, 8, &self.sender_mac).ok_or(Error::NoSpace)?;
        wire::put(out, 14, &self.sender_ip).ok_or(Error::NoSpace)?;
        wire::put(out, 18, &self.target_mac).ok_or(Error::NoSpace)?;
        wire::put(out, 24, &self.target_ip).ok_or(Error::NoSpace)?;
        Ok(PACKET_LEN)
    }

    /// The reply a host at `our_mac` owning this request's target address sends.
    #[must_use]
    pub const fn reply(&self, our_mac: Mac) -> Packet {
        Packet {
            operation: Operation::Reply,
            sender_mac: our_mac,
            sender_ip: self.target_ip,
            target_mac: self.sender_mac,
            target_ip: self.sender_ip,
        }
    }
}
