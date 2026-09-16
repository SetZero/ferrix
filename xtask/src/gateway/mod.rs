//! A user-mode network backend for the guest: a NAT gateway on a UNIX socket.
//!
//! # Why this exists rather than `-netdev user`
//!
//! QEMU's own user-mode network is slirp, and slirp is an optional build-time
//! dependency. The QEMU on the machine this was written for was built without
//! it, so the backend simply is not there:
//!
//! ```text
//! qemu-system-x86_64: -netdev user,id=n0: network backend 'user' is not
//! compiled into this binary
//! ```
//!
//! The two alternatives both want privilege this build tool does not have and
//! should not ask for. `-netdev tap` needs `CAP_NET_ADMIN` or a setuid helper,
//! and the usual way round that — a user namespace with a `tap` inside it — is
//! refused outright on this host, where `AppArmor` blocks unprivileged user
//! namespaces. Requiring either would mean `cargo xtask run --net` works on one
//! developer's machine and asks for a password on the next.
//!
//! What is always available is a datagram socket:
//!
//! ```text
//! -netdev dgram,id=net0,local.type=unix,local.path=A,remote.type=unix,remote.path=B
//! -device virtio-net-pci,netdev=net0,mac=...
//! ```
//!
//! QEMU binds `A`, and sends every Ethernet frame the guest transmits to `B` as
//! one datagram. So this module binds `B`, and sends the guest's answers back to
//! `A`. Nothing here needs a raw socket, a tun device or a capability: outside
//! the guest network it speaks through ordinary host `UdpSocket`s and
//! `TcpStream`s, the same ones any program gets.
//!
//! # The network it presents
//!
//! Deliberately the same numbers slirp uses, so that habits and every piece of
//! QEMU documentation carry over unchanged:
//!
//! | Address       | What it is                                  |
//! |---------------|---------------------------------------------|
//! | `10.0.2.0/24` | the guest network                           |
//! | `10.0.2.2`    | this gateway, at MAC `52:55:0a:00:02:02`    |
//! | `10.0.2.3`    | the DNS forwarder                           |
//! | `10.0.2.15`   | where the guest is expected, and what DHCP offers |
//!
//! # What it does, and what it deliberately does not
//!
//! * **ARP** — answers requests for `10.0.2.2` and `10.0.2.3`, and learns the
//!   guest's MAC from anything it sends.
//! * **DHCP** — a minimal server, enough for `udhcpc` to configure `eth0`.
//! * **ICMP echo** — answered for `10.0.2.2` and `10.0.2.3` only. Echo to the
//!   outside world is *not* forwarded: sending one needs either a raw socket
//!   (privileged) or `IPPROTO_ICMP` datagram sockets, which are gated behind
//!   `net.ipv4.ping_group_range` and so are not something a build tool can rely
//!   on. `ping 10.0.2.2` works; `ping 1.1.1.1` does not.
//! * **UDP** — one host socket per guest flow, with an idle timeout. A datagram
//!   to `10.0.2.3:53` goes to the resolver named in the host's `/etc/resolv.conf`.
//! * **TCP** — terminated here and re-opened as an ordinary host `TcpStream`,
//!   with the payload relayed between the two; see [`tcp`](self::tcp).
//! * **Fragments** — refused, in both directions. The MTU is 1500 and nothing
//!   here fragments; an over-long relayed datagram is dropped and counted.
//! * **IPv6** — not offered. The guest has no stack pointed at it yet, and a
//!   half-answered IPv6 is worse than none: a guest that gets a router
//!   advertisement will prefer the address in it.

mod dhcp;
mod tcp;
mod udp;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ferrix_netwire::ethernet::{self, Mac, ethertype};
use ferrix_netwire::{arp, icmpv4, ipv4};

use crate::{Error, Result};

/// The gateway's own address, which is the guest's default route.
pub(crate) const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

/// The address the DNS forwarder answers on.
pub(crate) const DNS_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 3);

/// The address DHCP offers the guest, and the only one this gateway routes for.
pub(crate) const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);

/// The guest network's mask.
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);

/// Its broadcast address.
const BROADCAST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 255);

/// Where an address the guest used actually goes on the host.
///
/// [`GATEWAY_IP`] is this gateway, and slirp's convention -- which every piece
/// of QEMU documentation assumes -- is that it is the host as well: a guest
/// that opens `10.0.2.2:8080` reaches whatever is listening on the host's
/// `127.0.0.1:8080`. That is also what lets a test be hermetic, because the
/// server the guest fetches from can be the test itself. Every other address
/// is left alone, because this is a gateway to the real network and not a set
/// of services pretending to be one.
pub(crate) fn host_of(seen: SocketAddrV4) -> SocketAddrV4 {
    if *seen.ip() == GATEWAY_IP {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, seen.port())
    } else {
        seen
    }
}

/// The gateway's MAC address. Locally administered, and slirp's.
pub(crate) const GATEWAY_MAC: Mac = [0x52, 0x55, 0x0A, 0x00, 0x02, 0x02];

/// The link's MTU: an IP packet may be this long, header included.
pub(crate) const MTU: usize = 1500;

/// The longest frame the gateway sends or accepts.
const MAX_FRAME: usize = ethernet::HEADER_LEN + MTU;

/// The shortest frame Ethernet carries. Shorter ones are padded, as a real
/// adapter's transmit path pads them, so that a guest driver counting on it is
/// not the thing that breaks first.
const MIN_FRAME: usize = 60;

/// The time to live the gateway stamps on what it originates.
const TTL: u8 = 64;

/// How long the serving thread waits for a frame before turning to its timers.
///
/// Every host socket is polled once per turn, so this is also the worst-case
/// latency the loop adds on the way back from the host. The path is a UNIX
/// socket to this machine's own kernel; five milliseconds of it is nothing
/// beside the guest's own emulated interrupt latency.
const TURN: Duration = Duration::from_millis(5);

/// The resolver used when `/etc/resolv.conf` names none that can be read.
const FALLBACK_RESOLVER: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

/// Where the host's resolvers are listed.
const RESOLV_CONF: &str = "/etc/resolv.conf";

/// What the gateway saw, so that a failing test says something more useful
/// than that the guest is quiet.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    /// Frames the guest sent.
    frames_in: AtomicU64,
    /// Frames sent to the guest.
    frames_out: AtomicU64,
    /// ARP requests answered.
    arp: AtomicU64,
    /// ICMP echoes answered.
    icmp: AtomicU64,
    /// DHCP offers and acknowledgments sent.
    dhcp: AtomicU64,
    /// UDP flows opened towards the host.
    udp_flows: AtomicU64,
    /// UDP datagrams relayed to the host.
    udp_out: AtomicU64,
    /// UDP datagrams relayed back to the guest.
    udp_in: AtomicU64,
    /// TCP connections opened towards the host.
    tcp_opened: AtomicU64,
    /// TCP connections the host refused, and which the guest saw reset.
    tcp_refused: AtomicU64,
    /// Packets dropped because relaying them would have exceeded the MTU.
    oversize: AtomicU64,
    /// Packets of a protocol the gateway does not speak.
    unsupported: AtomicU64,
    /// Packets that did not parse: a bad checksum, a truncated header.
    malformed: AtomicU64,
}

/// Add one to a counter, discarding the previous value.
fn bump(counter: &AtomicU64) {
    let _ = counter.fetch_add(1, Ordering::Relaxed);
}

impl Counters {
    /// One line naming everything that happened, for the boot log.
    pub(crate) fn report(&self) -> String {
        let get = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        format!(
            "frames {}/{} in/out, arp {}, icmp {}, dhcp {}, udp {} flows {}/{} out/in, \
             tcp {} opened {} refused, dropped {} oversize {} unsupported {} malformed",
            get(&self.frames_in),
            get(&self.frames_out),
            get(&self.arp),
            get(&self.icmp),
            get(&self.dhcp),
            get(&self.udp_flows),
            get(&self.udp_out),
            get(&self.udp_in),
            get(&self.tcp_opened),
            get(&self.tcp_refused),
            get(&self.oversize),
            get(&self.unsupported),
            get(&self.malformed),
        )
    }

    /// How many frames the guest sent, which is the first question a failing
    /// network test asks.
    pub(crate) fn frames_in(&self) -> u64 {
        self.frames_in.load(Ordering::Relaxed)
    }
}

impl From<ferrix_netwire::Error> for Error {
    fn from(error: ferrix_netwire::Error) -> Self {
        Error::new(error.to_string())
    }
}

/// Distinguishes one run's socket directory from the next's, so that two
/// gateways in one process — `--arch all` boots three machines in turn — never
/// collide, and a directory left behind by a killed run is never reused.
static NEXT_RUN: AtomicU32 = AtomicU32::new(0);

/// A running gateway: the thread, the sockets it owns, and the counters.
///
/// Dropping it stops the thread and removes the socket files. The QEMU process
/// it serves must therefore be waited for, or killed, before the handle goes.
#[derive(Debug)]
pub(crate) struct Gateway {
    /// Set to stop the serving thread at the end of its next turn.
    stop: Arc<AtomicBool>,
    /// The thread, taken and joined by [`Gateway::drop`].
    thread: Option<JoinHandle<()>>,
    /// The directory holding both socket files, removed with them.
    directory: PathBuf,
    /// The path QEMU binds, and this gateway sends to.
    qemu_socket: PathBuf,
    /// The path this gateway binds, and QEMU sends to.
    host_socket: PathBuf,
    /// What the thread saw.
    counters: Arc<Counters>,
}

impl Gateway {
    /// Bind the sockets under `parent` and start serving.
    ///
    /// The socket files go in a directory of their own so that removing them
    /// cannot remove anything else, and so that the names are the same on every
    /// run; only the directory carries the run number.
    ///
    /// `resolver` is where `10.0.2.3:53` forwards to, or the host's own when
    /// it is `None`. A test that must answer a name the same way on every
    /// machine, with or without a network, passes its own.
    pub(crate) fn start(parent: &Path, resolver: Option<SocketAddrV4>) -> Result<Gateway> {
        let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
        let directory = parent.join(format!("ferrix-net-{}-{run}", std::process::id()));
        std::fs::create_dir_all(&directory).map_err(|error| {
            Error::new(format!(
                "could not make the gateway's socket directory {}: {error}",
                directory.display()
            ))
        })?;
        let qemu_socket = directory.join("net-qemu.sock");
        let host_socket = directory.join("net-host.sock");
        // Paths left behind by a run that was killed rather than dropped: a
        // bind refuses to replace a file that is there, whether or not anything
        // is listening on it.
        let _ = std::fs::remove_file(&host_socket);
        let _ = std::fs::remove_file(&qemu_socket);

        let core = Core::bind(
            &host_socket,
            &qemu_socket,
            resolver.unwrap_or_else(default_resolver),
        )?;
        let counters = Arc::clone(&core.counters);
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("ferrix-net-gateway".to_owned())
            .spawn(move || serve(core, &signal))
            .map_err(|error| Error::new(format!("could not start the gateway thread: {error}")))?;
        Ok(Gateway {
            stop,
            thread: Some(thread),
            directory,
            qemu_socket,
            host_socket,
            counters,
        })
    }

    /// The path QEMU is told to bind as its `local.path`.
    pub(crate) fn qemu_socket(&self) -> &Path {
        &self.qemu_socket
    }

    /// The path QEMU is told to send to as its `remote.path`.
    pub(crate) fn host_socket(&self) -> &Path {
        &self.host_socket
    }

    /// What the gateway has seen so far.
    pub(crate) fn counters(&self) -> &Counters {
        &self.counters
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.host_socket);
        // QEMU's end. QEMU unlinks nothing on exit, and a stale file would stop
        // the next run's QEMU from binding the same name.
        let _ = std::fs::remove_file(&self.qemu_socket);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

/// The serving thread: read a frame, answer it, then let every timer run.
///
/// One thread and one turn, rather than a thread per flow: every shared piece
/// of state — the flow tables, the sequence numbers, the guest's MAC — is then
/// owned by one thread and needs no lock, and the counters are the only thing
/// anyone else reads.
fn serve(mut core: Core, stop: &AtomicBool) {
    let mut frame = [0_u8; MAX_FRAME];
    while !stop.load(Ordering::Relaxed) {
        // An error is the turn's timeout, which is how the timers below get
        // to run, or a datagram too long for the buffer, which is not a frame
        // this link could ever have carried.
        if let Ok(len) = core.socket.recv(&mut frame) {
            bump(&core.counters.frames_in);
            if let Some(bytes) = frame.get(..len) {
                // A frame whose bytes the guest chose. Nothing it can send is
                // allowed to end the thread, so a parse failure is a counter
                // and the next frame is read.
                if core.on_frame(bytes).is_err() {
                    bump(&core.counters.malformed);
                }
            }
        }
        core.poll_udp();
        core.poll_tcp();
        core.expire();
    }
}

/// Everything the serving thread owns.
#[derive(Debug)]
struct Core {
    /// Bound to the host path; QEMU's frames arrive here.
    socket: std::os::unix::net::UnixDatagram,
    /// QEMU's path, where answers go.
    guest_socket: PathBuf,
    /// The guest's MAC, learned from the first thing it sends.
    guest_mac: Option<Mac>,
    /// Host sockets standing in for the guest's UDP flows.
    udp: BTreeMap<udp::Key, udp::Flow>,
    /// Host connections standing in for the guest's TCP connections.
    tcp: BTreeMap<tcp::Key, tcp::Connection>,
    /// Where a host connection's outcome arrives from the thread that made it.
    connected: (
        std::sync::mpsc::Sender<tcp::Connected>,
        std::sync::mpsc::Receiver<tcp::Connected>,
    ),
    /// The resolver `10.0.2.3:53` forwards to, with its port: a test serves
    /// its own answers from a socket the kernel gave a free port, and asking
    /// it on 53 would reach nothing.
    resolver: SocketAddrV4,
    /// The initial send sequence number the next connection takes.
    next_iss: u32,
    /// What has happened.
    counters: Arc<Counters>,
}

impl Core {
    /// Bind the host socket and prepare the tables.
    fn bind(host_socket: &Path, guest_socket: &Path, resolver: SocketAddrV4) -> Result<Core> {
        let socket = std::os::unix::net::UnixDatagram::bind(host_socket).map_err(|error| {
            Error::new(format!(
                "could not bind the gateway socket {}: {error}",
                host_socket.display()
            ))
        })?;
        socket.set_read_timeout(Some(TURN))?;
        Ok(Core {
            socket,
            guest_socket: guest_socket.to_path_buf(),
            guest_mac: None,
            udp: BTreeMap::new(),
            tcp: BTreeMap::new(),
            connected: std::sync::mpsc::channel(),
            resolver,
            // Not random, and it does not need to be: this is a NAT on a
            // private wire with one guest on it, where an off-path attacker
            // guessing a sequence number is not a threat that exists. A fixed
            // start also makes one captured run comparable with the next.
            next_iss: 0x1000_0000,
            counters: Arc::new(Counters::default()),
        })
    }

    /// Dispatch one frame from the guest.
    fn on_frame(&mut self, frame: &[u8]) -> Result<()> {
        let (header, payload) = ethernet::Header::parse(frame)?;
        if header.source != ethernet::BROADCAST {
            self.guest_mac = Some(header.source);
        }
        match header.ethertype {
            ethertype::ARP => self.on_arp(payload),
            ethertype::IPV4 => self.on_ipv4(payload),
            _ => {
                bump(&self.counters.unsupported);
                Ok(())
            }
        }
    }

    /// Answer an ARP request for an address this gateway owns.
    fn on_arp(&mut self, bytes: &[u8]) -> Result<()> {
        let packet = arp::Packet::parse(bytes)?;
        if packet.operation != arp::Operation::Request {
            return Ok(());
        }
        let target = Ipv4Addr::from(packet.target_ip);
        if target != GATEWAY_IP && target != DNS_IP {
            return Ok(());
        }
        let mut reply = [0_u8; arp::PACKET_LEN];
        let len = packet.reply(GATEWAY_MAC).emit(&mut reply)?;
        bump(&self.counters.arp);
        self.send_frame(
            packet.sender_mac,
            ethertype::ARP,
            reply.get(..len).ok_or(ferrix_netwire::Error::NoSpace)?,
        )
    }

    /// Dispatch one IPv4 packet on its protocol.
    fn on_ipv4(&mut self, bytes: &[u8]) -> Result<()> {
        let packet = ipv4::Header::parse(bytes)?;
        if packet.header.is_fragment() {
            // Reassembly would be a second implementation of what `libs/net`
            // already has, for a path where nothing this gateway originates is
            // ever fragmented and the MTU is the same on both sides.
            bump(&self.counters.unsupported);
            return Ok(());
        }
        let source = Ipv4Addr::from(packet.header.source);
        let destination = Ipv4Addr::from(packet.header.destination);
        match packet.header.protocol {
            ipv4::protocol::ICMP => self.on_icmp(source, destination, packet.payload),
            ipv4::protocol::UDP => self.on_udp(source, destination, packet.payload),
            ipv4::protocol::TCP => self.on_tcp(source, destination, packet.payload),
            _ => {
                bump(&self.counters.unsupported);
                Ok(())
            }
        }
    }

    /// Answer an echo request addressed to the gateway or the forwarder.
    ///
    /// Only to those two. Forwarding an echo would mean originating ICMP on the
    /// host, which needs a raw socket or a permitted ping group; the module
    /// documentation says so and this is where it is true.
    fn on_icmp(&mut self, source: Ipv4Addr, destination: Ipv4Addr, bytes: &[u8]) -> Result<()> {
        if destination != GATEWAY_IP && destination != DNS_IP {
            bump(&self.counters.unsupported);
            return Ok(());
        }
        let message = icmpv4::Header::parse(bytes)?;
        let Some((identifier, sequence)) = message.header.echo_fields() else {
            bump(&self.counters.unsupported);
            return Ok(());
        };
        if message.header.kind != icmpv4::kind::ECHO_REQUEST {
            return Ok(());
        }
        let mut reply = [0_u8; MTU];
        let header = icmpv4::Header::echo(icmpv4::kind::ECHO_REPLY, identifier, sequence);
        let len = header.emit(message.body, &mut reply)?;
        bump(&self.counters.icmp);
        self.send_ipv4(
            ipv4::protocol::ICMP,
            destination,
            source,
            reply.get(..len).ok_or(ferrix_netwire::Error::NoSpace)?,
        )
    }

    /// Send an IPv4 packet to the guest, from `source` to `destination`.
    ///
    /// The identification field is zero and the don't-fragment bit is set, which
    /// RFC 6864 allows precisely because nothing that must not be fragmented
    /// needs an identity to be reassembled by.
    fn send_ipv4(
        &self,
        protocol: u8,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        payload: &[u8],
    ) -> Result<()> {
        let Some(mac) = self.guest_mac else {
            // Nothing has been heard from the guest, so there is no address to
            // send to. Only reachable if a host socket answers before the guest
            // has said anything, which it cannot: the flow began with a frame.
            return Ok(());
        };
        self.send_ipv4_to(mac, protocol, source, destination, payload)
    }

    /// The same, to a named MAC: DHCP answers a guest whose address is still
    /// the one in the request's hardware field.
    fn send_ipv4_to(
        &self,
        mac: Mac,
        protocol: u8,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        payload: &[u8],
    ) -> Result<()> {
        if ipv4::MIN_HEADER_LEN + payload.len() > MTU {
            bump(&self.counters.oversize);
            return Ok(());
        }
        let header = ipv4::Header {
            dscp: 0,
            ecn: 0,
            identification: 0,
            dont_fragment: true,
            more_fragments: false,
            fragment_offset: 0,
            ttl: TTL,
            protocol,
            source: source.octets(),
            destination: destination.octets(),
        };
        let mut packet = [0_u8; MTU];
        let header_len = header.emit(&[], payload.len(), &mut packet)?;
        put(&mut packet, header_len, payload)?;
        let len = header_len + payload.len();
        self.send_frame(
            mac,
            ethertype::IPV4,
            packet.get(..len).ok_or(ferrix_netwire::Error::NoSpace)?,
        )
    }

    /// Put one frame on the wire to QEMU.
    fn send_frame(&self, destination: Mac, ethertype: u16, payload: &[u8]) -> Result<()> {
        let header = ethernet::Header {
            destination,
            source: GATEWAY_MAC,
            vlan: None,
            ethertype,
        };
        let mut frame = [0_u8; MAX_FRAME];
        let at = header.emit(&mut frame)?;
        put(&mut frame, at, payload)?;
        let len = (at + payload.len()).max(MIN_FRAME);
        let bytes = frame.get(..len).ok_or(ferrix_netwire::Error::NoSpace)?;
        // A send that fails is a guest that has gone: QEMU has not bound its
        // path yet, or has exited. Neither is this thread's to report, and
        // neither should stop it serving whatever is still open.
        if self.socket.send_to(bytes, &self.guest_socket).is_ok() {
            bump(&self.counters.frames_out);
        }
        Ok(())
    }

    /// Drop what has gone idle, and what has finished.
    fn expire(&mut self) {
        self.expire_udp();
        self.expire_tcp();
    }
}

/// Copy `field` into `out` at `at`, refusing rather than indexing past the end.
fn put(out: &mut [u8], at: usize, field: &[u8]) -> Result<()> {
    let end = at
        .checked_add(field.len())
        .ok_or(ferrix_netwire::Error::NoSpace)?;
    out.get_mut(at..end)
        .ok_or(ferrix_netwire::Error::NoSpace)?
        .copy_from_slice(field);
    Ok(())
}

/// Where `10.0.2.3:53` forwards to when nobody said: the host's own resolver,
/// on port 53.
fn default_resolver() -> SocketAddrV4 {
    SocketAddrV4::new(host_resolver(), udp::DNS_PORT)
}

/// The first IPv4 resolver `/etc/resolv.conf` names, or [`FALLBACK_RESOLVER`].
///
/// Read once, at start. A file that cannot be read, names no resolver, or names
/// only IPv6 ones is not an error: the fallback is a public resolver, and a
/// gateway that refused to start because of `resolv.conf` would be a build tool
/// that refuses to boot a kernel because the host's DNS is unusual.
fn host_resolver() -> Ipv4Addr {
    let Ok(text) = std::fs::read_to_string(RESOLV_CONF) else {
        return FALLBACK_RESOLVER;
    };
    text.lines()
        .filter_map(|line| line.split_whitespace().collect::<Vec<_>>().try_into().ok())
        .filter_map(|[keyword, address]: [&str; 2]| match keyword {
            "nameserver" => address.parse::<Ipv4Addr>().ok(),
            _ => None,
        })
        .next()
        .unwrap_or(FALLBACK_RESOLVER)
}
