//! `ifaddrs.h`'s `getifaddrs` and `freeifaddrs`, and `net/if.h`'s
//! `if_nameindex` and `if_freenameindex`, over a route netlink socket.
//!
//! Ported from musl 1.2.5's `getifaddrs.c`, `if_nameindex.c` and `netlink.c`
//! (MIT). The kernel is asked to dump its links and then its addresses; each
//! message names an interface, and each address message is joined to the link
//! message of the same index, which holds the name and the flags.
//!
//! The kernel's structures — `struct nlmsghdr`, `ifinfomsg`, `ifaddrmsg`,
//! `rtgenmsg` and `rtattr` — are from the UAPI headers `linux/netlink.h`,
//! `linux/rtnetlink.h` and `linux/if_addr.h`, and their sizes are asserted.
//! The messages are read out of a byte buffer field by field, so a length the
//! kernel did not write, or one from a fuzzer, cannot lead a read out of the
//! buffer or a walk into a loop:
//!
//! * musl's `NLMSG_OK` and `RTA_OK` only check that a header fits, and trust
//!   the length in it; a length of zero would walk forever. Here a message or
//!   attribute shorter than its header, or longer than what is left, ends the
//!   walk.
//! * musl leaves `errno` as it was when the kernel answers with an error
//!   message or closes the dump early. Here `errno` is the error the kernel
//!   sent, or `EIO`.
//!
//! On a kernel without `AF_NETLINK` sockets, as Ferrix has until its
//! networking stage lands, `getifaddrs` returns -1 with `errno` from `socket`,
//! and `if_nameindex` returns null with `ENOBUFS`, as musl's does for every
//! failure.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::cancel;
use crate::errno;
use crate::growable::Growable;
#[cfg(test)]
use crate::inet;
use crate::malloc::{calloc, free, malloc};
use crate::netdb::{AF_INET, AF_INET6, is_linklocal, is_mc_linklocal};
use crate::socket::{recv, send, socket};
use crate::unistd::close;

/// `PF_NETLINK`, from `include/sys/socket.h`.
const AF_NETLINK: c_int = 16;
/// `PF_PACKET`, from `include/sys/socket.h`.
const AF_PACKET: u16 = 17;
/// `SOCK_RAW`, from `include/sys/socket.h`.
const SOCK_RAW: c_int = 3;
/// `SOCK_CLOEXEC`, from `include/sys/socket.h`.
const SOCK_CLOEXEC: c_int = 0o2_000_000;
/// `MSG_DONTWAIT`, from `include/sys/socket.h`.
const MSG_DONTWAIT: c_int = 0x0040;
/// `NETLINK_ROUTE`, from `linux/netlink.h`.
const NETLINK_ROUTE: c_int = 0;
/// `NLM_F_REQUEST`, from `linux/netlink.h`.
const NLM_F_REQUEST: u16 = 0x01;
/// `NLM_F_DUMP`, `NLM_F_ROOT | NLM_F_MATCH`, from `linux/netlink.h`.
const NLM_F_DUMP: u16 = 0x100 | 0x200;
/// `NLMSG_ERROR`, from `linux/netlink.h`.
const NLMSG_ERROR: u16 = 0x2;
/// `NLMSG_DONE`, from `linux/netlink.h`.
const NLMSG_DONE: u16 = 0x3;
/// `RTM_NEWLINK`, from `linux/rtnetlink.h`.
const RTM_NEWLINK: u16 = 16;
/// `RTM_GETLINK`, from `linux/rtnetlink.h`.
const RTM_GETLINK: u16 = 18;
/// `RTM_GETADDR`, from `linux/rtnetlink.h`.
const RTM_GETADDR: u16 = 22;
/// `IFLA_ADDRESS`, from `linux/if_link.h`.
const IFLA_ADDRESS: u16 = 1;
/// `IFLA_BROADCAST`, from `linux/if_link.h`.
const IFLA_BROADCAST: u16 = 2;
/// `IFLA_IFNAME`, from `linux/if_link.h`.
const IFLA_IFNAME: u16 = 3;
/// `IFLA_STATS`, from `linux/if_link.h`.
const IFLA_STATS: u16 = 7;
/// `IFA_ADDRESS`, from `linux/if_addr.h`.
const IFA_ADDRESS: u16 = 1;
/// `IFA_LOCAL`, from `linux/if_addr.h`.
const IFA_LOCAL: u16 = 2;
/// `IFA_LABEL`, from `linux/if_addr.h`.
const IFA_LABEL: u16 = 3;
/// `IFA_BROADCAST`, from `linux/if_addr.h`.
const IFA_BROADCAST: u16 = 4;
/// `IFNAMSIZ`, from `include/net/if.h`.
const IFNAMSIZ: usize = 16;
/// How many buckets the index hash has, as in musl.
const HASH_SIZE: usize = 64;

/// The kernel's `struct nlmsghdr`, from `linux/netlink.h`. Only its size and
/// the offsets of its fields are used; the messages are read as bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Nlmsghdr {
    nlmsg_len: u32,
    nlmsg_type: u16,
    nlmsg_flags: u16,
    nlmsg_seq: u32,
    nlmsg_pid: u32,
}

/// The kernel's `struct rtgenmsg`, from `linux/rtnetlink.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Rtgenmsg {
    rtgen_family: u8,
}

/// The kernel's `struct ifinfomsg`, from `linux/rtnetlink.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Ifinfomsg {
    ifi_family: u8,
    __ifi_pad: u8,
    ifi_type: u16,
    ifi_index: i32,
    ifi_flags: u32,
    ifi_change: u32,
}

/// The kernel's `struct ifaddrmsg`, from `linux/if_addr.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Ifaddrmsg {
    ifa_family: u8,
    ifa_prefixlen: u8,
    ifa_flags: u8,
    ifa_scope: u8,
    ifa_index: u32,
}

/// The kernel's `struct rtattr`, from `linux/rtnetlink.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Rtattr {
    rta_len: u16,
    rta_type: u16,
}

const _: () = assert!(size_of::<Nlmsghdr>() == 16);
const _: () = assert!(offset_of!(Nlmsghdr, nlmsg_type) == 4);
const _: () = assert!(offset_of!(Nlmsghdr, nlmsg_flags) == 6);
const _: () = assert!(offset_of!(Nlmsghdr, nlmsg_seq) == 8);
const _: () = assert!(offset_of!(Nlmsghdr, nlmsg_pid) == 12);
const _: () = assert!(size_of::<Rtgenmsg>() == 1);
const _: () = assert!(size_of::<Ifinfomsg>() == 16);
const _: () = assert!(offset_of!(Ifinfomsg, ifi_type) == 2);
const _: () = assert!(offset_of!(Ifinfomsg, ifi_index) == 4);
const _: () = assert!(offset_of!(Ifinfomsg, ifi_flags) == 8);
const _: () = assert!(size_of::<Ifaddrmsg>() == 8);
const _: () = assert!(offset_of!(Ifaddrmsg, ifa_prefixlen) == 1);
const _: () = assert!(offset_of!(Ifaddrmsg, ifa_index) == 4);
const _: () = assert!(size_of::<Rtattr>() == 4);
const _: () = assert!(offset_of!(Rtattr, rta_type) == 2);

/// The size of a netlink message's header.
const NLMSG_HDR: usize = size_of::<Nlmsghdr>();
/// The size of an attribute's header.
const RTA_HDR: usize = size_of::<Rtattr>();

/// C's `struct ifaddrs`, from `include/ifaddrs.h`. glibc's is the same, with
/// the same union of the broadcast and destination addresses.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ifaddrs {
    /// The next interface address, or null.
    pub ifa_next: *mut Ifaddrs,
    /// The interface's name.
    pub ifa_name: *mut c_char,
    /// The interface's flags, the kernel's `IFF_` values.
    pub ifa_flags: c_uint,
    /// The address, or null.
    pub ifa_addr: *mut c_void,
    /// The netmask, or null.
    pub ifa_netmask: *mut c_void,
    /// The broadcast or destination address, or null.
    pub ifa_ifu: *mut c_void,
    /// The interface's statistics, for a link, or null.
    pub ifa_data: *mut c_void,
}

const _: () = assert!(size_of::<Ifaddrs>() == 56);
const _: () = assert!(offset_of!(Ifaddrs, ifa_name) == 8);
const _: () = assert!(offset_of!(Ifaddrs, ifa_flags) == 16);
const _: () = assert!(offset_of!(Ifaddrs, ifa_addr) == 24);
const _: () = assert!(offset_of!(Ifaddrs, ifa_netmask) == 32);
const _: () = assert!(offset_of!(Ifaddrs, ifa_ifu) == 40);
const _: () = assert!(offset_of!(Ifaddrs, ifa_data) == 48);

/// C's `struct if_nameindex`, from `include/net/if.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IfNameindex {
    /// The interface's index, or 0 at the end of the array.
    pub if_index: c_uint,
    /// The interface's name, or null at the end.
    pub if_name: *mut c_char,
}

const _: () = assert!(size_of::<IfNameindex>() == 16);
const _: () = assert!(offset_of!(IfNameindex, if_name) == 8);

/// Room for any address a program may find here: a `struct sockaddr_in6`, or
/// musl's widened `struct sockaddr_ll`, whose hardware address is 24 bytes so
/// that an Infiniband address fits. A program reading it as `sockaddr_ll`
/// finds the eight bytes its header promises.
#[repr(C, align(4))]
#[derive(Debug, Clone, Copy)]
struct SockAny {
    bytes: [u8; 36],
}

const _: () = assert!(size_of::<SockAny>() == 36);

impl SockAny {
    /// All zeros: the family `AF_UNSPEC`.
    const fn new() -> Self {
        Self { bytes: [0; 36] }
    }

    /// Stores `value` at `offset`.
    fn put(&mut self, offset: usize, value: &[u8]) {
        for (slot, &byte) in self
            .bytes
            .iter_mut()
            .skip(offset)
            .zip(value)
            .take(value.len())
        {
            *slot = byte;
        }
    }
}

/// One interface address, with room for the addresses it points at, as musl's
/// `struct ifaddrs_storage`. The statistics follow it in the same allocation.
#[repr(C)]
#[derive(Debug)]
struct Storage {
    /// What the program sees.
    ifa: Ifaddrs,
    /// The next link with the same index hash.
    hash_next: *mut Storage,
    /// The address.
    addr: SockAny,
    /// The netmask.
    netmask: SockAny,
    /// The broadcast or destination address.
    ifu: SockAny,
    /// The interface's index.
    index: c_uint,
    /// The interface's name, with room for a NUL.
    name: [c_char; IFNAMSIZ + 1],
}

/// What the walk over the kernel's messages builds.
#[derive(Debug)]
struct Context {
    /// The first address found.
    first: *mut Ifaddrs,
    /// The last, which the next one is linked to.
    last: *mut Ifaddrs,
    /// The links found, by index.
    hash: [*mut Storage; HASH_SIZE],
}

/// A length rounded up to the kernel's four-byte alignment.
const fn align4(len: usize) -> usize {
    (len + 3) & !3
}

/// The little- or big-endian 16-bit value at `offset`, as the kernel wrote it.
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    bytes
        .get(offset..offset + 2)
        .and_then(|value| <[u8; 2]>::try_from(value).ok())
        .map_or(0, u16::from_ne_bytes)
}

/// The 32-bit value at `offset`.
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| <[u8; 4]>::try_from(value).ok())
        .map_or(0, u32::from_ne_bytes)
}

/// The byte at `offset`.
fn u8_at(bytes: &[u8], offset: usize) -> u8 {
    bytes.get(offset).copied().unwrap_or(0)
}

/// Calls `visit` with the type and data of each attribute in `payload` after
/// `start` bytes of message body.
fn attributes<'a>(payload: &'a [u8], start: usize, mut visit: impl FnMut(u16, &'a [u8])) {
    let mut offset = align4(start);
    while payload.len().saturating_sub(offset) >= RTA_HDR {
        let len = usize::from(u16_at(payload, offset));
        let kind = u16_at(payload, offset + 2);
        if len < RTA_HDR || len > payload.len() - offset {
            return;
        }
        if let Some(data) = payload.get(offset + RTA_HDR..offset + len) {
            visit(kind, data);
        }
        offset += align4(len);
    }
}

/// Asks the kernel for a dump of `kind` for the family `af`, and calls
/// `visit` with the type and body of each message. Returns 0 at the end of the
/// dump, or -1 with `errno` set.
fn enumerate_one(
    fd: c_int,
    seq: u32,
    kind: u16,
    af: u8,
    visit: &mut impl FnMut(u16, &[u8]) -> c_int,
) -> c_int {
    let mut request = [0u8; NLMSG_HDR + size_of::<Rtgenmsg>()];
    let len = request.len() as u32;
    let fields: [(usize, &[u8]); 5] = [
        (offset_of!(Nlmsghdr, nlmsg_len), &len.to_ne_bytes()),
        (offset_of!(Nlmsghdr, nlmsg_type), &kind.to_ne_bytes()),
        (
            offset_of!(Nlmsghdr, nlmsg_flags),
            &(NLM_F_DUMP | NLM_F_REQUEST).to_ne_bytes(),
        ),
        (offset_of!(Nlmsghdr, nlmsg_seq), &seq.to_ne_bytes()),
        (NLMSG_HDR + offset_of!(Rtgenmsg, rtgen_family), &[af]),
    ];
    for (offset, value) in fields {
        for (slot, &byte) in request.iter_mut().skip(offset).zip(value) {
            *slot = byte;
        }
    }
    // SAFETY: the request is a live local of its length.
    let sent = unsafe { send(fd, request.as_ptr().cast(), request.len(), 0) };
    if sent < 0 {
        return -1;
    }

    let mut buf = [0u8; 8192];
    loop {
        // SAFETY: the buffer is a live local of its length.
        let got = unsafe { recv(fd, buf.as_mut_ptr().cast(), buf.len(), MSG_DONTWAIT) };
        let Some(bytes) = usize::try_from(got)
            .ok()
            .filter(|&got| got > 0)
            .and_then(|got| buf.get(..got))
        else {
            if got == 0 {
                errno::set(errno::EIO);
            }
            return -1;
        };
        let mut offset = 0;
        while bytes.len() - offset >= NLMSG_HDR {
            let len = u32_at(bytes, offset) as usize;
            let kind = u16_at(bytes, offset + offset_of!(Nlmsghdr, nlmsg_type));
            if len < NLMSG_HDR || len > bytes.len() - offset {
                errno::set(errno::EIO);
                return -1;
            }
            if kind == NLMSG_DONE {
                return 0;
            }
            let payload = bytes
                .get(offset + NLMSG_HDR..offset + len)
                .unwrap_or_default();
            if kind == NLMSG_ERROR {
                // The message holds the negated error number.
                let error = u32_at(payload, 0).cast_signed();
                errno::set(if (-4095..0).contains(&error) {
                    -error
                } else {
                    errno::EIO
                });
                return -1;
            }
            let ret = visit(kind, payload);
            if ret != 0 {
                return ret;
            }
            offset += align4(len);
        }
    }
}

/// Dumps the kernel's links of `link_af` and addresses of `addr_af`, calling
/// `visit` with each message, as musl's `__rtnetlink_enumerate` does.
fn enumerate(link_af: u8, addr_af: u8, mut visit: impl FnMut(u16, &[u8]) -> c_int) -> c_int {
    let fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
    if fd < 0 {
        return -1;
    }
    let mut r = enumerate_one(fd, 1, RTM_GETLINK, link_af, &mut visit);
    if r == 0 {
        r = enumerate_one(fd, 2, RTM_GETADDR, addr_af, &mut visit);
    }
    let saved = crate::pwd::last_errno();
    let _ = close(fd);
    errno::set(saved);
    r
}

/// Stores the address `addr` of family `af` in `sa`, and points `slot` at it,
/// as musl's `copy_addr` does. An address too short for its family is left
/// out.
fn copy_addr(slot: &mut *mut c_void, af: c_int, sa: &mut SockAny, addr: &[u8], ifindex: c_uint) {
    let (len, offset) = match af {
        AF_INET => (4, 4),
        AF_INET6 => (16, 8),
        _ => return,
    };
    if addr.len() < len {
        return;
    }
    *sa = SockAny::new();
    sa.put(0, &(af as u16).to_ne_bytes());
    sa.put(offset, addr.get(..len).unwrap_or_default());
    if af == AF_INET6
        && let Some(bytes) = addr
            .get(..16)
            .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        && (is_linklocal(&bytes) || is_mc_linklocal(&bytes))
    {
        sa.put(24, &ifindex.to_ne_bytes());
    }
    *slot = (&raw mut *sa).cast();
}

/// Stores the netmask of `prefixlen` bits in `sa`, and points `slot` at it.
fn gen_netmask(slot: &mut *mut c_void, af: c_int, sa: &mut SockAny, prefixlen: u8) {
    let mut addr = [0u8; 16];
    let prefixlen = usize::from(prefixlen).min(8 * addr.len());
    let whole = prefixlen / 8;
    for byte in addr.iter_mut().take(whole) {
        *byte = 0xff;
    }
    if let Some(byte) = addr.get_mut(whole) {
        *byte = (0xff_u32 << (8 - prefixlen % 8)) as u8;
    }
    copy_addr(slot, af, sa, &addr, 0);
}

/// Stores the hardware address `addr` in `sa` as a `struct sockaddr_ll`, and
/// points `slot` at it, as musl's `copy_lladdr` does.
fn copy_lladdr(slot: &mut *mut c_void, sa: &mut SockAny, addr: &[u8], ifindex: i32, hatype: u16) {
    // The widened hardware address is 24 bytes.
    if addr.len() > 24 {
        return;
    }
    *sa = SockAny::new();
    sa.put(0, &AF_PACKET.to_ne_bytes());
    sa.put(4, &ifindex.to_ne_bytes());
    sa.put(8, &hatype.to_ne_bytes());
    sa.put(11, &[addr.len() as u8]);
    sa.put(12, addr);
    *slot = (&raw mut *sa).cast();
}

/// Takes one message of the dump into `ctx`, as musl's
/// `netlink_msg_to_ifaddr` does. Returns 0, or -1 if memory ran out.
fn message(ctx: &mut Context, kind: u16, payload: &[u8]) -> c_int {
    let is_link = kind == RTM_NEWLINK;
    let body = if is_link {
        size_of::<Ifinfomsg>()
    } else {
        size_of::<Ifaddrmsg>()
    };
    if payload.len() < body {
        return 0;
    }

    let mut stats_len = 0;
    let mut previous = null_mut();
    if is_link {
        attributes(payload, body, |kind, data| {
            if kind == IFLA_STATS && stats_len == 0 {
                stats_len = data.len();
            }
        });
    } else {
        let index = u32_at(payload, offset_of!(Ifaddrmsg, ifa_index));
        let mut at = ctx
            .hash
            .get(index as usize % HASH_SIZE)
            .copied()
            .unwrap_or(null_mut());
        while !at.is_null() {
            // SAFETY: the list holds links this walk allocated, which live
            // until `freeifaddrs`.
            let link = unsafe { &*at };
            if link.index == index {
                break;
            }
            at = link.hash_next;
        }
        if at.is_null() {
            return 0;
        }
        previous = at;
    }

    let Some(bytes) = size_of::<Storage>().checked_add(stats_len) else {
        return -1;
    };
    let storage = calloc(1, bytes).cast::<Storage>();
    if storage.is_null() {
        return -1;
    }
    // SAFETY: the memory is a fresh zeroed allocation of at least a
    // `Storage`, which zeros are a valid value of, and nothing else refers
    // to it.
    let ifs = unsafe { &mut *storage };

    if is_link {
        let ifi_index = u32_at(payload, offset_of!(Ifinfomsg, ifi_index)).cast_signed();
        let ifi_type = u16_at(payload, offset_of!(Ifinfomsg, ifi_type));
        ifs.index = ifi_index.cast_unsigned();
        ifs.ifa.ifa_flags = u32_at(payload, offset_of!(Ifinfomsg, ifi_flags));
        let mut name = None;
        let mut address = None;
        let mut broadcast = None;
        let mut stats = None;
        attributes(payload, body, |kind, data| match kind {
            IFLA_IFNAME if data.len() < size_of::<[c_char; IFNAMSIZ + 1]>() => name = Some(data),
            IFLA_ADDRESS => address = Some(data),
            IFLA_BROADCAST => broadcast = Some(data),
            IFLA_STATS if stats.is_none() => stats = Some(data),
            _ => {}
        });
        if let Some(name) = name {
            for (slot, &byte) in ifs.name.iter_mut().zip(name) {
                *slot = byte as c_char;
            }
            ifs.ifa.ifa_name = ifs.name.as_mut_ptr();
        }
        if let Some(address) = address {
            let (slot, sa) = (&mut ifs.ifa.ifa_addr, &mut ifs.addr);
            copy_lladdr(slot, sa, address, ifi_index, ifi_type);
        }
        if let Some(broadcast) = broadcast {
            let (slot, sa) = (&mut ifs.ifa.ifa_ifu, &mut ifs.ifu);
            copy_lladdr(slot, sa, broadcast, ifi_index, ifi_type);
        }
        if let Some(stats) = stats {
            let data = storage.wrapping_add(1).cast::<u8>();
            for (offset, &byte) in stats.iter().enumerate().take(stats_len) {
                // SAFETY: the allocation has `stats_len` bytes after the
                // storage.
                unsafe { data.wrapping_add(offset).write(byte) };
            }
            ifs.ifa.ifa_data = data.cast();
        }
        if !ifs.ifa.ifa_name.is_null() {
            let bucket = ifs.index as usize % HASH_SIZE;
            if let Some(head) = ctx.hash.get_mut(bucket) {
                ifs.hash_next = *head;
                *head = storage;
            }
        }
    } else {
        // SAFETY: the link message this address belongs to was found above,
        // and lives until `freeifaddrs`.
        let link = unsafe { &*previous };
        ifs.ifa.ifa_name = link.ifa.ifa_name;
        ifs.ifa.ifa_flags = link.ifa.ifa_flags;
        let family = c_int::from(u8_at(payload, offset_of!(Ifaddrmsg, ifa_family)));
        let prefixlen = u8_at(payload, offset_of!(Ifaddrmsg, ifa_prefixlen));
        let index = u32_at(payload, offset_of!(Ifaddrmsg, ifa_index));
        let mut address = None;
        let mut local = None;
        let mut broadcast = None;
        let mut label = None;
        attributes(payload, body, |kind, data| match kind {
            IFA_ADDRESS => address = Some(data),
            IFA_LOCAL => local = Some(data),
            IFA_BROADCAST => broadcast = Some(data),
            IFA_LABEL if data.len() < size_of::<[c_char; IFNAMSIZ + 1]>() => label = Some(data),
            _ => {}
        });
        if let Some(address) = address {
            if ifs.ifa.ifa_addr.is_null() && local.is_none() {
                let (slot, sa) = (&mut ifs.ifa.ifa_addr, &mut ifs.addr);
                copy_addr(slot, family, sa, address, index);
            } else {
                // With a local address, this one is the peer's.
                let (slot, sa) = (&mut ifs.ifa.ifa_ifu, &mut ifs.ifu);
                copy_addr(slot, family, sa, address, index);
            }
        }
        if let Some(local) = local {
            let (slot, sa) = (&mut ifs.ifa.ifa_addr, &mut ifs.addr);
            copy_addr(slot, family, sa, local, index);
        }
        if let Some(broadcast) = broadcast
            && ifs.ifa.ifa_ifu.is_null()
        {
            let (slot, sa) = (&mut ifs.ifa.ifa_ifu, &mut ifs.ifu);
            copy_addr(slot, family, sa, broadcast, index);
        }
        if let Some(label) = label {
            for (slot, &byte) in ifs.name.iter_mut().zip(label) {
                *slot = byte as c_char;
            }
            ifs.ifa.ifa_name = ifs.name.as_mut_ptr();
        }
        if !ifs.ifa.ifa_addr.is_null() {
            let (slot, sa) = (&mut ifs.ifa.ifa_netmask, &mut ifs.netmask);
            gen_netmask(slot, family, sa, prefixlen);
        }
    }

    if ifs.ifa.ifa_name.is_null() {
        // SAFETY: nothing refers to the allocation.
        unsafe { free(storage.cast()) };
        return 0;
    }
    let entry = (&raw mut ifs.ifa).cast::<Ifaddrs>();
    if ctx.first.is_null() {
        ctx.first = entry;
    }
    if !ctx.last.is_null() {
        let next = ctx
            .last
            .wrapping_byte_add(offset_of!(Ifaddrs, ifa_next))
            .cast::<*mut Ifaddrs>();
        // SAFETY: the last address is one this walk allocated.
        unsafe { next.write(entry) };
    }
    ctx.last = entry;
    0
}

/// Stores the list of the machine's interface addresses at `ifap`. Returns 0,
/// or -1 with `errno` set.
///
/// # Safety
///
/// `ifap` must be valid for a write.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getifaddrs(ifap: *mut *mut Ifaddrs) -> c_int {
    let mut ctx = Context {
        first: null_mut(),
        last: null_mut(),
        hash: [null_mut(); HASH_SIZE],
    };
    let r = enumerate(0, 0, |kind, payload| message(&mut ctx, kind, payload));
    if r == 0 {
        // SAFETY: the caller passes a writable pointer.
        unsafe { ifap.write(ctx.first) };
    } else {
        // SAFETY: the list is this call's own, and is not used again.
        unsafe { freeifaddrs(ctx.first) };
    }
    r
}

/// Frees the list of interface addresses `ifp` begins.
///
/// # Safety
///
/// `ifp` must be null or a list `getifaddrs` returned, not freed before.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn freeifaddrs(ifp: *mut Ifaddrs) {
    let mut at = ifp;
    while !at.is_null() {
        // SAFETY: the caller passes a list `getifaddrs` returned.
        let next = unsafe { (*at).ifa_next };
        // SAFETY: each entry is one allocation of its own.
        unsafe { free(at.cast()) };
        at = next;
    }
}

/// One interface's index and name, as `if_nameindex` collects them.
#[derive(Debug, Clone, Copy)]
struct NameMap {
    /// The interface's index.
    index: c_uint,
    /// How long its name is.
    namelen: usize,
    /// Its name, without a NUL.
    name: [u8; IFNAMSIZ],
}

/// Takes one message of the dump into `found`, as musl's
/// `netlink_msg_to_nameindex` does, leaving out what is already there.
fn nameindex_message(found: &mut Growable<NameMap>, kind: u16, payload: &[u8]) -> c_int {
    let (index, wanted, body) = if kind == RTM_NEWLINK {
        if payload.len() < size_of::<Ifinfomsg>() {
            return 0;
        }
        (
            u32_at(payload, offset_of!(Ifinfomsg, ifi_index)),
            IFLA_IFNAME,
            size_of::<Ifinfomsg>(),
        )
    } else {
        if payload.len() < size_of::<Ifaddrmsg>() {
            return 0;
        }
        (
            u32_at(payload, offset_of!(Ifaddrmsg, ifa_index)),
            IFA_LABEL,
            size_of::<Ifaddrmsg>(),
        )
    };

    let mut name = None;
    attributes(payload, body, |kind, data| {
        if kind == wanted && name.is_none() {
            name = Some(data);
        }
    });
    // The name's length without its NUL.
    let Some(name) = name.and_then(|data| data.get(..data.len().checked_sub(1)?)) else {
        return 0;
    };
    if name.len() > IFNAMSIZ {
        return 0;
    }
    if found
        .as_slice()
        .iter()
        .any(|map| map.index == index && map.name.get(..map.namelen) == Some(name))
    {
        return 0;
    }
    let mut map = NameMap {
        index,
        namelen: name.len(),
        name: [0; IFNAMSIZ],
    };
    for (slot, &byte) in map.name.iter_mut().zip(name) {
        *slot = byte;
    }
    if found.push(map) { 0 } else { -1 }
}

/// The array of the machine's interfaces, ending with a zero entry, or null
/// with `errno` set to `ENOBUFS`. `if_freenameindex` frees it.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(
    clippy::excessive_nesting,
    reason = "the allocation is built only after every checked size succeeds"
)]
pub extern "C" fn if_nameindex() -> *mut IfNameindex {
    let state = cancel::set_state(cancel::DISABLE);
    let mut found = Growable::new();
    let mut out = null_mut();
    if enumerate(0, AF_INET as u8, |kind, payload| {
        nameindex_message(&mut found, kind, payload)
    }) == 0
    {
        let names: usize = found.as_slice().iter().map(|map| map.namelen + 1).sum();
        let entries = found.len() + 1;
        if let Some(bytes) = entries
            .checked_mul(size_of::<IfNameindex>())
            .and_then(|bytes| bytes.checked_add(names))
        {
            let array = malloc(bytes).cast::<IfNameindex>();
            if !array.is_null() {
                let mut text = array.wrapping_add(entries).cast::<c_char>();
                for (index, map) in found.as_slice().iter().enumerate() {
                    let entry = array.wrapping_add(index);
                    // SAFETY: the array has room for every interface and the
                    // zero entry.
                    unsafe {
                        entry.write(IfNameindex {
                            if_index: map.index,
                            if_name: text,
                        });
                    }
                    for (offset, &byte) in map.name.iter().take(map.namelen).chain(&[0]).enumerate()
                    {
                        // SAFETY: the names were counted into the allocation.
                        unsafe { text.wrapping_add(offset).write(byte as c_char) };
                    }
                    text = text.wrapping_add(map.namelen + 1);
                }
                // SAFETY: the last entry is the array's own.
                unsafe {
                    array.wrapping_add(entries - 1).write(IfNameindex {
                        if_index: 0,
                        if_name: null_mut(),
                    });
                }
                out = array;
            }
        }
    }
    let _ = cancel::set_state(state);
    // As musl does, whether it worked or not.
    errno::set(errno::ENOBUFS);
    out
}

/// Frees what `if_nameindex` returned.
///
/// # Safety
///
/// `idx` must be null or an array `if_nameindex` returned, not freed before.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn if_freenameindex(idx: *mut IfNameindex) {
    // SAFETY: the caller passes an array `if_nameindex` returned.
    unsafe { free(idx.cast()) };
}

/// Keeps `inet`'s interface functions reachable from here in tests.
#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::excessive_nesting,
    reason = "tests index fixtures and inspect fields inside returned lists"
)]
mod tests {
    use super::*;

    /// The interfaces the list names, as (name, family, flags).
    fn entries(list: *mut Ifaddrs) -> Vec<(String, c_int, c_uint)> {
        let mut out = Vec::new();
        let mut at = list;
        while !at.is_null() {
            // SAFETY: the list is what `getifaddrs` returned.
            let entry = unsafe { at.read() };
            // SAFETY: the name is a NUL-terminated string.
            let name = unsafe { core::ffi::CStr::from_ptr(entry.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let family = if entry.ifa_addr.is_null() {
                -1
            } else {
                // SAFETY: an address begins with its family.
                c_int::from(unsafe { entry.ifa_addr.cast::<u16>().read() })
            };
            out.push((name, family, entry.ifa_flags));
            at = entry.ifa_next;
        }
        out
    }

    #[test]
    fn the_loopback_interface_is_listed_with_its_addresses() {
        let mut list = null_mut();
        // SAFETY: the pointer is a live local.
        let r = unsafe { getifaddrs(&raw mut list) };
        assert_eq!(r, 0);
        let found = entries(list);
        assert!(found.iter().any(|entry| entry.0 == "lo"), "{found:?}");
        // The link message of the loopback, with its hardware address.
        assert!(
            found
                .iter()
                .any(|entry| entry.0 == "lo" && entry.1 == c_int::from(AF_PACKET))
        );
        // And its IPv4 address, 127.0.0.1 with a netmask.
        let mut at = list;
        let mut seen = false;
        while !at.is_null() {
            // SAFETY: the list is what `getifaddrs` returned.
            let entry = unsafe { at.read() };
            // SAFETY: the name is a NUL-terminated string.
            let name = unsafe { core::ffi::CStr::from_ptr(entry.ifa_name) };
            if name == c"lo" && !entry.ifa_addr.is_null() {
                // SAFETY: an address begins with its family.
                let family = c_int::from(unsafe { entry.ifa_addr.cast::<u16>().read() });
                if family == AF_INET {
                    // SAFETY: an `AF_INET` address has four bytes at offset 4.
                    let addr = unsafe {
                        entry
                            .ifa_addr
                            .cast::<u8>()
                            .wrapping_add(4)
                            .cast::<[u8; 4]>()
                            .read()
                    };
                    if addr == [127, 0, 0, 1] {
                        assert!(!entry.ifa_netmask.is_null());
                        seen = true;
                    }
                }
            }
            at = entry.ifa_next;
        }
        assert!(seen, "no IPv4 address for lo");
        // SAFETY: the list came from `getifaddrs` and is not used again.
        unsafe { freeifaddrs(list) };
        // SAFETY: freeing nothing is allowed.
        unsafe { freeifaddrs(null_mut()) };
    }

    #[test]
    fn if_nameindex_names_every_interface() {
        let array = if_nameindex();
        assert!(!array.is_null());
        let mut index = 0;
        let mut found = Vec::new();
        loop {
            // SAFETY: the array ends with a zero entry.
            let entry = unsafe { array.wrapping_add(index).read() };
            if entry.if_name.is_null() {
                break;
            }
            // SAFETY: each name is a NUL-terminated string.
            let name = unsafe { core::ffi::CStr::from_ptr(entry.if_name) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: the name is NUL-terminated.
            let by_name = unsafe { inet::if_nametoindex(entry.if_name) };
            assert_eq!(by_name, entry.if_index, "{name}");
            found.push(name);
            index += 1;
        }
        assert!(found.iter().any(|name| name == "lo"), "{found:?}");
        // SAFETY: the array came from `if_nameindex` and is not used again.
        unsafe { if_freenameindex(array) };
    }

    #[test]
    fn malformed_messages_end_the_walk_rather_than_looping() {
        // An attribute of length zero would walk forever.
        let mut seen = 0;
        attributes(&[0, 0, 3, 0, 1, 2, 3, 4], 0, |_, _| seen += 1);
        assert_eq!(seen, 0);
        // One longer than the message is left out.
        let mut seen = 0;
        attributes(&[8, 0, 3, 0, 1, 2], 0, |_, _| seen += 1);
        assert_eq!(seen, 0);
        // Two well-formed attributes, the first padded to four bytes.
        let mut seen = Vec::new();
        attributes(
            &[5, 0, 3, 0, b'a', 0, 0, 0, 6, 0, 1, 0, 1, 2],
            0,
            |kind, data| {
                seen.push((kind, data.to_vec()));
            },
        );
        assert_eq!(seen, [(3, vec![b'a']), (1, vec![1, 2])]);
        assert_eq!(align4(0), 0);
        assert_eq!(align4(1), 4);
        assert_eq!(align4(4), 4);
    }

    #[test]
    fn an_address_message_without_its_link_is_left_out() {
        let mut ctx = Context {
            first: null_mut(),
            last: null_mut(),
            hash: [null_mut(); HASH_SIZE],
        };
        // An address for an interface no link message named.
        let mut payload = vec![AF_INET as u8, 8, 0, 0];
        payload.extend_from_slice(&7u32.to_ne_bytes());
        payload.extend_from_slice(&[8, 0, IFA_ADDRESS as u8, 0, 10, 0, 0, 1]);
        assert_eq!(message(&mut ctx, 20, &payload), 0);
        assert!(ctx.first.is_null());
        // A link message too short for its body.
        assert_eq!(message(&mut ctx, RTM_NEWLINK, &[0, 0, 0, 0]), 0);
        assert!(ctx.first.is_null());

        // A link, then its address, are joined by the index.
        let mut link = vec![AF_INET as u8, 0, 1, 0];
        link.extend_from_slice(&7i32.to_ne_bytes());
        link.extend_from_slice(&9u32.to_ne_bytes());
        link.extend_from_slice(&0u32.to_ne_bytes());
        link.extend_from_slice(&[7, 0, IFLA_IFNAME as u8, 0, b'x', b'y', 0, 0]);
        assert_eq!(message(&mut ctx, RTM_NEWLINK, &link), 0);
        assert_eq!(message(&mut ctx, 20, &payload), 0);
        let found = entries(ctx.first);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], ("xy".into(), -1, 9));
        assert_eq!(found[1], ("xy".into(), AF_INET, 9));
        // SAFETY: the list is this test's own.
        unsafe { freeifaddrs(ctx.first) };
    }
}
