//! ICMP echo to the world beyond the gateway, sent by the host.
//!
//! An echo request can only leave this host as ICMP from a raw socket, which
//! needs privilege, or through what the host offers an unprivileged program:
//! `IcmpSendEcho` on Windows, and on Linux an ICMP datagram socket where
//! `net.ipv4.ping_group_range` admits the user. `std` binds neither, so both
//! are declared here, and where neither is there -- a Linux whose range admits
//! nobody, as WSL's does, or another Unix -- the host's own `ping` is run
//! instead. Each request is carried out on a thread of its own so the serving
//! loop never waits for it, and a reply built from the guest's request goes
//! back when the host was answered. Nothing is sent back when it was not, which
//! is what a lost echo looks like.
//!
//! What the guest measures is the host's round trip plus the gateway's turn,
//! and, only where the host's `ping` has to be run, starting a process, which
//! on Windows was 30 ms more than the network's own; the reply's TTL is this
//! gateway's, not the far host's.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// How long one host `ping` waits for its answer. Longer than any real round
/// trip and shorter than a guest's `ping -W`, whose default is ten seconds.
const WAIT: Duration = Duration::from_secs(3);

/// How many host `ping`s may run at once. A guest flooding echo requests
/// should not become a host starting processes without bound; the ones over
/// this are dropped, as a busy router drops them.
const MAX_RUNNING: usize = 16;

/// A request the host has finished with.
#[derive(Debug)]
pub(super) struct Answered {
    /// Who asked.
    pub(super) guest: Ipv4Addr,
    /// Who was asked.
    pub(super) destination: Ipv4Addr,
    /// The request's identifier.
    pub(super) identifier: u16,
    /// The request's sequence number.
    pub(super) sequence: u16,
    /// The request's body, which the reply carries back.
    pub(super) body: Vec<u8>,
}

/// The requests in flight, and where their answers arrive.
#[derive(Debug)]
pub(super) struct Forwarder {
    /// How many host `ping`s are running.
    running: Arc<AtomicUsize>,
    /// Where a thread sends a request whose destination answered.
    sender: mpsc::Sender<Answered>,
    /// Where the serving loop collects them.
    receiver: mpsc::Receiver<Answered>,
    /// The gateway's own socket and address: an empty datagram to itself ends
    /// the serving loop's wait for a frame, so an answer goes to the guest at
    /// once rather than on the loop's next turn, which on Windows is a timer
    /// tick of 15 ms.
    waker: Option<Arc<(UdpSocket, SocketAddr)>>,
}

impl Forwarder {
    /// Nothing in flight, with the gateway's socket and address to wake it by.
    pub(super) fn new(waker: Option<(UdpSocket, SocketAddr)>) -> Forwarder {
        let (sender, receiver) = mpsc::channel();
        Forwarder {
            running: Arc::new(AtomicUsize::new(0)),
            sender,
            receiver,
            waker: waker.map(Arc::new),
        }
    }

    /// Ask the host to ping `request.destination`, and answer through
    /// [`Forwarder::answered`] if it replies. Answers whether the request was
    /// taken, or dropped because [`MAX_RUNNING`] are already out.
    pub(super) fn forward(&self, request: Answered) -> bool {
        if self.running.fetch_add(1, Ordering::Relaxed) >= MAX_RUNNING {
            let _ = self.running.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        let running = Arc::clone(&self.running);
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        let started = std::thread::Builder::new()
            .name("ferrix-net-ping".to_owned())
            .spawn(move || {
                if echo(request.destination, &request.body)
                    && sender.send(request).is_ok()
                    && let Some((socket, address)) = waker.as_deref()
                {
                    let _ = socket.send_to(&[], address);
                }
                let _ = running.fetch_sub(1, Ordering::Relaxed);
            });
        if started.is_err() {
            let _ = self.running.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// The requests whose destinations answered since the last turn.
    pub(super) fn answered(&self) -> Vec<Answered> {
        self.receiver.try_iter().collect()
    }
}

/// Whether `destination` answered an echo carrying `body`, asked the fastest
/// way this host allows.
pub(super) fn echo(destination: Ipv4Addr, body: &[u8]) -> bool {
    match native_echo(destination, body) {
        Some(answered) => answered,
        None => host_ping(destination),
    }
}

/// `IcmpSendEcho`, or `None` when no ICMP handle could be had.
#[cfg(windows)]
fn native_echo(destination: Ipv4Addr, body: &[u8]) -> Option<bool> {
    windows::echo(destination, body)
}

/// An ICMP datagram socket, or `None` when this user may not open one.
#[cfg(target_os = "linux")]
fn native_echo(destination: Ipv4Addr, body: &[u8]) -> Option<bool> {
    linux::echo(destination, body)
}

/// Nothing native: the host's `ping` it is.
#[cfg(not(any(windows, target_os = "linux")))]
fn native_echo(_destination: Ipv4Addr, _body: &[u8]) -> Option<bool> {
    None
}

/// The longest body sent natively; the guest's link carries no more.
const MAX_BODY: usize = 1472;

/// `IcmpSendEcho` from `iphlpapi`.
#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::net::Ipv4Addr;

    /// A Win32 handle.
    type Handle = *mut c_void;

    /// `IP_SUCCESS`, the status of an echo that was answered.
    const IP_SUCCESS: u32 = 0;

    /// `INVALID_HANDLE_VALUE`.
    const INVALID: usize = usize::MAX;

    // AUDIT: three entry points of `iphlpapi`, declared rather than depended
    // on, for the reason `console/relay.rs` gives for its four of `kernel32`.
    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        #[link_name = "IcmpCreateFile"]
        fn icmp_create_file() -> Handle;
        #[link_name = "IcmpCloseHandle"]
        fn icmp_close_handle(handle: Handle) -> i32;
        #[link_name = "IcmpSendEcho"]
        fn icmp_send_echo(
            handle: Handle,
            destination: u32,
            request: *const c_void,
            request_size: u16,
            options: *const c_void,
            reply: *mut c_void,
            reply_size: u32,
            timeout: u32,
        ) -> u32;
    }

    /// Send one echo and wait for its reply, for at most [`super::WAIT`].
    pub(super) fn echo(destination: Ipv4Addr, body: &[u8]) -> Option<bool> {
        // SAFETY: takes no arguments and returns a handle this function owns
        // and closes below, or `INVALID_HANDLE_VALUE`.
        let handle = unsafe { icmp_create_file() };
        if handle.is_null() || handle.addr() == INVALID {
            return None;
        }
        let body = body
            .get(..body.len().min(super::MAX_BODY))
            .unwrap_or_default();
        // `ICMP_ECHO_REPLY` is 40 bytes on a 64-bit Windows and 28 on a 32-bit
        // one; the documentation asks for it, the request's data and 8 bytes
        // more. Words, so the structure's pointers land aligned.
        let mut reply = vec![0_u64; (64 + body.len() + 8).div_ceil(8)];
        let reply_bytes = u32::try_from(reply.len() * 8).unwrap_or(u32::MAX);
        let timeout = u32::try_from(super::WAIT.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `handle` is the live ICMP handle above; `body` is readable
        // for the `u16` length passed, which is at most `MAX_BODY`; the options
        // are null, which asks for the defaults; and `reply` is writable for
        // `reply_bytes`, which is larger than the documented minimum. The
        // address is `IPAddr`: the four octets in network order, as they lie.
        let replies = unsafe {
            icmp_send_echo(
                handle,
                u32::from_ne_bytes(destination.octets()),
                body.as_ptr().cast(),
                u16::try_from(body.len()).unwrap_or(0),
                std::ptr::null(),
                reply.as_mut_ptr().cast(),
                reply_bytes,
                timeout,
            )
        };
        // SAFETY: `handle` is the handle `icmp_create_file` gave, closed once.
        let _closed = unsafe { icmp_close_handle(handle) };
        // `Status` is the second `u32` of the first reply: the high half of
        // its first word on this little-endian machine.
        let status = reply.first().map_or(u32::MAX, |word| (word >> 32) as u32);
        Some(replies > 0 && status == IP_SUCCESS)
    }
}

/// An ICMP datagram socket, which Linux lets a user in
/// `net.ipv4.ping_group_range` open.
#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{c_int, c_void};
    use std::net::Ipv4Addr;

    /// `AF_INET`.
    const AF_INET: c_int = 2;
    /// `SOCK_DGRAM`.
    const SOCK_DGRAM: c_int = 2;
    /// `IPPROTO_ICMP`.
    const IPPROTO_ICMP: c_int = 1;
    /// `SOL_SOCKET`.
    const SOL_SOCKET: c_int = 1;
    /// `SO_RCVTIMEO`, with a 64-bit `timeval`, which both 64-bit Linux
    /// architectures this runs on have.
    const SO_RCVTIMEO: c_int = 20;
    /// An echo request's type.
    const ECHO_REQUEST: u8 = 8;
    /// An echo reply's.
    const ECHO_REPLY: u8 = 0;

    /// `struct sockaddr_in`.
    #[repr(C)]
    struct SockaddrIn {
        /// `AF_INET`.
        family: u16,
        /// Unused for ICMP.
        port: u16,
        /// The address, in network order.
        address: u32,
        /// Padding to `struct sockaddr`'s size.
        zero: [u8; 8],
    }

    /// `struct timeval` on a 64-bit Linux.
    #[repr(C)]
    struct Timeval {
        /// Seconds.
        seconds: i64,
        /// Microseconds.
        microseconds: i64,
    }

    // AUDIT: five entry points of the C library `std` already links, declared
    // rather than depended on.
    unsafe extern "C" {
        fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
        fn setsockopt(
            fd: c_int,
            level: c_int,
            name: c_int,
            value: *const c_void,
            length: u32,
        ) -> c_int;
        fn sendto(
            fd: c_int,
            buffer: *const c_void,
            length: usize,
            flags: c_int,
            address: *const SockaddrIn,
            address_length: u32,
        ) -> isize;
        fn recv(fd: c_int, buffer: *mut c_void, length: usize, flags: c_int) -> isize;
        fn close(fd: c_int) -> c_int;
    }

    /// Send one echo and wait for its reply, for at most [`super::WAIT`].
    pub(super) fn echo(destination: Ipv4Addr, body: &[u8]) -> Option<bool> {
        // SAFETY: plain integers; the descriptor it returns is closed below.
        let fd = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP) };
        if fd < 0 {
            return None;
        }
        let answered = send_and_wait(fd, destination, body);
        // SAFETY: `fd` is the socket opened above, closed once.
        let _closed = unsafe { close(fd) };
        Some(answered)
    }

    /// The exchange on an open socket.
    fn send_and_wait(fd: c_int, destination: Ipv4Addr, body: &[u8]) -> bool {
        let timeout = Timeval {
            seconds: i64::try_from(super::WAIT.as_secs()).unwrap_or(3),
            microseconds: 0,
        };
        // SAFETY: `fd` is an open socket and `timeout` a live `timeval` of the
        // size passed.
        let set = unsafe {
            setsockopt(
                fd,
                SOL_SOCKET,
                SO_RCVTIMEO,
                (&raw const timeout).cast(),
                size_of::<Timeval>() as u32,
            )
        };
        if set != 0 {
            return false;
        }
        let body = body
            .get(..body.len().min(super::MAX_BODY))
            .unwrap_or_default();
        // The kernel fills in the identifier, which is the socket's port, and
        // the checksum; the sequence number is the program's.
        let mut request = vec![ECHO_REQUEST, 0, 0, 0, 0, 0, 0, 1];
        request.extend_from_slice(body);
        let address = SockaddrIn {
            family: AF_INET as u16,
            port: 0,
            address: u32::from_ne_bytes(destination.octets()),
            zero: [0; 8],
        };
        // SAFETY: `request` is readable for its length and `address` is a live
        // `sockaddr_in` of the size passed.
        let sent = unsafe {
            sendto(
                fd,
                request.as_ptr().cast(),
                request.len(),
                0,
                &raw const address,
                size_of::<SockaddrIn>() as u32,
            )
        };
        if sent < 0 {
            return false;
        }
        let mut reply = [0_u8; 1600];
        // SAFETY: `reply` is writable for its length.
        let received = unsafe { recv(fd, reply.as_mut_ptr().cast(), reply.len(), 0) };
        received > 0 && reply.first() == Some(&ECHO_REPLY)
    }
}

/// One run of the host's `ping` to `destination`: whether it was answered.
///
/// Windows' `ping` exits 0 when any reply came, including a router's
/// "destination host unreachable", so there the reply has to be one carrying a
/// TTL, which only an echo reply does and which reads `TTL=` in every display
/// language. Elsewhere the exit status is the answer.
pub(super) fn host_ping(destination: Ipv4Addr) -> bool {
    let address = destination.to_string();
    let mut command = Command::new("ping");
    if cfg!(windows) {
        let wait = WAIT.as_millis().to_string();
        let _ = command.args(["-n", "1", "-w", &wait, &address]);
    } else if cfg!(target_os = "macos") {
        let wait = WAIT.as_millis().to_string();
        let _ = command.args(["-c", "1", "-W", &wait, &address]);
    } else {
        let wait = WAIT.as_secs().to_string();
        let _ = command.args(["-c", "1", "-W", &wait, &address]);
    }
    let Ok(output) = command.stdin(Stdio::null()).stderr(Stdio::null()).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    !cfg!(windows)
        || String::from_utf8_lossy(&output.stdout)
            .to_ascii_uppercase()
            .contains("TTL=")
}
