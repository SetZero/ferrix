//! UDP, relayed one host socket per guest flow.
//!
//! A guest flow is a (guest address, guest port, destination address,
//! destination port) quadruple, and each one gets an ephemeral host
//! `UdpSocket` of its own. That is the whole of the address translation: the
//! host kernel picks the source port, remembers nothing, and answers arrive on
//! the socket that sent, so there is no mapping table to get wrong and no way
//! for one flow's reply to be delivered into another's.
//!
//! The flow is kept for [`IDLE`] after the last datagram in either direction.
//! UDP has no close, so something has to decide when the socket goes, and the
//! quantity that matters is how long a slow answer may take to come back — a
//! DNS retry, a NTP round trip — not how long the guest intends to keep using
//! the port.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use ferrix_netwire::checksum::Pseudo;
use ferrix_netwire::ipv4;
use ferrix_netwire::udp as wire;

use super::{Core, DNS_IP, MTU, Result, bump};
use crate::Error;

/// The port a DNS query goes to, and which `10.0.2.3` is forwarded on.
pub(super) const DNS_PORT: u16 = 53;

/// The port a DHCP server listens on; those never leave the gateway.
const DHCP_SERVER_PORT: u16 = 67;

/// The most data a relayed datagram may carry before it would need
/// fragmenting, which nothing here does.
pub(super) const MAX_PAYLOAD: usize = MTU - ipv4::MIN_HEADER_LEN - wire::HEADER_LEN;

/// How long a flow outlives its last datagram.
const IDLE: Duration = Duration::from_secs(60);

/// How many datagrams one flow may deliver in one turn of the serving loop.
///
/// A bound, so that a host socket being written to faster than the guest reads
/// cannot hold the loop and starve every other flow and the guest's own frames.
const PER_TURN: usize = 64;

/// What identifies a guest's UDP flow.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) struct Key {
    /// Where the guest sent from.
    guest: SocketAddrV4,
    /// Where the guest addressed, which may be one of the gateway's own
    /// addresses rather than where the datagram actually went.
    seen: SocketAddrV4,
}

/// The host socket standing in for one guest flow.
#[derive(Debug)]
pub(super) struct Flow {
    /// The ephemeral host socket.
    socket: UdpSocket,
    /// Where its datagrams really go, which differs from `Key::seen` for DNS.
    target: SocketAddrV4,
    /// When it last carried anything, in either direction.
    last: Instant,
}

impl Core {
    /// Relay one datagram the guest sent.
    pub(super) fn on_udp(
        &mut self,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        bytes: &[u8],
    ) -> Result<()> {
        let pseudo = Pseudo::V4 {
            source: source.octets(),
            destination: destination.octets(),
        };
        let datagram = wire::Header::parse(bytes, pseudo)?;
        let seen = SocketAddrV4::new(destination, datagram.header.destination_port);
        if seen.port() == DHCP_SERVER_PORT {
            return self.on_dhcp(datagram.payload);
        }
        if datagram.payload.len() > MAX_PAYLOAD {
            bump(&self.counters.oversize);
            return Ok(());
        }
        let key = Key {
            guest: SocketAddrV4::new(source, datagram.header.source_port),
            seen,
        };
        self.open_flow(key)?;
        let sent = match self.udp.get_mut(&key) {
            Some(flow) => {
                flow.last = Instant::now();
                flow.socket.send_to(datagram.payload, flow.target).is_ok()
            }
            None => false,
        };
        if sent {
            bump(&self.counters.udp_out);
        }
        Ok(())
    }

    /// Make sure `key` has a host socket, opening one if it is new.
    fn open_flow(&mut self, key: Key) -> Result<()> {
        if self.udp.contains_key(&key) {
            return Ok(());
        }
        let target = self.target_of(key.seen);
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
            .map_err(|error| Error::new(format!("no host socket for {}: {error}", key.guest)))?;
        socket.set_nonblocking(true)?;
        let _ = self.udp.insert(
            key,
            Flow {
                socket,
                target,
                last: Instant::now(),
            },
        );
        bump(&self.counters.udp_flows);
        Ok(())
    }

    /// Where a datagram addressed to `seen` actually goes.
    ///
    /// `10.0.2.3:53` goes to the resolver, wherever that is. The gateway's own
    /// address is the host's loopback, as [`super::host_of`] explains.
    /// Everything else goes where the guest addressed it.
    fn target_of(&mut self, seen: SocketAddrV4) -> SocketAddrV4 {
        if *seen.ip() == DNS_IP && seen.port() == DNS_PORT {
            *self.resolver.get_or_insert_with(super::default_resolver)
        } else {
            super::host_of(seen)
        }
    }

    /// Collect whatever the host sockets have, and give it to the guest.
    ///
    /// Read first and send afterwards, because sending borrows the whole core
    /// and reading borrows the flow table; the buffer between them is one turn
    /// of datagrams and bounded by [`PER_TURN`].
    pub(super) fn poll_udp(&mut self) {
        // One byte over the limit, so that a reply too long to relay is seen to
        // be too long rather than silently truncated by the read itself.
        let mut buffer = [0_u8; MAX_PAYLOAD + 1];
        let mut replies = Vec::new();
        for (key, flow) in &mut self.udp {
            for _ in 0..PER_TURN {
                let Ok((len, _from)) = flow.socket.recv_from(&mut buffer) else {
                    break;
                };
                flow.last = Instant::now();
                if let Some(payload) = buffer.get(..len) {
                    replies.push((*key, payload.to_vec()));
                }
            }
        }
        for (key, payload) in replies {
            // A reply that cannot be built is one datagram lost, not a reason
            // to stop relaying the flow it belongs to.
            if self.reply_to_guest(&key, &payload).is_err() {
                bump(&self.counters.malformed);
            }
        }
    }

    /// Give one host reply back to the guest, addressed as the guest expects:
    /// from where it sent to, not from where the answer came from.
    fn reply_to_guest(&self, key: &Key, payload: &[u8]) -> Result<()> {
        if payload.len() > MAX_PAYLOAD {
            bump(&self.counters.oversize);
            return Ok(());
        }
        let source = *key.seen.ip();
        let destination = *key.guest.ip();
        let pseudo = Pseudo::V4 {
            source: source.octets(),
            destination: destination.octets(),
        };
        let header = wire::Header {
            source_port: key.seen.port(),
            destination_port: key.guest.port(),
        };
        let mut out = [0_u8; MTU];
        let len = header.emit(payload, pseudo, &mut out)?;
        bump(&self.counters.udp_in);
        self.send_ipv4(
            ipv4::protocol::UDP,
            source,
            destination,
            out.get(..len).ok_or(ferrix_netwire::Error::NoSpace)?,
        )
    }

    /// Close the flows nothing has used for [`IDLE`].
    pub(super) fn expire_udp(&mut self) {
        self.udp.retain(|_, flow| flow.last.elapsed() < IDLE);
    }
}
