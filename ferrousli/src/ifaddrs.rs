//! `ifaddrs.h`'s `getifaddrs` and `freeifaddrs`, and `net/if.h`'s
//! `if_nameindex` and `if_freenameindex`.
//!
//! Ported from musl 1.2.5's `getifaddrs.c`, `if_nameindex.c` and `netlink.c`
//! (MIT): a route netlink socket is asked to dump the links and then the
//! addresses, and each answer becomes an entry. `if_nameindex` is here rather
//! than beside `if_nametoindex` in [`crate::inet`] because it is the same
//! walk over the same messages, as it is in musl.
//!
//! Three things differ from musl:
//!
//! * The messages are read out of a byte slice at offsets rather than through
//!   pointers, so a length that runs past the end of what the kernel wrote is
//!   refused rather than read. musl's macros check the header's size but take
//!   `nlmsg_len` and `rta_len` on trust.
//! * An address message finds its link by walking the list built so far, where
//!   musl keeps a 64-bucket hash table. A machine has tens of interfaces, not
//!   thousands, and the walk needs no second allocation that has to be freed
//!   on every path out.
//! * When the kernel has no `AF_NETLINK` — which a Ferrix kernel does not yet
//!   — `getifaddrs` falls back to `SIOCGIFCONF`, asking each interface for its
//!   flags and netmask in turn. That answer is a lesser one: IPv4 addresses
//!   only, and no hardware addresses. musl fails instead.

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use crate::growable::Growable;
use crate::inet::{AF_INET, AF_INET6};
use crate::ioctl::ioctl;
use crate::malloc::{calloc, free, malloc};
use crate::netdb::{SockaddrIn6, is_linklocal, is_mc_linklocal};
use crate::socket::{recv, send, socket};
use crate::unistd::close;
use crate::{cancel, errno};

/// `IFNAMSIZ`, from `include/net/if.h`.
const IFNAMSIZ: usize = 16;
/// The size of `struct ifreq`, from `include/net/if.h`: the name, then a
/// 24-byte union.
const IFREQ_SIZE: usize = 40;
/// `AF_UNSPEC`, from `include/sys/socket.h`.
const AF_UNSPEC: c_int = 0;
/// `AF_PACKET`, from `include/sys/socket.h`.
const AF_PACKET: c_int = 17;
/// `PF_NETLINK`, from `include/sys/socket.h`.
const PF_NETLINK: c_int = 16;
/// `SOCK_RAW | SOCK_CLOEXEC`, from `include/sys/socket.h`.
const RAW_CLOEXEC: c_int = 3 | 0o2_000_000;
/// `SOCK_DGRAM | SOCK_CLOEXEC`, from `include/sys/socket.h`.
const DGRAM_CLOEXEC: c_int = 2 | 0o2_000_000;
/// `NETLINK_ROUTE`, from `linux/netlink.h`.
const NETLINK_ROUTE: c_int = 0;
/// `MSG_DONTWAIT`, from `include/sys/socket.h`.
const MSG_DONTWAIT: c_int = 0x0040;
/// `SIOCGIFCONF`, from `include/sys/ioctl.h`.
const SIOCGIFCONF: c_int = 0x8912;
/// `SIOCGIFFLAGS`, from `include/sys/ioctl.h`.
const SIOCGIFFLAGS: c_int = 0x8913;
/// `SIOCGIFNETMASK`, from `include/sys/ioctl.h`.
const SIOCGIFNETMASK: c_int = 0x891b;

/// `NLM_F_REQUEST | NLM_F_ROOT | NLM_F_MATCH`, from `linux/netlink.h`.
const NLM_F_DUMP_REQUEST: u16 = 1 | 0x100 | 0x200;
/// `NLMSG_ERROR`, from `linux/netlink.h`.
const NLMSG_ERROR: u16 = 2;
/// `NLMSG_DONE`, from `linux/netlink.h`.
const NLMSG_DONE: u16 = 3;
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
/// `IFA_ADDRESS`, from `linux/if_addr.h`.
const IFA_ADDRESS: u16 = 1;
/// `IFA_LOCAL`, from `linux/if_addr.h`.
const IFA_LOCAL: u16 = 2;
/// `IFA_LABEL`, from `linux/if_addr.h`.
const IFA_LABEL: u16 = 3;
/// `IFA_BROADCAST`, from `linux/if_addr.h`.
const IFA_BROADCAST: u16 = 4;

/// The size of `struct nlmsghdr`, from `linux/netlink.h`.
const NLMSG_HDR: usize = 16;
/// The size of `struct rtattr`, from `linux/rtnetlink.h`.
const RTA_HDR: usize = 4;
/// The size of `struct ifinfomsg`, from `linux/rtnetlink.h`.
const IFINFOMSG: usize = 16;
/// The size of `struct ifaddrmsg`, from `linux/if_addr.h`.
const IFADDRMSG: usize = 8;
/// The buffer one netlink read fills, as musl's is.
const NETLINK_BUF: usize = 8192;

/// `len` rounded up to netlink's four-byte alignment.
const fn align4(len: usize) -> usize {
    len.wrapping_add(3) & !3
}

/// Stores `bytes` at `offset` of `buf`, as much as fits.
fn put_at(buf: &mut [u8], offset: usize, bytes: &[u8]) {
    for (slot, &byte) in buf.iter_mut().skip(offset).zip(bytes) {
        *slot = byte;
    }
}

/// The little-endian 16-bit value at `index` of `bytes`, or 0.
fn get16(bytes: &[u8], index: usize) -> u16 {
    let low = bytes.get(index).copied().unwrap_or(0);
    let high = bytes.get(index + 1).copied().unwrap_or(0);
    u16::from_le_bytes([low, high])
}

/// The little-endian 32-bit value at `index` of `bytes`, or 0.
fn get32(bytes: &[u8], index: usize) -> u32 {
    u32::from(get16(bytes, index)) | u32::from(get16(bytes, index + 2)) << 16
}

/// C's `struct sockaddr_ll`, from `include/netpacket/packet.h`, with musl's
/// longer `sll_addr`: an Infiniband hardware address does not fit in the
/// eight bytes the header declares, and a caller that reads the eight it
/// declares still reads what it expects.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SockaddrLl {
    /// `AF_PACKET`.
    sll_family: u16,
    /// The protocol, which is 0 here.
    sll_protocol: u16,
    /// The interface's index.
    sll_ifindex: c_int,
    /// The hardware type, from `ifi_type`.
    sll_hatype: u16,
    /// The packet type, which is 0 here.
    sll_pkttype: u8,
    /// How many bytes of `sll_addr` are the address.
    sll_halen: u8,
    /// The hardware address.
    sll_addr: [u8; 24],
}

const _: () = assert!(size_of::<SockaddrLl>() == 36);
const _: () = assert!(offset_of!(SockaddrLl, sll_ifindex) == 4);
const _: () = assert!(offset_of!(SockaddrLl, sll_hatype) == 8);
const _: () = assert!(offset_of!(SockaddrLl, sll_halen) == 11);
const _: () = assert!(offset_of!(SockaddrLl, sll_addr) == 12);

/// Room for any address an interface has: a `struct sockaddr_in`, a
/// `struct sockaddr_in6`, or a [`SockaddrLl`]. Four-byte aligned, which is
/// what the widest field of any of them needs.
#[repr(C, align(4))]
#[derive(Debug, Clone, Copy)]
struct Sockany([u8; size_of::<SockaddrLl>()]);

impl Sockany {
    /// All zeros: no address.
    const EMPTY: Self = Self([0; size_of::<SockaddrLl>()]);

    /// Stores `bytes` from the start, as much as fits.
    fn set(&mut self, bytes: &[u8]) {
        for (slot, &byte) in self.0.iter_mut().zip(bytes) {
            *slot = byte;
        }
    }

    /// Stores `value` at `offset`, as a little-endian 16-bit field.
    fn put16(&mut self, offset: usize, value: u16) {
        self.set_at(offset, &value.to_le_bytes());
    }

    /// Stores `bytes` at `offset`, as much as fits.
    fn set_at(&mut self, offset: usize, bytes: &[u8]) {
        for (slot, &byte) in self.0.iter_mut().skip(offset).zip(bytes) {
            *slot = byte;
        }
    }

    /// A pointer to hand a C caller as a `struct sockaddr *`.
    fn as_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// C's `struct ifaddrs`, from `include/ifaddrs.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ifaddrs {
    /// The next interface, or null.
    pub ifa_next: *mut Ifaddrs,
    /// The interface's name.
    pub ifa_name: *mut c_char,
    /// The `IFF_` flags.
    pub ifa_flags: c_uint,
    /// The address, or null.
    pub ifa_addr: *mut c_void,
    /// The netmask, or null.
    pub ifa_netmask: *mut c_void,
    /// The broadcast address, or the peer's on a point-to-point link.
    pub ifa_ifu: *mut c_void,
    /// The interface's statistics, or null.
    pub ifa_data: *mut c_void,
}

const _: () = assert!(size_of::<Ifaddrs>() == 56);
const _: () = assert!(offset_of!(Ifaddrs, ifa_flags) == 16);
const _: () = assert!(offset_of!(Ifaddrs, ifa_addr) == 24);
const _: () = assert!(offset_of!(Ifaddrs, ifa_netmask) == 32);
const _: () = assert!(offset_of!(Ifaddrs, ifa_ifu) == 40);
const _: () = assert!(offset_of!(Ifaddrs, ifa_data) == 48);

/// One entry of the list, with the storage its pointers point into. The
/// `struct ifaddrs` is first, so a pointer to one is a pointer to the entry.
#[repr(C)]
#[derive(Debug)]
struct Storage {
    /// What the caller sees.
    ifa: Ifaddrs,
    /// The interface's index, which an address message is matched against.
    index: u32,
    /// Whether this entry came from a link message.
    link: bool,
    /// The address.
    addr: Sockany,
    /// The netmask.
    netmask: Sockany,
    /// The broadcast or peer address.
    ifu: Sockany,
    /// The name, always NUL-terminated.
    name: [u8; IFNAMSIZ + 1],
}

const _: () = assert!(offset_of!(Storage, ifa) == 0);

impl Storage {
    /// An entry with nothing filled in.
    const EMPTY: Self = Self {
        ifa: Ifaddrs {
            ifa_next: null_mut(),
            ifa_name: null_mut(),
            ifa_flags: 0,
            ifa_addr: null_mut(),
            ifa_netmask: null_mut(),
            ifa_ifu: null_mut(),
            ifa_data: null_mut(),
        },
        index: 0,
        link: false,
        addr: Sockany::EMPTY,
        netmask: Sockany::EMPTY,
        ifu: Sockany::EMPTY,
        name: [0; IFNAMSIZ + 1],
    };

    /// Stores `bytes` as the name, if it fits, and points `ifa_name` at it.
    fn set_name(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.name.len() {
            return;
        }
        self.name = [0; IFNAMSIZ + 1];
        for (slot, &byte) in self.name.iter_mut().zip(bytes) {
            *slot = byte;
        }
        self.ifa_name_here();
    }

    /// Points `ifa_name` at this entry's own name buffer.
    fn ifa_name_here(&mut self) {
        self.ifa.ifa_name = self.name.as_mut_ptr().cast();
    }
}

/// Stores the address `addr` of family `af` in `sa` and returns whether it
/// did, as musl's `copy_addr` does. A link-local IPv6 address is given
/// `ifindex` as its scope.
fn copy_addr(sa: &mut Sockany, af: u8, addr: &[u8], ifindex: u32) -> bool {
    match c_int::from(af) {
        AF_INET if addr.len() >= 4 => {
            let v4 = [
                addr.first().copied().unwrap_or(0),
                addr.get(1).copied().unwrap_or(0),
                addr.get(2).copied().unwrap_or(0),
                addr.get(3).copied().unwrap_or(0),
            ];
            sa.set(&SockaddrIn6::v4(v4, 0).bytes());
            true
        }
        AF_INET6 if addr.len() >= 16 => {
            let mut v6 = [0u8; 16];
            for (slot, &byte) in v6.iter_mut().zip(addr) {
                *slot = byte;
            }
            let scope = if is_linklocal(&v6) || is_mc_linklocal(&v6) {
                ifindex
            } else {
                0
            };
            sa.set(&SockaddrIn6::v6(v6, 0, scope).bytes());
            true
        }
        _ => false,
    }
}

/// Stores the netmask of `prefixlen` bits for family `af` in `sa`, as musl's
/// `gen_netmask` builds it.
fn gen_netmask(sa: &mut Sockany, af: u8, prefixlen: u8) -> bool {
    let bits = usize::from(prefixlen).min(128);
    let mut addr = [0u8; 16];
    let whole = bits / 8;
    for slot in addr.iter_mut().take(whole) {
        *slot = 0xff;
    }
    // C's `0xff << 8` is an `int` shift that leaves nothing in the byte, so a
    // prefix that ends on a byte boundary has no partial byte.
    let partial = bits % 8;
    if let Some(slot) = addr.get_mut(whole) {
        *slot = if partial == 0 {
            0
        } else {
            0xffu8 << (8 - partial)
        };
    }
    copy_addr(sa, af, &addr, 0)
}

/// Stores a `struct sockaddr_ll` for the hardware address `addr` of interface
/// `index`, of type `hatype`, as musl's `copy_lladdr` does.
fn copy_lladdr(sa: &mut Sockany, addr: &[u8], index: c_int, hatype: u16) -> bool {
    if addr.len() > size_of::<[u8; 24]>() {
        return false;
    }
    *sa = Sockany::EMPTY;
    sa.put16(offset_of!(SockaddrLl, sll_family), AF_PACKET as u16);
    sa.set_at(offset_of!(SockaddrLl, sll_ifindex), &index.to_le_bytes());
    sa.put16(offset_of!(SockaddrLl, sll_hatype), hatype);
    sa.set_at(offset_of!(SockaddrLl, sll_halen), &[addr.len() as u8]);
    sa.set_at(offset_of!(SockaddrLl, sll_addr), addr);
    true
}

/// Calls `each` with the type and data of every attribute after `skip` bytes
/// of `payload`.
fn attributes(payload: &[u8], skip: usize, mut each: impl FnMut(u16, &[u8])) {
    let mut offset = align4(skip);
    while payload.len().saturating_sub(offset) >= RTA_HDR {
        let len = usize::from(get16(payload, offset));
        let kind = get16(payload, offset + 2);
        if len < RTA_HDR || offset + len > payload.len() {
            return;
        }
        let data = payload
            .get(offset + RTA_HDR..offset + len)
            .unwrap_or_default();
        each(kind, data);
        offset += align4(len);
    }
}

/// Dumps `kind` for family `af` on `fd` with sequence `seq`, calling `each`
/// with every message's type and payload. `Err` holds the error number.
fn enumerate(
    fd: c_int,
    seq: u32,
    kind: u16,
    af: u8,
    each: &mut impl FnMut(u16, &[u8]) -> Result<(), c_int>,
) -> Result<(), c_int> {
    // `struct nlmsghdr` then `struct rtgenmsg`, padded to four bytes.
    const REQUEST: usize = 20;
    let mut request = [0u8; REQUEST];
    put_at(&mut request, 0, &(REQUEST as u32).to_le_bytes());
    put_at(&mut request, 4, &kind.to_le_bytes());
    put_at(&mut request, 6, &NLM_F_DUMP_REQUEST.to_le_bytes());
    put_at(&mut request, 8, &seq.to_le_bytes());
    put_at(&mut request, 16, &[af]);
    // SAFETY: `request` is a live local of 20 bytes.
    if unsafe { send(fd, request.as_ptr().cast(), request.len(), 0) } < 0 {
        return Err(crate::pwd::last_errno());
    }

    let mut buf = [0u8; NETLINK_BUF];
    loop {
        // SAFETY: `buf` is a live local of `NETLINK_BUF` bytes.
        let got = unsafe { recv(fd, buf.as_mut_ptr().cast(), buf.len(), MSG_DONTWAIT) };
        let Some(got) = usize::try_from(got).ok().filter(|&got| got > 0) else {
            return Err(crate::pwd::last_errno());
        };
        let filled = buf.get(..got).unwrap_or_default();
        let mut offset = 0;
        while filled.len().saturating_sub(offset) >= NLMSG_HDR {
            let len = get32(filled, offset) as usize;
            let kind = get16(filled, offset + 4);
            if len < NLMSG_HDR || offset + len > filled.len() {
                return Err(errno::EBADMSG);
            }
            if kind == NLMSG_DONE {
                return Ok(());
            }
            if kind == NLMSG_ERROR {
                return Err(errno::EBADMSG);
            }
            let payload = filled
                .get(offset + NLMSG_HDR..offset + len)
                .unwrap_or_default();
            each(kind, payload)?;
            offset += align4(len);
        }
    }
}

/// Dumps the links of `link_af` and then the addresses of `addr_af`, as
/// musl's `__rtnetlink_enumerate` does.
fn rtnetlink_enumerate(
    link_af: u8,
    addr_af: u8,
    each: &mut impl FnMut(u16, &[u8]) -> Result<(), c_int>,
) -> Result<(), c_int> {
    let fd = socket(PF_NETLINK, RAW_CLOEXEC, NETLINK_ROUTE);
    if fd < 0 {
        return Err(crate::pwd::last_errno());
    }
    let mut result = enumerate(fd, 1, RTM_GETLINK, link_af, each);
    if result.is_ok() {
        result = enumerate(fd, 2, RTM_GETADDR, addr_af, each);
    }
    let _ = close(fd);
    result
}

/// The list being built.
#[derive(Debug)]
struct Ctx {
    /// The first entry, or null.
    first: *mut Storage,
    /// The last, or null.
    last: *mut Storage,
}

impl Ctx {
    /// Nothing yet.
    const EMPTY: Self = Self {
        first: null_mut(),
        last: null_mut(),
    };

    /// Adds `entry` to the end of the list.
    fn push(&mut self, entry: *mut Storage) {
        if self.first.is_null() {
            self.first = entry;
        }
        if !self.last.is_null() {
            // SAFETY: the last entry is one of this list's allocations.
            let next = unsafe { &raw mut (*self.last).ifa.ifa_next };
            // SAFETY: the field is inside that entry.
            unsafe { next.write(entry.cast()) };
        }
        self.last = entry;
    }

    /// The link entry for interface `index`, or null. musl keeps a hash table
    /// for this; a machine has few enough interfaces to walk.
    fn link(&self, index: u32) -> *mut Storage {
        let mut entry = self.first;
        while !entry.is_null() {
            // SAFETY: the entry is one of this list's allocations.
            let found = unsafe { &*entry };
            if found.link && found.index == index {
                return entry;
            }
            entry = found.ifa.ifa_next.cast();
        }
        null_mut()
    }
}

/// A fresh, zeroed entry, or null when there is no memory.
fn entry() -> *mut Storage {
    let storage = calloc(1, size_of::<Storage>()).cast::<Storage>();
    if !storage.is_null() {
        // SAFETY: `calloc` gave `size_of::<Storage>()` writable bytes.
        unsafe { storage.write(Storage::EMPTY) };
    }
    storage
}

/// Takes a link message, as musl's `netlink_msg_to_ifaddr` does for
/// `RTM_NEWLINK`.
fn take_link(ctx: &mut Ctx, payload: &[u8]) -> Result<(), c_int> {
    let index = get32(payload, 4) as c_int;
    let hatype = get16(payload, 2);
    let flags = get32(payload, 8);
    let storage = entry();
    if storage.is_null() {
        return Err(errno::ENOMEM);
    }
    // SAFETY: the entry is this call's own allocation.
    let found = unsafe { &mut *storage };
    found.link = true;
    found.index = index.cast_unsigned();
    found.ifa.ifa_flags = flags;
    attributes(payload, IFINFOMSG, |kind, data| match kind {
        IFLA_IFNAME => found.set_name(data.strip_suffix(b"\0").unwrap_or(data)),
        IFLA_ADDRESS => {
            if copy_lladdr(&mut found.addr, data, index, hatype) {
                found.ifa.ifa_addr = found.addr.as_ptr();
            }
        }
        IFLA_BROADCAST if copy_lladdr(&mut found.ifu, data, index, hatype) => {
            found.ifa.ifa_ifu = found.ifu.as_ptr();
        }
        _ => {}
    });
    if found.ifa.ifa_name.is_null() {
        // SAFETY: nothing refers to the entry, which came from `calloc`.
        unsafe { free(storage.cast()) };
        return Ok(());
    }
    ctx.push(storage);
    Ok(())
}

/// Takes an address message, as musl's `netlink_msg_to_ifaddr` does for
/// `RTM_NEWADDR`.
fn take_address(ctx: &mut Ctx, payload: &[u8]) -> Result<(), c_int> {
    let af = payload.first().copied().unwrap_or(0);
    let prefixlen = payload.get(1).copied().unwrap_or(0);
    let index = get32(payload, 4);
    let link = ctx.link(index);
    if link.is_null() {
        return Ok(());
    }
    let storage = entry();
    if storage.is_null() {
        return Err(errno::ENOMEM);
    }
    // SAFETY: the entry is this call's own allocation.
    let found = unsafe { &mut *storage };
    // SAFETY: the link entry is one of this list's allocations.
    let named = unsafe { &*link };
    found.index = index;
    found.name = named.name;
    found.ifa.ifa_flags = named.ifa.ifa_flags;
    found.ifa_name_here();
    attributes(payload, IFADDRMSG, |kind, data| match kind {
        // An `IFA_ADDRESS` after an `IFA_LOCAL` is the peer's address.
        IFA_ADDRESS => {
            let (slot, field) = if found.ifa.ifa_addr.is_null() {
                (&mut found.addr, &mut found.ifa.ifa_addr)
            } else {
                (&mut found.ifu, &mut found.ifa.ifa_ifu)
            };
            if copy_addr(slot, af, data, index) {
                *field = slot.as_ptr();
            }
        }
        IFA_BROADCAST => {
            if copy_addr(&mut found.ifu, af, data, index) {
                found.ifa.ifa_ifu = found.ifu.as_ptr();
            }
        }
        // An `IFA_LOCAL` after an `IFA_ADDRESS` is a point-to-point link:
        // what was stored is the peer's address.
        IFA_LOCAL => {
            if !found.ifa.ifa_addr.is_null() {
                found.ifu = found.addr;
                found.ifa.ifa_ifu = found.ifu.as_ptr();
                found.addr = Sockany::EMPTY;
                found.ifa.ifa_addr = null_mut();
            }
            if copy_addr(&mut found.addr, af, data, index) {
                found.ifa.ifa_addr = found.addr.as_ptr();
            }
        }
        IFA_LABEL => found.set_name(data.strip_suffix(b"\0").unwrap_or(data)),
        _ => {}
    });
    if !found.ifa.ifa_addr.is_null() && gen_netmask(&mut found.netmask, af, prefixlen) {
        found.ifa.ifa_netmask = found.netmask.as_ptr();
    }
    if found.ifa.ifa_name.is_null() {
        // SAFETY: nothing refers to the entry, which came from `calloc`.
        unsafe { free(storage.cast()) };
        return Ok(());
    }
    ctx.push(storage);
    Ok(())
}

/// Frees the list `ifp`, which `getifaddrs` returned.
///
/// # Safety
///
/// `ifp` must be null or a list `getifaddrs` returned, not freed already.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn freeifaddrs(ifp: *mut Ifaddrs) {
    let mut entry = ifp;
    while !entry.is_null() {
        // SAFETY: the entry is one `getifaddrs` allocated.
        let next = unsafe { (*entry).ifa_next };
        // SAFETY: nothing refers to it any more, and it came from `calloc`.
        unsafe { free(entry.cast()) };
        entry = next;
    }
}

/// Builds the list from `SIOCGIFCONF`, for a kernel without `AF_NETLINK`.
/// IPv4 addresses only, each interface's flags and netmask asked for in turn.
fn from_ioctl(ctx: &mut Ctx) -> Result<(), c_int> {
    let fd = socket(AF_INET, DGRAM_CLOEXEC, 0);
    if fd < 0 {
        return Err(crate::pwd::last_errno());
    }
    let mut list = [0u8; IFREQ_SIZE * 32];
    // `struct ifconf`: the length, padding, then the buffer.
    let mut conf = [0u8; 16];
    put_at(&mut conf, 0, &(list.len() as u32).to_le_bytes());
    put_at(&mut conf, 8, &list.as_mut_ptr().addr().to_le_bytes());
    // SAFETY: the kernel reads and writes `conf`, a `struct ifconf`, and
    // fills the buffer it points at, which is `list`.
    let ret = unsafe { ioctl(fd, SIOCGIFCONF, conf.as_mut_ptr().addr() as c_ulong) };
    if ret < 0 {
        let error = crate::pwd::last_errno();
        let _ = close(fd);
        return Err(error);
    }
    let filled = (get32(&conf, 0) as usize).min(list.len());
    let mut offset = 0;
    let mut result = Ok(());
    while offset + IFREQ_SIZE <= filled {
        let request = list.get(offset..offset + IFREQ_SIZE).unwrap_or_default();
        result = take_ifreq(ctx, fd, request);
        if result.is_err() {
            break;
        }
        offset += IFREQ_SIZE;
    }
    let _ = close(fd);
    result
}

/// Takes one `struct ifreq` from `SIOCGIFCONF` as an entry of the list.
fn take_ifreq(ctx: &mut Ctx, fd: c_int, request: &[u8]) -> Result<(), c_int> {
    let name = request.get(..IFNAMSIZ).unwrap_or_default();
    let name = name.split(|&byte| byte == 0).next().unwrap_or_default();
    if name.is_empty() {
        return Ok(());
    }
    let af = request.get(IFNAMSIZ).copied().unwrap_or(0);
    let address = request
        .get(IFNAMSIZ + 4..IFNAMSIZ + 8)
        .unwrap_or_default()
        .to_owned_array();
    let storage = entry();
    if storage.is_null() {
        return Err(errno::ENOMEM);
    }
    // SAFETY: the entry is this call's own allocation.
    let found = unsafe { &mut *storage };
    found.set_name(name);
    if copy_addr(&mut found.addr, af, &address, 0) {
        found.ifa.ifa_addr = found.addr.as_ptr();
    }
    let mut query = [0u8; IFREQ_SIZE];
    for (slot, &byte) in query.iter_mut().zip(name) {
        *slot = byte;
    }
    // SAFETY: the kernel reads and writes `query`, a `struct ifreq`.
    if unsafe { ioctl(fd, SIOCGIFFLAGS, query.as_mut_ptr().addr() as u64) } == 0 {
        found.ifa.ifa_flags = u32::from(get16(&query, IFNAMSIZ));
    }
    // SAFETY: as above.
    if unsafe { ioctl(fd, SIOCGIFNETMASK, query.as_mut_ptr().addr() as u64) } == 0 {
        let mask = query
            .get(IFNAMSIZ + 4..IFNAMSIZ + 8)
            .unwrap_or_default()
            .to_owned_array();
        if copy_addr(&mut found.netmask, af, &mask, 0) {
            found.ifa.ifa_netmask = found.netmask.as_ptr();
        }
    }
    ctx.push(storage);
    Ok(())
}

/// The four bytes at the start of a slice, zero-padded.
trait FourBytes {
    /// The four bytes, or zeros where there are none.
    fn to_owned_array(&self) -> [u8; 4];
}

impl FourBytes for [u8] {
    fn to_owned_array(&self) -> [u8; 4] {
        let mut out = [0u8; 4];
        for (slot, &byte) in out.iter_mut().zip(self) {
            *slot = byte;
        }
        out
    }
}

/// Lists the machine's network interfaces and their addresses at `*ifap`.
/// Returns 0, or -1 with `errno` set.
///
/// # Safety
///
/// `ifap` must be valid for writes of a pointer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getifaddrs(ifap: *mut *mut Ifaddrs) -> c_int {
    let mut ctx = Ctx::EMPTY;
    let mut result = rtnetlink_enumerate(AF_UNSPEC as u8, AF_UNSPEC as u8, &mut |kind, payload| {
        if kind == RTM_NEWLINK {
            take_link(&mut ctx, payload)
        } else {
            take_address(&mut ctx, payload)
        }
    });
    if result == Err(errno::EAFNOSUPPORT) || result == Err(errno::EPROTONOSUPPORT) {
        // No route netlink in this kernel: what the ioctls can say instead.
        // SAFETY: the list built so far is this call's own allocations.
        unsafe { freeifaddrs(ctx.first.cast()) };
        ctx = Ctx::EMPTY;
        result = from_ioctl(&mut ctx);
    }
    match result {
        Ok(()) => {
            // SAFETY: the caller passes a writable pointer.
            unsafe { ifap.write(ctx.first.cast()) };
            0
        }
        Err(error) => {
            // SAFETY: the list is this call's own allocations.
            unsafe { freeifaddrs(ctx.first.cast()) };
            errno::set(error);
            -1
        }
    }
}

/// C's `struct if_nameindex`, from `include/net/if.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IfNameindex {
    /// The interface's index, or 0 to end the list.
    pub if_index: c_uint,
    /// The interface's name, or null to end the list.
    pub if_name: *mut c_char,
}

const _: () = assert!(size_of::<IfNameindex>() == 16);
const _: () = assert!(offset_of!(IfNameindex, if_name) == 8);

/// One interface's index and name, while the list is being gathered.
#[derive(Debug, Clone, Copy)]
struct NameEntry {
    /// The index.
    index: u32,
    /// How many bytes of `name` there are.
    len: usize,
    /// The name.
    name: [u8; IFNAMSIZ],
}

/// Frees the list `idx`, which `if_nameindex` returned.
///
/// # Safety
///
/// `idx` must be null or a list `if_nameindex` returned, not freed already.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn if_freenameindex(idx: *mut IfNameindex) {
    // SAFETY: the list is one allocation, which `if_nameindex` made.
    unsafe { free(idx.cast()) };
}

/// Every network interface's index and name, ending with an entry of 0 and
/// null, or null with `errno` set. The caller frees it with
/// `if_freenameindex`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn if_nameindex() -> *mut IfNameindex {
    let state = cancel::set_state(cancel::DISABLE);
    let mut found: Growable<NameEntry> = Growable::new();
    let mut room = true;
    let result = rtnetlink_enumerate(AF_UNSPEC as u8, AF_INET as u8, &mut |kind, payload| {
        let (index, wanted, skip) = if kind == RTM_NEWLINK {
            (get32(payload, 4), IFLA_IFNAME, IFINFOMSG)
        } else {
            (get32(payload, 4), IFA_LABEL, IFADDRMSG)
        };
        attributes(payload, skip, |attr, data| {
            if attr != wanted || !room {
                return;
            }
            let name = data.strip_suffix(b"\0").unwrap_or(data);
            if name.len() > IFNAMSIZ || known(found.as_slice(), index, name) {
                return;
            }
            let mut entry = NameEntry {
                index,
                len: name.len(),
                name: [0; IFNAMSIZ],
            };
            for (slot, &byte) in entry.name.iter_mut().zip(name) {
                *slot = byte;
            }
            room = found.push(entry);
        });
        Ok(())
    });
    let _ = cancel::set_state(state);
    if result.is_err() || !room {
        errno::set(if room {
            crate::pwd::last_errno()
        } else {
            errno::ENOBUFS
        });
        return null_mut();
    }
    build_nameindex(found.as_slice())
}

/// Whether `index` and `name` are already in `found`.
fn known(found: &[NameEntry], index: u32, name: &[u8]) -> bool {
    found.iter().any(|entry| {
        entry.index == index && entry.len == name.len() && entry.name.get(..entry.len) == Some(name)
    })
}

/// The one allocation `if_nameindex` returns: the array, its null end, and
/// the names after it.
fn build_nameindex(found: &[NameEntry]) -> *mut IfNameindex {
    let names: usize = found.iter().map(|entry| entry.len + 1).sum();
    let size = (found.len() + 1) * size_of::<IfNameindex>() + names;
    let list = malloc(size).cast::<IfNameindex>();
    if list.is_null() {
        errno::set(errno::ENOMEM);
        return null_mut();
    }
    let mut text = list.wrapping_add(found.len() + 1).cast::<c_char>();
    for (index, entry) in found.iter().enumerate() {
        let slot = IfNameindex {
            if_index: entry.index,
            if_name: text,
        };
        // SAFETY: the allocation holds `found.len() + 1` entries.
        unsafe { list.wrapping_add(index).write(slot) };
        // SAFETY: the allocation holds each name and its NUL after them.
        unsafe { crate::netdb::copy_out(text, &entry.name, entry.len) };
        text = text.wrapping_add(entry.len + 1);
    }
    let end = IfNameindex {
        if_index: 0,
        if_name: null_mut(),
    };
    // SAFETY: the last of the `found.len() + 1` entries.
    unsafe { list.wrapping_add(found.len()).write(end) };
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name, flags and address family of every entry `getifaddrs` gives.
    fn listed() -> Vec<(String, c_uint, u16)> {
        let mut list: *mut Ifaddrs = null_mut();
        // SAFETY: `list` is a live local.
        let code = unsafe { getifaddrs(&raw mut list) };
        assert_eq!(code, 0, "getifaddrs failed: {}", crate::pwd::last_errno());
        let mut found = Vec::new();
        let mut entry = list;
        while !entry.is_null() {
            // SAFETY: the entry is one `getifaddrs` allocated.
            let ifa = unsafe { entry.read() };
            // SAFETY: the name is NUL-terminated.
            let name = unsafe { core::ffi::CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let family = if ifa.ifa_addr.is_null() {
                0
            } else {
                // SAFETY: an address starts with its family.
                unsafe { ifa.ifa_addr.cast::<u16>().read_unaligned() }
            };
            found.push((name, ifa.ifa_flags, family));
            entry = ifa.ifa_next;
        }
        // SAFETY: the list is freed once, and not used again.
        unsafe { freeifaddrs(list) };
        found
    }

    #[test]
    fn every_interface_is_listed_with_its_addresses() {
        let found = listed();
        assert!(!found.is_empty());
        // The loopback interface is always there, with its link entry and at
        // least its IPv4 address.
        let loopback: Vec<_> = found.iter().filter(|entry| entry.0 == "lo").collect();
        assert!(loopback.len() >= 2, "{found:?}");
        // `IFF_UP | IFF_LOOPBACK` are set on every one of its entries.
        assert!(
            loopback.iter().all(|entry| entry.1 & 0x9 == 0x9),
            "{found:?}"
        );
        assert!(
            loopback.iter().any(|entry| c_int::from(entry.2) == AF_INET),
            "{found:?}"
        );
        assert!(
            loopback
                .iter()
                .any(|entry| c_int::from(entry.2) == AF_PACKET),
            "{found:?}"
        );
        // Freeing nothing is nothing.
        // SAFETY: a null list has no entries.
        unsafe { freeifaddrs(null_mut()) };
    }

    #[test]
    fn the_loopback_netmask_is_eight_bits_of_ones() {
        let mut list: *mut Ifaddrs = null_mut();
        // SAFETY: `list` is a live local.
        assert_eq!(unsafe { getifaddrs(&raw mut list) }, 0);
        let mut seen = None;
        let mut entry = list;
        while !entry.is_null() {
            // SAFETY: the entry is one `getifaddrs` allocated.
            let ifa = unsafe { entry.read() };
            // SAFETY: the name is NUL-terminated.
            let name = unsafe { core::ffi::CStr::from_ptr(ifa.ifa_name) };
            // SAFETY: an address starts with its family.
            let family = (!ifa.ifa_addr.is_null())
                .then(|| unsafe { ifa.ifa_addr.cast::<u16>().read_unaligned() });
            if name == c"lo" && family == Some(AF_INET as u16) && !ifa.ifa_netmask.is_null() {
                // SAFETY: the netmask is a `struct sockaddr_in` of the entry's.
                seen = Some(unsafe { ifa.ifa_netmask.cast::<SockaddrIn6>().read_unaligned() });
            }
            entry = ifa.ifa_next;
        }
        // SAFETY: the list is freed once.
        unsafe { freeifaddrs(list) };
        assert_eq!(seen.map(|mask| mask.v4_addr()), Some([255, 0, 0, 0]));
    }

    #[test]
    fn every_interface_has_an_index_and_a_name() {
        let list = if_nameindex();
        assert!(!list.is_null(), "{}", crate::pwd::last_errno());
        let mut found = Vec::new();
        let mut index = 0;
        loop {
            // SAFETY: the list ends with an entry of 0 and null.
            let entry = unsafe { list.wrapping_add(index).read() };
            if entry.if_name.is_null() {
                assert_eq!(entry.if_index, 0);
                break;
            }
            // SAFETY: the name is NUL-terminated inside the allocation.
            let name = unsafe { core::ffi::CStr::from_ptr(entry.if_name) }
                .to_string_lossy()
                .into_owned();
            found.push((entry.if_index, name));
            index += 1;
        }
        // SAFETY: the list is one allocation, freed once.
        unsafe { if_freenameindex(list) };
        assert!(found.iter().any(|entry| entry.1 == "lo"), "{found:?}");
        // No interface is listed twice, though it has a link message and an
        // address message.
        let mut names: Vec<_> = found.iter().map(|entry| entry.1.clone()).collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "{found:?}");
        // The index of `lo` is the one `if_nametoindex` gives.
        // SAFETY: the name is NUL-terminated.
        let expected = unsafe { crate::inet::if_nametoindex(c"lo".as_ptr()) };
        assert_eq!(
            found
                .iter()
                .find(|entry| entry.1 == "lo")
                .map(|entry| entry.0),
            Some(expected)
        );
        // SAFETY: freeing nothing is nothing.
        unsafe { if_freenameindex(null_mut()) };
    }

    #[test]
    fn netlink_lengths_are_checked_against_what_was_read() {
        // An attribute whose length runs past the message stops the walk.
        let mut seen = Vec::new();
        let payload = [0u8; IFINFOMSG + 8];
        attributes(&payload, IFINFOMSG, |kind, data| {
            seen.push((kind, data.len()))
        });
        assert!(seen.is_empty());
        let mut payload = [0u8; IFINFOMSG + 8];
        put_at(&mut payload, IFINFOMSG, &[8, 0, 3, 0, b'l', b'o', 0, 0]);
        attributes(&payload, IFINFOMSG, |kind, data| {
            seen.push((kind, data.len()))
        });
        assert_eq!(seen, [(IFLA_IFNAME, 4)]);
        // A length larger than what is left is refused.
        let mut payload = [0u8; IFINFOMSG + 8];
        put_at(&mut payload, IFINFOMSG, &[200, 0, 3, 0]);
        seen.clear();
        attributes(&payload, IFINFOMSG, |kind, data| {
            seen.push((kind, data.len()))
        });
        assert!(seen.is_empty());
        assert_eq!(align4(0), 0);
        assert_eq!(align4(1), 4);
        assert_eq!(align4(4), 4);
        assert_eq!(align4(5), 8);
    }

    #[test]
    fn a_netmask_is_the_prefix_length_in_ones() {
        let mut sa = Sockany::EMPTY;
        assert!(gen_netmask(&mut sa, AF_INET as u8, 24));
        assert_eq!(sa.0.get(4..8), Some(&[255, 255, 255, 0][..]));
        assert!(gen_netmask(&mut sa, AF_INET6 as u8, 64));
        assert_eq!(
            sa.0.get(8..24),
            Some(
                &[
                    255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0
                ][..]
            )
        );
        assert!(gen_netmask(&mut sa, AF_INET as u8, 0));
        assert_eq!(sa.0.get(4..8), Some(&[0, 0, 0, 0][..]));
        // A family that is not IPv4 or IPv6 stores nothing.
        assert!(!gen_netmask(&mut sa, AF_PACKET as u8, 24));
    }
}
