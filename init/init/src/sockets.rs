//! The sockets `.socket` units listen on (§6, landing L9).
//!
//! `ListenStream=` takes a port (`7777`, every IPv4 address), an address
//! and port (`127.0.0.1:7777`), or an absolute path for a Unix socket;
//! `ListenDatagram=` the same for datagrams; `ListenSequentialPacket=` a
//! path; `ListenFIFO=` a path, made a FIFO. The sockets stay blocking, as
//! systemd hands them over: a service accepts on them as if it had made
//! them. Init only waits for them to be readable, and with `Accept=yes`
//! accepts the one connection that made one readable.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::io;
use std::net::{SocketAddrV4, TcpListener, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixDatagram, UnixListener};

use ferrix_svc::event::{ListenSpec, Token, UnitId};
use ferrix_svc::kind::Listen;

/// One socket of a unit's.
#[derive(Debug)]
struct Socket {
    fd: OwnedFd,
    /// Whether it takes connections, rather than datagrams or bytes.
    connections: bool,
    /// The file it is in the file system, removed when it closes.
    path: Option<String>,
}

/// Every socket unit's sockets, and the connections taken for instances.
#[derive(Debug, Default)]
pub(crate) struct Sockets {
    units: BTreeMap<UnitId, (Vec<Socket>, bool)>,
    connections: BTreeMap<u64, OwnedFd>,
    next: u64,
}

/// A port, or an IPv4 address and a port.
fn address(text: &str) -> io::Result<SocketAddrV4> {
    if let Ok(port) = text.parse::<u16>() {
        return Ok(SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, port));
    }
    text.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{text:?} is not a port, an IPv4 address and port, or a path"),
        )
    })
}

/// A Unix socket's path, emptied of what a last boot left there.
fn fresh(path: &str) -> io::Result<&str> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(path),
    }
}

/// Make one socket.
fn open(listen: &Listen, mode: u32) -> io::Result<Socket> {
    let socket = match listen {
        Listen::Stream(text) if text.starts_with('/') => {
            let listener = UnixListener::bind(fresh(text)?)?;
            fs::set_permissions(text, fs::Permissions::from_mode(mode))?;
            Socket {
                fd: listener.into(),
                connections: true,
                path: Some(text.clone()),
            }
        }
        Listen::Stream(text) => Socket {
            fd: TcpListener::bind(address(text)?)?.into(),
            connections: true,
            path: None,
        },
        Listen::Datagram(text) if text.starts_with('/') => {
            let socket = UnixDatagram::bind(fresh(text)?)?;
            fs::set_permissions(text, fs::Permissions::from_mode(mode))?;
            Socket {
                fd: socket.into(),
                connections: false,
                path: Some(text.clone()),
            }
        }
        Listen::Datagram(text) => Socket {
            fd: UdpSocket::bind(address(text)?)?.into(),
            connections: false,
            path: None,
        },
        Listen::SequentialPacket(path) => Socket {
            fd: seqpacket(fresh(path)?, mode)?,
            connections: true,
            path: Some(path.clone()),
        },
        Listen::Fifo(path) => Socket {
            fd: fifo(path, mode)?,
            connections: false,
            path: Some(path.clone()),
        },
    };
    Ok(socket)
}

/// A listening `SOCK_SEQPACKET` Unix socket at `path`.
fn seqpacket(path: &str, mode: u32) -> io::Result<OwnedFd> {
    // SAFETY: no pointers.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `socket` returned a new descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: an all-zero `sockaddr_un` is a valid one.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_bytes();
    if bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path is too long",
        ));
    }
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    let size = libc::socklen_t::try_from(size_of::<libc::sockaddr_un>()).unwrap_or(0);
    // SAFETY: `address` is a valid `sockaddr_un` of `size` bytes.
    let bound = unsafe { libc::bind(fd.as_raw_fd(), std::ptr::from_ref(&address).cast(), size) };
    if bound < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: no pointers.
    if unsafe { libc::listen(fd.as_raw_fd(), 128) } < 0 {
        return Err(io::Error::last_os_error());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(fd)
}

/// A FIFO at `path`, opened for reading and writing so it never reads as
/// ended while nobody writes.
fn fifo(path: &str, mode: u32) -> io::Result<OwnedFd> {
    let c_path = CString::new(path).map_err(io::Error::other)?;
    // SAFETY: `c_path` is NUL-terminated.
    let made = unsafe { libc::mkfifo(c_path.as_ptr(), mode) };
    if made < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    // SAFETY: `c_path` is NUL-terminated.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `open` returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl Sockets {
    /// Make `unit`'s sockets, all or none.
    pub(crate) fn listen(&mut self, unit: UnitId, spec: &ListenSpec) -> io::Result<()> {
        let mut made = Vec::new();
        for listen in &spec.listen {
            match open(listen, spec.mode) {
                Ok(socket) => made.push(socket),
                Err(error) => {
                    for socket in made {
                        remove(socket);
                    }
                    return Err(error);
                }
            }
        }
        if let Some((old, _)) = self.units.insert(unit, (made, spec.accept)) {
            for socket in old {
                remove(socket);
            }
        }
        Ok(())
    }

    /// Close `unit`'s sockets; returns their descriptors, which the caller
    /// stops watching first.
    pub(crate) fn fds(&self, unit: UnitId) -> Vec<RawFd> {
        self.units
            .get(&unit)
            .map(|(sockets, _)| sockets.iter().map(|socket| socket.fd.as_raw_fd()).collect())
            .unwrap_or_default()
    }

    /// Close `unit`'s sockets.
    pub(crate) fn unlisten(&mut self, unit: UnitId) {
        if let Some((sockets, _)) = self.units.remove(&unit) {
            for socket in sockets {
                remove(socket);
            }
        }
    }

    /// Whether `unit` accepts connections itself (`Accept=yes`).
    pub(crate) fn accepts(&self, unit: UnitId) -> bool {
        self.units.get(&unit).is_some_and(|(_, accept)| *accept)
    }

    /// Accept one connection waiting on `unit`'s sockets, as a token.
    pub(crate) fn accept(&mut self, unit: UnitId) -> Option<Token> {
        let (sockets, _) = self.units.get(&unit)?;
        for socket in sockets.iter().filter(|socket| socket.connections) {
            let mut pollfd = libc::pollfd {
                fd: socket.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pollfd` is one valid entry; a zero timeout does not wait.
            let ready = unsafe { libc::poll(&mut pollfd, 1, 0) };
            if ready <= 0 {
                continue;
            }
            // SAFETY: null address pointers ask for no peer address.
            let fd = unsafe {
                libc::accept4(
                    socket.fd.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            if fd < 0 {
                continue;
            }
            // SAFETY: `accept4` returned a new descriptor that nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let id = self.next;
            self.next += 1;
            let _ = self.connections.insert(id, fd);
            return Some(Token(id));
        }
        None
    }

    /// The descriptor of connection `token`, while init holds it.
    pub(crate) fn connection(&self, token: Token) -> Option<RawFd> {
        self.connections.get(&token.0).map(AsRawFd::as_raw_fd)
    }

    /// Close connection `token`.
    pub(crate) fn close(&mut self, token: Token) {
        let _ = self.connections.remove(&token.0);
    }
}

/// Close a socket, and remove its file.
fn remove(socket: Socket) {
    if let Some(path) = socket.path {
        let _ = fs::remove_file(path);
    }
}
