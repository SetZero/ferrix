//! `AF_PACKET`: sockets on a link, below IP.
//!
//! Linux's `packet(7)`, the part a DHCP client needs. A packet socket names an
//! Ethernet protocol, or every protocol, and is handed a copy of each frame of
//! it that an interface takes in -- before IP decides whether the packet is for
//! this host, which is the point: a DHCP offer arrives for an address the
//! interface does not have yet. It sends frames too, either whole
//! (`SOCK_RAW`) or as a payload the stack puts a link header on
//! (`SOCK_DGRAM`).
//!
//! Not here yet, and each is a difference from Linux a program could see:
//! frames this host sends are not copied back to an `ETH_P_ALL` socket
//! (`PACKET_OUTGOING`); a socket on the loopback receives nothing and sends
//! nothing, because this stack's loopback carries bare IP packets rather than
//! frames; and no filter program can be attached.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use ferrix_netwire::ethernet::{self, Mac};

use crate::iface::Medium;
use crate::socket::{Error, Readiness, SocketId};
use crate::stack::{Outgoing, Stack};

/// `ETH_P_ALL`: every protocol, as a packet socket's protocol.
pub const ALL_PROTOCOLS: u16 = 0x0003;

/// The shortest frame Ethernet carries, padded to if shorter.
const MIN_FRAME: usize = 60;

/// Whether a packet socket reads and writes whole frames or their payloads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PacketKind {
    /// `SOCK_RAW`: the link header is part of what is read and written.
    Raw,
    /// `SOCK_DGRAM`: the link header is taken off on the way in and put on by
    /// the stack on the way out.
    Datagram,
}

/// Who a received frame was addressed to, as `sll_pkttype` says it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PacketType {
    /// `PACKET_HOST`: this interface's own address.
    Host,
    /// `PACKET_BROADCAST`: the link's broadcast address.
    Broadcast,
    /// `PACKET_MULTICAST`: a multicast group.
    Multicast,
}

impl PacketType {
    /// The number `sll_pkttype` carries.
    #[must_use]
    pub const fn linux(self) -> u8 {
        match self {
            PacketType::Host => 0,
            PacketType::Broadcast => 1,
            PacketType::Multicast => 2,
        }
    }
}

/// One frame, as a packet socket received it.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Which interface it arrived on.
    pub interface: u32,
    /// Its Ethernet protocol.
    pub protocol: u16,
    /// The hardware address it came from.
    pub source: Mac,
    /// Who it was addressed to.
    pub kind: PacketType,
    /// The frame, or its payload on a `SOCK_DGRAM` socket.
    pub bytes: Vec<u8>,
}

/// Where a `sendto` on a packet socket goes: `sockaddr_ll`'s fields that
/// matter for a send.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LinkAddress {
    /// The interface to send on; zero for the one the socket is bound to.
    pub interface: u32,
    /// The Ethernet protocol the link header names; zero for the socket's.
    pub protocol: u16,
    /// The hardware address it goes to, on a `SOCK_DGRAM` socket.
    pub address: Mac,
}

/// What a receive on a packet socket answered.
#[derive(Clone, Debug)]
pub struct Received {
    /// How many bytes were copied out.
    pub bytes: usize,
    /// How many more the frame held than there was room for.
    pub truncated: usize,
    /// Where it came from, for `sockaddr_ll`.
    pub interface: u32,
    /// Its Ethernet protocol.
    pub protocol: u16,
    /// The hardware address it came from.
    pub source: Mac,
    /// Who it was addressed to.
    pub kind: PacketType,
}

/// A packet socket.
#[derive(Debug)]
pub struct PacketSocket {
    /// Whole frames or payloads.
    pub kind: PacketKind,
    /// The Ethernet protocol it receives, [`ALL_PROTOCOLS`] for all of them,
    /// or zero for none until it is bound to one.
    pub protocol: u16,
    /// The interface it is bound to, or `None` for every interface.
    pub interface: Option<u32>,
    /// How many bytes of frames may wait.
    pub capacity: usize,
    /// How many are waiting.
    queued: usize,
    /// The frames waiting.
    queue: VecDeque<Frame>,
}

impl PacketSocket {
    /// A socket for `protocol` on every interface.
    #[must_use]
    pub fn new(kind: PacketKind, protocol: u16, capacity: usize) -> PacketSocket {
        PacketSocket {
            kind,
            protocol,
            interface: None,
            capacity,
            queued: 0,
            queue: VecDeque::new(),
        }
    }

    /// Whether a frame of `protocol` on `interface` is one this socket takes.
    #[must_use]
    pub fn wants(&self, interface: u32, protocol: u16) -> bool {
        let protocol_matches =
            self.protocol != 0 && (self.protocol == ALL_PROTOCOLS || self.protocol == protocol);
        protocol_matches && self.interface.is_none_or(|bound| bound == interface)
    }

    /// Keep a frame, or drop it when the queue is full.
    pub fn deliver(&mut self, frame: Frame) -> bool {
        if self.queued + frame.bytes.len() > self.capacity {
            return false;
        }
        self.queued += frame.bytes.len();
        self.queue.push_back(frame);
        true
    }

    /// The frame at the front, without taking it.
    #[must_use]
    pub fn peek(&self) -> Option<&Frame> {
        self.queue.front()
    }

    /// Take the frame at the front.
    pub fn take(&mut self) -> Option<Frame> {
        let frame = self.queue.pop_front()?;
        self.queued = self.queued.saturating_sub(frame.bytes.len());
        Some(frame)
    }

    /// What it can do now: read when a frame waits, and always write.
    #[must_use]
    pub fn readiness(&self) -> Readiness {
        Readiness {
            readable: !self.queue.is_empty(),
            writable: true,
            hangup: false,
            error: false,
        }
    }
}

impl Stack {
    /// Open a packet socket of `kind` for the Ethernet `protocol`.
    ///
    /// Like a raw IP socket it needs privilege, which is the kernel's to ask.
    pub fn open_packet(&mut self, kind: PacketKind, protocol: u16) -> SocketId {
        let id = self.allocate_id();
        let socket = PacketSocket::new(kind, protocol, self.config.datagram_capacity);
        let _ = self.packets.insert(id.0, socket);
        id
    }

    /// The packet socket, if `id` is one.
    #[must_use]
    pub fn packet_socket(&self, id: SocketId) -> Option<&PacketSocket> {
        self.packets.get(&id.0)
    }

    /// Bind a packet socket: to a protocol, unless `protocol` is zero, which
    /// keeps the one it has, and to an interface, or to every interface when
    /// `interface` is zero -- `packet_do_bind`'s rules.
    ///
    /// # Errors
    ///
    /// [`Error::NoDevice`] for an interface that does not exist.
    pub fn bind_packet(
        &mut self,
        id: SocketId,
        protocol: u16,
        interface: u32,
    ) -> Result<(), Error> {
        if interface != 0 && self.interface(interface).is_none() {
            return Err(Error::NoDevice);
        }
        let socket = self.packets.get_mut(&id.0).ok_or(Error::NoSocket)?;
        if protocol != 0 {
            socket.protocol = protocol;
        }
        socket.interface = (interface != 0).then_some(interface);
        Ok(())
    }

    /// Send `data` from a packet socket, to `to` if the call named a link
    /// address.
    ///
    /// # Errors
    ///
    /// [`Error::NoDevice`] with no interface named or bound,
    /// [`Error::NetworkDown`] when it is down, [`Error::TooLarge`] past its
    /// MTU, and [`Error::Invalid`] for a raw frame shorter than its link
    /// header or an interface that does not carry frames.
    pub fn send_packet(
        &mut self,
        id: SocketId,
        data: &[u8],
        to: Option<LinkAddress>,
    ) -> Result<usize, Error> {
        let socket = self.packets.get(&id.0).ok_or(Error::NoSocket)?;
        let kind = socket.kind;
        let named = to.map_or(0, |address| address.interface);
        let interface = match (named, socket.interface) {
            (0, Some(bound)) => bound,
            (0, None) => return Err(Error::NoDevice),
            (named, _) => named,
        };
        let protocol = to
            .map(|address| address.protocol)
            .filter(|protocol| *protocol != 0)
            .unwrap_or(socket.protocol);
        let link = self.interface(interface).ok_or(Error::NoDevice)?;
        if !matches!(link.medium, Medium::Ethernet) {
            return Err(Error::Invalid);
        }
        if !link.is_up() {
            return Err(Error::NetworkDown);
        }
        let mtu = link.mtu as usize;
        match kind {
            PacketKind::Datagram => {
                if data.len() > mtu {
                    return Err(Error::TooLarge);
                }
                let destination = to.map_or([0; 6], |address| address.address);
                self.emit_frame(interface, destination, protocol, data);
            }
            PacketKind::Raw => {
                if data.len() < ethernet::HEADER_LEN {
                    return Err(Error::Invalid);
                }
                if data.len() > mtu + ethernet::HEADER_LEN {
                    return Err(Error::TooLarge);
                }
                let mut frame = vec![0_u8; data.len().max(MIN_FRAME)];
                if let Some(start) = frame.get_mut(..data.len()) {
                    start.copy_from_slice(data);
                }
                self.count_sent(interface, frame.len());
                self.egress.push_back(Outgoing { interface, frame });
            }
        }
        Ok(data.len())
    }

    /// Take the next frame from a packet socket.
    ///
    /// # Errors
    ///
    /// [`Error::WouldBlock`] when nothing waits.
    pub fn recv_packet(
        &mut self,
        id: SocketId,
        out: &mut [u8],
        peek: bool,
    ) -> Result<Received, Error> {
        let socket = self.packets.get_mut(&id.0).ok_or(Error::NoSocket)?;
        let frame = socket.peek().ok_or(Error::WouldBlock)?;
        let mut copied = 0;
        for (slot, byte) in out.iter_mut().zip(frame.bytes.iter()) {
            *slot = *byte;
            copied += 1;
        }
        let received = Received {
            bytes: copied,
            truncated: frame.bytes.len().saturating_sub(copied),
            interface: frame.interface,
            protocol: frame.protocol,
            source: frame.source,
            kind: frame.kind,
        };
        if !peek {
            let _ = socket.take();
        }
        Ok(received)
    }

    /// Give every packet socket that wants it a copy of a frame an Ethernet
    /// interface took in.
    pub(crate) fn tap_frame(
        &mut self,
        interface: u32,
        header: &ethernet::Header,
        frame: &[u8],
        payload: &[u8],
    ) {
        if self.packets.is_empty() {
            return;
        }
        let Some(hardware) = self.interface(interface).map(|link| link.hardware) else {
            return;
        };
        let kind = if header.destination == ethernet::BROADCAST {
            PacketType::Broadcast
        } else if header.destination != hardware
            && header.destination.first().is_some_and(|byte| byte & 1 == 1)
        {
            PacketType::Multicast
        } else {
            PacketType::Host
        };
        for socket in self.packets.values_mut() {
            if !socket.wants(interface, header.ethertype) {
                continue;
            }
            let bytes = match socket.kind {
                PacketKind::Raw => frame,
                PacketKind::Datagram => payload,
            };
            let _ = socket.deliver(Frame {
                interface,
                protocol: header.ethertype,
                source: header.source,
                kind,
                bytes: bytes.to_vec(),
            });
        }
    }
}
