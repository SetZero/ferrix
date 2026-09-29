//! Internet sockets: the protocol numbers `socket` takes, the option levels and
//! names `setsockopt` takes for IPv4, IPv6 and TCP, and the two address
//! structures `bind`, `connect` and `accept` pass.
//!
//! # Where the numbers come from
//!
//! The protocols, options and address layouts are the ones in
//! `include/uapi/linux/in.h`, `linux/in6.h` and `linux/tcp.h`, which x86-64,
//! AArch64 and ARMv7-A take unchanged. The `SOL_` levels are not in any UAPI
//! header -- they live in the kernel's own `include/linux/socket.h` -- so they
//! are the numbers musl 1.2.5 and glibc both copy from it, which agree.
//!
//! # No width to pass
//!
//! [`crate::socket`] takes a [`Width`](crate::socket::Width) because
//! `struct msghdr` carries pointers. Neither `struct sockaddr_in` nor
//! `struct sockaddr_in6` carries anything pointer-sized: they are 16 and 28
//! bytes on all three architectures, with every field at the same offset. So
//! the decoders here take bytes alone.
//!
//! # Two byte orders in one structure
//!
//! `sin_family` is host order, which under Ferrix is little-endian. The port,
//! the address and IPv6's flow information are network order, big-endian,
//! because a program fills them with `htons` and `inet_pton`. IPv6's scope
//! identifier is an interface index, host order again. [`InetAddress`] holds
//! each as a number or the address bytes, so no caller swaps anything.

use crate::socket::{AF_INET, AF_INET6, AddressError};
use crate::wire;

// ---------------------------------------------------------------------------
// Protocols
// ---------------------------------------------------------------------------
//
// `i32` because `socket`'s protocol argument is a C `int`. The same numbers
// are the one-byte protocol field of an IPv4 header and IPv6's next header.

/// The default protocol for the family and type: TCP for a stream, UDP for a
/// datagram socket.
pub const IPPROTO_IP: i32 = 0;
/// IPv6 hop-by-hop options: the same number as [`IPPROTO_IP`], in its role as
/// a next header.
pub const IPPROTO_HOPOPTS: i32 = 0;
/// The Internet Control Message Protocol.
pub const IPPROTO_ICMP: i32 = 1;
/// The Transmission Control Protocol.
pub const IPPROTO_TCP: i32 = 6;
/// The User Datagram Protocol.
pub const IPPROTO_UDP: i32 = 17;
/// IPv6, carried in IPv4.
pub const IPPROTO_IPV6: i32 = 41;
/// IPv6's routing header.
pub const IPPROTO_ROUTING: i32 = 43;
/// IPv6's fragment header.
pub const IPPROTO_FRAGMENT: i32 = 44;
/// `ICMPv6`.
pub const IPPROTO_ICMPV6: i32 = 58;
/// IPv6's "no next header".
pub const IPPROTO_NONE: i32 = 59;
/// IPv6's destination options header.
pub const IPPROTO_DSTOPTS: i32 = 60;
/// Raw IP packets: a `SOCK_RAW` socket that writes its own IP header.
pub const IPPROTO_RAW: i32 = 255;
/// One past the highest protocol number `socket` accepts: `IPPROTO_MPTCP`,
/// 262, is the last.
pub const IPPROTO_MAX: i32 = 263;

// ---------------------------------------------------------------------------
// Option levels
// ---------------------------------------------------------------------------

/// Options of IPv4 itself.
pub const SOL_IP: i32 = 0;
/// Options of TCP. The same number as [`IPPROTO_TCP`], as every protocol
/// level is.
pub const SOL_TCP: i32 = 6;
/// Options of UDP.
pub const SOL_UDP: i32 = 17;
/// Options of IPv6 itself.
pub const SOL_IPV6: i32 = 41;
/// Options of `ICMPv6`, such as its type filter.
pub const SOL_ICMPV6: i32 = 58;
/// Options of raw sockets.
pub const SOL_RAW: i32 = 255;

/// At [`SOL_RAW`] on a raw ICMP socket: a 32-bit mask with one bit per ICMP
/// type, set for a type the socket is not to receive.
pub const ICMP_FILTER: i32 = 1;

/// At [`SOL_ICMPV6`] on a raw ICMPv6 socket: `struct icmp6_filter`, eight
/// 32-bit words with one bit per ICMPv6 type, set for a type the socket is
/// not to receive. `<netinet/icmp6.h>` calls it `ICMP6_FILTER`.
pub const ICMPV6_FILTER: i32 = 1;

// ---------------------------------------------------------------------------
// IPv4 options, at SOL_IP
// ---------------------------------------------------------------------------

/// The type-of-service byte sent packets carry.
pub const IP_TOS: i32 = 1;
/// The time to live sent unicast packets carry.
pub const IP_TTL: i32 = 2;
/// A raw socket's sends include their own IP header.
pub const IP_HDRINCL: i32 = 3;
/// IP options sent with every packet.
pub const IP_OPTIONS: i32 = 4;
/// Receive an `IP_PKTINFO` control message naming the arrival interface and
/// address.
pub const IP_PKTINFO: i32 = 8;
/// Path MTU discovery: one of the `IP_PMTUDISC_` values.
pub const IP_MTU_DISCOVER: i32 = 10;
/// Queue ICMP errors for [`MSG_ERRQUEUE`](crate::socket::MSG_ERRQUEUE).
pub const IP_RECVERR: i32 = 11;
/// Receive each packet's time to live as a control message.
pub const IP_RECVTTL: i32 = 12;
/// Receive each packet's type-of-service byte as a control message.
pub const IP_RECVTOS: i32 = 13;
/// Read only: the connected route's path MTU.
pub const IP_MTU: i32 = 14;
/// Bind an address no interface has yet.
pub const IP_FREEBIND: i32 = 15;
/// The interface multicast sends leave by.
pub const IP_MULTICAST_IF: i32 = 32;
/// The time to live sent multicast packets carry.
pub const IP_MULTICAST_TTL: i32 = 33;
/// Whether multicast sends loop back to local listeners.
pub const IP_MULTICAST_LOOP: i32 = 34;
/// Join a multicast group.
pub const IP_ADD_MEMBERSHIP: i32 = 35;
/// Leave a multicast group.
pub const IP_DROP_MEMBERSHIP: i32 = 36;

/// [`IP_MTU_DISCOVER`]: never set Don't Fragment.
pub const IP_PMTUDISC_DONT: i32 = 0;
/// [`IP_MTU_DISCOVER`]: discover the path MTU when the route says to.
pub const IP_PMTUDISC_WANT: i32 = 1;
/// [`IP_MTU_DISCOVER`]: always set Don't Fragment.
pub const IP_PMTUDISC_DO: i32 = 2;
/// [`IP_MTU_DISCOVER`]: set Don't Fragment and ignore the path MTU.
pub const IP_PMTUDISC_PROBE: i32 = 3;

/// The wildcard address, 0.0.0.0, as a host-order number.
pub const INADDR_ANY: u32 = 0;
/// The limited broadcast address, 255.255.255.255.
pub const INADDR_BROADCAST: u32 = 0xffff_ffff;
/// `inet_addr`'s failure value: the same bits as [`INADDR_BROADCAST`].
pub const INADDR_NONE: u32 = 0xffff_ffff;
/// The loopback address, 127.0.0.1.
pub const INADDR_LOOPBACK: u32 = 0x7f00_0001;

// ---------------------------------------------------------------------------
// IPv6 options, at SOL_IPV6
// ---------------------------------------------------------------------------

/// Turn an IPv6 socket into an IPv4 one.
pub const IPV6_ADDRFORM: i32 = 1;
/// At [`SOL_RAW`], or at [`SOL_IPV6`] on a socket that is not ICMPv6: the
/// offset in a raw socket's messages at which the kernel writes and checks
/// the checksum, or -1 for none.
pub const IPV6_CHECKSUM: i32 = 7;
/// RFC 2292's [`IPV6_RECVHOPLIMIT`], still offered, whose control message is
/// of this type too; musl's headers give it to a program that asks for
/// `IPV6_HOPLIMIT` the old way.
pub const IPV6_2292HOPLIMIT: i32 = 8;
/// The hop limit sent unicast packets carry.
pub const IPV6_UNICAST_HOPS: i32 = 16;
/// The interface multicast sends leave by.
pub const IPV6_MULTICAST_IF: i32 = 17;
/// The hop limit sent multicast packets carry.
pub const IPV6_MULTICAST_HOPS: i32 = 18;
/// Whether multicast sends loop back to local listeners.
pub const IPV6_MULTICAST_LOOP: i32 = 19;
/// Join a multicast group. `IPV6_JOIN_GROUP` in RFC 3493.
pub const IPV6_ADD_MEMBERSHIP: i32 = 20;
/// Leave a multicast group. `IPV6_LEAVE_GROUP` in RFC 3493.
pub const IPV6_DROP_MEMBERSHIP: i32 = 21;
/// Path MTU discovery, with IPv4's `IP_PMTUDISC_` values.
pub const IPV6_MTU_DISCOVER: i32 = 23;
/// The connected route's path MTU.
pub const IPV6_MTU: i32 = 24;
/// Queue `ICMPv6` errors for the error queue.
pub const IPV6_RECVERR: i32 = 25;
/// Refuse IPv4-mapped addresses, so the socket is IPv6 only.
pub const IPV6_V6ONLY: i32 = 26;
/// Receive an [`IPV6_PKTINFO`] control message with every packet.
pub const IPV6_RECVPKTINFO: i32 = 49;
/// The control message naming the arrival interface and address; on send, the
/// source to use.
pub const IPV6_PKTINFO: i32 = 50;
/// Receive each packet's hop limit as an [`IPV6_HOPLIMIT`] control message.
pub const IPV6_RECVHOPLIMIT: i32 = 51;
/// The control message carrying a hop limit.
pub const IPV6_HOPLIMIT: i32 = 52;
/// Receive each packet's traffic class as a control message.
pub const IPV6_RECVTCLASS: i32 = 66;
/// The traffic class sent packets carry.
pub const IPV6_TCLASS: i32 = 67;

// ---------------------------------------------------------------------------
// TCP options, at SOL_TCP
// ---------------------------------------------------------------------------

/// Send small segments at once rather than waiting to coalesce them: Nagle's
/// algorithm off.
pub const TCP_NODELAY: i32 = 1;
/// The largest segment to send, below what the route allows.
pub const TCP_MAXSEG: i32 = 2;
/// Hold back partial segments until the option is cleared.
pub const TCP_CORK: i32 = 3;
/// Seconds idle before the first keep-alive probe.
pub const TCP_KEEPIDLE: i32 = 4;
/// Seconds between keep-alive probes.
pub const TCP_KEEPINTVL: i32 = 5;
/// Unanswered probes before the connection is dropped.
pub const TCP_KEEPCNT: i32 = 6;
/// SYN retransmissions before `connect` gives up.
pub const TCP_SYNCNT: i32 = 7;
/// Seconds an orphaned connection stays in `FIN-WAIT-2`.
pub const TCP_LINGER2: i32 = 8;
/// Seconds `accept` waits for data before returning a connection.
pub const TCP_DEFER_ACCEPT: i32 = 9;
/// The largest window to advertise.
pub const TCP_WINDOW_CLAMP: i32 = 10;
/// Read only: a `struct tcp_info` about the connection.
pub const TCP_INFO: i32 = 11;
/// Acknowledge at once rather than delaying.
pub const TCP_QUICKACK: i32 = 12;
/// The congestion control algorithm, by name.
pub const TCP_CONGESTION: i32 = 13;
/// Milliseconds sent data may go unacknowledged before the connection is
/// dropped.
pub const TCP_USER_TIMEOUT: i32 = 18;
/// Accept data in a SYN on a listener: TCP Fast Open's queue length.
pub const TCP_FASTOPEN: i32 = 23;

// ---------------------------------------------------------------------------
// struct sockaddr_in and struct sockaddr_in6
// ---------------------------------------------------------------------------

/// Bytes in `struct sockaddr_in`: the family, the port, the address and eight
/// bytes of zero padding to the size of `struct sockaddr`.
pub const SOCKADDR_IN_SIZE: usize = 16;

/// Bytes in `struct sockaddr_in6`.
pub const SOCKADDR_IN6_SIZE: usize = 28;

/// The shortest `struct sockaddr_in6` Linux accepts: RFC 2133's, from before
/// the scope identifier was added. Such an address has scope 0.
pub const SOCKADDR_IN6_RFC2133_SIZE: usize = 24;

/// Bytes in `struct sockaddr_storage`, the most any address may claim: longer
/// is `EINVAL` before any family looks at it.
pub const SOCKADDR_STORAGE_SIZE: usize = 128;

/// An IPv4 or IPv6 socket address, read from a `struct sockaddr_in` or
/// `struct sockaddr_in6` and the length a program gave with it.
///
/// The port and flow information are numbers, already out of network order;
/// the addresses are their bytes as they go on the wire, so `127.0.0.1` is
/// `[127, 0, 0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InetAddress {
    /// `AF_INET`.
    V4 {
        /// `sin_port`.
        port: u16,
        /// `sin_addr`.
        address: [u8; 4],
    },
    /// `AF_INET6`.
    V6 {
        /// `sin6_port`.
        port: u16,
        /// `sin6_flowinfo`: the traffic class and flow label as a program
        /// wrote them.
        flow_info: u32,
        /// `sin6_addr`.
        address: [u8; 16],
        /// `sin6_scope_id`: the interface a link-local address belongs to, or
        /// 0; 0 as well when the length stops before it.
        scope_id: u32,
    },
}

impl InetAddress {
    /// Read the address in the first `addr_len` bytes of `bytes`.
    ///
    /// The family decides the least length: all of `struct sockaddr_in`, or
    /// RFC 2133's 24 bytes of `struct sockaddr_in6`. More is allowed up to
    /// [`SOCKADDR_STORAGE_SIZE`] and ignored, and `sin_zero` is not checked,
    /// as Linux checks neither. A family that is neither is
    /// [`AddressError::WrongFamily`], which Linux answers with `EAFNOSUPPORT`
    /// rather than the `EINVAL` of the two length errors.
    pub fn parse(bytes: &[u8], addr_len: usize) -> Result<InetAddress, AddressError> {
        if addr_len > SOCKADDR_STORAGE_SIZE {
            return Err(AddressError::TooLong);
        }
        let bytes = bytes.get(..addr_len).ok_or(AddressError::TooShort)?;
        let family = wire::array(bytes, 0).ok_or(AddressError::TooShort)?;
        let short = |len: usize| {
            if addr_len < len {
                Err(AddressError::TooShort)
            } else {
                Ok(())
            }
        };
        let port = || wire::array(bytes, 2).map(u16::from_be_bytes);
        match u16::from_le_bytes(family) {
            AF_INET => {
                short(SOCKADDR_IN_SIZE)?;
                Ok(InetAddress::V4 {
                    port: port().ok_or(AddressError::TooShort)?,
                    address: wire::array(bytes, 4).ok_or(AddressError::TooShort)?,
                })
            }
            AF_INET6 => {
                short(SOCKADDR_IN6_RFC2133_SIZE)?;
                Ok(InetAddress::V6 {
                    port: port().ok_or(AddressError::TooShort)?,
                    flow_info: wire::array(bytes, 4)
                        .map(u32::from_be_bytes)
                        .ok_or(AddressError::TooShort)?,
                    address: wire::array(bytes, 8).ok_or(AddressError::TooShort)?,
                    scope_id: wire::array(bytes, 24).map_or(0, u32::from_le_bytes),
                })
            }
            _ => Err(AddressError::WrongFamily),
        }
    }

    /// The family, [`AF_INET`] or [`AF_INET6`].
    #[must_use]
    pub const fn family(&self) -> u16 {
        match self {
            InetAddress::V4 { .. } => AF_INET,
            InetAddress::V6 { .. } => AF_INET6,
        }
    }

    /// The port, whichever the family.
    #[must_use]
    pub const fn port(&self) -> u16 {
        match self {
            InetAddress::V4 { port, .. } | InetAddress::V6 { port, .. } => *port,
        }
    }

    /// The length [`InetAddress::encode`] writes: the whole structure.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        match self {
            InetAddress::V4 { .. } => SOCKADDR_IN_SIZE,
            InetAddress::V6 { .. } => SOCKADDR_IN6_SIZE,
        }
    }

    /// Write the whole structure at the start of `out`, padding zero, and
    /// answer its length, as `getsockname`, `accept` and `recvfrom` report
    /// one. `None`, with nothing written, if `out` is too short.
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        let len = self.encoded_len();
        let mut record = [0_u8; SOCKADDR_IN6_SIZE];
        wire::put(&mut record, 0, &self.family().to_le_bytes())?;
        wire::put(&mut record, 2, &self.port().to_be_bytes())?;
        match self {
            InetAddress::V4 { address, .. } => wire::put(&mut record, 4, address)?,
            InetAddress::V6 {
                flow_info,
                address,
                scope_id,
                ..
            } => {
                wire::put(&mut record, 4, &flow_info.to_be_bytes())?;
                wire::put(&mut record, 8, address)?;
                wire::put(&mut record, 24, &scope_id.to_le_bytes())?;
            }
        }
        out.get_mut(..len)?.copy_from_slice(record.get(..len)?);
        Some(len)
    }
}
