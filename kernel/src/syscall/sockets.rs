//! The socket calls: `AF_UNIX` sockets, and an honest refusal for every other
//! family.
//!
//! # What there is
//!
//! `socket` and `socketpair` make `AF_UNIX` stream, sequenced-packet and
//! datagram sockets (`crate::fs::socket`). Every other family is refused with
//! `EAFNOSUPPORT`, the answer Linux gives for a family it was built without,
//! and the one every program already handles. The calls on a socket send and
//! receive, shut it down, name it unnamed, and ask and set its options.
//!
//! Naming a socket -- `bind`, `listen`, `connect`, `accept`, and a datagram
//! sent to a name -- and passing descriptors are `EOPNOTSUPP` on a socket until
//! they land. On a descriptor that is not a socket every call is `ENOTSOCK`,
//! and on a closed one `EBADF`, exactly as Linux answers.
//!
//! # Linux's order
//!
//! The checks Linux makes before it looks at the descriptor are made first, in
//! its order -- a bad flag is `EINVAL`, a buffer outside the user half `EFAULT`
//! -- so a program sees the same first error it would on Linux.
//!
//! # One copy in, one copy out
//!
//! A send gathers the program's buffers into one kernel buffer and a receive
//! scatters one out, so a record is queued and taken whole whatever iovecs
//! carried it. One call holds at most [`MAX_TRANSFER`] in the kernel: a stream
//! send past it is a short send, as a full buffer makes one anyway, and a record
//! that large is larger than any socket buffer and refused regardless.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_bootinfo::is_user_address;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::inet::{
    IPPROTO_ICMP, IPPROTO_ICMPV6, IPPROTO_MAX, IPPROTO_TCP, IPPROTO_UDP, SOCKADDR_STORAGE_SIZE,
};
use ferrix_linux_abi::netlink::NETLINK_ROUTE;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::socket::{
    AF_INET, AF_INET6, AF_MAX, AF_NETLINK, AF_UNIX, ControlMessages, MSG_CMSG_COMPAT, MSG_OOB,
    MSG_TRUNC, MsgHdr, SCM_CREDENTIALS, SCM_RIGHTS, SOCK_CLOEXEC, SOCK_DGRAM, SOCK_NONBLOCK,
    SOCK_RAW, SOCK_STREAM, SOCK_TYPE_MASK, SOL_SOCKET, UnixAddress, Width,
};
use ferrix_net::socket::Family;
use ferrix_vfs::OpenFile;

use crate::fs;
use crate::fs::socket::{Received, Socket, SocketType};
use crate::net::netlink::{self as netlink, NetlinkSocket};
use crate::net::socket::{self as inet, InetKind, InetSocket};
use crate::syscall::attributes::int;
use crate::syscall::fd;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// One past the last socket type: `SOCK_MAX` in `linux/net.h`.
const SOCK_MAX: u32 = 11;

/// This build's pointer width, which is the width of every `msghdr`,
/// `cmsghdr` and `timeval` a program hands it.
const NATIVE: Width = if size_of::<usize>() == 8 {
    Width::Bits64
} else {
    Width::Bits32
};

/// Linux's `MAX_RW_COUNT`: no transfer reports more, so the count fits a
/// 32-bit return register.
const MAX_RW_COUNT: usize = 0x7FFF_F000;

/// `UIO_MAXIOV`: the most iovecs one message may have.
const UIO_MAXIOV: u64 = 1024;

/// The most one send or receive holds in the kernel at once.
const MAX_TRANSFER: usize = 16 << 20;

/// The longest control buffer a send takes: Linux's default `optmem_max`.
const MAX_CONTROL: usize = 20_480;

/// The longest option value `setsockopt` reads.
const MAX_OPTION: usize = 64;

/// Answer `call` if it is one of this module's.
pub(crate) fn dispatch(
    call: Syscall,
    a: &[u64; 6],
    process: &Process,
) -> Option<Result<usize, Errno>> {
    let descriptor = fd::arg(a[0]);
    let answer = match call {
        Syscall::Socket => sys_socket(process, int(a[0]), a[1] as u32, int(a[2])),
        Syscall::Socketpair => sys_socketpair(process, int(a[0]), a[1] as u32, int(a[2]), a[3]),
        Syscall::Bind => sys_bind(process, descriptor, a[1], a[2]),
        Syscall::Listen => sys_listen(process, descriptor, int(a[1])),
        Syscall::Connect => sys_connect(process, descriptor, a[1], a[2]),
        Syscall::Accept => sys_accept(process, descriptor, a[1], a[2], 0),
        Syscall::Accept4 => sys_accept(process, descriptor, a[1], a[2], a[3] as u32),
        Syscall::Getsockname => sys_getname(process, descriptor, a[1], a[2], false),
        Syscall::Getpeername => sys_getname(process, descriptor, a[1], a[2], true),
        Syscall::Shutdown => sys_shutdown(process, descriptor, a[1] as u32),
        Syscall::Sendto => sys_sendto(
            process,
            descriptor,
            a[1],
            a[2],
            a[3] as u32,
            a[4],
            name_length(a[4], a[5]),
        ),
        Syscall::Recvfrom => sys_recvfrom(process, descriptor, a[1], a[2], a[3] as u32, a[4], a[5]),
        Syscall::Sendmsg => sys_sendmsg(process, descriptor, a[1], a[2] as u32),
        Syscall::Recvmsg => sys_recvmsg(process, descriptor, a[1], a[2] as u32),
        Syscall::Getsockopt => {
            sys_getsockopt(process, descriptor, int(a[1]), int(a[2]), a[3], a[4])
        }
        Syscall::Setsockopt => {
            sys_setsockopt(process, descriptor, int(a[1]), int(a[2]), a[3], int(a[4]))
        }
        _ => return None,
    };
    Some(answer)
}

/// A socket reached through a descriptor, whatever family it is.
///
/// The syscall layer is one set of checks in Linux's order, and only the last
/// step of each call differs between the families. This is that step: every
/// call above takes an `Any` and asks it, so the order of the checks cannot
/// drift apart between `AF_UNIX` and `AF_INET`.
#[derive(Debug)]
enum Any {
    /// An `AF_UNIX` socket.
    Unix(Arc<Socket>),
    /// An `AF_INET` or `AF_INET6` socket.
    Inet(Arc<InetSocket>),
    /// An `AF_NETLINK` socket.
    Netlink(Arc<NetlinkSocket>),
}

impl Any {
    /// Its type, as `SO_TYPE` reports it.
    fn kind(&self) -> SocketType {
        match self {
            Any::Unix(socket) => socket.kind(),
            Any::Inet(socket) => match socket.kind() {
                InetKind::Stream => SocketType::Stream,
                InetKind::Datagram | InetKind::Echo | InetKind::Raw { .. } => SocketType::Datagram,
            },
            // `SOCK_RAW` and `SOCK_DGRAM` are the same socket on netlink, and
            // both carry records.
            Any::Netlink(_) => SocketType::Datagram,
        }
    }

    /// Whether it has a peer.
    fn is_connected(&self) -> bool {
        match self {
            Any::Unix(socket) => socket.is_connected(),
            Any::Inet(socket) => socket.is_connected(),
            // A netlink socket has no peer: it talks to the kernel, which is
            // not a socket `getpeername` can name.
            Any::Netlink(_) => false,
        }
    }

    /// Stop one or both directions.
    fn shutdown(&self, how: u32) -> Result<(), Errno> {
        match self {
            Any::Unix(socket) => socket.shutdown(how),
            Any::Inet(socket) => socket.shutdown(how),
            Any::Netlink(socket) => socket.shutdown(how),
        }
    }

    /// Send, to `to` if the call named a destination.
    fn send(
        &self,
        data: &[u8],
        flags: u32,
        nonblock: bool,
        to: Option<&[u8]>,
    ) -> Result<usize, Errno> {
        match self {
            Any::Unix(socket) => socket.send(data, flags, nonblock),
            Any::Inet(socket) => socket.send(data, flags, nonblock, to),
            // Every netlink request is answered as it is made, so neither the
            // flags that ask not to wait nor the descriptor's own change
            // anything.
            Any::Netlink(socket) => socket.send(data, to),
        }
    }

    /// Receive, and say where it came from.
    fn recv(
        &self,
        out: &mut [u8],
        flags: u32,
        nonblock: bool,
    ) -> Result<(Received, Option<Vec<u8>>), Errno> {
        match self {
            Any::Unix(socket) => socket.recv(out, flags, nonblock).map(|taken| (taken, None)),
            Any::Inet(socket) => socket.recv(out, flags, nonblock).map(|(taken, from)| {
                (
                    Received {
                        bytes: taken.bytes,
                        full: taken.full,
                    },
                    from,
                )
            }),
            Any::Netlink(socket) => socket.recv(out, flags, nonblock).map(|(taken, from)| {
                (
                    Received {
                        bytes: taken.bytes,
                        full: taken.full,
                    },
                    from,
                )
            }),
        }
    }

    /// An option's value.
    fn get_option(&self, level: i32, name: i32, width: Width) -> Result<Vec<u8>, Errno> {
        match self {
            Any::Unix(socket) => socket.get_option(level, name, width),
            Any::Inet(socket) => socket.get_option(level, name, width),
            Any::Netlink(socket) => socket.get_option(level, name, width),
        }
    }

    /// Set an option.
    fn set_option(&self, level: i32, name: i32, value: &[u8], width: Width) -> Result<(), Errno> {
        match self {
            Any::Unix(socket) => socket.set_option(level, name, value, width),
            Any::Inet(socket) => socket.set_option(level, name, value, width),
            Any::Netlink(socket) => socket.set_option(level, name, value, width),
        }
    }

    /// The name it is bound to, as a `sockaddr`.
    ///
    /// An `AF_UNIX` socket without a name is `AF_UNIX` alone, two bytes long,
    /// which is what Linux reports for one.
    fn local_name(&self) -> Result<Vec<u8>, Errno> {
        match self {
            Any::Unix(socket) => Ok(socket.sock_name()),
            Any::Inet(socket) => Ok(socket.local_name()),
            Any::Netlink(socket) => Ok(socket.local_name()),
        }
    }

    /// The name of its peer.
    fn peer_name(&self) -> Result<Vec<u8>, Errno> {
        match self {
            Any::Unix(socket) => socket.peer_sock_name(),
            Any::Inet(socket) => socket.peer_name().ok_or(Errno::ENOTCONN),
            Any::Netlink(_) => Err(Errno::ENOTCONN),
        }
    }

    /// Give it a name.
    fn bind(&self, process: &Process, raw: &[u8]) -> Result<(), Errno> {
        match self {
            Any::Unix(socket) => {
                let address = unix_address(raw)?;
                socket.bind(&crate::syscall::path::context(process), &address)
            }
            Any::Inet(socket) => socket.bind(raw),
            Any::Netlink(socket) => socket.bind(raw),
        }
    }

    /// Start listening.
    fn listen(&self, backlog: i32) -> Result<(), Errno> {
        match self {
            Any::Unix(socket) => socket.listen(backlog),
            Any::Netlink(_) => Err(Errno::EOPNOTSUPP),
            Any::Inet(socket) => socket.listen(backlog),
        }
    }

    /// Connect to a peer.
    fn connect(&self, process: &Process, raw: &[u8], nonblock: bool) -> Result<(), Errno> {
        match self {
            Any::Unix(socket) => {
                let address = unix_address(raw)?;
                socket.connect(&crate::syscall::path::context(process), &address, nonblock)
            }
            Any::Netlink(_) => Err(Errno::EOPNOTSUPP),
            Any::Inet(socket) => socket.connect(raw, nonblock),
        }
    }

    /// Take a connection, and say who made it.
    fn accept(&self, nonblock: bool, owner: (u32, u32)) -> Result<(Arc<OpenFile>, Vec<u8>), Errno> {
        match self {
            Any::Unix(socket) => socket.accept(nonblock),
            Any::Netlink(_) => Err(Errno::EOPNOTSUPP),
            Any::Inet(socket) => socket.accept(nonblock, owner),
        }
    }
}

/// The `AF_UNIX` address in the bytes a program passed, which Linux refuses
/// with `EINVAL` however it is wrong.
fn unix_address(raw: &[u8]) -> Result<UnixAddress<'_>, Errno> {
    UnixAddress::parse(raw, raw.len()).map_err(|_| Errno::EINVAL)
}

/// What a `socket` call asked for.
#[derive(Clone, Copy, Debug)]
enum Opened {
    /// An `AF_UNIX` socket of this type.
    Unix(SocketType),
    /// An `AF_INET` or `AF_INET6` socket of this kind.
    Inet(Family, InetKind),
    /// An `AF_NETLINK` socket of this type.
    Netlink(u32),
}

/// Whether `socket` may open what `socket_type` named: a raw socket needs
/// `CAP_NET_RAW`, which here is being root, as for every capability.
///
/// Asked after the type and protocol are known to exist, as `inet_create`
/// asks it after its protocol lookup, so a raw socket at protocol zero is
/// `EPROTONOSUPPORT` for everyone.
fn permitted(process: &Process, opened: &Opened) -> Result<(), Errno> {
    let raw = matches!(opened, Opened::Inet(_, InetKind::Raw { .. }));
    if raw && !process.with_credentials(|ids| ids.privileged()) {
        return Err(Errno::EPERM);
    }
    Ok(())
}

/// Only `SOCK_NONBLOCK` and `SOCK_CLOEXEC` may accompany a type, or be given
/// to `accept4`.
fn known_flags(flags: u32) -> Result<(), Errno> {
    if flags & !(SOCK_CLOEXEC | SOCK_NONBLOCK) != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// What `socket`'s arguments name: `__sys_socket`, `__sock_create` and the
/// family's own create function, in their order.
fn socket_type(family: i32, kind: u32, protocol: i32) -> Result<Opened, Errno> {
    known_flags(kind & !SOCK_TYPE_MASK)?;
    if !(0..i32::from(AF_MAX)).contains(&family) {
        return Err(Errno::EAFNOSUPPORT);
    }
    if kind & SOCK_TYPE_MASK >= SOCK_MAX {
        return Err(Errno::EINVAL);
    }
    let kind = kind & SOCK_TYPE_MASK;
    if family == i32::from(AF_INET) {
        return inet_type(Family::V4, kind, protocol);
    }
    if family == i32::from(AF_INET6) {
        return inet_type(Family::V6, kind, protocol);
    }
    if family == i32::from(AF_NETLINK) {
        return netlink_type(kind, protocol);
    }
    if family != i32::from(AF_UNIX) {
        return Err(Errno::EAFNOSUPPORT);
    }
    // `PF_UNIX`, which is `AF_UNIX`, is the one protocol besides zero.
    if protocol != 0 && protocol != i32::from(AF_UNIX) {
        return Err(Errno::EPROTONOSUPPORT);
    }
    SocketType::from_linux(kind)
        .map(Opened::Unix)
        .ok_or(Errno::ESOCKTNOSUPPORT)
}

/// The `AF_INET` or `AF_INET6` socket a type and a protocol name.
///
/// `SOCK_RAW` takes any protocol but zero, which `inet_create`'s lookup
/// finds no match for, and `IPPROTO_MAX` and above, which it refuses first.
/// Whether the caller may have one is [`permitted`]'s question. An `AF_INET6`
/// raw socket is not implemented yet and is `EPROTONOSUPPORT` to a caller
/// who could otherwise have had it.
fn inet_type(family: Family, kind: u32, protocol: i32) -> Result<Opened, Errno> {
    if !(0..IPPROTO_MAX).contains(&protocol) {
        return Err(Errno::EINVAL);
    }
    let echo = match family {
        Family::V4 => IPPROTO_ICMP,
        Family::V6 => IPPROTO_ICMPV6,
    };
    match (kind, protocol) {
        (SOCK_STREAM, 0 | IPPROTO_TCP) => Ok(Opened::Inet(family, InetKind::Stream)),
        (SOCK_DGRAM, 0 | IPPROTO_UDP) => Ok(Opened::Inet(family, InetKind::Datagram)),
        (SOCK_DGRAM, given) if given == echo => Ok(Opened::Inet(family, InetKind::Echo)),
        (SOCK_RAW, 0) => Err(Errno::EPROTONOSUPPORT),
        (SOCK_RAW, given) => match (family, u8::try_from(given)) {
            (Family::V4, Ok(protocol)) => Ok(Opened::Inet(family, InetKind::Raw { protocol })),
            _ => Err(Errno::EPROTONOSUPPORT),
        },
        (SOCK_STREAM | SOCK_DGRAM, _) => Err(Errno::EPROTONOSUPPORT),
        _ => Err(Errno::ESOCKTNOSUPPORT),
    }
}

/// The `AF_NETLINK` socket a type and a protocol name.
///
/// `netlink_create` takes `SOCK_RAW` and `SOCK_DGRAM` and nothing else, and
/// makes no distinction between them: a netlink socket carries records
/// whichever was asked for. `NETLINK_ROUTE` is the one protocol this kernel
/// has; the others are `EPROTONOSUPPORT`, which is what Linux answers for a
/// family built without them.
fn netlink_type(kind: u32, protocol: i32) -> Result<Opened, Errno> {
    if kind != SOCK_DGRAM && kind != SOCK_RAW {
        return Err(Errno::ESOCKTNOSUPPORT);
    }
    if protocol != NETLINK_ROUTE {
        return Err(Errno::EPROTONOSUPPORT);
    }
    Ok(Opened::Netlink(kind))
}

/// `socket`.
pub(crate) fn sys_socket(
    process: &Process,
    family: i32,
    kind: u32,
    protocol: i32,
) -> Result<usize, Errno> {
    let opened = socket_type(family, kind, protocol)?;
    permitted(process, &opened)?;
    let owner = crate::syscall::path::creator_ids(process);
    let nonblock = kind & SOCK_NONBLOCK != 0;
    let file = match opened {
        Opened::Unix(socket_type) => fs::socket::new_socket(socket_type, nonblock, process)?,
        Opened::Inet(family, kind) => InetSocket::open(family, kind, nonblock, owner)?,
        Opened::Netlink(kind) => NetlinkSocket::open(kind, nonblock, owner)?,
    };
    let descriptor = process
        .files()
        .lock()
        .insert(file, kind & SOCK_CLOEXEC != 0)?;
    usize::try_from(descriptor).map_err(|_| Errno::EMFILE)
}

/// `socketpair`: two sockets connected to each other, written into `pair`.
///
/// Linux reserves the two descriptors and writes their numbers before it
/// creates the sockets, so an unwritable pointer is `EFAULT` ahead of the
/// family's refusal. The pointer's range is checked here; a mapped-but-
/// unwritable page inside it is the one case answered after the sockets are
/// made, and then both descriptors are closed again.
pub(crate) fn sys_socketpair(
    process: &Process,
    family: i32,
    kind: u32,
    protocol: i32,
    pair: u64,
) -> Result<usize, Errno> {
    known_flags(kind & !SOCK_TYPE_MASK)?;
    user_buffer(pair, 8)?;
    let opened = socket_type(family, kind, protocol)?;
    permitted(process, &opened)?;
    let socket_type = match opened {
        Opened::Unix(socket_type) => socket_type,
        // `inet_socketpair` is `sock_no_socketpair`: the internet families
        // have no way to make two connected sockets without a listener, and
        // `netlink_ops` leaves the call at the same refusal.
        Opened::Inet(_, _) | Opened::Netlink(_) => return Err(Errno::EOPNOTSUPP),
    };
    let (one, other) = fs::socket::new_pair(socket_type, kind & SOCK_NONBLOCK != 0, process)?;
    let (first, second) = install_pair(process, one, other, kind & SOCK_CLOEXEC != 0)?;
    let numbers: Vec<u8> = first
        .to_le_bytes()
        .into_iter()
        .chain(second.to_le_bytes())
        .collect();
    if uaccess::copy_to_user(process.space(), pair, &numbers).is_err() {
        // Taken out under the lock, dropped after it: see `fd`.
        let taken = {
            let mut files = process.files().lock();
            (files.remove(first), files.remove(second))
        };
        drop(taken);
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

/// Put both sockets of a pair in the table in one hold of its lock, or
/// neither.
///
/// A socket a full table refuses is dropped inside the lock, which is bounded:
/// it closes its directions and wakes their queues.
fn install_pair(
    process: &Process,
    one: Arc<OpenFile>,
    other: Arc<OpenFile>,
    cloexec: bool,
) -> Result<(i32, i32), Errno> {
    let mut files = process.files().lock();
    let first = files.insert(one, cloexec)?;
    match files.insert(other, cloexec) {
        Ok(second) => Ok((first, second)),
        Err(errno) => {
            let displaced = files.remove(first);
            drop(files);
            drop(displaced);
            Err(errno)
        }
    }
}

/// `access_ok`: a buffer must lie in the user half. Only the range is checked,
/// not whether it is mapped, which is all `import_ubuf` checks before the
/// descriptor is looked up.
fn user_buffer(at: u64, len: u64) -> Result<(), Errno> {
    let len = len as usize as u64;
    if len == 0 {
        return Ok(());
    }
    let last = at.checked_add(len - 1).ok_or(Errno::EFAULT)?;
    if is_user_address(at) && is_user_address(last) {
        Ok(())
    } else {
        Err(Errno::EFAULT)
    }
}

/// The socket behind `descriptor`, with the open file it was reached through:
/// `EBADF` if nothing is open there, and `ENOTSOCK` if something that is not a
/// socket is.
fn socket_of(process: &Process, descriptor: i32) -> Result<(Arc<OpenFile>, Any), Errno> {
    let file = fd::file(process, descriptor)?;
    if let Some(socket) = fs::socket::of(&file) {
        return Ok((file, Any::Unix(socket)));
    }
    if let Some(socket) = inet::of(&file) {
        return Ok((file, Any::Inet(socket)));
    }
    let socket = netlink::of(&file).ok_or(Errno::ENOTSOCK)?;
    Ok((file, Any::Netlink(socket)))
}

/// A length a program gave for a buffer it passes by pointer: `EINVAL` if it
/// is negative, as Linux reads it as an `int`.
fn buffer_length(process: &Process, at: u64) -> Result<usize, Errno> {
    let length = uaccess::get_u32(process.space(), at)?;
    let length = length.cast_signed();
    usize::try_from(length).map_err(|_| Errno::EINVAL)
}

/// `getsockname`, and `getpeername` with `peer`.
///
/// An `AF_UNIX` socket without a name is reported as `AF_UNIX` alone, two
/// bytes long, as Linux reports it; an internet socket reports the address and
/// port it is bound or connected to.
fn sys_getname(
    process: &Process,
    descriptor: i32,
    address: u64,
    length: u64,
    peer: bool,
) -> Result<usize, Errno> {
    let (_file, socket) = socket_of(process, descriptor)?;
    if peer && !socket.is_connected() {
        return Err(Errno::ENOTCONN);
    }
    let capacity = buffer_length(process, length)?;
    let encoded = if peer {
        socket.peer_name()?
    } else {
        socket.local_name()?
    };
    write_address(process, address, length, &encoded, capacity)?;
    Ok(0)
}

/// Write a `sockaddr` back to a program, cut to the room it offered, with the
/// length it would have taken.
///
/// That is Linux's `move_addr_to_user`: the length reported is the address's
/// own, not the number of bytes written, so a program that gave a short buffer
/// can tell it was cut.
fn write_address(
    process: &Process,
    address: u64,
    length: u64,
    encoded: &[u8],
    capacity: usize,
) -> Result<(), Errno> {
    if address != 0 {
        let shown = encoded
            .get(..capacity.min(encoded.len()))
            .unwrap_or_default();
        uaccess::copy_to_user(process.space(), address, shown).map_err(|_| Errno::EFAULT)?;
    }
    if length != 0 {
        uaccess::put_u32(
            process.space(),
            length,
            u32::try_from(encoded.len()).unwrap_or(u32::MAX),
        )?;
    }
    Ok(())
}

/// A `sockaddr` a program passed by pointer and length.
fn read_address(process: &Process, address: u64, length: u64) -> Result<Vec<u8>, Errno> {
    let length = length & u64::from(u32::MAX);
    if (length as u32).cast_signed() < 0 {
        return Err(Errno::EINVAL);
    }
    let length = usize::try_from(length).map_err(|_| Errno::EINVAL)?;
    if length > SOCKADDR_STORAGE_SIZE {
        return Err(Errno::EINVAL);
    }
    // `move_addr_to_kernel` takes a length of zero and copies nothing, leaving
    // the family to refuse an address it cannot read. Refusing here instead
    // would answer `EINVAL` where Linux answers whatever the family answers.
    if length == 0 {
        return Ok(Vec::new());
    }
    copy_in(process, address, length)
}

/// `bind`.
fn sys_bind(process: &Process, descriptor: i32, address: u64, length: u64) -> Result<usize, Errno> {
    let (_file, socket) = socket_of(process, descriptor)?;
    let raw = read_address(process, address, length)?;
    socket.bind(process, &raw)?;
    Ok(0)
}

/// `listen`.
fn sys_listen(process: &Process, descriptor: i32, backlog: i32) -> Result<usize, Errno> {
    let (_file, socket) = socket_of(process, descriptor)?;
    socket.listen(backlog)?;
    Ok(0)
}

/// `connect`.
fn sys_connect(
    process: &Process,
    descriptor: i32,
    address: u64,
    length: u64,
) -> Result<usize, Errno> {
    let (file, socket) = socket_of(process, descriptor)?;
    let raw = read_address(process, address, length)?;
    socket.connect(process, &raw, file.status().nonblock)?;
    Ok(0)
}

/// `accept` and `accept4`: the connection is installed as a new descriptor and
/// the peer's address written back.
///
/// The new descriptor's non-blocking and close-on-exec flags come from
/// `accept4`'s own flags and are not inherited from the listener, which is
/// what Linux does and what a program that forgets to set them relies on.
fn sys_accept(
    process: &Process,
    descriptor: i32,
    address: u64,
    length: u64,
    flags: u32,
) -> Result<usize, Errno> {
    known_flags(flags)?;
    let (file, socket) = socket_of(process, descriptor)?;
    let owner = crate::syscall::path::creator_ids(process);
    let nonblock = flags & SOCK_NONBLOCK != 0;
    let (accepted, peer) = socket.accept(file.status().nonblock, owner)?;
    if nonblock {
        let mut status = accepted.status();
        status.nonblock = true;
        accepted.set_status(status);
    }
    let taken = process
        .files()
        .lock()
        .insert(accepted, flags & SOCK_CLOEXEC != 0)?;
    // The address is written last, as `__sys_accept4` writes it: a program
    // that passed an unreadable length still gets its connection taken, and
    // closing the descriptor again is this call's job rather than its
    // caller's.
    if address != 0 {
        let written = buffer_length(process, length)
            .and_then(|capacity| write_address(process, address, length, &peer, capacity));
        if let Err(errno) = written {
            let displaced = process.files().lock().remove(taken);
            drop(displaced);
            return Err(errno);
        }
    }
    usize::try_from(taken).map_err(|_| Errno::EMFILE)
}

/// `shutdown`.
fn sys_shutdown(process: &Process, descriptor: i32, how: u32) -> Result<usize, Errno> {
    let (_file, socket) = socket_of(process, descriptor)?;
    socket.shutdown(how)?;
    Ok(0)
}

/// Whether a send may name a destination of `length` bytes: a connected
/// stream socket refuses one with `EISCONN` and an unconnected one with
/// `EOPNOTSUPP`, as `unix_stream_sendmsg` does; a sequenced-packet socket
/// ignores it, as `unix_seqpacket_sendmsg` does; and a datagram to a name
/// waits for names.
fn check_destination(socket: &Any, length: u64) -> Result<(), Errno> {
    if length == 0 {
        return Ok(());
    }
    if let Any::Netlink(_) = socket {
        // Every netlink send may name the kernel, whatever the socket's type;
        // `netlink_sendmsg` reads the address and refuses only what it holds.
        return Ok(());
    }
    if let Any::Inet(_) = socket {
        // An internet datagram socket takes a destination on every send; a
        // connected stream one refuses it, as `tcp_sendmsg` does.
        return match socket.kind() {
            SocketType::Stream => Err(Errno::EISCONN),
            SocketType::Datagram | SocketType::SeqPacket => Ok(()),
        };
    }
    match socket.kind() {
        SocketType::Stream if socket.is_connected() => Err(Errno::EISCONN),
        SocketType::Stream | SocketType::Datagram => Err(Errno::EOPNOTSUPP),
        SocketType::SeqPacket => Ok(()),
    }
}

/// A transfer length clamped as Linux clamps it, and to what one call holds
/// in the kernel.
fn clamped(length: u64) -> usize {
    usize::try_from(length)
        .unwrap_or(usize::MAX)
        .min(MAX_RW_COUNT)
        .min(MAX_TRANSFER)
}

/// A zeroed kernel buffer of `length` bytes, or `ENOMEM`.
fn zeroed(length: usize) -> Result<Vec<u8>, Errno> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| Errno::ENOMEM)?;
    bytes.resize(length, 0);
    Ok(bytes)
}

/// `length` bytes of the caller's memory.
fn copy_in(process: &Process, at: u64, length: usize) -> Result<Vec<u8>, Errno> {
    let mut bytes = zeroed(length)?;
    if length > 0 {
        uaccess::copy_from_user(process.space(), at, &mut bytes).map_err(|_| Errno::EFAULT)?;
    }
    Ok(bytes)
}

/// What a receive answers: a record's whole length under `MSG_TRUNC`, and the
/// bytes copied otherwise.
fn received_count(socket: &Any, received: Received, flags: u32) -> usize {
    if flags & MSG_TRUNC != 0 && socket.kind() != SocketType::Stream {
        received.full
    } else {
        received.bytes
    }
}

/// The destination length `sendto` was given: none without an address,
/// whatever the length says, since `__sys_sendto` reads one only with one.
fn name_length(address: u64, length: u64) -> u64 {
    if address == 0 { 0 } else { length }
}

/// A message's name length as `__copy_msghdr` reads it: none without a name,
/// and `EINVAL` for a negative one.
fn name_length_of(message: &MsgHdr) -> Result<u64, Errno> {
    if message.name == 0 {
        return Ok(0);
    }
    u64::try_from(message.name_len.cast_signed()).map_err(|_| Errno::EINVAL)
}

/// `sendto`. With no destination, what `send` and `write` do.
fn sys_sendto(
    process: &Process,
    descriptor: i32,
    buffer: u64,
    length: u64,
    flags: u32,
    a_address: u64,
    address_length: u64,
) -> Result<usize, Errno> {
    user_buffer(buffer, length)?;
    let (file, socket) = socket_of(process, descriptor)?;
    let address_length = address_length & u64::from(u32::MAX);
    // `move_addr_to_kernel`'s refusal, made before the send looks at the name.
    if address_length != 0 && (address_length as u32).cast_signed() < 0 {
        return Err(Errno::EINVAL);
    }
    check_destination(&socket, address_length)?;
    if flags & MSG_OOB != 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    let destination = if address_length == 0 {
        None
    } else {
        Some(read_address(process, a_address, address_length)?)
    };
    let data = copy_in(process, buffer, clamped(length))?;
    socket.send(&data, flags, file.status().nonblock, destination.as_deref())
}

/// `recvfrom`. A peer without a name reports an address of length zero, as
/// `unix_copy_addr` leaves it.
fn sys_recvfrom(
    process: &Process,
    descriptor: i32,
    buffer: u64,
    length: u64,
    flags: u32,
    address: u64,
    address_length: u64,
) -> Result<usize, Errno> {
    user_buffer(buffer, length)?;
    let (file, socket) = socket_of(process, descriptor)?;
    if flags & MSG_OOB != 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    let capacity = if address == 0 {
        0
    } else {
        buffer_length(process, address_length)?
    };
    let mut data = zeroed(clamped(length))?;
    let (received, from) = socket.recv(&mut data, flags, file.status().nonblock)?;
    let taken = data.get(..received.bytes).unwrap_or_default();
    uaccess::copy_to_user(process.space(), buffer, taken).map_err(|_| Errno::EFAULT)?;
    if address != 0 {
        let encoded = from.unwrap_or_default();
        write_address(process, address, address_length, &encoded, capacity)?;
    }
    Ok(received_count(&socket, received, flags))
}

/// A program's `struct msghdr`.
fn read_header(process: &Process, at: u64) -> Result<MsgHdr, Errno> {
    let mut bytes = [0_u8; MsgHdr::size(Width::Bits64)];
    let header = bytes.get_mut(..MsgHdr::size(NATIVE)).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, header).map_err(|_| Errno::EFAULT)?;
    MsgHdr::decode(header, NATIVE).ok_or(Errno::EINVAL)
}

/// A message's iovecs, as (address, length) pairs: `EMSGSIZE` past
/// `UIO_MAXIOV`, `EINVAL` for a length that would make the total negative,
/// `EFAULT` for a buffer outside the user half, and the lengths clamped so the
/// total stays within `MAX_RW_COUNT` -- `import_iovec`'s rules.
fn iovecs(process: &Process, message: &MsgHdr) -> Result<Vec<(u64, usize)>, Errno> {
    if message.iov_len > UIO_MAXIOV {
        return Err(Errno::EMSGSIZE);
    }
    let count = usize::try_from(message.iov_len).map_err(|_| Errno::EMSGSIZE)?;
    let word = NATIVE.bytes();
    let raw = copy_in(process, message.iov, count * word * 2)?;
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(count)
        .map_err(|_| Errno::ENOMEM)?;
    let mut total = 0_usize;
    for index in 0..count {
        let at = index * word * 2;
        let base = NATIVE.word(&raw, at).ok_or(Errno::EINVAL)?;
        let length = NATIVE.word(&raw, at + word).ok_or(Errno::EINVAL)?;
        if isize::try_from(length).is_err() {
            return Err(Errno::EINVAL);
        }
        let length = usize::try_from(length)
            .map_err(|_| Errno::EINVAL)?
            .min(MAX_RW_COUNT - total);
        user_buffer(base, length as u64)?;
        total += length;
        segments.push((base, length));
    }
    Ok(segments)
}

/// A send's control messages, checked: none may carry descriptors or
/// credentials until descriptor passing lands, a malformed buffer is `EINVAL`,
/// and messages for levels other than `SOL_SOCKET` are ignored, as
/// `__scm_send` ignores them.
fn check_control(process: &Process, message: &MsgHdr) -> Result<(), Errno> {
    if message.control_len == 0 {
        return Ok(());
    }
    let length = usize::try_from(message.control_len)
        .ok()
        .filter(|&length| length <= MAX_CONTROL)
        .ok_or(Errno::ENOBUFS)?;
    let control = copy_in(process, message.control, length)?;
    for entry in ControlMessages::new(&control, NATIVE) {
        let entry = entry.map_err(|_| Errno::EINVAL)?;
        if entry.level != SOL_SOCKET {
            continue;
        }
        return Err(match entry.kind {
            SCM_RIGHTS | SCM_CREDENTIALS => Errno::EOPNOTSUPP,
            _ => Errno::EINVAL,
        });
    }
    Ok(())
}

/// `sendmsg`: the message's buffers gathered into one send.
fn sys_sendmsg(
    process: &Process,
    descriptor: i32,
    header: u64,
    flags: u32,
) -> Result<usize, Errno> {
    if flags & MSG_CMSG_COMPAT != 0 {
        return Err(Errno::EINVAL);
    }
    let (file, socket) = socket_of(process, descriptor)?;
    let message = read_header(process, header)?;
    let name_length = name_length_of(&message)?;
    check_destination(&socket, name_length)?;
    if flags & MSG_OOB != 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    let destination = if name_length == 0 {
        None
    } else {
        Some(read_address(process, message.name, name_length)?)
    };
    let segments = iovecs(process, &message)?;
    check_control(process, &message)?;
    let total = segments
        .iter()
        .map(|&(_, length)| length)
        .sum::<usize>()
        .min(MAX_TRANSFER);
    let mut data = zeroed(total)?;
    let mut filled = 0;
    for (base, length) in segments {
        let take = length.min(total - filled);
        if take == 0 {
            continue;
        }
        let slot = data.get_mut(filled..filled + take).ok_or(Errno::EINVAL)?;
        uaccess::copy_from_user(process.space(), base, slot).map_err(|_| Errno::EFAULT)?;
        filled += take;
    }
    socket.send(&data, flags, file.status().nonblock, destination.as_deref())
}

/// `recvmsg`: one receive scattered over the message's buffers, with its
/// length fields and flags written back. No control messages arrive yet, and
/// a peer without a name reports an address of length zero.
fn sys_recvmsg(
    process: &Process,
    descriptor: i32,
    header: u64,
    flags: u32,
) -> Result<usize, Errno> {
    if flags & MSG_CMSG_COMPAT != 0 {
        return Err(Errno::EINVAL);
    }
    let (file, socket) = socket_of(process, descriptor)?;
    let message = read_header(process, header)?;
    let _ = name_length_of(&message)?;
    if flags & MSG_OOB != 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    let segments = iovecs(process, &message)?;
    let total = segments
        .iter()
        .map(|&(_, length)| length)
        .sum::<usize>()
        .min(MAX_TRANSFER);
    let capacity = if message.name == 0 {
        0
    } else {
        usize::try_from(message.name_len).unwrap_or(0)
    };
    let mut data = zeroed(total)?;
    let (received, from) = socket.recv(&mut data, flags, file.status().nonblock)?;
    let mut offset = 0;
    for (base, length) in segments {
        if offset >= received.bytes {
            break;
        }
        let take = length.min(received.bytes - offset);
        let piece = data.get(offset..offset + take).ok_or(Errno::EINVAL)?;
        uaccess::copy_to_user(process.space(), base, piece).map_err(|_| Errno::EFAULT)?;
        offset += take;
    }
    let field = |offset: usize| header.saturating_add(offset as u64);
    if message.name != 0 {
        let encoded = from.unwrap_or_default();
        write_address(
            process,
            message.name,
            field(MsgHdr::name_len_offset(NATIVE)),
            &encoded,
            capacity,
        )?;
    }
    let truncated = socket.kind() != SocketType::Stream && received.full > received.bytes;
    uaccess::put_u32(
        process.space(),
        field(MsgHdr::flags_offset(NATIVE)),
        if truncated { MSG_TRUNC } else { 0 },
    )?;
    uaccess::put_word(
        process.space(),
        field(MsgHdr::control_len_offset(NATIVE)),
        0,
    )?;
    Ok(received_count(&socket, received, flags))
}

/// `getsockopt`: the option's value, cut to the length the program offers,
/// with the length written back.
fn sys_getsockopt(
    process: &Process,
    descriptor: i32,
    level: i32,
    name: i32,
    value: u64,
    length: u64,
) -> Result<usize, Errno> {
    let (_file, socket) = socket_of(process, descriptor)?;
    let capacity = buffer_length(process, length)?;
    let bytes = socket.get_option(level, name, NATIVE)?;
    let shown = bytes.get(..capacity.min(bytes.len())).unwrap_or_default();
    uaccess::copy_to_user(process.space(), value, shown).map_err(|_| Errno::EFAULT)?;
    uaccess::put_u32(
        process.space(),
        length,
        u32::try_from(shown.len()).unwrap_or(u32::MAX),
    )?;
    Ok(0)
}

/// `setsockopt`. A negative length is `EINVAL` before the descriptor is
/// looked up, as on Linux.
fn sys_setsockopt(
    process: &Process,
    descriptor: i32,
    level: i32,
    name: i32,
    value: u64,
    length: i32,
) -> Result<usize, Errno> {
    let length = usize::try_from(length).map_err(|_| Errno::EINVAL)?;
    let (_file, socket) = socket_of(process, descriptor)?;
    let bytes = copy_in(process, value, length.min(MAX_OPTION))?;
    socket.set_option(level, name, &bytes, NATIVE)?;
    Ok(0)
}
