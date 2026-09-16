//! The net core's self-check, run at boot like every other subsystem's.
//!
//! It uses the loopback and nothing else, so it passes on a machine with no
//! network device at all — which every machine is until a driver lands. What
//! it proves is that the whole path is wired: a socket call reaches the stack,
//! the stack builds a packet, the packet goes round the loopback and back up
//! through the input path, and the bytes come out of the other socket.
//!
//! Five things are required:
//!
//! * the loopback interface is up and owns `127.0.0.1` and `::1`;
//! * a UDP datagram sent to a bound port arrives with the sender's address,
//!   and one sent to a port nobody holds earns `ECONNREFUSED` from the
//!   unreachable the host sends itself;
//! * a TCP connection to a listening port is made and accepted, carries bytes
//!   in both directions, and ends as a clean close at both ends;
//! * a connection to a port nobody listens on is refused rather than left to
//!   time out;
//! * the same over IPv6, so that the second family is not a claim.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_net::socket::Family;
use ferrix_net::{Endpoint, IpAddress, Ipv4, Ipv6};

use super::socket::{InetKind, InetSocket, of};
use crate::net;

/// What the check saw.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Why it did not run, if it did not.
    pub(crate) skipped: Option<&'static str>,
    /// How many interfaces are up.
    pub(crate) interfaces: usize,
    /// Bytes carried over the loopback, both directions and both families.
    pub(crate) bytes: usize,
    /// How many calls were refused exactly as specified.
    pub(crate) refusals: usize,
    /// How many connections were made and accepted.
    pub(crate) connections: usize,
}

/// The body every check sends, chosen so a truncation shows.
const BODY: &[u8] = b"Ferrix carries this over its own loopback, both ways.";

/// Run the check.
///
/// # Errors
///
/// A string naming what did not hold, which the caller turns into a panic.
pub(crate) fn run() -> Result<Report, &'static str> {
    let mut report = Report::default();
    let core = net::core();
    report.interfaces = core.with(|stack, _| {
        stack
            .interfaces()
            .iter()
            .filter(|interface| interface.is_up())
            .count()
    });
    if report.interfaces == 0 {
        return Err("the loopback interface is not up");
    }
    let owns = core.with(|stack, _| {
        stack.interfaces().iter().any(|interface| {
            interface.owns(IpAddress::V4(Ipv4::LOOPBACK))
                && interface.owns(IpAddress::V6(Ipv6::LOOPBACK))
        })
    });
    if !owns {
        return Err("the loopback interface does not own 127.0.0.1 and ::1");
    }

    datagrams(
        &mut report,
        Family::V4,
        IpAddress::V4(Ipv4::LOOPBACK),
        7_777,
    )?;
    datagrams(
        &mut report,
        Family::V6,
        IpAddress::V6(Ipv6::LOOPBACK),
        7_778,
    )?;
    streams(
        &mut report,
        Family::V4,
        IpAddress::V4(Ipv4::LOOPBACK),
        7_779,
    )?;
    streams(
        &mut report,
        Family::V6,
        IpAddress::V6(Ipv6::LOOPBACK),
        7_780,
    )?;
    refused(
        &mut report,
        Family::V4,
        IpAddress::V4(Ipv4::LOOPBACK),
        7_781,
    )?;

    // Nothing here leaves this host, so nothing should be waiting for a
    // driver. A frame queued for one is a route pointing at an interface with
    // nobody behind it, which is a mistake worth catching here rather than as
    // silence on a wire.
    if core.queued() != 0 || core.dropped() != 0 {
        return Err("the loopback check left frames waiting for a driver");
    }
    Ok(report)
}

/// A socket of a kind, as the inode behind the open file it comes as.
fn socket(family: Family, kind: InetKind) -> Result<Arc<InetSocket>, &'static str> {
    let file = InetSocket::open(family, kind, false, (0, 0))
        .map_err(|_| "a socket could not be opened")?;
    of(&file).ok_or("a socket's open file does not hold a socket")
}

/// A datagram to a bound port arrives; one to an empty port is refused.
fn datagrams(
    report: &mut Report,
    family: Family,
    address: IpAddress,
    port: u16,
) -> Result<(), &'static str> {
    let core = net::core();
    let server = socket(family, InetKind::Datagram)?;
    server
        .bind(&encode(address, port))
        .map_err(|_| "a datagram socket could not bind the loopback")?;
    let client = socket(family, InetKind::Datagram)?;
    let sent = client
        .send(BODY, 0, false, Some(&encode(address, port)))
        .map_err(|_| "a datagram to a bound port was refused")?;
    if sent != BODY.len() {
        return Err("a datagram was sent short");
    }
    let mut out = [0_u8; 128];
    let (received, from) = server
        .recv(&mut out, 0, true)
        .map_err(|_| "a datagram that was sent did not arrive")?;
    if out.get(..received.bytes) != Some(BODY) {
        return Err("a datagram arrived as something else");
    }
    if from.is_none() {
        return Err("a datagram arrived without a sender");
    }
    report.bytes += received.bytes;

    // A port nobody holds: the host sends itself an unreachable, and the next
    // call on the socket reports it.
    let lonely = socket(family, InetKind::Datagram)?;
    lonely
        .connect(&encode(address, 9), false)
        .map_err(|_| "a datagram socket could not be connected")?;
    let before = core.with(|stack, _| stack.counters());
    if lonely.send(BODY, 0, false, None).is_err() {
        return Err("a datagram to an empty port could not even be sent");
    }
    let after = core.with(|stack, _| stack.counters());
    if after.malformed != before.malformed {
        return Err("a datagram to an empty port came back malformed");
    }
    if after.delivered != before.delivered {
        return Err("a datagram to an empty port was delivered to a socket");
    }
    if after.no_socket == before.no_socket {
        return Err("a datagram to an empty port never reached the demultiplexer");
    }
    if after.unreachable_sent == before.unreachable_sent {
        return Err("a datagram to an empty port earned no unreachable message");
    }
    if after.errors_reported == before.errors_reported {
        return Err("an unreachable message was sent and never came back up");
    }
    let answer = lonely.recv(&mut out, 0, true);
    match answer {
        Err(Errno::ECONNREFUSED) => report.refusals += 1,
        Err(Errno::EAGAIN) => {
            return Err("a datagram to an empty port answered EAGAIN: no unreachable came back");
        }
        Ok(_) => return Err("a datagram to an empty port answered with data"),
        Err(_) => return Err("a datagram to an empty port was refused with the wrong errno"),
    }
    Ok(())
}

/// A connection is made, accepted, carries bytes both ways, and closes.
fn streams(
    report: &mut Report,
    family: Family,
    address: IpAddress,
    port: u16,
) -> Result<(), &'static str> {
    let listener = socket(family, InetKind::Stream)?;
    listener
        .bind(&encode(address, port))
        .map_err(|_| "a stream socket could not bind the loopback")?;
    listener
        .listen(4)
        .map_err(|_| "a stream socket could not listen")?;

    let client = socket(family, InetKind::Stream)?;
    client
        .connect(&encode(address, port), false)
        .map_err(|_| "a connection to a listening port was refused")?;
    let (accepted, peer) = listener
        .accept(false, (0, 0))
        .map_err(|_| "a connection that was made was not accepted")?;
    if peer.is_empty() {
        return Err("an accepted connection has no peer address");
    }
    let server = of(&accepted).ok_or("an accepted connection is not a socket")?;
    report.connections += 1;

    report.bytes += carry(&client, &server, "from the connecting end")?;
    report.bytes += carry(&server, &client, "from the accepting end")?;

    // A close at one end is the end of the stream at the other.
    client
        .shutdown(ferrix_linux_abi::socket::SHUT_WR)
        .map_err(|_| "a connection could not be shut down")?;
    let mut out = [0_u8; 8];
    let (end, _) = server
        .recv(&mut out, 0, false)
        .map_err(|_| "a closed connection did not end its stream")?;
    if end.bytes != 0 {
        return Err("a closed connection gave bytes after its close");
    }
    Ok(())
}

/// Send the body one way and require it whole at the other end.
fn carry(
    from: &Arc<InetSocket>,
    to: &Arc<InetSocket>,
    which: &'static str,
) -> Result<usize, &'static str> {
    let mut written = 0;
    while written < BODY.len() {
        let rest = BODY.get(written..).unwrap_or_default();
        written += from.send(rest, 0, false, None).map_err(|_| which)?;
    }
    let mut out = Vec::new();
    let mut chunk = [0_u8; 128];
    while out.len() < BODY.len() {
        let (received, _) = to.recv(&mut chunk, 0, false).map_err(|_| which)?;
        if received.bytes == 0 {
            return Err(which);
        }
        out.extend(chunk.iter().take(received.bytes).copied());
    }
    if out.as_slice() != BODY {
        return Err(which);
    }
    Ok(out.len())
}

/// A connection to a port nobody listens on is refused.
fn refused(
    report: &mut Report,
    family: Family,
    address: IpAddress,
    port: u16,
) -> Result<(), &'static str> {
    let client = socket(family, InetKind::Stream)?;
    match client.connect(&encode(address, port), false) {
        Err(Errno::ECONNREFUSED) => {
            report.refusals += 1;
            Ok(())
        }
        _ => Err("a connection to a port nobody listens on was not refused"),
    }
}

/// A `sockaddr` for an address and a port.
fn encode(address: IpAddress, port: u16) -> Vec<u8> {
    let endpoint = Endpoint::new(address, port);
    let inet = match endpoint.address {
        IpAddress::V4(four) => ferrix_linux_abi::inet::InetAddress::V4 {
            port: endpoint.port,
            address: four.octets(),
        },
        IpAddress::V6(six) => ferrix_linux_abi::inet::InetAddress::V6 {
            port: endpoint.port,
            flow_info: 0,
            address: six.octets(),
            scope_id: 0,
        },
    };
    let mut bytes = alloc::vec![0_u8; ferrix_linux_abi::inet::SOCKADDR_STORAGE_SIZE];
    let written = inet.encode(&mut bytes).unwrap_or(0);
    bytes.truncate(written);
    bytes
}
