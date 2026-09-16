//! An `AF_INET` or `AF_INET6` socket, as the inode its open file reads and
//! writes through.
//!
//! The socket itself lives in the stack and is named by a [`SocketId`]. What
//! is here is the shell around it: the inode a descriptor points at, the
//! waiting a blocking call does, the timeouts and options a program sets, and
//! the translation between Linux's `sockaddr_in` and the stack's endpoints.
//!
//! # A socket owns its place in the stack
//!
//! Dropping this closes the stack's socket. There is no other path: a
//! descriptor closed, a process exiting and a failed `accept` all end in the
//! same `Drop`, which is why a leaked connection is not possible without a
//! leaked `Arc`.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::inet::{
    IP_TOS, IP_TTL, IPPROTO_ICMP, IPPROTO_ICMPV6, IPPROTO_TCP, IPPROTO_UDP, IPV6_UNICAST_HOPS,
    IPV6_V6ONLY, InetAddress, SOCKADDR_STORAGE_SIZE, SOL_IP, SOL_IPV6, SOL_TCP, TCP_MAXSEG,
    TCP_NODELAY,
};
use ferrix_linux_abi::socket::{
    AF_INET, AF_INET6, MSG_DONTWAIT, MSG_PEEK, SHUT_RD, SHUT_RDWR, SHUT_WR, SO_ACCEPTCONN,
    SO_BROADCAST, SO_DOMAIN, SO_ERROR, SO_KEEPALIVE, SO_PROTOCOL, SO_RCVBUF, SO_RCVTIMEO_NEW,
    SO_RCVTIMEO_OLD, SO_REUSEADDR, SO_SNDBUF, SO_SNDTIMEO_NEW, SO_SNDTIMEO_OLD, SO_TYPE,
    SOCK_DGRAM, SOCK_STREAM, SOCKET_BUFFER_DEFAULT, SOL_SOCKET, Width,
};
use ferrix_net::socket::{Error, Family, Shutdown, Socket as NetSocket};
use ferrix_net::{Endpoint, IpAddress, Ipv4, Ipv6, SocketId, to_v6};
use ferrix_vfs::{Inode, Metadata, OpenFile, Readiness};

use crate::fs;
use crate::net;
use crate::sync::SpinLock;

/// What kind of `AF_INET` socket this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InetKind {
    /// `SOCK_STREAM`: TCP.
    Stream,
    /// `SOCK_DGRAM` at `IPPROTO_UDP`: UDP.
    Datagram,
    /// `SOCK_DGRAM` at `IPPROTO_ICMP`: the echo socket `ping` uses without
    /// privilege.
    Echo,
}

impl InetKind {
    /// The type `SO_TYPE` reports.
    const fn linux(self) -> u32 {
        match self {
            InetKind::Stream => SOCK_STREAM,
            InetKind::Datagram | InetKind::Echo => SOCK_DGRAM,
        }
    }

    /// The protocol `SO_PROTOCOL` reports, for a socket of this family.
    const fn protocol(self, family: Family) -> i32 {
        match (self, family) {
            (InetKind::Stream, _) => IPPROTO_TCP,
            (InetKind::Datagram, _) => IPPROTO_UDP,
            (InetKind::Echo, Family::V4) => IPPROTO_ICMP,
            (InetKind::Echo, Family::V6) => IPPROTO_ICMPV6,
        }
    }

    /// Whether it carries a stream rather than records.
    const fn is_stream(self) -> bool {
        matches!(self, InetKind::Stream)
    }
}

/// What a program has set that the stack does not keep.
#[derive(Clone, Copy, Debug)]
struct Options {
    /// `SO_RCVTIMEO`, in nanoseconds; zero waits forever.
    receive_timeout: u64,
    /// `SO_SNDTIMEO`, likewise.
    send_timeout: u64,
    /// `SO_SNDBUF` as set, reported back.
    send_buffer: usize,
    /// `SO_RCVBUF` as set, reported back.
    receive_buffer: usize,
    /// `SO_KEEPALIVE`, kept and reported; nothing probes yet.
    keepalive: bool,
}

/// An `AF_INET` or `AF_INET6` socket.
pub(crate) struct InetSocket {
    /// Which socket in the stack.
    id: SocketId,
    /// What kind it is.
    kind: InetKind,
    /// Which family it was opened as.
    family: Family,
    /// What `stat` reports through it.
    metadata: Metadata,
    /// What a program has set.
    options: SpinLock<Options>,
}

impl fmt::Debug for InetSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InetSocket")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("ino", &self.metadata.ino)
            .finish_non_exhaustive()
    }
}

impl Drop for InetSocket {
    fn drop(&mut self) {
        net::core().with(|stack, _| stack.close(self.id));
    }
}

/// What a receive answered.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Received {
    /// Bytes copied into the caller's buffer.
    pub(crate) bytes: usize,
    /// What the whole record held, which `MSG_TRUNC` answers with.
    pub(crate) full: usize,
}

impl InetSocket {
    /// Open a socket of `kind` in `family`, as the open file `socket`
    /// installs.
    ///
    /// # Errors
    ///
    /// Whatever [`OpenFile::new`] refuses, which for a socket is nothing.
    pub(crate) fn open(
        family: Family,
        kind: InetKind,
        nonblock: bool,
        owner: (u32, u32),
    ) -> Result<Arc<OpenFile>, Errno> {
        let id = net::core().with(|stack, _| match kind {
            InetKind::Stream => stack.open_tcp(family),
            InetKind::Datagram => stack.open_udp(family),
            InetKind::Echo => stack.open_icmp(family),
        });
        Self::wrap(id, family, kind, nonblock, owner)
    }

    /// Put an existing stack socket behind an open file, which is what
    /// `accept` does with the connection it takes.
    fn wrap(
        id: SocketId,
        family: Family,
        kind: InetKind,
        nonblock: bool,
        owner: (u32, u32),
    ) -> Result<Arc<OpenFile>, Errno> {
        let ino = fs::socket::next_ino();
        let socket = Arc::new(InetSocket {
            id,
            kind,
            family,
            metadata: fs::socket::socket_metadata(ino, owner),
            options: SpinLock::new(Options {
                receive_timeout: 0,
                send_timeout: 0,
                send_buffer: SOCKET_BUFFER_DEFAULT,
                receive_buffer: SOCKET_BUFFER_DEFAULT,
                keepalive: false,
            }),
        });
        fs::socket::open_on_sockfs(socket, ino, nonblock)
    }

    /// Its type.
    pub(crate) const fn kind(&self) -> InetKind {
        self.kind
    }

    /// Whether it has a peer.
    pub(crate) fn is_connected(&self) -> bool {
        net::core().with(|stack, _| stack.remote_endpoint(self.id).is_some())
    }

    /// What it can do right now.
    pub(crate) fn readiness(&self) -> Readiness {
        let ready = net::core().with(|stack, _| stack.readiness(self.id));
        Readiness {
            readable: ready.readable,
            writable: ready.writable,
            hangup: ready.hangup,
            error: ready.error,
        }
    }

    /// Give it a name.
    pub(crate) fn bind(&self, raw: &[u8]) -> Result<(), Errno> {
        let endpoint = self.endpoint_of(raw)?;
        net::core()
            .with(|stack, _| stack.bind(self.id, endpoint))
            .map_err(errno)
    }

    /// Start listening.
    pub(crate) fn listen(&self, backlog: i32) -> Result<(), Errno> {
        if !self.kind.is_stream() {
            return Err(Errno::EOPNOTSUPP);
        }
        let backlog = usize::try_from(backlog.max(0)).unwrap_or(0);
        net::core()
            .with(|stack, _| stack.listen(self.id, backlog))
            .map_err(errno)
    }

    /// Take a connection that finished its handshake.
    pub(crate) fn accept(
        &self,
        nonblock: bool,
        owner: (u32, u32),
    ) -> Result<(Arc<OpenFile>, Vec<u8>), Errno> {
        if !self.kind.is_stream() {
            return Err(Errno::EOPNOTSUPP);
        }
        let deadline = self.deadline(self.options.lock().receive_timeout, nonblock);
        loop {
            let taken = net::core().with(|stack, _| stack.accept(self.id));
            match taken {
                Ok(id) => {
                    let peer = net::core()
                        .with(|stack, _| stack.remote_endpoint(id))
                        .unwrap_or(Endpoint::new(self.unspecified(), 0));
                    let file = Self::wrap(id, self.family, self.kind, nonblock, owner)?;
                    return Ok((file, self.encode(peer)));
                }
                Err(Error::WouldBlock) if nonblock => return Err(Errno::EAGAIN),
                Err(Error::WouldBlock) => {}
                Err(other) => return Err(errno(other)),
            }
            self.wait(|| self.readiness().readable, deadline)?;
        }
    }

    /// Connect to a peer.
    ///
    /// A stream socket's connect blocks until the handshake finishes, unless
    /// the descriptor is non-blocking, in which case it answers `EINPROGRESS`
    /// and the program waits with `poll`, as Linux does.
    pub(crate) fn connect(&self, raw: &[u8], nonblock: bool) -> Result<(), Errno> {
        let endpoint = self.endpoint_of(raw)?;
        let started = net::core().with(|stack, at| stack.connect(self.id, endpoint, at));
        match started {
            Ok(()) => {}
            Err(Error::AlreadyDone) if self.is_connected() => return Err(Errno::EISCONN),
            Err(Error::AlreadyDone) => return Err(Errno::EALREADY),
            Err(other) => return Err(errno(other)),
        }
        if !self.kind.is_stream() {
            return Ok(());
        }
        if nonblock {
            return Err(Errno::EINPROGRESS);
        }
        let deadline = self.deadline(self.options.lock().send_timeout, false);
        loop {
            let ready = self.readiness();
            if ready.error {
                let error = net::core().with(|stack, _| stack.take_error(self.id));
                return Err(error.map_or(Errno::ECONNREFUSED, errno));
            }
            if ready.writable {
                return Ok(());
            }
            self.wait(
                || {
                    let ready = self.readiness();
                    ready.writable || ready.error
                },
                deadline,
            )?;
        }
    }

    /// Stop one or both directions.
    pub(crate) fn shutdown(&self, how: u32) -> Result<(), Errno> {
        let how = match how {
            SHUT_RD => Shutdown::Read,
            SHUT_WR => Shutdown::Write,
            SHUT_RDWR => Shutdown::Both,
            _ => return Err(Errno::EINVAL),
        };
        if self.kind.is_stream() && !self.is_connected() {
            return Err(Errno::ENOTCONN);
        }
        net::core()
            .with(|stack, _| stack.shutdown(self.id, how))
            .map_err(errno)
    }

    /// Send `data`, to `to` if it is given.
    pub(crate) fn send(
        &self,
        data: &[u8],
        flags: u32,
        nonblock: bool,
        to: Option<&[u8]>,
    ) -> Result<usize, Errno> {
        let destination = match to {
            Some(raw) => Some(self.endpoint_of(raw)?),
            None => None,
        };
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        let deadline = self.deadline(self.options.lock().send_timeout, nonblock);
        loop {
            let sent = net::core().with(|stack, at| stack.send(self.id, data, destination, at));
            match sent {
                Ok(count) => return Ok(count),
                Err(Error::WouldBlock) if nonblock => return Err(Errno::EAGAIN),
                Err(Error::WouldBlock) => {}
                Err(Error::InProgress) if nonblock => return Err(Errno::EAGAIN),
                Err(Error::InProgress) => {}
                Err(other) => return Err(errno(other)),
            }
            self.wait(
                || {
                    let ready = self.readiness();
                    ready.writable || ready.error || ready.hangup
                },
                deadline,
            )?;
        }
    }

    /// Take received bytes, and the address they came from.
    pub(crate) fn recv(
        &self,
        out: &mut [u8],
        flags: u32,
        nonblock: bool,
    ) -> Result<(Received, Option<Vec<u8>>), Errno> {
        let peek = flags & MSG_PEEK != 0;
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        let deadline = self.deadline(self.options.lock().receive_timeout, nonblock);
        loop {
            let taken = net::core().with(|stack, _| stack.recv(self.id, out, peek));
            match taken {
                Ok(received) => {
                    let full = received.bytes + received.truncated;
                    let from = received.remote.map(|endpoint| self.encode(endpoint));
                    return Ok((
                        Received {
                            bytes: received.bytes,
                            full,
                        },
                        from,
                    ));
                }
                Err(Error::WouldBlock) if nonblock => return Err(Errno::EAGAIN),
                Err(Error::WouldBlock) => {}
                Err(other) => return Err(errno(other)),
            }
            self.wait(
                || {
                    let ready = self.readiness();
                    ready.readable || ready.error || ready.hangup
                },
                deadline,
            )?;
        }
    }

    /// The name it is bound to, encoded as a `sockaddr`.
    pub(crate) fn local_name(&self) -> Vec<u8> {
        let endpoint = net::core()
            .with(|stack, _| stack.local_endpoint(self.id))
            .unwrap_or(Endpoint::new(self.unspecified(), 0));
        self.encode(endpoint)
    }

    /// The name of its peer, if it has one.
    pub(crate) fn peer_name(&self) -> Option<Vec<u8>> {
        let endpoint = net::core().with(|stack, _| stack.remote_endpoint(self.id))?;
        Some(self.encode(endpoint))
    }

    /// The unspecified address of this socket's family.
    fn unspecified(&self) -> IpAddress {
        match self.family {
            Family::V4 => IpAddress::V4(Ipv4::UNSPECIFIED),
            Family::V6 => IpAddress::V6(Ipv6::UNSPECIFIED),
        }
    }

    /// Read a `sockaddr` a program passed, and refuse one of the wrong family.
    fn endpoint_of(&self, raw: &[u8]) -> Result<Endpoint, Errno> {
        let address = InetAddress::parse(raw, raw.len()).map_err(|_| Errno::EINVAL)?;
        let wanted = match self.family {
            Family::V4 => AF_INET,
            Family::V6 => AF_INET6,
        };
        if address.family() != wanted {
            return Err(Errno::EAFNOSUPPORT);
        }
        Ok(match address {
            InetAddress::V4 { port, address } => {
                Endpoint::new(IpAddress::V4(Ipv4::new(address)), port)
            }
            InetAddress::V6 { port, address, .. } => {
                let six = Ipv6::new(address);
                match six.v4_mapped() {
                    Some(four) => Endpoint::new(IpAddress::V4(four), port),
                    None => Endpoint::new(IpAddress::V6(six), port),
                }
            }
        })
    }

    /// Write an endpoint as the `sockaddr` this socket's family uses.
    ///
    /// An `AF_INET6` socket reports an IPv4 peer as `::ffff:a.b.c.d`, which is
    /// what a program that opened one expects to read back.
    fn encode(&self, endpoint: Endpoint) -> Vec<u8> {
        let address = match self.family {
            Family::V4 => endpoint.address,
            Family::V6 => to_v6(endpoint.address),
        };
        let inet = match address {
            IpAddress::V4(four) => InetAddress::V4 {
                port: endpoint.port,
                address: four.octets(),
            },
            IpAddress::V6(six) => InetAddress::V6 {
                port: endpoint.port,
                flow_info: 0,
                address: six.octets(),
                scope_id: 0,
            },
        };
        let mut bytes = vec![0_u8; SOCKADDR_STORAGE_SIZE];
        let written = inet.encode(&mut bytes).unwrap_or(0);
        bytes.truncate(written);
        bytes
    }

    /// When a wait must give up.
    fn deadline(&self, timeout: u64, nonblock: bool) -> u64 {
        if nonblock {
            return 0;
        }
        fs::socket::deadline_after(timeout)
    }

    /// Wait for the stack to move, or for a signal or the deadline.
    fn wait(&self, ready: impl FnMut() -> bool, deadline: u64) -> Result<(), Errno> {
        fs::socket::wait_on(net::core().progress(), ready, deadline)
    }
}

/// What a socket option reads or writes.
impl InetSocket {
    /// `getsockopt`: the option's value, in the layout a program reads.
    ///
    /// # Errors
    ///
    /// `ENOPROTOOPT` for an option this socket does not have.
    pub(crate) fn get_option(&self, level: i32, name: i32, width: Width) -> Result<Vec<u8>, Errno> {
        let options = *self.options.lock();
        match (level, name) {
            (SOL_SOCKET, SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW) => {
                Ok(fs::socket::timeval(options.receive_timeout, width))
            }
            (SOL_SOCKET, SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW) => {
                Ok(fs::socket::timeval(options.send_timeout, width))
            }
            _ => Ok(self.option_value(level, name)?.to_le_bytes().to_vec()),
        }
    }

    /// The number an option reads as.
    fn option_value(&self, level: i32, name: i32) -> Result<i32, Errno> {
        let options = *self.options.lock();
        match (level, name) {
            (SOL_SOCKET, SO_TYPE) => Ok(self.kind.linux().cast_signed()),
            (SOL_SOCKET, SO_DOMAIN) => Ok(match self.family {
                Family::V4 => i32::from(AF_INET),
                Family::V6 => i32::from(AF_INET6),
            }),
            (SOL_SOCKET, SO_PROTOCOL) => Ok(self.kind.protocol(self.family)),
            (SOL_SOCKET, SO_ACCEPTCONN) => Ok(i32::from(self.is_listening())),
            (SOL_SOCKET, SO_ERROR) => {
                let error = net::core().with(|stack, _| stack.take_error(self.id));
                Ok(error.map_or(0, |error| i32::from(errno(error).0)))
            }
            (SOL_SOCKET, SO_SNDBUF) => Ok(i32::try_from(options.send_buffer).unwrap_or(i32::MAX)),
            (SOL_SOCKET, SO_RCVBUF) => {
                Ok(i32::try_from(options.receive_buffer).unwrap_or(i32::MAX))
            }
            (SOL_SOCKET, SO_KEEPALIVE) => Ok(i32::from(options.keepalive)),
            (SOL_SOCKET, SO_REUSEADDR) => Ok(i32::from(self.stack_options().reuse_address)),
            (SOL_SOCKET, SO_BROADCAST) => Ok(i32::from(self.stack_options().broadcast)),
            (SOL_IP, IP_TTL) | (SOL_IPV6, IPV6_UNICAST_HOPS) => {
                Ok(i32::from(self.stack_options().hop_limit))
            }
            (SOL_IP, IP_TOS) => Ok(i32::from(self.stack_options().traffic_class)),
            (SOL_IPV6, IPV6_V6ONLY) => Ok(i32::from(self.stack_options().v6_only)),
            (SOL_TCP, TCP_NODELAY) => Ok(i32::from(self.nodelay())),
            (SOL_TCP, TCP_MAXSEG) => Ok(self.segment_size()),
            _ => Err(Errno::ENOPROTOOPT),
        }
    }

    /// Whether it is a listening socket.
    fn is_listening(&self) -> bool {
        net::core().with(|stack, _| {
            matches!(stack.socket(self.id), Some(NetSocket::Listen(listener)) if listener.backlog > 0)
        })
    }

    /// `setsockopt`.
    ///
    /// # Errors
    ///
    /// `ENOPROTOOPT` for a level this socket does not have, `EINVAL` for a
    /// value that is the wrong size or out of range.
    pub(crate) fn set_option(
        &self,
        level: i32,
        name: i32,
        value: &[u8],
        width: Width,
    ) -> Result<(), Errno> {
        if matches!(
            (level, name),
            (
                SOL_SOCKET,
                SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW | SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW
            )
        ) {
            let nanos = fs::socket::read_timeval(value, width)?;
            let mut options = self.options.lock();
            if name == SO_RCVTIMEO_OLD || name == SO_RCVTIMEO_NEW {
                options.receive_timeout = nanos;
            } else {
                options.send_timeout = nanos;
            }
            return Ok(());
        }
        let number = read_int(value)?;
        match (level, name) {
            (SOL_SOCKET, SO_SNDBUF) => {
                self.options.lock().send_buffer = clamp_buffer(number);
                Ok(())
            }
            (SOL_SOCKET, SO_RCVBUF) => {
                self.options.lock().receive_buffer = clamp_buffer(number);
                Ok(())
            }
            (SOL_SOCKET, SO_KEEPALIVE) => {
                self.options.lock().keepalive = number != 0;
                Ok(())
            }
            (SOL_SOCKET, SO_REUSEADDR) => {
                self.with_options(|options| options.reuse_address = number != 0);
                Ok(())
            }
            (SOL_SOCKET, SO_BROADCAST) => {
                self.with_options(|options| options.broadcast = number != 0);
                Ok(())
            }
            (SOL_IP, IP_TTL) | (SOL_IPV6, IPV6_UNICAST_HOPS) => {
                if !(-1..=255).contains(&number) {
                    return Err(Errno::EINVAL);
                }
                let hops = u8::try_from(number).unwrap_or(64);
                self.with_options(|options| options.hop_limit = hops.max(1));
                Ok(())
            }
            (SOL_IP, IP_TOS) => {
                let class = u8::try_from(number & 0xFF).unwrap_or(0);
                self.with_options(|options| options.traffic_class = class);
                Ok(())
            }
            (SOL_IPV6, IPV6_V6ONLY) => {
                if matches!(self.family, Family::V4) {
                    return Err(Errno::ENOPROTOOPT);
                }
                self.with_options(|options| options.v6_only = number != 0);
                Ok(())
            }
            (SOL_TCP, TCP_NODELAY) => {
                self.set_nodelay(number != 0);
                Ok(())
            }
            // An option a program may set that changes nothing here is
            // accepted rather than refused: refusing one makes a program that
            // sets it for luck fail, and Linux accepts them all.
            (SOL_SOCKET | SOL_IP | SOL_IPV6 | SOL_TCP, _) => Ok(()),
            _ => Err(Errno::ENOPROTOOPT),
        }
    }

    /// The options the stack keeps for this socket.
    fn stack_options(&self) -> ferrix_net::socket::Options {
        net::core().with(|stack, _| {
            stack
                .socket(self.id)
                .map_or(ferrix_net::socket::Options::default(), NetSocket::options)
        })
    }

    /// Change the options the stack keeps.
    fn with_options(&self, body: impl FnOnce(&mut ferrix_net::socket::Options)) {
        net::core().with(|stack, _| {
            if let Some(socket) = stack.socket_mut(self.id) {
                body(socket.options_mut());
            }
        });
    }

    /// Whether Nagle's algorithm is off.
    fn nodelay(&self) -> bool {
        net::core().with(|stack, _| match stack.socket(self.id) {
            Some(NetSocket::Stream(stream)) => stream.connection.config().no_delay,
            _ => false,
        })
    }

    /// Turn Nagle's algorithm off or on.
    fn set_nodelay(&self, off: bool) {
        net::core().with(|stack, _| {
            if let Some(NetSocket::Stream(stream)) = stack.socket_mut(self.id) {
                stream.connection.set_no_delay(off);
            }
        });
    }

    /// The largest segment the connection sends.
    fn segment_size(&self) -> i32 {
        net::core().with(|stack, _| match stack.socket(self.id) {
            Some(NetSocket::Stream(stream)) => i32::from(stream.connection.segment_size()),
            _ => i32::from(ferrix_nettcp::conn::DEFAULT_MSS),
        })
    }

    /// `SIOCINQ` and `SIOCOUTQ`, which ask what is queued each way.
    ///
    /// # Errors
    ///
    /// `ENOTTY` for any other request, which is what a socket answers.
    pub(crate) fn ioctl(
        &self,
        process: &crate::syscall::process::Process,
        request: u32,
        arg: u64,
    ) -> Result<usize, Errno> {
        let count = match request {
            ferrix_linux_abi::socket::SIOCINQ => self.queued(),
            ferrix_linux_abi::socket::SIOCOUTQ => self.unsent(),
            // Everything else is about an interface rather than about this
            // socket, and `sys_ioctl` sends it on to `net::ifreq` for every
            // socket family alike.
            _ => return Err(Errno::ENOTTY),
        };
        crate::syscall::uaccess::put_u32(
            process.space(),
            arg,
            u32::try_from(count).unwrap_or(u32::MAX),
        )?;
        Ok(0)
    }

    /// How many bytes are waiting to be read, which `SIOCINQ` reports.
    fn queued(&self) -> usize {
        net::core().with(|stack, _| match stack.socket(self.id) {
            Some(NetSocket::Stream(stream)) => stream.connection.receive_queued(),
            Some(NetSocket::Udp(socket) | NetSocket::Icmp(socket)) => {
                socket.peek().map_or(0, |datagram| datagram.payload.len())
            }
            _ => 0,
        })
    }

    /// How many bytes are waiting to be sent, which `SIOCOUTQ` reports.
    fn unsent(&self) -> usize {
        net::core().with(|stack, _| match stack.socket(self.id) {
            Some(NetSocket::Stream(stream)) => stream.connection.send_queued(),
            _ => 0,
        })
    }
}

impl Inode for InetSocket {
    fn metadata(&self) -> Metadata {
        self.metadata
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll(&self) -> Readiness {
        self.readiness()
    }

    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> ferrix_vfs::Result<usize> {
        self.recv(buf, 0, nonblock)
            .map(|(received, _from)| received.bytes)
    }

    fn write_stream(&self, data: &[u8], nonblock: bool) -> ferrix_vfs::Result<usize> {
        self.send(data, 0, nonblock, None)
    }
}

/// The socket an open file reads and writes through, if it is an inet one.
pub(crate) fn of(file: &OpenFile) -> Option<Arc<InetSocket>> {
    Arc::clone(file.io())
        .into_any()
        .downcast::<InetSocket>()
        .ok()
}

/// Which errno the net core's answer is.
pub(crate) const fn errno(error: Error) -> Errno {
    match error {
        Error::NoSocket => Errno::EBADF,
        Error::WrongKind => Errno::EOPNOTSUPP,
        Error::AlreadyDone => Errno::EINVAL,
        Error::NotConnected => Errno::EDESTADDRREQ,
        Error::AddressInUse => Errno::EADDRINUSE,
        Error::AddressNotAvailable => Errno::EADDRNOTAVAIL,
        Error::Unreachable => Errno::ENETUNREACH,
        Error::Refused | Error::PortUnreachable => Errno::ECONNREFUSED,
        Error::Reset => Errno::ECONNRESET,
        Error::InProgress => Errno::EAGAIN,
        Error::WouldBlock => Errno::EAGAIN,
        Error::TooLarge => Errno::EMSGSIZE,
        Error::ShutDown => Errno::EPIPE,
        Error::Invalid => Errno::EINVAL,
        Error::NoMemory => Errno::ENOBUFS,
        Error::TimedOut => Errno::ETIMEDOUT,
    }
}

/// An option's value as the `int` most of them are.
fn read_int(value: &[u8]) -> Result<i32, Errno> {
    let bytes = value.first_chunk::<4>().ok_or(Errno::EINVAL)?;
    Ok(i32::from_ne_bytes(*bytes))
}

/// A buffer size as Linux stores it: doubled, and held between its bounds.
fn clamp_buffer(requested: i32) -> usize {
    let wanted = usize::try_from(requested.max(0))
        .unwrap_or(0)
        .saturating_mul(2);
    wanted.clamp(
        ferrix_linux_abi::socket::SOCKET_BUFFER_MIN,
        ferrix_linux_abi::socket::SOCKET_BUFFER_MAX,
    )
}
