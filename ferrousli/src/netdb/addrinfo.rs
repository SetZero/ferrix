//! `getaddrinfo`, `freeaddrinfo` and `getnameinfo`.
//!
//! Ported from musl 1.2.5's `getaddrinfo.c`, `freeaddrinfo.c` and
//! `getnameinfo.c` (MIT). `getaddrinfo` puts every result, its address and the
//! canonical name in one allocation, which `freeaddrinfo` gives back once
//! every part of the list has been freed, counting under a lock as musl does.
//!
//! One thing differs from musl: `freeaddrinfo(NULL)` does nothing, where
//! musl's reads through the null pointer.
//!
//! `getnameinfo` reads the hosts file, then asks DNS for a `PTR` record, then
//! falls back to the numeric form unless `NI_NAMEREQD` was given, so it
//! answers on a kernel without `AF_INET` sockets as long as a name is not
//! required.

use core::ffi::{c_char, c_int, c_short, c_uint, c_void};
use core::mem::{offset_of, size_of};
#[cfg(test)]
use core::ptr::null;
use core::ptr::null_mut;

use super::dns::{self, RR_PTR};
use super::lookup::{
    Address, CANON, MAXADDRS, MAXSERVS, Service, first4, ipliteral, lookup_name, lookup_serv,
};
use super::resolver;
use super::{
    AF_INET, AF_INET6, AF_UNSPEC, AI_ADDRCONFIG, AI_ALL, AI_CANONNAME, AI_NUMERICHOST,
    AI_NUMERICSERV, AI_PASSIVE, AI_V4MAPPED, Addrinfo, Database, EAI_BADFLAGS, EAI_FAMILY,
    EAI_MEMORY, EAI_NODATA, EAI_NONAME, EAI_OVERFLOW, EAI_SYSTEM, IPPROTO_UDP, NI_DGRAM,
    NI_NAMEREQD, NI_NUMERICHOST, NI_NUMERICSCOPE, NI_NUMERICSERV, SOCK_CLOEXEC, SOCK_DGRAM,
    SOCKADDR_IN_LEN, SOCKADDR_IN6_LEN, SYSTEM, SockaddrIn, SockaddrIn6, Sources, V4MAPPED, at,
    c_bytes, c_bytes_max, c_len, is_linklocal, is_mc_linklocal, is_space,
};
use crate::cancel;
use crate::errno;
use crate::inet::{if_indextoname, inet_ntop};
use crate::lock::SpinLock;
use crate::malloc::{calloc, free};
use crate::socket::{connect, socket};
use crate::unistd::close;

/// One result: the `addrinfo` a program sees, the address it points at, and
/// the count of results still to be freed, as musl's `struct aibuf`.
#[repr(C)]
#[derive(Debug)]
struct AiBuf {
    /// What `getaddrinfo` returns a pointer to.
    ai: Addrinfo,
    /// The address `ai.ai_addr` points at.
    sa: SockaddrIn6,
    /// Held while `ref` changes, since two threads may free two parts of one
    /// list at once.
    lock: SpinLock,
    /// Which result this is, so that the first can be found from any of them.
    slot: c_short,
    /// In the first result: how many results are still to be freed.
    r#ref: c_short,
}

const _: () = assert!(size_of::<AiBuf>() == 88);
const _: () = assert!(offset_of!(AiBuf, ai) == 0);
const _: () = assert!(offset_of!(AiBuf, sa) == 48);

/// Stores `value` at `index` of `buf`, if it is inside.
fn set(buf: &mut [u8], index: usize, value: u8) {
    if let Some(slot) = buf.get_mut(index) {
        *slot = value;
    }
}

/// Writes `value` in decimal, with a NUL, and returns its length.
fn decimal(out: &mut [u8], value: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut len = 0;
    let mut left = value;
    loop {
        set(&mut digits, len, b'0' + (left % 10) as u8);
        len += 1;
        left /= 10;
        if left == 0 {
            break;
        }
    }
    for index in 0..len {
        let digit = at(&digits, len - 1 - index);
        set(out, index, digit);
    }
    set(out, len, 0);
    len
}

/// The longest `in-addr.arpa` or `ip6.arpa` name, with its NUL.
const PTR_MAX: usize = 64 + 14;

/// Writes `ip`'s `in-addr.arpa` name.
fn mkptr4(out: &mut [u8; PTR_MAX], ip: [u8; 4]) {
    let mut len = 0;
    for &byte in ip.iter().rev() {
        let mut digits = [0u8; 8];
        let written = decimal(&mut digits, u32::from(byte));
        for index in 0..written {
            set(out, len, at(&digits, index));
            len += 1;
        }
        set(out, len, b'.');
        len += 1;
    }
    for &byte in b"in-addr.arpa\0" {
        set(out, len, byte);
        len += 1;
    }
}

/// Writes `ip`'s `ip6.arpa` name: every nibble, last first.
fn mkptr6(out: &mut [u8; PTR_MAX], ip: &[u8; 16]) {
    let hex = b"0123456789abcdef";
    let mut len = 0;
    for &byte in ip.iter().rev() {
        for nibble in [byte & 15, byte >> 4] {
            set(
                out,
                len,
                hex.get(usize::from(nibble)).copied().unwrap_or(b'0'),
            );
            set(out, len + 1, b'.');
            len += 2;
        }
    }
    for &byte in b"ip6.arpa\0" {
        set(out, len, byte);
        len += 1;
    }
}

/// The name the hosts file gives the address `a`, as musl's `reverse_hosts`
/// finds it: the first name on the line whose address matches.
fn reverse_hosts(
    src: &Sources<'_>,
    buf: &mut [u8; CANON],
    a: &[u8; 16],
    scopeid: c_uint,
    family: c_int,
) {
    let Ok(Some(mut db)) = Database::open(src.hosts) else {
        return;
    };
    let wanted = if family == AF_INET {
        super::lookup::mapped(first4(a))
    } else {
        *a
    };
    let mut line = [0u8; 512];
    while let Some(len) = db.line(&mut line) {
        if let Some(hash) = line.iter().take(len).position(|&byte| byte == b'#') {
            set(&mut line, hash, b'\n');
            set(&mut line, hash + 1, 0);
        }
        let mut p = 0;
        while at(&line, p) != 0 && !is_space(at(&line, p)) {
            p += 1;
        }
        if at(&line, p) == 0 {
            continue;
        }
        set(&mut line, p, 0);
        p += 1;

        let mut iplit = Address::default();
        if ipliteral(
            &mut iplit,
            line.get(..c_len(&line)).unwrap_or_default(),
            AF_UNSPEC,
        ) <= 0
        {
            continue;
        }
        if iplit.family == AF_INET {
            iplit.addr = super::lookup::mapped(first4(&iplit.addr));
            iplit.scopeid = 0;
        }
        if wanted != iplit.addr || iplit.scopeid != scopeid {
            continue;
        }

        while at(&line, p) != 0 && is_space(at(&line, p)) {
            p += 1;
        }
        let mut z = p;
        while at(&line, z) != 0 && !is_space(at(&line, z)) {
            z += 1;
        }
        set(&mut line, z, 0);
        if z - p < CANON {
            super::copy_c(buf, &line, p);
            break;
        }
    }
}

/// The name the services file gives `port`, as musl's `reverse_services`
/// finds it: the first line with that port and protocol.
fn reverse_services(src: &Sources<'_>, buf: &mut [u8; CANON], port: u16, dgram: bool) {
    let Ok(Some(mut db)) = Database::open(src.services) else {
        return;
    };
    let mut line = [0u8; 128];
    while let Some(len) = db.line(&mut line) {
        if let Some(hash) = line.iter().take(len).position(|&byte| byte == b'#') {
            set(&mut line, hash, b'\n');
            set(&mut line, hash + 1, 0);
        }
        let mut p = 0;
        while at(&line, p) != 0 && !is_space(at(&line, p)) {
            p += 1;
        }
        if at(&line, p) == 0 {
            continue;
        }
        set(&mut line, p, 0);
        p += 1;
        let (svport, z) = super::strtoul(&line, p, 10);
        if svport != u64::from(port) || z == p {
            continue;
        }
        if dgram && !super::has_at(&line, z, b"/udp") {
            continue;
        }
        if !dgram && !super::has_at(&line, z, b"/tcp") {
            continue;
        }
        // The name and its NUL, which musl keeps to 32 bytes.
        if p > 32 {
            continue;
        }
        for (slot, &byte) in buf.iter_mut().zip(line.iter()).take(p) {
            *slot = byte;
        }
        break;
    }
}

/// Fills `node` and `serv` with the name and service of the address `sa`, as
/// musl's `getnameinfo` does, reading its files from `src`.
///
/// # Safety
///
/// `sa` must be valid for reads of `sl` bytes and at least a `sa_family_t`,
/// `node` null or valid for writes of `nodelen` bytes, and `serv` null or
/// valid for writes of `servlen`.
#[allow(
    clippy::excessive_nesting,
    clippy::too_many_arguments,
    reason = "the C interface has eight arguments and follows musl's lookup stages"
)]
pub unsafe fn nameinfo(
    src: &Sources<'_>,
    sa: *const c_void,
    sl: c_uint,
    node: *mut c_char,
    nodelen: c_uint,
    serv: *mut c_char,
    servlen: c_uint,
    flags: c_int,
) -> c_int {
    let mut ptr = [0u8; PTR_MAX];
    let mut a = [0u8; 16];
    let scopeid;
    let port;
    // SAFETY: the caller passes at least a `sa_family_t`.
    let family = c_int::from(unsafe { sa.cast::<u16>().read_unaligned() });
    match family {
        AF_INET => {
            if sl < SOCKADDR_IN_LEN {
                return EAI_FAMILY;
            }
            // SAFETY: the caller passes a `struct sockaddr_in`.
            let sin = unsafe { sa.cast::<SockaddrIn>().read_unaligned() };
            for (slot, byte) in a.iter_mut().zip(sin.sin_addr) {
                *slot = byte;
            }
            mkptr4(&mut ptr, sin.sin_addr);
            scopeid = 0;
            port = sin.sin_port;
        }
        AF_INET6 => {
            if sl < SOCKADDR_IN6_LEN {
                return EAI_FAMILY;
            }
            // SAFETY: the caller passes a `struct sockaddr_in6`.
            let sin6 = unsafe { sa.cast::<SockaddrIn6>().read_unaligned() };
            a = sin6.sin6_addr;
            if a.get(..12) == Some(&V4MAPPED[..]) {
                // An IPv4-mapped address has an `in-addr.arpa` name.
                let [.., v0, v1, v2, v3] = a;
                mkptr4(&mut ptr, [v0, v1, v2, v3]);
            } else {
                mkptr6(&mut ptr, &a);
            }
            scopeid = sin6.sin6_scope_id;
            port = sin6.sin6_port;
        }
        _ => return EAI_FAMILY,
    }

    if !node.is_null() && nodelen != 0 {
        let mut buf = [0u8; CANON];
        if flags & NI_NUMERICHOST == 0 {
            reverse_hosts(src, &mut buf, &a, scopeid, family);
        }
        if buf[0] == 0 && flags & NI_NUMERICHOST == 0 {
            let name = ptr.get(..c_len(&ptr)).unwrap_or_default();
            if let Some((mut query, qlen)) = dns::mkquery(0, name, 1, RR_PTR, dns::query_id()) {
                // No need for the AD flag.
                set(&mut query, 3, 0);
                let mut reply = [0u8; 512];
                let rlen =
                    resolver::res_send_from(src, query.get(..qlen).unwrap_or_default(), &mut reply);
                buf[0] = 0;
                if rlen > 0 {
                    let rlen = usize::try_from(rlen).unwrap_or(0).min(reply.len());
                    let packet = reply.get(..rlen).unwrap_or_default();
                    let _ = dns::parse(packet, |rr, data, _| {
                        if rr != RR_PTR {
                            return 0;
                        }
                        if dns::expand(packet, data, &mut buf).is_none() {
                            buf[0] = 0;
                        }
                        0
                    });
                }
            }
        }
        if buf[0] == 0 {
            if flags & NI_NAMEREQD != 0 {
                return EAI_NONAME;
            }
            // SAFETY: `a` holds the address's bytes, four for `AF_INET` and
            // sixteen for `AF_INET6`, and `buf` has room for its text.
            let _ = unsafe {
                inet_ntop(
                    family,
                    a.as_ptr().cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len() as c_uint,
                )
            };
            if scopeid != 0 {
                let mut text = [0u8; 20];
                let mut name = [0u8; 17];
                let named = flags & NI_NUMERICSCOPE == 0
                    && (is_linklocal(&a) || is_mc_linklocal(&a))
                    // SAFETY: `name` has more than `IF_NAMESIZE` bytes.
                    && !unsafe { if_indextoname(scopeid, name.as_mut_ptr().cast()) }.is_null();
                let scope: &[u8] = if named {
                    name.get(..c_len(&name)).unwrap_or_default()
                } else {
                    let len = decimal(&mut text, scopeid);
                    text.get(..len).unwrap_or_default()
                };
                let mut len = c_len(&buf);
                set(&mut buf, len, b'%');
                len += 1;
                for &byte in scope {
                    set(&mut buf, len, byte);
                    len += 1;
                }
                set(&mut buf, len, 0);
            }
        }
        let len = c_len(&buf);
        if len >= nodelen as usize {
            return EAI_OVERFLOW;
        }
        for (index, &byte) in buf.iter().take(len + 1).enumerate() {
            // SAFETY: `len + 1` is at most `nodelen` writable bytes.
            unsafe { node.wrapping_add(index).write(byte as c_char) };
        }
    }

    if !serv.is_null() && servlen != 0 {
        let mut buf = [0u8; CANON];
        let port = u16::from_be(port);
        if flags & NI_NUMERICSERV == 0 {
            reverse_services(src, &mut buf, port, flags & NI_DGRAM != 0);
        }
        if buf[0] == 0 {
            let _ = decimal(&mut buf, u32::from(port));
        }
        let len = c_len(&buf);
        if len >= servlen as usize {
            return EAI_OVERFLOW;
        }
        for (index, &byte) in buf.iter().take(len + 1).enumerate() {
            // SAFETY: `len + 1` is at most `servlen` writable bytes.
            unsafe { serv.wrapping_add(index).write(byte as c_char) };
        }
    }
    0
}

/// Fills `node` and `serv` with the name and service of the address `sa`.
/// Returns 0, or an `EAI_` error.
///
/// # Safety
///
/// As [`nameinfo`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getnameinfo(
    sa: *const c_void,
    sl: c_uint,
    node: *mut c_char,
    nodelen: c_uint,
    serv: *mut c_char,
    servlen: c_uint,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `nameinfo`'s.
    unsafe { nameinfo(&SYSTEM, sa, sl, node, nodelen, serv, servlen, flags) }
}

/// Whether an address of `family` can be reached, as `AI_ADDRCONFIG` asks:
/// whether a socket of that family can be made and the loopback address
/// connected to. Returns the family left to look up, and whether the family
/// asked for is gone.
fn addrconfig(mut family: c_int) -> Result<(c_int, bool), c_int> {
    let mut no_family = false;
    let loopback = [
        SockaddrIn6::v4([127, 0, 0, 1], 65535),
        SockaddrIn6::v6([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 65535, 0),
    ];
    let families = [AF_INET, AF_INET6];
    let lengths = [SOCKADDR_IN_LEN, SOCKADDR_IN6_LEN];
    for ((&this, &length), address) in families.iter().zip(&lengths).zip(&loopback) {
        let other = if this == AF_INET { AF_INET6 } else { AF_INET };
        if family == other {
            continue;
        }
        let fd = socket(this, SOCK_CLOEXEC | SOCK_DGRAM, IPPROTO_UDP);
        if fd >= 0 {
            let state = cancel::set_state(cancel::DISABLE);
            // SAFETY: the address is a live local of its length.
            let r = unsafe { connect(fd, address.as_ptr(), length) };
            let saved = crate::pwd::last_errno();
            let _ = cancel::set_state(state);
            let _ = close(fd);
            if r == 0 {
                continue;
            }
            errno::set(saved);
        }
        match crate::pwd::last_errno() {
            errno::EADDRNOTAVAIL
            | errno::EAFNOSUPPORT
            | errno::EHOSTUNREACH
            | errno::ENETDOWN
            | errno::ENETUNREACH => {}
            _ => return Err(EAI_SYSTEM),
        }
        if family == this {
            no_family = true;
        }
        family = other;
    }
    Ok((family, no_family))
}

/// Looks `host` and `serv` up, as musl's `getaddrinfo` does, reading its files
/// from `src`, and stores the list of results at `res`.
///
/// # Safety
///
/// `host` and `serv` must be null or NUL-terminated strings, `hint` null or a
/// valid `addrinfo`, and `res` valid for a write.
pub unsafe fn addrinfo(
    src: &Sources<'_>,
    host: *const c_char,
    serv: *const c_char,
    hint: *const Addrinfo,
    res: *mut *mut Addrinfo,
) -> c_int {
    if host.is_null() && serv.is_null() {
        return EAI_NONAME;
    }
    let mut family = AF_UNSPEC;
    let mut flags = 0;
    let mut proto = 0;
    let mut socktype = 0;
    if !hint.is_null() {
        // SAFETY: the caller passes a valid `addrinfo`.
        let hint = unsafe { hint.read() };
        family = hint.ai_family;
        flags = hint.ai_flags;
        proto = hint.ai_protocol;
        socktype = hint.ai_socktype;
        let mask = AI_PASSIVE
            | AI_CANONNAME
            | AI_NUMERICHOST
            | AI_V4MAPPED
            | AI_ALL
            | AI_ADDRCONFIG
            | AI_NUMERICSERV;
        if flags & mask != flags {
            return EAI_BADFLAGS;
        }
        if family != AF_INET && family != AF_INET6 && family != AF_UNSPEC {
            return EAI_FAMILY;
        }
    }

    let mut no_family = false;
    if flags & AI_ADDRCONFIG != 0 {
        match addrconfig(family) {
            Ok((left, none)) => {
                family = left;
                no_family = none;
            }
            Err(error) => return error,
        }
    }

    let mut ports = [Service::default(); MAXSERVS];
    // SAFETY: the caller passes a NUL-terminated string or null.
    let serv_name = (!serv.is_null()).then(|| unsafe { c_bytes(serv) });
    let nservs = lookup_serv(src, &mut ports, serv_name, proto, socktype, flags);
    if nservs < 0 {
        return nservs;
    }

    let mut addrs = [Address::default(); MAXADDRS];
    let mut canon = [0u8; CANON];
    // SAFETY: as above; at most 255 bytes of the name are read.
    let host_name = (!host.is_null()).then(|| unsafe { c_bytes_max(host, 255) });
    let naddrs = lookup_name(src, &mut addrs, &mut canon, host_name, family, flags);
    if naddrs < 0 {
        return naddrs;
    }
    if no_family {
        return EAI_NODATA;
    }

    let nais = (nservs as usize) * (naddrs as usize);
    let canon_len = c_len(&canon);
    let Some(bytes) = nais
        .checked_mul(size_of::<AiBuf>())
        .and_then(|bytes| bytes.checked_add(canon_len + 1))
    else {
        return EAI_MEMORY;
    };
    let out = calloc(1, bytes).cast::<AiBuf>();
    if out.is_null() {
        return EAI_MEMORY;
    }
    let outcanon = if canon_len > 0 {
        let text = out.wrapping_add(nais).cast::<c_char>();
        for (index, &byte) in canon.iter().take(canon_len + 1).enumerate() {
            // SAFETY: the allocation has `canon_len + 1` bytes after the
            // results.
            unsafe { text.wrapping_add(index).write(byte as c_char) };
        }
        text
    } else {
        null_mut()
    };

    let mut k = 0;
    for address in addrs.iter().take(naddrs as usize) {
        for port in ports.iter().take(nservs as usize) {
            let entry = out.wrapping_add(k);
            let sa = if address.family == AF_INET {
                SockaddrIn6::v4(first4(&address.addr), port.port.to_be())
            } else {
                SockaddrIn6::v6(address.addr, port.port.to_be(), address.scopeid)
            };
            // SAFETY: `entry` is result `k` of the allocation and therefore
            // points at a live `AiBuf` slot.
            let ai_addr = unsafe { (&raw mut (*entry).sa).cast() };
            let ai = Addrinfo {
                ai_flags: 0,
                ai_family: address.family,
                ai_socktype: c_int::from(port.socktype),
                ai_protocol: c_int::from(port.proto),
                ai_addrlen: if address.family == AF_INET {
                    SOCKADDR_IN_LEN
                } else {
                    SOCKADDR_IN6_LEN
                },
                ai_addr,
                ai_canonname: outcanon,
                ai_next: null_mut(),
            };
            // SAFETY: `entry` is result `k` of the allocation, which nothing
            // else refers to yet.
            unsafe {
                entry.write(AiBuf {
                    ai,
                    sa,
                    lock: SpinLock::new(),
                    slot: k as c_short,
                    r#ref: 0,
                });
            }
            if k > 0 {
                let previous = out.wrapping_add(k - 1);
                let next = previous
                    .wrapping_byte_add(offset_of!(AiBuf, ai) + offset_of!(Addrinfo, ai_next))
                    .cast::<*mut Addrinfo>();
                let current = entry
                    .wrapping_byte_add(offset_of!(AiBuf, ai))
                    .cast::<Addrinfo>();
                // SAFETY: result `k - 1` was written by the round before.
                unsafe { next.write(current) };
            }
            k += 1;
        }
    }
    // SAFETY: the first result was written above; `nais` is at least one.
    let references = out
        .wrapping_byte_add(offset_of!(AiBuf, r#ref))
        .cast::<c_short>();
    // SAFETY: the first result was written above; `nais` is at least one.
    unsafe { references.write(nais as c_short) };
    // SAFETY: the caller passes a writable pointer.
    let first = out
        .wrapping_byte_add(offset_of!(AiBuf, ai))
        .cast::<Addrinfo>();
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(first) };
    0
}

/// Looks `host` and `serv` up and stores the list of results at `res`.
/// Returns 0, or an `EAI_` error.
///
/// # Safety
///
/// As [`addrinfo`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getaddrinfo(
    host: *const c_char,
    serv: *const c_char,
    hint: *const Addrinfo,
    res: *mut *mut Addrinfo,
) -> c_int {
    // SAFETY: the caller's contract is `addrinfo`'s.
    unsafe { addrinfo(&SYSTEM, host, serv, hint, res) }
}

/// Frees the results `p` begins, once every part of the list has been freed.
/// A null pointer is ignored, where musl reads through it.
///
/// # Safety
///
/// `p` must be null or a list `getaddrinfo` returned, or the tail of one, not
/// freed before.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn freeaddrinfo(p: *mut Addrinfo) {
    if p.is_null() {
        return;
    }
    let mut cnt: c_short = 1;
    let mut last = p;
    loop {
        // SAFETY: the caller passes results `getaddrinfo` returned.
        let next = unsafe { (*last).ai_next };
        if next.is_null() {
            break;
        }
        cnt = cnt.wrapping_add(1);
        last = next;
    }
    let entry = last.cast::<AiBuf>();
    // SAFETY: the `addrinfo` is the first member of its `AiBuf`.
    let slot = unsafe { (*entry).slot };
    let base = entry.wrapping_sub(slot as usize);
    // SAFETY: the first result lives as long as any of them.
    let lock = unsafe { &(*base).lock };
    lock.acquire();
    // SAFETY: as above, and the lock is held.
    let left = unsafe { (*base).r#ref }.wrapping_sub(cnt);
    // SAFETY: as above.
    let references = base
        .wrapping_byte_add(offset_of!(AiBuf, r#ref))
        .cast::<c_short>();
    // SAFETY: the first result lives as long as any of them, and the lock is
    // held.
    unsafe { references.write(left) };
    if left == 0 {
        // SAFETY: nothing refers to the allocation now.
        unsafe { free(base.cast()) };
    } else {
        lock.release();
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "a test that indexes out of range fails, which is what a test does"
)]
mod tests {
    use super::*;
    use crate::netdb::testing::{Paths, Record, Reply, Responder};
    use crate::netdb::{EAI_SERVICE, NI_NUMERICSERV};

    /// The results as (family, socktype, protocol, address text, port).
    fn results(list: *mut Addrinfo) -> Vec<(c_int, c_int, c_int, String, u16)> {
        let mut out = Vec::new();
        let mut at = list;
        while !at.is_null() {
            // SAFETY: the list is what `getaddrinfo` returned.
            let ai = unsafe { at.read() };
            let mut text = [0u8; 64];
            let sa = ai.ai_addr.cast::<SockaddrIn6>();
            // SAFETY: the address is the result's own.
            let sa = unsafe { sa.read() };
            let addr: *const c_void = if ai.ai_family == AF_INET {
                core::ptr::from_ref(&sa.sin6_flowinfo).cast()
            } else {
                sa.sin6_addr.as_ptr().cast()
            };
            // SAFETY: the address has the bytes its family needs.
            let _ = unsafe {
                inet_ntop(
                    ai.ai_family,
                    addr,
                    text.as_mut_ptr().cast(),
                    text.len() as c_uint,
                )
            };
            out.push((
                ai.ai_family,
                ai.ai_socktype,
                ai.ai_protocol,
                String::from_utf8_lossy(&text[..c_len(&text)]).into_owned(),
                u16::from_be(sa.sin6_port),
            ));
            at = ai.ai_next;
        }
        out
    }

    fn canonname(list: *mut Addrinfo) -> String {
        // SAFETY: the list is what `getaddrinfo` returned.
        let name = unsafe { (*list).ai_canonname };
        if name.is_null() {
            return String::new();
        }
        // SAFETY: the canonical name is a NUL-terminated string.
        unsafe { core::ffi::CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn numeric_lookups_need_no_files_or_sockets() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let hint = Addrinfo {
            ai_flags: AI_NUMERICHOST | AI_NUMERICSERV,
            ai_family: AF_UNSPEC,
            ai_socktype: SOCK_DGRAM,
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_addr: null_mut(),
            ai_canonname: null_mut(),
            ai_next: null_mut(),
        };
        let mut list = null_mut();
        // SAFETY: the strings are NUL-terminated and the pointers live.
        let r = unsafe {
            addrinfo(
                &src,
                c"192.0.2.7".as_ptr(),
                c"53".as_ptr(),
                &raw const hint,
                &raw mut list,
            )
        };
        assert_eq!(r, 0);
        assert_eq!(
            results(list),
            [(AF_INET, SOCK_DGRAM, IPPROTO_UDP, "192.0.2.7".into(), 53)]
        );
        // SAFETY: the list came from `addrinfo` and is not used again.
        unsafe { freeaddrinfo(list) };

        let mut list = null_mut();
        // SAFETY: as above.
        let r = unsafe {
            addrinfo(
                &src,
                c"::1".as_ptr(),
                null(),
                &raw const hint,
                &raw mut list,
            )
        };
        assert_eq!(r, 0);
        assert_eq!(results(list).len(), 1);
        assert_eq!(results(list)[0].3, "::1");
        // SAFETY: as above.
        unsafe { freeaddrinfo(list) };

        // A name is not numeric.
        let mut list = null_mut();
        // SAFETY: as above.
        let r = unsafe {
            addrinfo(
                &src,
                c"example.test".as_ptr(),
                null(),
                &raw const hint,
                &raw mut list,
            )
        };
        assert_eq!(r, EAI_NONAME);
        // Neither host nor service.
        // SAFETY: as above.
        let r = unsafe { addrinfo(&src, null(), null(), null(), &raw mut list) };
        assert_eq!(r, EAI_NONAME);
        // An unknown flag and an unknown family.
        let bad = Addrinfo {
            ai_flags: 0x8000,
            ..hint
        };
        // SAFETY: as above.
        let r = unsafe { addrinfo(&src, c"::1".as_ptr(), null(), &raw const bad, &raw mut list) };
        assert_eq!(r, EAI_BADFLAGS);
        let bad = Addrinfo {
            ai_family: 1,
            ..hint
        };
        // SAFETY: as above.
        let r = unsafe { addrinfo(&src, c"::1".as_ptr(), null(), &raw const bad, &raw mut list) };
        assert_eq!(r, EAI_FAMILY);
    }

    #[test]
    fn every_address_is_paired_with_every_port() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut list = null_mut();
        // SAFETY: the strings are NUL-terminated and the pointers live.
        let r = unsafe {
            addrinfo(
                &src,
                c"multi.example.test".as_ptr(),
                c"http".as_ptr(),
                null(),
                &raw mut list,
            )
        };
        assert_eq!(r, 0);
        let found = results(list);
        // Three addresses, each with a TCP and a UDP port.
        assert_eq!(found.len(), 6);
        assert!(found.iter().all(|result| result.4 == 8080));
        assert_eq!(
            found.iter().filter(|result| result.1 == SOCK_DGRAM).count(),
            3
        );
        assert_eq!(canonname(list), "multi.example.test");
        // Freeing the tail first leaves the rest usable.
        // SAFETY: the list came from `addrinfo`.
        let second = unsafe { (*list).ai_next };
        // Detach the sublist before freeing it, as a C caller retaining the
        // prefix must do.
        // SAFETY: `list` is the live head of the returned list.
        unsafe { (*list).ai_next = null_mut() };
        // SAFETY: the tail is part of the same allocation.
        unsafe { freeaddrinfo(second) };
        assert_eq!(results(list).len(), 1);
        // SAFETY: the head is freed last, which frees the allocation.
        unsafe { freeaddrinfo(list) };

        let mut list = null_mut();
        // SAFETY: as above.
        let r = unsafe {
            addrinfo(
                &src,
                c"alias".as_ptr(),
                c"nosuchservice".as_ptr(),
                null(),
                &raw mut list,
            )
        };
        assert_eq!(r, EAI_SERVICE);
    }

    #[test]
    fn names_resolve_through_dns_with_a_canonical_name() {
        let paths = Paths::new("resolv.conf");
        let responder = Responder::start(|name, _| match name {
            "www.search.test" => Reply::Records(vec![
                Record::Cname("real.search.test"),
                Record::A([198, 51, 100, 7]),
            ]),
            _ => Reply::Code(3),
        });
        let src = paths.sources(responder.port);
        let hint = Addrinfo {
            ai_flags: AI_CANONNAME,
            ai_family: AF_INET,
            ai_socktype: 0,
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_addr: null_mut(),
            ai_canonname: null_mut(),
            ai_next: null_mut(),
        };
        let mut list = null_mut();
        // SAFETY: the strings are NUL-terminated and the pointers live.
        let r = unsafe {
            addrinfo(
                &src,
                c"www".as_ptr(),
                c"80".as_ptr(),
                &raw const hint,
                &raw mut list,
            )
        };
        assert_eq!(r, 0);
        assert_eq!(results(list)[0].3, "198.51.100.7");
        assert_eq!(canonname(list), "real.search.test");
        // SAFETY: the list came from `addrinfo`.
        unsafe { freeaddrinfo(list) };
    }

    #[test]
    fn getnameinfo_reads_the_hosts_file_then_falls_back_to_numbers() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut node = [0 as c_char; 256];
        let mut serv = [0 as c_char; 32];
        let sa = SockaddrIn6::v4([192, 0, 2, 10], 8080u16.to_be());
        let name = |sa: &SockaddrIn6, len, flags, node: &mut [c_char], serv: &mut [c_char]| {
            // SAFETY: the address and the buffers are live locals.
            unsafe {
                nameinfo(
                    &src,
                    sa.as_ptr(),
                    len,
                    node.as_mut_ptr(),
                    node.len() as c_uint,
                    serv.as_mut_ptr(),
                    serv.len() as c_uint,
                    flags,
                )
            }
        };
        let text = |buf: &[c_char]| {
            // SAFETY: the buffer holds a NUL-terminated string.
            unsafe { core::ffi::CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(name(&sa, SOCKADDR_IN_LEN, 0, &mut node, &mut serv), 0);
        assert_eq!(text(&node), "server.example.test");
        assert_eq!(text(&serv), "http");
        assert_eq!(
            name(
                &sa,
                SOCKADDR_IN_LEN,
                NI_NUMERICHOST | NI_NUMERICSERV,
                &mut node,
                &mut serv
            ),
            0
        );
        assert_eq!(
            (text(&node), text(&serv)),
            ("192.0.2.10".into(), "8080".into())
        );
        // An address in no file, with no name server listening.
        let sa = SockaddrIn6::v4([203, 0, 113, 9], 0);
        assert_eq!(name(&sa, SOCKADDR_IN_LEN, 0, &mut node, &mut serv), 0);
        assert_eq!(
            (text(&node), text(&serv)),
            ("203.0.113.9".into(), "0".into())
        );
        assert_eq!(
            name(&sa, SOCKADDR_IN_LEN, NI_NAMEREQD, &mut node, &mut serv),
            EAI_NONAME
        );
        // Too little room, a short address, and an unknown family.
        let mut small = [0 as c_char; 4];
        assert_eq!(
            name(&sa, SOCKADDR_IN_LEN, 0, &mut small, &mut serv),
            EAI_OVERFLOW
        );
        assert_eq!(name(&sa, 4, 0, &mut node, &mut serv), EAI_FAMILY);
        let other = SockaddrIn6 {
            sin6_family: 1,
            ..SockaddrIn6::default()
        };
        assert_eq!(
            name(&other, SOCKADDR_IN6_LEN, 0, &mut node, &mut serv),
            EAI_FAMILY
        );
        // An IPv6 address with a scope.
        let sa = SockaddrIn6::v6([0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 0, 1);
        assert_eq!(
            name(
                &sa,
                SOCKADDR_IN6_LEN,
                NI_NUMERICHOST | NI_NUMERICSERV,
                &mut node,
                &mut serv
            ),
            0
        );
        assert!(text(&node).starts_with("fe80::1%"), "{}", text(&node));
        assert_eq!(
            name(
                &sa,
                SOCKADDR_IN6_LEN,
                NI_NUMERICHOST | NI_NUMERICSERV | NI_NUMERICSCOPE,
                &mut node,
                &mut serv
            ),
            0
        );
        assert_eq!(text(&node), "fe80::1%1");
    }

    #[test]
    fn getnameinfo_asks_dns_for_a_pointer_record() {
        let paths = Paths::new("resolv.conf");
        let responder = Responder::start(|name, _| {
            if name == "9.113.0.203.in-addr.arpa" {
                Reply::Records(vec![Record::Ptr("named.test")])
            } else if name
                == "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.8.b.d.0.1.0.0.2.ip6.arpa"
            {
                Reply::Records(vec![Record::Ptr("six.test")])
            } else {
                Reply::Code(3)
            }
        });
        let src = paths.sources(responder.port);
        let mut node = [0 as c_char; 256];
        let sa = SockaddrIn6::v4([203, 0, 113, 9], 0);
        // SAFETY: the address and the buffer are live locals.
        let r = unsafe {
            nameinfo(
                &src,
                sa.as_ptr(),
                SOCKADDR_IN_LEN,
                node.as_mut_ptr(),
                256,
                null_mut(),
                0,
                0,
            )
        };
        assert_eq!(r, 0);
        // SAFETY: the buffer holds a NUL-terminated string.
        let name = unsafe { core::ffi::CStr::from_ptr(node.as_ptr()) };
        assert_eq!(name, c"named.test");
        let mut addr = [0u8; 16];
        addr[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
        addr[15] = 1;
        let sa = SockaddrIn6::v6(addr, 0, 0);
        // SAFETY: as above.
        let r = unsafe {
            nameinfo(
                &src,
                sa.as_ptr(),
                SOCKADDR_IN6_LEN,
                node.as_mut_ptr(),
                256,
                null_mut(),
                0,
                0,
            )
        };
        assert_eq!(r, 0);
        // SAFETY: as above.
        let name = unsafe { core::ffi::CStr::from_ptr(node.as_ptr()) };
        assert_eq!(name, c"six.test");
    }
}
