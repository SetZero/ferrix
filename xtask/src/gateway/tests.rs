//! The gateway tested from the guest's side of the wire.
//!
//! The Ferrix guest has no network driver yet, so there is no end-to-end test
//! to write. What there is instead is the wire: these tests bind the socket
//! QEMU would bind, send the frames QEMU would send, and read what comes back,
//! so the half of the path that is ours is exercised by `cargo test` on every
//! machine, with no QEMU and no network of any kind.
//!
//! Nothing here reaches the internet. Where a test needs something on the far
//! side it binds its own listener on `127.0.0.1` and has the guest address it,
//! which the gateway relays as it would relay anything else.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, UdpSocket};
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

use ferrix_netwire::checksum::Pseudo;
use ferrix_netwire::ethernet::{self, Mac, ethertype};
use ferrix_netwire::tcp::Flags;
use ferrix_netwire::{arp, icmpv4, ipv4, tcp, udp};

use super::{DNS_IP, GATEWAY_IP, GATEWAY_MAC, GUEST_IP, Gateway};

/// The MAC `xtask` gives the guest's virtio-net device.
const GUEST_MAC: Mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// How long a test waits for an answer before calling it lost.
const PATIENCE: Duration = Duration::from_secs(5);

/// How long a test waits before concluding that nothing is coming.
const SILENCE: Duration = Duration::from_millis(250);

/// The gateway, and the socket QEMU would have bound.
struct Guest {
    /// Bound to the gateway's `qemu_socket`, connected to its `host_socket`.
    socket: UnixDatagram,
    /// The gateway under test. Dropped last, which stops its thread.
    gateway: Gateway,
}

impl Guest {
    /// Start a gateway and take QEMU's place on the other end of it.
    fn start() -> Guest {
        let gateway = Gateway::start(&std::env::temp_dir(), None).unwrap();
        let socket = UnixDatagram::bind(gateway.qemu_socket()).unwrap();
        socket.connect(gateway.host_socket()).unwrap();
        socket.set_read_timeout(Some(PATIENCE)).unwrap();
        Guest { socket, gateway }
    }

    /// Put one frame on the wire.
    fn send(&self, ethertype: u16, payload: &[u8]) {
        let header = ethernet::Header {
            destination: GATEWAY_MAC,
            source: GUEST_MAC,
            vlan: None,
            ethertype,
        };
        let mut frame = vec![0_u8; ethernet::HEADER_LEN + payload.len()];
        let at = header.emit(&mut frame).unwrap();
        frame[at..].copy_from_slice(payload);
        let _ = self.socket.send(&frame).unwrap();
    }

    /// Put one IPv4 packet on the wire.
    fn send_ipv4(&self, protocol: u8, source: Ipv4Addr, destination: Ipv4Addr, payload: &[u8]) {
        let header = ipv4::Header {
            dscp: 0,
            ecn: 0,
            identification: 1,
            dont_fragment: true,
            more_fragments: false,
            fragment_offset: 0,
            ttl: 64,
            protocol,
            source: source.octets(),
            destination: destination.octets(),
        };
        let mut packet = vec![0_u8; ipv4::MIN_HEADER_LEN + payload.len()];
        let at = header.emit(&[], payload.len(), &mut packet).unwrap();
        packet[at..].copy_from_slice(payload);
        self.send(ethertype::IPV4, &packet);
    }

    /// The next frame, or `None` if none arrives.
    fn frame(&self) -> Option<Vec<u8>> {
        let mut buffer = [0_u8; 2048];
        let len = self.socket.recv(&mut buffer).ok()?;
        Some(buffer[..len].to_vec())
    }

    /// The next frame carrying `ethertype`, skipping anything else.
    fn payload_of(&self, ethertype: u16) -> Vec<u8> {
        for _ in 0..64 {
            let Some(frame) = self.frame() else { break };
            let (header, payload) = ethernet::Header::parse(&frame).unwrap();
            assert_eq!(
                header.source, GATEWAY_MAC,
                "every frame comes from the gateway's own address"
            );
            if header.ethertype == ethertype {
                return payload.to_vec();
            }
        }
        panic!("no frame of EtherType {ethertype:#06x} arrived");
    }

    /// The next IPv4 packet of `protocol`, with its header.
    fn ipv4_of(&self, protocol: u8) -> (ipv4::Header, Vec<u8>) {
        for _ in 0..64 {
            let bytes = self.payload_of(ethertype::IPV4);
            let packet = ipv4::Header::parse(&bytes).unwrap();
            if packet.header.protocol == protocol {
                return (packet.header, packet.payload.to_vec());
            }
        }
        panic!("no IPv4 packet of protocol {protocol} arrived");
    }

    /// Assert that the gateway says nothing at all.
    fn expect_silence(&self, why: &str) {
        self.socket.set_read_timeout(Some(SILENCE)).unwrap();
        let heard = self.frame();
        self.socket.set_read_timeout(Some(PATIENCE)).unwrap();
        assert!(heard.is_none(), "{why}");
    }
}

/// An ARP request as the guest would send it.
fn arp_request(target: Ipv4Addr) -> Vec<u8> {
    let packet = arp::Packet {
        operation: arp::Operation::Request,
        sender_mac: GUEST_MAC,
        sender_ip: GUEST_IP.octets(),
        target_mac: [0; 6],
        target_ip: target.octets(),
    };
    let mut bytes = vec![0_u8; arp::PACKET_LEN];
    let _ = packet.emit(&mut bytes).unwrap();
    bytes
}

#[test]
fn answers_arp_for_the_addresses_it_owns() {
    let guest = Guest::start();
    for target in [GATEWAY_IP, DNS_IP] {
        guest.send(ethertype::ARP, &arp_request(target));
        let reply = arp::Packet::parse(&guest.payload_of(ethertype::ARP)).unwrap();
        assert_eq!(
            reply.operation,
            arp::Operation::Reply,
            "an ARP request is answered with a reply"
        );
        assert_eq!(
            Ipv4Addr::from(reply.sender_ip),
            target,
            "the reply answers for the address that was asked about"
        );
        assert_eq!(
            reply.sender_mac, GATEWAY_MAC,
            "the gateway answers from its own MAC"
        );
        assert_eq!(
            reply.target_mac, GUEST_MAC,
            "and addresses the guest that asked"
        );
    }
    assert!(
        guest.gateway.counters().report().contains("arp 2"),
        "both answers are counted: {}",
        guest.gateway.counters().report()
    );
}

#[test]
fn ignores_arp_for_an_address_it_does_not_own() {
    let guest = Guest::start();
    guest.send(ethertype::ARP, &arp_request(Ipv4Addr::new(10, 0, 2, 99)));
    guest.expect_silence("the gateway must not answer for an address it does not have");
}

#[test]
fn answers_a_ping_to_the_gateway() {
    let guest = Guest::start();
    let body = b"ferrix echoes";
    let request = icmpv4::Header::echo(icmpv4::kind::ECHO_REQUEST, 0x4321, 7);
    let mut message = vec![0_u8; icmpv4::HEADER_LEN + body.len()];
    let _ = request.emit(body, &mut message).unwrap();
    guest.send_ipv4(ipv4::protocol::ICMP, GUEST_IP, GATEWAY_IP, &message);

    let (header, payload) = guest.ipv4_of(ipv4::protocol::ICMP);
    assert_eq!(
        Ipv4Addr::from(header.source),
        GATEWAY_IP,
        "the echo reply comes from the address that was pinged"
    );
    assert_eq!(
        Ipv4Addr::from(header.destination),
        GUEST_IP,
        "and goes back to the guest"
    );
    let reply = icmpv4::Header::parse(&payload).unwrap();
    assert_eq!(
        reply.header.kind,
        icmpv4::kind::ECHO_REPLY,
        "an echo request is answered with an echo reply"
    );
    assert_eq!(
        reply.header.echo_fields(),
        Some((0x4321, 7)),
        "the identifier and sequence are the request's"
    );
    assert_eq!(reply.body, body, "and the body is echoed unchanged");
}

#[test]
fn does_not_answer_a_ping_it_is_not_addressed_by() {
    let guest = Guest::start();
    let request = icmpv4::Header::echo(icmpv4::kind::ECHO_REQUEST, 1, 1);
    let mut message = vec![0_u8; icmpv4::HEADER_LEN];
    let _ = request.emit(&[], &mut message).unwrap();
    // Forwarding this would need a raw socket, so it is dropped rather than
    // half-answered from an address that never saw it.
    guest.send_ipv4(
        ipv4::protocol::ICMP,
        GUEST_IP,
        Ipv4Addr::new(1, 1, 1, 1),
        &message,
    );
    guest.expect_silence("an echo to the outside world is dropped, not answered");
}

/// Send a UDP datagram from the guest, and return the frame the gateway makes
/// of whatever comes back.
fn udp_datagram(source: SocketAddrV4, destination: SocketAddrV4, payload: &[u8]) -> Vec<u8> {
    let pseudo = Pseudo::V4 {
        source: source.ip().octets(),
        destination: destination.ip().octets(),
    };
    let header = udp::Header {
        source_port: source.port(),
        destination_port: destination.port(),
    };
    let mut bytes = vec![0_u8; udp::HEADER_LEN + payload.len()];
    let _ = header.emit(payload, pseudo, &mut bytes).unwrap();
    bytes
}

#[test]
fn relays_udp_to_a_host_socket_and_back() {
    let guest = Guest::start();
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server.set_read_timeout(Some(PATIENCE)).unwrap();
    let listening = match server.local_addr().unwrap() {
        std::net::SocketAddr::V4(address) => address,
        other => panic!("a socket bound to 127.0.0.1 is not {other}"),
    };
    let from = SocketAddrV4::new(GUEST_IP, 4000);

    guest.send_ipv4(
        ipv4::protocol::UDP,
        GUEST_IP,
        *listening.ip(),
        &udp_datagram(from, listening, b"question"),
    );

    let mut buffer = [0_u8; 64];
    let (len, peer) = server.recv_from(&mut buffer).unwrap();
    assert_eq!(
        &buffer[..len],
        b"question",
        "the payload reaches the host unchanged"
    );
    let _ = server.send_to(b"answer", peer).unwrap();

    let (header, payload) = guest.ipv4_of(ipv4::protocol::UDP);
    assert_eq!(
        Ipv4Addr::from(header.source),
        *listening.ip(),
        "the answer appears to come from where the guest addressed"
    );
    let pseudo = Pseudo::V4 {
        source: header.source,
        destination: header.destination,
    };
    let datagram = udp::Header::parse(&payload, pseudo).unwrap();
    assert_eq!(
        datagram.header.source_port,
        listening.port(),
        "from the port it addressed, whatever port the host really used"
    );
    assert_eq!(
        datagram.header.destination_port,
        from.port(),
        "and back to the port the guest sent from"
    );
    assert_eq!(datagram.payload, b"answer", "carrying what the host sent");
}

/// A BOOTP message as a client sends it.
fn dhcp_message(kind: u8, xid: u32, requested: Option<Ipv4Addr>) -> Vec<u8> {
    let mut message = vec![0_u8; 240];
    message[0] = 1;
    message[1] = 1;
    message[2] = 6;
    message[4..8].copy_from_slice(&xid.to_be_bytes());
    message[28..34].copy_from_slice(&GUEST_MAC);
    message[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    message.extend_from_slice(&[53, 1, kind]);
    if let Some(address) = requested {
        message.extend_from_slice(&[50, 4]);
        message.extend_from_slice(&address.octets());
    }
    message.push(255);
    message
}

/// Send one DHCP message and read the reply's BOOTP bytes.
fn exchange(guest: &Guest, kind: u8, requested: Option<Ipv4Addr>) -> Vec<u8> {
    let client = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 68);
    let server = SocketAddrV4::new(Ipv4Addr::BROADCAST, 67);
    guest.send_ipv4(
        ipv4::protocol::UDP,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        &udp_datagram(client, server, &dhcp_message(kind, 0xF00D_BEEF, requested)),
    );
    let (header, payload) = guest.ipv4_of(ipv4::protocol::UDP);
    assert_eq!(
        Ipv4Addr::from(header.source),
        GATEWAY_IP,
        "a DHCP reply comes from the server's address"
    );
    let pseudo = Pseudo::V4 {
        source: header.source,
        destination: header.destination,
    };
    let datagram = udp::Header::parse(&payload, pseudo).unwrap();
    assert_eq!(
        datagram.header.destination_port, 68,
        "and goes to the client port"
    );
    datagram.payload.to_vec()
}

#[test]
fn offers_the_guest_its_address_over_dhcp() {
    let guest = Guest::start();
    let offer = exchange(&guest, 1, None);
    let options = &offer[240..];
    assert_eq!(
        super::dhcp::option_value(options, 53),
        Some([2].as_slice()),
        "a discover is answered with an offer"
    );
    assert_eq!(
        &offer[16..20],
        &GUEST_IP.octets(),
        "the offered address is the one the gateway routes for"
    );
    assert_eq!(
        super::dhcp::option_value(options, 3),
        Some(GATEWAY_IP.octets().as_slice()),
        "the router is the gateway"
    );
    assert_eq!(
        super::dhcp::option_value(options, 6),
        Some(DNS_IP.octets().as_slice()),
        "the resolver is the forwarder"
    );
    assert_eq!(
        super::dhcp::option_value(options, 1),
        Some([255, 255, 255, 0].as_slice()),
        "on a /24"
    );

    let acknowledgment = exchange(&guest, 3, Some(GUEST_IP));
    assert_eq!(
        super::dhcp::option_value(&acknowledgment[240..], 53),
        Some([5].as_slice()),
        "a request for that address is acknowledged"
    );
}

#[test]
fn refuses_a_dhcp_request_for_another_address() {
    let guest = Guest::start();
    let reply = exchange(&guest, 3, Some(Ipv4Addr::new(192, 168, 1, 50)));
    assert_eq!(
        super::dhcp::option_value(&reply[240..], 53),
        Some([6].as_slice()),
        "a lease from somewhere else is refused, so the guest asks again"
    );
}

#[test]
fn counts_what_the_guest_sent() {
    let guest = Guest::start();
    guest.send(ethertype::ARP, &arp_request(GATEWAY_IP));
    let _ = guest.payload_of(ethertype::ARP);
    assert!(
        guest.gateway.counters().frames_in() > 0,
        "the gateway counts the frames it is given"
    );
}

/// The guest's side of one TCP connection, driven by hand.
///
/// This is the client half of RFC 9293, written out rather than reused, because
/// the point of these tests is to hold the gateway to the protocol as somebody
/// else's stack would. Anything it shares with the code under test would be a
/// mistake they could agree on.
struct Stream<'a> {
    /// The wire.
    guest: &'a Guest,
    /// The port the guest connected from.
    port: u16,
    /// Where it connected to.
    peer: SocketAddrV4,
    /// The next sequence number the guest will send.
    sequence: u32,
    /// The next sequence number the guest expects from the gateway.
    expected: u32,
}

impl Stream<'_> {
    /// Open a connection: SYN, SYN-ACK, ACK.
    fn open(guest: &Guest, peer: SocketAddrV4, port: u16) -> Stream<'_> {
        let mut stream = Stream {
            guest,
            port,
            peer,
            sequence: 0x0BAD_0000,
            expected: 0,
        };
        stream.send(Flags::SYN, &[]);
        stream.sequence = stream.sequence.wrapping_add(1);

        let (header, _) = stream.segment();
        assert!(
            header.flags.contains(Flags::SYN) && header.flags.contains(Flags::ACK),
            "a SYN is answered with a SYN-ACK once the host connection is up"
        );
        assert_eq!(
            header.acknowledgment, stream.sequence,
            "the SYN-ACK acknowledges the SYN's sequence number"
        );
        assert!(
            header.options.mss.is_some(),
            "and offers a maximum segment size"
        );
        stream.expected = header.sequence.wrapping_add(1);
        stream.send(Flags::ACK, &[]);
        stream
    }

    /// Put one segment on the wire.
    fn send(&self, flags: Flags, payload: &[u8]) {
        let pseudo = Pseudo::V4 {
            source: GUEST_IP.octets(),
            destination: self.peer.ip().octets(),
        };
        let header = tcp::Header {
            source_port: self.port,
            destination_port: self.peer.port(),
            sequence: self.sequence,
            acknowledgment: self.expected,
            flags,
            window: u16::MAX,
            urgent_pointer: 0,
            options: tcp::Options::default(),
        };
        let mut bytes = vec![0_u8; tcp::MIN_HEADER_LEN + payload.len()];
        let _ = header.emit(payload, pseudo, &mut bytes).unwrap();
        self.guest
            .send_ipv4(ipv4::protocol::TCP, GUEST_IP, *self.peer.ip(), &bytes);
    }

    /// Send data, and count it.
    fn write(&mut self, payload: &[u8]) {
        self.send(Flags::ACK.union(Flags::PSH), payload);
        self.sequence = self.sequence.wrapping_add(payload.len() as u32);
    }

    /// The next segment from the gateway.
    fn segment(&self) -> (tcp::Header, Vec<u8>) {
        let (ip, bytes) = self.guest.ipv4_of(ipv4::protocol::TCP);
        let pseudo = Pseudo::V4 {
            source: ip.source,
            destination: ip.destination,
        };
        let segment = tcp::Header::parse(&bytes, pseudo).unwrap();
        assert_eq!(
            segment.header.source_port,
            self.peer.port(),
            "a segment comes from the port the guest connected to"
        );
        (segment.header, segment.payload.to_vec())
    }

    /// Take a segment's data if it is the next expected, and acknowledge it.
    ///
    /// A repeat of something already taken is dropped rather than counted
    /// twice: the retransmission timer is short and a slow test host can see a
    /// segment again before its acknowledgment lands.
    fn absorb(&mut self, header: &tcp::Header, payload: &[u8], into: &mut Vec<u8>) {
        if payload.is_empty() {
            return;
        }
        if header.sequence == self.expected {
            into.extend_from_slice(payload);
            self.expected = self.expected.wrapping_add(payload.len() as u32);
        }
        self.send(Flags::ACK, &[]);
    }

    /// Read until the gateway closes its direction, and return what it sent.
    fn read_to_fin(&mut self) -> Vec<u8> {
        let mut data = Vec::new();
        for _ in 0..8192 {
            let (header, payload) = self.segment();
            let ends_at = header.sequence.wrapping_add(payload.len() as u32);
            self.absorb(&header, &payload, &mut data);
            if header.flags.contains(Flags::FIN) && ends_at == self.expected {
                self.expected = self.expected.wrapping_add(1);
                self.send(Flags::ACK, &[]);
                return data;
            }
        }
        panic!("the gateway never closed its half of the connection");
    }

    /// Close the guest's direction, and require the gateway to acknowledge it.
    fn finish(&mut self) {
        self.send(Flags::ACK.union(Flags::FIN), &[]);
        self.sequence = self.sequence.wrapping_add(1);
        for _ in 0..64 {
            let (header, _) = self.segment();
            if header.flags.contains(Flags::ACK) && header.acknowledgment == self.sequence {
                return;
            }
        }
        panic!("the gateway never acknowledged the guest's FIN");
    }
}

/// A loopback address with nothing listening on it.
fn closed_port() -> SocketAddrV4 {
    // Binding a port and letting it go leaves a window in which somebody else
    // -- another test in this run, or anything on the machine -- can take it,
    // and then the connection the test expects to be refused succeeds. So the
    // refusal is confirmed before the address is handed back, and a port that
    // was taken in the meantime is simply passed over.
    for _ in 0..32 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = match listener.local_addr().unwrap() {
            std::net::SocketAddr::V4(address) => address,
            other => panic!("a listener bound to 127.0.0.1 is not {other}"),
        };
        drop(listener);
        let refused =
            std::net::TcpStream::connect_timeout(&address.into(), Duration::from_millis(200))
                .is_err();
        if refused {
            return address;
        }
    }
    panic!("no port on the loopback stayed closed long enough to test a refusal");
}

#[test]
fn carries_a_tcp_connection_in_both_directions_and_closes_it() {
    let guest = Guest::start();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(address) => address,
        other => panic!("a listener bound to 127.0.0.1 is not {other}"),
    };

    // The far end: read the request, answer it, and close, which is the shape
    // of every protocol this gateway is for.
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 5];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"GET /", "the guest's bytes arrive unchanged");
        stream.write_all(b"hello from the host").unwrap();
    });

    let mut stream = Stream::open(&guest, address, 40_000);
    stream.write(b"GET /");
    let answer = stream.read_to_fin();
    assert_eq!(
        answer, b"hello from the host",
        "the host's bytes reach the guest unchanged"
    );
    stream.finish();
    server.join().unwrap();

    let report = guest.gateway.counters().report();
    assert!(
        report.contains("tcp 1 opened 0 refused"),
        "one connection opened and none refused: {report}"
    );
}

#[test]
fn resets_a_connection_to_a_port_nothing_listens_on() {
    let guest = Guest::start();
    let dead = closed_port();
    let stream = Stream {
        guest: &guest,
        port: 40_001,
        peer: dead,
        sequence: 1,
        expected: 0,
    };
    stream.send(Flags::SYN, &[]);

    let (header, _) = stream.segment();
    assert!(
        header.flags.contains(Flags::RST),
        "a refused connection is a reset, not a silence the guest waits out"
    );
    assert_eq!(
        header.acknowledgment, 2,
        "the reset acknowledges the SYN it refuses"
    );
    let report = guest.gateway.counters().report();
    assert!(
        report.contains("tcp 0 opened 1 refused"),
        "and is counted as refused: {report}"
    );
}

#[test]
fn resets_a_segment_for_a_connection_that_does_not_exist() {
    let guest = Guest::start();
    let stream = Stream {
        guest: &guest,
        port: 40_002,
        peer: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9),
        sequence: 500,
        expected: 900,
    };
    // Data on a connection the gateway has never heard of, which is what a
    // guest that rebooted mid-connection sends.
    stream.send(Flags::ACK.union(Flags::PSH), b"stray");

    let (header, _) = stream.segment();
    assert!(
        header.flags.contains(Flags::RST),
        "a stray segment is reset so the guest stops retransmitting it"
    );
    assert!(
        !header.flags.contains(Flags::ACK),
        "and the reset stays in the sender's own sequence space"
    );
    assert_eq!(
        header.sequence, 900,
        "which is the acknowledgment the stray segment carried"
    );
}

#[test]
fn segments_a_response_too_large_for_one_packet() {
    let guest = Guest::start();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = match listener.local_addr().unwrap() {
        std::net::SocketAddr::V4(address) => address,
        other => panic!("a listener bound to 127.0.0.1 is not {other}"),
    };
    // Far more than one segment, and more than the gateway will hold for the
    // guest at once, so the path through its send buffer is the one a real
    // download takes rather than a single write that happens to fit.
    let body: Vec<u8> = (0..200_000_u32).map(|at| (at % 251) as u8).collect();
    let sent = body.clone();

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(&sent).unwrap();
    });

    let mut stream = Stream::open(&guest, address, 40_003);
    let received = stream.read_to_fin();
    stream.finish();
    server.join().unwrap();

    assert_eq!(
        received.len(),
        body.len(),
        "every byte the host wrote reaches the guest"
    );
    assert!(
        received == body,
        "and in the order it wrote them, with no segment lost or repeated"
    );
}
