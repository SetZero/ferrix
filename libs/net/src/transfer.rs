//! Moving bytes between a program and a socket.
//!
//! A datagram send builds a packet and puts it on a link there and then, so a
//! program that gets `Ok` knows the packet left. A stream send only fills the
//! send queue: the segments go out when the windows allow, which is the whole
//! difference between the two protocols stated as an API.

use alloc::vec;

use ferrix_netwire::{icmpv4, icmpv6, udp};

use crate::addr::{Endpoint, IpAddress, Ipv6};
use crate::socket::{Error, Socket};
use crate::stack::{Millis, Stack, unspecified};

/// What a receive answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Received {
    /// How many bytes were copied out.
    pub bytes: usize,
    /// How many more the datagram held than there was room for, which
    /// `MSG_TRUNC` reports.
    pub truncated: usize,
    /// Who sent it, for a socket that is not connected.
    pub remote: Option<Endpoint>,
}

impl Stack {
    /// Send `data`, to `to` if the socket is not connected.
    pub fn send(
        &mut self,
        id: crate::socket::SocketId,
        data: &[u8],
        to: Option<Endpoint>,
        now: Millis,
    ) -> Result<usize, Error> {
        match self.sockets.get(&id.0).ok_or(Error::NoSocket)? {
            Socket::Udp(_) => self.send_udp(id, data, to, now),
            Socket::Icmp(_) => self.send_icmp(id, data, to, now),
            Socket::Stream(_) => self.send_stream(id, data),
            Socket::Listen(_) => Err(Error::NotConnected),
        }
    }

    /// Fill in what a datagram send needs and answer it, or say why not.
    fn addresses_for(
        &mut self,
        id: crate::socket::SocketId,
        to: Option<Endpoint>,
    ) -> Result<(Endpoint, Endpoint, Option<u32>, u8), Error> {
        let family = self.sockets.get(&id.0).ok_or(Error::NoSocket)?.family();
        let Some(Socket::Udp(socket) | Socket::Icmp(socket)) = self.sockets.get(&id.0) else {
            return Err(Error::WrongKind);
        };
        if socket.write_shut {
            return Err(Error::ShutDown);
        }
        let destination = normalise(to.or(socket.remote).ok_or(Error::NotConnected)?);
        if destination.address.is_unspecified() {
            return Err(Error::Unreachable);
        }
        let options = socket.options;
        let bound = socket.local;
        let source = if bound.address.is_unspecified() {
            self.source_for(destination.address)?
        } else {
            bound.address
        };
        let port = if bound.port == 0 {
            self.bind_ephemeral(id, family)?
        } else {
            bound.port
        };
        Ok((
            Endpoint::new(source, port),
            destination,
            options.device,
            options.hop_limit,
        ))
    }

    /// Give an unbound socket a port, and remember it.
    fn bind_ephemeral(
        &mut self,
        id: crate::socket::SocketId,
        family: crate::socket::Family,
    ) -> Result<u16, Error> {
        let address = unspecified(family);
        self.bind(id, Endpoint::new(address, 0))?;
        let _ = address;
        self.local_endpoint(id)
            .map(|endpoint| endpoint.port)
            .ok_or(Error::NoSocket)
    }

    /// Build and send a UDP datagram.
    fn send_udp(
        &mut self,
        id: crate::socket::SocketId,
        data: &[u8],
        to: Option<Endpoint>,
        now: Millis,
    ) -> Result<usize, Error> {
        let (local, remote, device, hop_limit) = self.addresses_for(id, to)?;
        if local.address.is_v4() != remote.address.is_v4() {
            return Err(Error::Invalid);
        }
        let header = udp::Header {
            source_port: local.port,
            destination_port: remote.port,
        };
        let pseudo = crate::input::pseudo_of(local.address, remote.address);
        let mut packet = vec![0_u8; udp::HEADER_LEN + data.len()];
        let _ = header
            .emit(data, pseudo, &mut packet)
            .map_err(|_| Error::TooLarge)?;
        self.send_ip(
            local.address,
            remote.address,
            udp::PROTOCOL,
            &packet,
            hop_limit,
            device,
            now,
        )?;
        Ok(data.len())
    }

    /// Send an ICMP echo the program built, with its identifier replaced by
    /// the socket's port.
    ///
    /// That replacement is the whole trick of an unprivileged ping socket: the
    /// kernel owns the identifier, so one program's replies cannot be read by
    /// another, and no program needs the privilege a raw socket would.
    fn send_icmp(
        &mut self,
        id: crate::socket::SocketId,
        data: &[u8],
        to: Option<Endpoint>,
        now: Millis,
    ) -> Result<usize, Error> {
        let (local, remote, device, hop_limit) = self.addresses_for(id, to)?;
        let kind = *data.first().ok_or(Error::Invalid)?;
        let code = *data.get(1).ok_or(Error::Invalid)?;
        let rest = data.get(4..).ok_or(Error::Invalid)?;
        let mut body = rest.to_vec();
        let identifier = local.port.to_be_bytes();
        for (slot, byte) in body.iter_mut().zip(identifier.iter()) {
            *slot = *byte;
        }
        let packet = match (local.address, remote.address) {
            (IpAddress::V4(_), IpAddress::V4(_)) => {
                let header = icmpv4::Header {
                    kind,
                    code,
                    rest: *data
                        .get(4..8)
                        .and_then(|bytes| bytes.first_chunk::<4>())
                        .ok_or(Error::Invalid)?,
                };
                let payload = data.get(8..).unwrap_or_default();
                let mut header = header;
                header.rest = with_identifier(header.rest, local.port);
                let mut packet = vec![0_u8; icmpv4::HEADER_LEN + payload.len()];
                let _ = header
                    .emit(payload, &mut packet)
                    .map_err(|_| Error::TooLarge)?;
                packet
            }
            (IpAddress::V6(from), IpAddress::V6(to)) => {
                let message = icmpv6::Message {
                    kind,
                    code,
                    body: &body,
                };
                let mut packet = vec![0_u8; icmpv6::HEADER_LEN + body.len()];
                let _ = message
                    .emit(from.octets(), to.octets(), &mut packet)
                    .map_err(|_| Error::TooLarge)?;
                packet
            }
            _ => return Err(Error::Invalid),
        };
        let protocol = if local.address.is_v4() {
            ferrix_netwire::ipv4::protocol::ICMP
        } else {
            icmpv6::PROTOCOL
        };
        self.send_ip(
            local.address,
            remote.address,
            protocol,
            &packet,
            hop_limit,
            device,
            now,
        )?;
        Ok(data.len())
    }

    /// Put bytes in a connection's send queue.
    fn send_stream(&mut self, id: crate::socket::SocketId, data: &[u8]) -> Result<usize, Error> {
        let Some(Socket::Stream(stream)) = self.sockets.get_mut(&id.0) else {
            return Err(Error::WrongKind);
        };
        if let Some(error) = stream.error.take() {
            return Err(error);
        }
        if let Some(error) = Stack::stream_error(&stream.connection) {
            return Err(error);
        }
        let state = stream.connection.state();
        if state == ferrix_nettcp::State::SynSent {
            return Err(Error::InProgress);
        }
        if !state.can_send() {
            return Err(Error::ShutDown);
        }
        let taken = stream.connection.write(data);
        if taken == 0 && !data.is_empty() {
            return Err(Error::WouldBlock);
        }
        Ok(taken)
    }

    /// Take received bytes.
    pub fn recv(
        &mut self,
        id: crate::socket::SocketId,
        out: &mut [u8],
        peek: bool,
    ) -> Result<Received, Error> {
        match self.sockets.get_mut(&id.0).ok_or(Error::NoSocket)? {
            Socket::Udp(socket) | Socket::Icmp(socket) => {
                if let Some(error) = socket.take_error() {
                    return Err(error);
                }
                let Some(datagram) = socket.peek() else {
                    if socket.read_shut {
                        return Ok(Received::default());
                    }
                    return Err(Error::WouldBlock);
                };
                let copied = copy(out, &datagram.payload);
                let truncated = datagram.payload.len().saturating_sub(copied);
                let remote = datagram.remote;
                if !peek {
                    let _ = socket.take();
                }
                Ok(Received {
                    bytes: copied,
                    truncated,
                    remote: Some(remote),
                })
            }
            Socket::Stream(stream) => {
                if let Some(error) = stream.error.take() {
                    return Err(error);
                }
                let taken = if peek {
                    stream.connection.peek(out)
                } else {
                    stream.connection.read(out)
                };
                if taken > 0 {
                    return Ok(Received {
                        bytes: taken,
                        truncated: 0,
                        remote: Some(stream.remote),
                    });
                }
                if let Some(error) = Stack::stream_error(&stream.connection) {
                    return Err(error);
                }
                if stream.connection.state().can_receive() || out.is_empty() {
                    return Err(Error::WouldBlock);
                }
                // The peer closed and everything it sent has been read: this
                // is the end of the stream, which is a read of zero bytes.
                Ok(Received {
                    bytes: 0,
                    truncated: 0,
                    remote: Some(stream.remote),
                })
            }
            Socket::Listen(_) => Err(Error::NotConnected),
        }
    }
}

/// Copy as much of `from` as fits, and say how much that was.
fn copy(out: &mut [u8], from: &[u8]) -> usize {
    let mut copied = 0;
    for (slot, byte) in out.iter_mut().zip(from.iter()) {
        *slot = *byte;
        copied += 1;
    }
    copied
}

/// An echo header's four trailing bytes with the identifier replaced.
fn with_identifier(rest: [u8; 4], identifier: u16) -> [u8; 4] {
    let [high, low] = identifier.to_be_bytes();
    [high, low, rest[2], rest[3]]
}

/// An IPv4-mapped IPv6 endpoint written as the IPv4 one it is.
///
/// The stack keeps one form on the wire and in its tables; the kernel is what
/// hands a program back the mapped spelling when the socket was opened as
/// `AF_INET6`.
pub(crate) fn normalise(endpoint: Endpoint) -> Endpoint {
    match endpoint.address {
        IpAddress::V6(address) => match address.v4_mapped() {
            Some(four) => Endpoint::new(IpAddress::V4(four), endpoint.port),
            None => endpoint,
        },
        IpAddress::V4(_) => endpoint,
    }
}

/// The IPv6 spelling of an address, for a socket opened as `AF_INET6`.
#[must_use]
pub fn to_v6(address: IpAddress) -> IpAddress {
    match address {
        IpAddress::V4(four) => IpAddress::V6(Ipv6::from_v4_mapped(four)),
        IpAddress::V6(_) => address,
    }
}
