//! The four kinds of socket the net core holds, and what a program does to
//! them.
//!
//! None of these touch the wire. A datagram socket is a queue and an address;
//! a stream socket is a [`ferrix_nettcp::Connection`] and two addresses; a
//! listener is a queue of connections that have finished their handshake. The
//! stack moves bytes between them and the interfaces.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use ferrix_kmem::{Charge, buffer_footprint, reserve_deque};
use ferrix_nettcp::Connection;

use crate::addr::{Endpoint, IpAddress};

/// Which socket, within one stack.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SocketId(pub u32);

/// Which address family a socket was opened for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    /// `AF_INET`.
    V4,
    /// `AF_INET6`. Unless `IPV6_V6ONLY` is set it also carries IPv4 through
    /// `::ffff:0:0/96`, as Linux does.
    V6,
}

/// What went wrong, in the terms the socket layer thinks in. The kernel turns
/// these into the errno Linux gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// No socket by that identifier.
    NoSocket,
    /// The call does not apply to this kind of socket.
    WrongKind,
    /// The socket is already bound, connected or listening.
    AlreadyDone,
    /// The socket has to be bound or connected first.
    NotConnected,
    /// The address is already in use.
    AddressInUse,
    /// The address is not one of this host's.
    AddressNotAvailable,
    /// No route to the destination.
    Unreachable,
    /// The peer refused the connection.
    Refused,
    /// The peer reset the connection, or it timed out.
    Reset,
    /// The connection is still being made.
    InProgress,
    /// Nothing to report yet; the caller should wait or answer `EAGAIN`.
    WouldBlock,
    /// The message is larger than the path will carry and may not be
    /// fragmented.
    TooLarge,
    /// The socket has been shut down for this direction.
    ShutDown,
    /// An argument does not make sense.
    Invalid,
    /// The stack has no memory for this.
    NoMemory,
    /// The peer's port had nobody on it.
    PortUnreachable,
    /// The connection timed out.
    TimedOut,
    /// No such interface, or none named where one had to be.
    NoDevice,
    /// The interface is down.
    NetworkDown,
}

/// Which directions a shutdown closes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    /// Reading.
    Read,
    /// Writing.
    Write,
    /// Both.
    Both,
}

/// What a socket can do right now, in the terms `poll` wants.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Readiness {
    /// A read would answer without waiting.
    pub readable: bool,
    /// A write would take at least one byte without waiting.
    pub writable: bool,
    /// The peer has closed its half.
    pub hangup: bool,
    /// An error is waiting to be reported.
    pub error: bool,
}

/// One datagram, as it was received.
#[derive(Clone, Debug)]
pub struct Datagram {
    /// Who sent it.
    pub remote: Endpoint,
    /// Which of this host's addresses it came to.
    pub local: IpAddress,
    /// Which interface it arrived on.
    pub interface: u32,
    /// The hop limit or time to live it arrived with, which
    /// `IPV6_RECVHOPLIMIT` reports.
    pub hop_limit: u8,
    /// The payload.
    pub payload: Vec<u8>,
}

/// Options every socket has, whatever its protocol.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The hop limit outgoing packets carry.
    pub hop_limit: u8,
    /// Whether a broadcast address may be sent to.
    pub broadcast: bool,
    /// Whether an `AF_INET6` socket refuses IPv4.
    pub v6_only: bool,
    /// The interface a socket was pinned to with `SO_BINDTODEVICE`.
    pub device: Option<u32>,
    /// Whether the port may be bound while another socket holds it.
    pub reuse_address: bool,
    /// The type-of-service or traffic-class byte.
    pub traffic_class: u8,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            hop_limit: 64,
            broadcast: false,
            v6_only: false,
            device: None,
            reuse_address: false,
            traffic_class: 0,
        }
    }
}

/// What a waiting datagram counts against its socket's capacity beside its
/// payload: its place in the queue and the smallest allocation a payload
/// takes.
pub const DATAGRAM_COST: usize = size_of::<Datagram>() + 8;

/// A datagram socket: UDP, or the ICMP echo socket `ping` uses.
#[derive(Debug)]
pub struct DatagramSocket {
    /// What it was opened as.
    pub family: Family,
    /// What it is bound to.
    pub local: Endpoint,
    /// What it is connected to, if it is.
    pub remote: Option<Endpoint>,
    /// Its options.
    pub options: Options,
    /// How many bytes of payload may wait.
    pub capacity: usize,
    /// How many bytes of payload are waiting.
    queued: usize,
    /// What the waiting datagrams count against the capacity: their payload
    /// and [`DATAGRAM_COST`] each, so an empty datagram fills the queue too.
    cost: usize,
    /// The datagrams waiting.
    queue: VecDeque<Datagram>,
    /// The heap `queue` holds, and the payloads in it, charged to the job
    /// that made the socket as they arrive (certification finding F-37).
    room: Charge,
    /// Likewise, the payloads.
    payloads: Charge,
    /// An error waiting to be reported, from an ICMP message or a failed send.
    pub error: Option<Error>,
    /// Whether reading has been shut down.
    pub read_shut: bool,
    /// Whether writing has been shut down.
    pub write_shut: bool,
}

impl DatagramSocket {
    /// An unbound socket.
    #[must_use]
    pub fn new(family: Family, capacity: usize) -> DatagramSocket {
        let address = match family {
            Family::V4 => IpAddress::V4(crate::addr::Ipv4::UNSPECIFIED),
            Family::V6 => IpAddress::V6(crate::addr::Ipv6::UNSPECIFIED),
        };
        DatagramSocket {
            family,
            local: Endpoint::new(address, 0),
            remote: None,
            options: Options::default(),
            capacity,
            queued: 0,
            cost: 0,
            queue: VecDeque::new(),
            // To the job opening the socket; nothing is never refused.
            room: Charge::bytes(0).unwrap_or_default(),
            payloads: Charge::bytes(0).unwrap_or_default(),
            error: None,
            read_shut: false,
            write_shut: false,
        }
    }

    /// The heap its queue holds and is charged for, in bytes.
    #[must_use]
    pub fn charged(&self) -> u64 {
        self.room.charged().saturating_add(self.payloads.charged())
    }

    /// Put a datagram in the queue, or drop it because the queue is full.
    ///
    /// Answers whether it was kept. A datagram dropped for want of room is
    /// silently lost, which is what UDP means.
    ///
    /// A datagram counts its payload and [`DATAGRAM_COST`] against the
    /// capacity, as Linux counts an skb's truesize, so a flood of empty ones
    /// fills the queue. One whose socket's job is at its memory limit is
    /// dropped as one arriving at a full queue is.
    pub fn deliver(&mut self, datagram: Datagram) -> bool {
        let cost = datagram.payload.len().saturating_add(DATAGRAM_COST);
        if self.read_shut || self.cost.saturating_add(cost) > self.capacity {
            return false;
        }
        let payload = buffer_footprint::<u8>(datagram.payload.capacity());
        if reserve_deque(&mut self.queue, 1, &mut self.room).is_err()
            || self.payloads.grow(payload).is_err()
        {
            return false;
        }
        self.cost += cost;
        self.queued += datagram.payload.len();
        // Room was had just above: this does not grow the queue.
        self.queue.push_back(datagram);
        true
    }

    /// The datagram at the front, without taking it.
    #[must_use]
    pub fn peek(&self) -> Option<&Datagram> {
        self.queue.front()
    }

    /// Take the datagram at the front.
    pub fn take(&mut self) -> Option<Datagram> {
        let datagram = self.queue.pop_front()?;
        self.queued = self.queued.saturating_sub(datagram.payload.len());
        self.cost = self
            .cost
            .saturating_sub(datagram.payload.len().saturating_add(DATAGRAM_COST));
        self.payloads
            .shrink(buffer_footprint::<u8>(datagram.payload.capacity()));
        Some(datagram)
    }

    /// How many bytes are waiting, which `SIOCINQ` reports.
    #[must_use]
    pub const fn queued(&self) -> usize {
        self.queued
    }

    /// What the socket can do now.
    #[must_use]
    pub fn readiness(&self) -> Readiness {
        Readiness {
            readable: !self.queue.is_empty() || self.read_shut,
            writable: !self.write_shut,
            hangup: self.read_shut && self.write_shut,
            error: self.error.is_some(),
        }
    }

    /// Take the waiting error, as `SO_ERROR` and a failing call do.
    pub fn take_error(&mut self) -> Option<Error> {
        self.error.take()
    }
}

/// A raw IP socket: every packet of one protocol that reaches this host, and
/// packets of that protocol a program builds.
///
/// Linux's `raw(7)`. The queue is a datagram socket's, and so are the
/// addresses, with the port always zero: a raw socket is bound and connected
/// to addresses only. What arrives is a copy, taken beside the stack's own
/// handling of the packet, so an echo request both reaches a raw ICMP socket
/// and is answered.
///
/// An IPv4 one reads packets with their header, as `raw(7)` says; an IPv6 one
/// reads only what follows the header and its extensions, as RFC 3542 says,
/// and has the stack compute the checksum at [`RawSocket::checksum`].
#[derive(Debug)]
pub struct RawSocket {
    /// The queue, the addresses and the options.
    pub datagram: DatagramSocket,
    /// The protocol number it was opened with: which packets it receives, and
    /// what the header of a packet it sends names.
    pub protocol: u8,
    /// `IP_HDRINCL`: what the program sends starts with the IPv4 header.
    /// Always set for `IPPROTO_RAW`, which may not clear it.
    pub header_included: bool,
    /// `ICMP_FILTER`: one bit per ICMP type, set for a type not to deliver.
    pub icmp_filter: u32,
    /// `ICMP6_FILTER`: one bit per ICMPv6 type, set for a type not to deliver,
    /// the lowest bit of the first word for type 0.
    pub icmp6_filter: [u32; 8],
    /// `IPV6_CHECKSUM`: where in what an IPv6 socket sends the stack writes the
    /// checksum, and where in what it receives the stack checks one. Always
    /// 2 for ICMPv6, which RFC 3542 says the stack sums whatever is asked.
    pub checksum: Option<usize>,
}

impl RawSocket {
    /// `IPPROTO_RAW`: a socket that sends packets whose header it wrote, and
    /// receives nothing.
    pub const IPPROTO_RAW: u8 = 255;

    /// An unbound raw socket for `protocol`.
    #[must_use]
    pub fn new(family: Family, protocol: u8, capacity: usize) -> RawSocket {
        RawSocket {
            datagram: DatagramSocket::new(family, capacity),
            protocol,
            header_included: protocol == Self::IPPROTO_RAW,
            icmp_filter: 0,
            icmp6_filter: [0; 8],
            checksum: (matches!(family, Family::V6) && protocol == Self::IPPROTO_ICMPV6)
                .then_some(2),
        }
    }

    /// ICMPv6's protocol number, the one whose checksum is always computed.
    pub const IPPROTO_ICMPV6: u8 = 58;

    /// Whether `ICMP6_FILTER` lets a message of ICMPv6 type `kind` through.
    #[must_use]
    pub fn passes_icmp6(&self, kind: u8) -> bool {
        let word = self
            .icmp6_filter
            .get(usize::from(kind >> 5))
            .copied()
            .unwrap_or(0);
        word & (1 << (kind & 31)) == 0
    }
}

/// A stream socket: one TCP connection and the two addresses it is between.
#[derive(Debug)]
pub struct StreamSocket {
    /// What it was opened as.
    pub family: Family,
    /// This end.
    pub local: Endpoint,
    /// The other end.
    pub remote: Endpoint,
    /// The state machine.
    pub connection: Connection,
    /// Its options.
    pub options: Options,
    /// An error waiting to be reported.
    pub error: Option<Error>,
    /// Whether the program has shut reading down, so arriving data is
    /// discarded rather than queued.
    pub read_shut: bool,
    /// The listener this connection arrived on, while it is still waiting to
    /// be accepted.
    pub listener: Option<SocketId>,
    /// Whether the program has let go of the socket, so the stack may forget
    /// it once the connection has finished closing.
    pub closing: bool,
    /// Its own heap, charged to the job that made it: the program that
    /// connected, or the one listening where it arrived (certification
    /// finding F-37). Its queues charge their growth to the same job.
    pub heap: Charge,
}

impl StreamSocket {
    /// What the socket can do now.
    #[must_use]
    pub fn readiness(&self) -> Readiness {
        let state = self.connection.state();
        Readiness {
            readable: self.connection.is_readable(),
            writable: self.connection.is_writable(),
            hangup: !state.can_receive() && state != ferrix_nettcp::State::SynSent,
            error: self.error.is_some() || self.connection.failure().is_some(),
        }
    }
}

/// A listening socket: a port, and the connections that have finished their
/// handshake on it.
#[derive(Debug)]
pub struct ListenSocket {
    /// What it was opened as.
    pub family: Family,
    /// What it is bound to.
    pub local: Endpoint,
    /// How many finished connections may wait.
    pub backlog: usize,
    /// Connections that finished their handshake and have not been accepted.
    pub ready: VecDeque<SocketId>,
    /// Connections still in their handshake, at most `backlog` of them,
    /// apart from those that finished.
    pub pending: Vec<SocketId>,
    /// Its options.
    pub options: Options,
    /// The job of the program that opened it, which the connections that
    /// arrive on it are charged to until one is accepted, as Linux charges a
    /// request socket to its listener's cgroup (certification finding F-37).
    pub owner: u32,
}

impl ListenSocket {
    /// What the socket can do now: readable means a connection is waiting.
    #[must_use]
    pub fn readiness(&self) -> Readiness {
        Readiness {
            readable: !self.ready.is_empty(),
            writable: false,
            hangup: false,
            error: false,
        }
    }
}

/// One of the five kinds.
#[derive(Debug)]
pub enum Socket {
    /// A UDP socket.
    Udp(DatagramSocket),
    /// An ICMP echo socket, which `ping` uses without privilege.
    Icmp(DatagramSocket),
    /// A raw IP socket, which needs privilege.
    Raw(RawSocket),
    /// A TCP connection.
    ///
    /// Boxed because it is four times the size of the others: a connection
    /// carries two byte queues, a reassembly list and the whole state
    /// machine, and every socket in the table would otherwise be that large.
    Stream(Box<StreamSocket>),
    /// A TCP listener.
    Listen(ListenSocket),
}

impl Socket {
    /// What the socket can do now.
    #[must_use]
    pub fn readiness(&self) -> Readiness {
        match self {
            Socket::Udp(socket) | Socket::Icmp(socket) => socket.readiness(),
            Socket::Raw(socket) => socket.datagram.readiness(),
            Socket::Stream(socket) => socket.readiness(),
            Socket::Listen(socket) => socket.readiness(),
        }
    }

    /// What it is bound to.
    #[must_use]
    pub fn local(&self) -> Endpoint {
        match self {
            Socket::Udp(socket) | Socket::Icmp(socket) => socket.local,
            Socket::Raw(socket) => socket.datagram.local,
            Socket::Stream(socket) => socket.local,
            Socket::Listen(socket) => socket.local,
        }
    }

    /// What it is connected to, if anything.
    #[must_use]
    pub fn remote(&self) -> Option<Endpoint> {
        match self {
            Socket::Udp(socket) | Socket::Icmp(socket) => socket.remote,
            Socket::Raw(socket) => socket.datagram.remote,
            Socket::Stream(socket) => Some(socket.remote),
            Socket::Listen(_) => None,
        }
    }

    /// Its options.
    #[must_use]
    pub fn options(&self) -> Options {
        match self {
            Socket::Udp(socket) | Socket::Icmp(socket) => socket.options,
            Socket::Raw(socket) => socket.datagram.options,
            Socket::Stream(socket) => socket.options,
            Socket::Listen(socket) => socket.options,
        }
    }

    /// Its options, to change.
    pub fn options_mut(&mut self) -> &mut Options {
        match self {
            Socket::Udp(socket) | Socket::Icmp(socket) => &mut socket.options,
            Socket::Raw(socket) => &mut socket.datagram.options,
            Socket::Stream(socket) => &mut socket.options,
            Socket::Listen(socket) => &mut socket.options,
        }
    }

    /// Which family it was opened as.
    #[must_use]
    pub fn family(&self) -> Family {
        match self {
            Socket::Udp(socket) | Socket::Icmp(socket) => socket.family,
            Socket::Raw(socket) => socket.datagram.family,
            Socket::Stream(socket) => socket.family,
            Socket::Listen(socket) => socket.family,
        }
    }
}
