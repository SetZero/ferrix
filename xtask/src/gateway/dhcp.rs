//! A DHCPv4 server, just large enough for `udhcpc` to configure `eth0`.
//!
//! It has exactly one address to hand out — [`GUEST_IP`] — and hands it to
//! whoever asks, which is the honest model for a gateway with a single guest on
//! a private wire. There is no lease database, because there is nothing a
//! second client could be given that the first does not already have; a request
//! for any *other* address is refused with a NAK, so a guest that boots holding
//! a lease from somewhere else gives it up and asks again instead of
//! configuring an address this gateway will not route.
//!
//! Without this the guest configures itself statically:
//!
//! ```text
//! ip addr add 10.0.2.15/24 dev eth0
//! ip link set eth0 up
//! ip route add default via 10.0.2.2
//! ```
//!
//! which is two more commands to remember and one more place for the numbers
//! to be wrong.

use ferrix_netwire::checksum::Pseudo;
use ferrix_netwire::ipv4;
use ferrix_netwire::udp as wire;

use super::{BROADCAST_IP, Core, DNS_IP, GATEWAY_IP, GUEST_IP, MTU, NETMASK, Result, bump, put};

/// Bytes before the options: the BOOTP fixed fields and the magic cookie.
const FIXED_LEN: usize = 240;

/// The shortest reply to send. RFC 951 sized a BOOTP message at 300 bytes and
/// some clients still insist on it; padding costs nothing.
const MIN_REPLY: usize = 300;

/// The four bytes at offset 236 that make the options that follow DHCP's
/// rather than BOOTP's vendor area (RFC 2132 section 2).
const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

/// A request from a client.
const OP_REQUEST: u8 = 1;

/// A reply from a server.
const OP_REPLY: u8 = 2;

/// The port a client listens on.
const CLIENT_PORT: u16 = 68;

/// The port a server listens on.
const SERVER_PORT: u16 = 67;

/// The address a reply is broadcast to, since the client has none yet.
const LIMITED_BROADCAST: std::net::Ipv4Addr = std::net::Ipv4Addr::new(255, 255, 255, 255);

/// How long the offered address is good for. A day: long enough that nothing
/// renews during a boot test, short enough to be a lease rather than a lie.
const LEASE_SECONDS: u32 = 86_400;

/// The DHCP message types this server reads and writes (RFC 2132 section 9.6).
mod kind {
    /// "Is there a server?"
    pub(super) const DISCOVER: u8 = 1;
    /// "Here is an address."
    pub(super) const OFFER: u8 = 2;
    /// "I will take that address."
    pub(super) const REQUEST: u8 = 3;
    /// "It is yours."
    pub(super) const ACK: u8 = 5;
    /// "No, and stop using it."
    pub(super) const NAK: u8 = 6;
    /// "Tell me the options; I have the address already."
    pub(super) const INFORM: u8 = 8;
}

/// The option codes this server reads and writes.
mod option {
    /// Subnet mask.
    pub(super) const NETMASK: u8 = 1;
    /// Default gateway.
    pub(super) const ROUTER: u8 = 3;
    /// Domain name servers.
    pub(super) const DNS: u8 = 6;
    /// Interface MTU.
    pub(super) const MTU: u8 = 26;
    /// Broadcast address.
    pub(super) const BROADCAST: u8 = 28;
    /// The address the client is asking for.
    pub(super) const REQUESTED: u8 = 50;
    /// How long the lease lasts.
    pub(super) const LEASE: u8 = 51;
    /// What kind of message this is.
    pub(super) const KIND: u8 = 53;
    /// Which server sent it.
    pub(super) const SERVER: u8 = 54;
    /// Padding, one byte, no length.
    pub(super) const PAD: u8 = 0;
    /// The end of the option list.
    pub(super) const END: u8 = 255;
}

/// The fields of a client's message this server acts on.
#[derive(Debug)]
struct Request {
    /// The transaction identifier, echoed in the reply.
    xid: u32,
    /// The flags field, echoed so the broadcast bit is preserved.
    flags: u16,
    /// The client's hardware address.
    mac: ferrix_netwire::ethernet::Mac,
    /// What the client is asking for.
    kind: u8,
    /// The address it wants, from option 50 or from `ciaddr`.
    wanted: Option<std::net::Ipv4Addr>,
}

impl Request {
    /// Read a client's message, or `None` if it is not one.
    fn parse(bytes: &[u8]) -> Option<Request> {
        let fixed = bytes.get(..FIXED_LEN)?;
        if fixed.first() != Some(&OP_REQUEST) || fixed.get(236..FIXED_LEN)? != MAGIC {
            return None;
        }
        let options = bytes.get(FIXED_LEN..)?;
        let ciaddr = address(fixed.get(12..16)?)?;
        let wanted = match option_value(options, option::REQUESTED) {
            Some(value) => address(value),
            None if ciaddr.is_unspecified() => None,
            None => Some(ciaddr),
        };
        Some(Request {
            xid: u32::from_be_bytes(fixed.get(4..8)?.try_into().ok()?),
            flags: u16::from_be_bytes(fixed.get(10..12)?.try_into().ok()?),
            mac: fixed.get(28..34)?.try_into().ok()?,
            kind: *option_value(options, option::KIND)?.first()?,
            wanted,
        })
    }

    /// What to answer with, or `None` for a message this server ignores — a
    /// RELEASE or a DECLINE, neither of which it has state to act on.
    fn answer(&self) -> Option<u8> {
        match self.kind {
            kind::DISCOVER => Some(kind::OFFER),
            kind::REQUEST if self.wanted.is_some_and(|wanted| wanted != GUEST_IP) => {
                Some(kind::NAK)
            }
            kind::REQUEST | kind::INFORM => Some(kind::ACK),
            _ => None,
        }
    }
}

/// The four bytes at `value` as an address.
fn address(value: &[u8]) -> Option<std::net::Ipv4Addr> {
    let octets: [u8; 4] = value.try_into().ok()?;
    Some(std::net::Ipv4Addr::from(octets))
}

/// The value of option `code`, walking the list rather than indexing it.
pub(super) fn option_value(options: &[u8], code: u8) -> Option<&[u8]> {
    let mut rest = options;
    loop {
        let (&kind, tail) = rest.split_first()?;
        if kind == option::END {
            return None;
        }
        if kind == option::PAD {
            rest = tail;
            continue;
        }
        let (&len, tail) = tail.split_first()?;
        let value = tail.get(..usize::from(len))?;
        if kind == code {
            return Some(value);
        }
        rest = tail.get(usize::from(len)..)?;
    }
}

impl Core {
    /// Answer a datagram addressed to the DHCP server port.
    pub(super) fn on_dhcp(&mut self, payload: &[u8]) -> Result<()> {
        let Some(request) = Request::parse(payload) else {
            bump(&self.counters.malformed);
            return Ok(());
        };
        let Some(answer) = request.answer() else {
            return Ok(());
        };
        let reply = build(&request, answer);
        let pseudo = Pseudo::V4 {
            source: GATEWAY_IP.octets(),
            destination: LIMITED_BROADCAST.octets(),
        };
        let header = wire::Header {
            source_port: SERVER_PORT,
            destination_port: CLIENT_PORT,
        };
        let mut datagram = [0_u8; MTU];
        let len = header.emit(&reply, pseudo, &mut datagram)?;
        bump(&self.counters.dhcp);
        // To the client's own hardware address, with the broadcast address in
        // the IP header: the client has no address yet, so it accepts the
        // packet on the broadcast destination rather than on a match.
        self.send_ipv4_to(
            request.mac,
            ipv4::protocol::UDP,
            GATEWAY_IP,
            LIMITED_BROADCAST,
            datagram.get(..len).ok_or(ferrix_netwire::Error::NoSpace)?,
        )
    }
}

/// Write the reply to `request`, of type `answer`.
fn build(request: &Request, answer: u8) -> Vec<u8> {
    let offered = if answer == kind::NAK {
        std::net::Ipv4Addr::UNSPECIFIED
    } else {
        GUEST_IP
    };
    let mut reply = vec![0_u8; FIXED_LEN];
    let fields: [(usize, &[u8]); 7] = [
        (0, &[OP_REPLY, 1, 6, 0]),
        (4, &request.xid.to_be_bytes()),
        (10, &request.flags.to_be_bytes()),
        (16, &offered.octets()),
        (20, &GATEWAY_IP.octets()),
        (28, &request.mac),
        (236, &MAGIC),
    ];
    for (at, value) in fields {
        // The buffer is `FIXED_LEN` long and every offset above is inside it,
        // so this cannot fail; if it somehow did, the field is left zero and
        // the client asks again, which is what it does for a lost reply.
        let _ = put(&mut reply, at, value);
    }
    reply.extend_from_slice(&[option::KIND, 1, answer]);
    push(&mut reply, option::SERVER, &GATEWAY_IP.octets());
    if answer != kind::NAK {
        push(&mut reply, option::LEASE, &LEASE_SECONDS.to_be_bytes());
        push(&mut reply, option::NETMASK, &NETMASK.octets());
        push(&mut reply, option::ROUTER, &GATEWAY_IP.octets());
        push(&mut reply, option::DNS, &DNS_IP.octets());
        push(&mut reply, option::BROADCAST, &BROADCAST_IP.octets());
        let mtu = u16::try_from(MTU).unwrap_or(u16::MAX);
        push(&mut reply, option::MTU, &mtu.to_be_bytes());
    }
    reply.push(option::END);
    reply.resize(reply.len().max(MIN_REPLY), 0);
    reply
}

/// Append one option, whose length is the value's.
fn push(reply: &mut Vec<u8>, code: u8, value: &[u8]) {
    reply.push(code);
    reply.push(u8::try_from(value.len()).unwrap_or(0));
    reply.extend_from_slice(value);
}
