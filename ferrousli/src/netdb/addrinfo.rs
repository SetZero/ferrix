//! `getaddrinfo`, `freeaddrinfo` and `getnameinfo`.
//!
//! Ported from musl 1.2.5's `getaddrinfo.c`, `freeaddrinfo.c` and
//! `getnameinfo.c` (MIT). One `malloc` holds a whole `getaddrinfo` result: an
//! array of [`Aibuf`], each an `addrinfo` a caller sees followed by the
//! address it points at, and the canonical name after the array. A caller may
//! free the list from any of its entries, and musl's reference count is what
//! makes that work.
//!
//! Two things differ from musl:
//!
//! * `freeaddrinfo(NULL)` does nothing. musl dereferences it.
//! * The reference count is an atomic rather than a count behind a lock. The
//!   operation the lock protected is one subtraction, which `fetch_sub` does.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicI16, Ordering};

use super::dns::{self, RR_PTR};
use super::lookup::{
    Address, CANON, MAXADDRS, MAXSERVS, Service, first4, ipliteral, lookup_name, lookup_serv,
    mapped,
};
use super::resolver;
use super::{
    AF_INET, AF_INET6, AF_UNSPEC, AI_ADDRCONFIG, AI_ALL, AI_CANONNAME, AI_NUMERICHOST,
    AI_NUMERICSERV, AI_PASSIVE, AI_V4MAPPED, Addrinfo, Database, EAI_BADFLAGS, EAI_FAMILY,
    EAI_MEMORY, EAI_NODATA, EAI_NONAME, EAI_OVERFLOW, EAI_SYSTEM, IPPROTO_UDP, NI_DGRAM,
    NI_NAMEREQD, NI_NUMERICHOST, NI_NUMERICSCOPE, NI_NUMERICSERV, SOCK_CLOEXEC, SOCK_DGRAM,
    SOCKADDR_IN_LEN, SOCKADDR_IN6_LEN, SYSTEM, SockaddrIn, SockaddrIn6, Sources, V4MAPPED, at,
    c_bytes, c_bytes_max, c_len, copy_c, is_linklocal, is_mc_linklocal, is_space,
};
use crate::inet::{if_indextoname, inet_ntop};
use crate::malloc::{calloc, free};
use crate::socket::{connect, socket};
use crate::unistd::close;
use crate::{cancel, errno};

/// `IF_NAMESIZE`, from `include/net/if.h`.
const IF_NAMESIZE: usize = 16;

/// The longest reverse name: 64 nibbles with their dots, then `ip6.arpa` or
/// `in-addr.arpa`, and a NUL.
const PTR_MAX: usize = 80;

/// The longest text `inet_ntop` writes, `INET6_ADDRSTRLEN` from
/// `include/netinet/in.h`.
const INET6_ADDRSTRLEN: c_uint = 46;

/// The loopback address `::1`.
const LOOPBACK6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

/// The flags a hint may carry, as musl's `getaddrinfo` checks them.
const HINT_FLAGS: c_int = AI_PASSIVE
    | AI_CANONNAME
    | AI_NUMERICHOST
    | AI_V4MAPPED
    | AI_ALL
    | AI_ADDRCONFIG
    | AI_NUMERICSERV;

/// One result, as musl's `struct aibuf`: the `addrinfo` the caller sees, the
/// address it points at, and what [`freeaddrinfo`] needs to find the
/// allocation from any entry of it.
///
/// musl's `lock` is kept for its layout, and is never taken: `refs` is an
/// atomic, so the one subtraction the lock protected needs no lock.
#[repr(C)]
#[derive(Debug)]
struct Aibuf {
    /// What the caller sees. It is first, so a pointer to it is a pointer to
    /// the whole entry.
    ai: Addrinfo,
    /// The address `ai.ai_addr` points at.
    sa: SockaddrIn6,
    /// musl's lock word, never taken.
    lock: c_int,
    /// Which entry of the allocation this is.
    slot: i16,
    /// How many entries of the allocation are still held, counted on the
    /// first entry alone.
    refs: AtomicI16,
}

const _: () = assert!(offset_of!(Aibuf, ai) == 0);
const _: () = assert!(offset_of!(Aibuf, sa) == 48);
const _: () = assert!(offset_of!(Aibuf, slot) == 80);
const _: () = assert!(size_of::<Aibuf>() == 88);

/// Stores `value` at `index`, if it is inside `buf`.
fn set(buf: &mut [u8], index: usize, value: u8) {
    if let Some(slot) = buf.get_mut(index) {
        *slot = value;
    }
}

/// Writes `byte` at `*len` in `out` and counts it, whether or not it fits.
fn put(out: &mut [u8], len: &mut usize, byte: u8) {
    set(out, *len, byte);
    *len += 1;
}

/// Writes `value` in decimal at `*len` in `out`.
fn decimal(out: &mut [u8], len: &mut usize, value: u32) {
    if value >= 10 {
        decimal(out, len, value / 10);
    }
    put(
        out,
        len,
        b"0123456789"
            .get((value % 10) as usize)
            .copied()
            .unwrap_or(b'0'),
    );
}

/// Writes `text` at `*len` in `out`.
fn write_bytes(out: &mut [u8], len: &mut usize, text: &[u8]) {
    for &byte in text {
        put(out, len, byte);
    }
}

/// Copies `len` bytes of `text` and a NUL to `dest`.
///
/// # Safety
///
/// `dest` must be valid for writes of `len + 1` bytes.
unsafe fn copy_out(dest: *mut c_char, text: &[u8], len: usize) {
    for index in 0..=len {
        let byte = at(text, index);
        // SAFETY: the caller vouches for `len + 1` writable bytes.
        unsafe { dest.wrapping_add(index).write(byte as c_char) };
    }
}

// ---------------------------------------------------------------------------
// getaddrinfo
// ---------------------------------------------------------------------------

/// Whether `af` is configured, as musl's `AI_ADDRCONFIG` decides it: a
/// datagram socket for the family opens, and connects to `address`, the
/// family's own loopback address. `Err` holds `EAI_SYSTEM` for a failure that
/// says nothing about the family.
fn configured(af: c_int, address: &SockaddrIn6, len: c_uint) -> Result<bool, c_int> {
    let fd = socket(af, SOCK_DGRAM | SOCK_CLOEXEC, IPPROTO_UDP);
    let error = if fd < 0 {
        crate::pwd::last_errno()
    } else {
        let state = cancel::set_state(cancel::DISABLE);
        // SAFETY: `address` is a live local of at least `len` bytes.
        let connected = unsafe { connect(fd, address.as_ptr(), len) } == 0;
        let error = crate::pwd::last_errno();
        let _ = cancel::set_state(state);
        let _ = close(fd);
        if connected {
            return Ok(true);
        }
        error
    };
    match error {
        errno::EADDRNOTAVAIL
        | errno::EAFNOSUPPORT
        | errno::EHOSTUNREACH
        | errno::ENETDOWN
        | errno::ENETUNREACH => Ok(false),
        _ => Err(EAI_SYSTEM),
    }
}

/// The family to look up under `AI_ADDRCONFIG`, and whether the family the
/// caller asked for is the one that is not configured, which answers with no
/// data. `Err` holds the error to return.
fn addrconfig(family: c_int) -> Result<(c_int, bool), c_int> {
    let each = [
        (
            AF_INET,
            AF_INET6,
            SockaddrIn6::v4([127, 0, 0, 1], 65535),
            SOCKADDR_IN_LEN,
        ),
        (
            AF_INET6,
            AF_INET,
            SockaddrIn6::v6(LOOPBACK6, 65535, 0),
            SOCKADDR_IN6_LEN,
        ),
    ];
    let mut family = family;
    let mut no_family = false;
    for (af, other, address, len) in each {
        if family == other || configured(af, &address, len)? {
            continue;
        }
        if family == af {
            no_family = true;
        }
        family = other;
    }
    Ok((family, no_family))
}

/// What a hint asks for: the family, flags, protocol and socket type.
#[derive(Debug, Clone, Copy, Default)]
struct Hint {
    family: c_int,
    flags: c_int,
    proto: c_int,
    socktype: c_int,
}

/// Reads `hint`, or the defaults if it is null. `Err` holds the error a hint
/// that asks for something impossible gives.
///
/// # Safety
///
/// `hint` must be null or point to a readable `struct addrinfo`.
unsafe fn read_hint(hint: *const Addrinfo) -> Result<Hint, c_int> {
    if hint.is_null() {
        return Ok(Hint {
            family: AF_UNSPEC,
            ..Hint::default()
        });
    }
    // SAFETY: the caller passes a readable hint.
    let hint = unsafe { hint.read() };
    if hint.ai_flags & HINT_FLAGS != hint.ai_flags {
        return Err(EAI_BADFLAGS);
    }
    if hint.ai_family != AF_INET && hint.ai_family != AF_INET6 && hint.ai_family != AF_UNSPEC {
        return Err(EAI_FAMILY);
    }
    Ok(Hint {
        family: hint.ai_family,
        flags: hint.ai_flags,
        proto: hint.ai_protocol,
        socktype: hint.ai_socktype,
    })
}

/// Fills the `nais` results at `out` with every pairing of the `naddrs`
/// addresses and the `nservs` ports, naming `canon` as the canonical name,
/// and links them into a list.
///
/// # Safety
///
/// `out` must be an allocation of `nais` zeroed [`Aibuf`], where `nais` is
/// `naddrs * nservs`.
unsafe fn fill(
    out: *mut Aibuf,
    addrs: &[Address; MAXADDRS],
    naddrs: usize,
    ports: &[Service; MAXSERVS],
    nservs: usize,
    canon: *mut c_char,
) {
    let mut k = 0;
    for i in 0..naddrs {
        let address = addrs.get(i).copied().unwrap_or_default();
        let v4 = address.family == AF_INET;
        for j in 0..nservs {
            let service = ports.get(j).copied().unwrap_or_default();
            let entry = out.wrapping_add(k);
            let port = service.port.to_be();
            let buf = Aibuf {
                ai: Addrinfo {
                    ai_flags: 0,
                    ai_family: address.family,
                    ai_socktype: c_int::from(service.socktype),
                    ai_protocol: c_int::from(service.proto),
                    ai_addrlen: if v4 {
                        SOCKADDR_IN_LEN
                    } else {
                        SOCKADDR_IN6_LEN
                    },
                    ai_addr: entry.wrapping_byte_add(offset_of!(Aibuf, sa)).cast(),
                    ai_canonname: canon,
                    ai_next: null_mut(),
                },
                sa: if v4 {
                    SockaddrIn6::v4(first4(&address.addr), port)
                } else {
                    SockaddrIn6::v6(address.addr, port, address.scopeid)
                },
                lock: 0,
                slot: k as i16,
                refs: AtomicI16::new(0),
            };
            // SAFETY: `entry` is the `k`th of the allocation's results.
            unsafe { entry.write(buf) };
            k += 1;
        }
    }
    for index in 1..k {
        let previous = out.wrapping_add(index - 1);
        let current = out.wrapping_add(index).cast::<Addrinfo>();
        // SAFETY: `previous` is a result of the allocation.
        let next = unsafe { &raw mut (*previous).ai.ai_next };
        // SAFETY: the field is inside that result.
        unsafe { next.write(current) };
    }
}

/// Resolves `host` and `serv` through `src` into a list of addresses to
/// connect or bind to, as musl's `getaddrinfo` does. Returns 0 with the list
/// at `*res`, or an `EAI_` error.
///
/// # Safety
///
/// `host` and `serv` must be null or NUL-terminated strings, `hint` null or a
/// readable `struct addrinfo`, and `res` valid for writes of a pointer.
pub unsafe fn getaddrinfo_from(
    src: &Sources<'_>,
    host: *const c_char,
    serv: *const c_char,
    hint: *const Addrinfo,
    res: *mut *mut Addrinfo,
) -> c_int {
    if host.is_null() && serv.is_null() {
        return EAI_NONAME;
    }
    // SAFETY: the caller passes a readable hint or null.
    let hint = match unsafe { read_hint(hint) } {
        Ok(hint) => hint,
        Err(error) => return error,
    };
    let (family, no_family) = if hint.flags & AI_ADDRCONFIG == 0 {
        (hint.family, false)
    } else {
        match addrconfig(hint.family) {
            Ok(found) => found,
            Err(error) => return error,
        }
    };

    let mut ports = [Service::default(); MAXSERVS];
    let service = if serv.is_null() {
        None
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        Some(unsafe { c_bytes(serv) })
    };
    let nservs = lookup_serv(
        src,
        &mut ports,
        service,
        hint.proto,
        hint.socktype,
        hint.flags,
    );
    if nservs < 0 {
        return nservs;
    }

    let mut addrs = [Address::default(); MAXADDRS];
    let mut canon = [0u8; CANON];
    let name = if host.is_null() {
        None
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        Some(unsafe { c_bytes_max(host, 255) })
    };
    let naddrs = lookup_name(src, &mut addrs, &mut canon, name, family, hint.flags);
    if naddrs < 0 {
        return naddrs;
    }
    if no_family {
        return EAI_NODATA;
    }

    let (nservs, naddrs) = (nservs as usize, naddrs as usize);
    let nais = nservs * naddrs;
    let canon_len = c_len(&canon);
    let out = calloc(1, nais * size_of::<Aibuf>() + canon_len + 1).cast::<Aibuf>();
    if out.is_null() {
        return EAI_MEMORY;
    }
    // The canonical name goes after the results, in the same allocation.
    let outcanon = if canon_len == 0 {
        null_mut()
    } else {
        let text = out.wrapping_add(nais).cast::<c_char>();
        // SAFETY: the allocation has `canon_len + 1` bytes past the results.
        unsafe { copy_out(text, &canon, canon_len) };
        text
    };
    // SAFETY: the allocation holds `nais` zeroed results.
    unsafe { fill(out, &addrs, naddrs, &ports, nservs, outcanon) };
    // SAFETY: `out` is the allocation's first result.
    let refs = unsafe { &(*out).refs };
    refs.store(nais as i16, Ordering::Relaxed);
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(out.cast::<Addrinfo>()) };
    0
}

/// Resolves `host` and `serv` into a list of addresses to connect or bind to.
/// Returns 0 with the list at `*res`, or an `EAI_` error.
///
/// # Safety
///
/// `host` and `serv` must be null or NUL-terminated strings, `hint` null or a
/// readable `struct addrinfo`, and `res` valid for writes of a pointer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getaddrinfo(
    host: *const c_char,
    serv: *const c_char,
    hint: *const Addrinfo,
    res: *mut *mut Addrinfo,
) -> c_int {
    // SAFETY: the caller's contract is `getaddrinfo_from`'s.
    unsafe { getaddrinfo_from(&SYSTEM, host, serv, hint, res) }
}

/// Frees what `getaddrinfo` returned. `p` may be any entry of the list: the
/// allocation goes back once every entry of it has been freed. A null pointer
/// does nothing, where musl dereferences it.
///
/// # Safety
///
/// `p` must be null or a list `getaddrinfo` returned, no entry of which has
/// been freed already.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn freeaddrinfo(p: *mut Addrinfo) {
    if p.is_null() {
        return;
    }
    let mut last = p;
    let mut cnt: i16 = 1;
    loop {
        // SAFETY: the caller passes an entry of a list `getaddrinfo` built.
        let next = unsafe { (*last).ai_next };
        if next.is_null() {
            break;
        }
        last = next;
        cnt = cnt.wrapping_add(1);
    }
    let entry = last.cast::<Aibuf>();
    // SAFETY: the last entry is one of the allocation's results.
    let slot = unsafe { (*entry).slot };
    let base = entry.wrapping_offset(-isize::from(slot));
    // SAFETY: `base` is the allocation's first result, which holds the count.
    let refs = unsafe { &(*base).refs };
    if refs.fetch_sub(cnt, Ordering::AcqRel) == cnt {
        // SAFETY: no entry of the allocation is held any more, and it came
        // from `calloc`.
        unsafe { free(base.cast()) };
    }
}

// ---------------------------------------------------------------------------
// getnameinfo
// ---------------------------------------------------------------------------

/// The address `getnameinfo` was given.
#[derive(Debug, Clone, Copy)]
struct Target {
    /// `AF_INET` or `AF_INET6`.
    af: c_int,
    /// The address: the first four bytes for `AF_INET`, all sixteen for
    /// `AF_INET6`.
    addr: [u8; 16],
    /// The scope, or 0.
    scopeid: u32,
    /// The port, in network byte order.
    port: u16,
}

impl Target {
    /// The address in its IPv6 form, which is what the hosts file is
    /// searched with.
    fn as_v6(&self) -> [u8; 16] {
        if self.af == AF_INET {
            mapped(first4(&self.addr))
        } else {
            self.addr
        }
    }
}

/// Writes `ip` reversed under `in-addr.arpa`, as musl's `mkptr4` does.
fn mkptr4(out: &mut [u8], ip: [u8; 4]) {
    let mut len = 0;
    for &byte in ip.iter().rev() {
        decimal(out, &mut len, u32::from(byte));
        put(out, &mut len, b'.');
    }
    write_bytes(out, &mut len, b"in-addr.arpa");
    put(out, &mut len, 0);
}

/// Writes `ip`'s nibbles reversed under `ip6.arpa`, as musl's `mkptr6` does.
fn mkptr6(out: &mut [u8], ip: &[u8; 16]) {
    let xdigits = b"0123456789abcdef";
    let nibble = |value: u8| xdigits.get(usize::from(value)).copied().unwrap_or(b'0');
    let mut len = 0;
    for &byte in ip.iter().rev() {
        put(out, &mut len, nibble(byte & 15));
        put(out, &mut len, b'.');
        put(out, &mut len, nibble(byte >> 4));
        put(out, &mut len, b'.');
    }
    write_bytes(out, &mut len, b"ip6.arpa");
    put(out, &mut len, 0);
}

/// The reverse name to ask DNS for `target`, as musl builds it: an
/// IPv4-mapped IPv6 address is asked for under `in-addr.arpa`.
fn reverse_name(target: &Target) -> [u8; PTR_MAX] {
    let mut out = [0u8; PTR_MAX];
    let v4 = first4(&target.addr);
    if target.af == AF_INET {
        mkptr4(&mut out, v4);
    } else if target.addr.get(..12) == Some(&V4MAPPED[..]) {
        let [.., a, b, c, d] = target.addr;
        mkptr4(&mut out, [a, b, c, d]);
    } else {
        mkptr6(&mut out, &target.addr);
    }
    out
}

/// The name the hosts file gives `target`, as musl's `reverse_hosts` finds
/// it: the first line whose address is `target`'s names it. `buf` is left as
/// it was when there is none.
fn reverse_hosts(src: &Sources<'_>, buf: &mut [u8; 256], target: &Target) {
    let Ok(Some(mut db)) = Database::open(src.hosts) else {
        return;
    };
    let want = target.as_v6();
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
        let text = line.get(..c_len(&line)).unwrap_or_default();
        if ipliteral(&mut iplit, text, AF_UNSPEC) <= 0 {
            continue;
        }
        let (found, scope) = if iplit.family == AF_INET {
            (mapped(first4(&iplit.addr)), 0)
        } else {
            (iplit.addr, iplit.scopeid)
        };
        if found != want || scope != target.scopeid {
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
        if z - p < buf.len() {
            copy_c(buf, &line, p);
            break;
        }
    }
}

/// The name DNS gives for the reverse name `name`, as musl's `getnameinfo`
/// asks for it: one `PTR` query, whose answer's last `PTR` record names it.
/// `buf` is left as it was when there is none.
fn reverse_dns(src: &Sources<'_>, buf: &mut [u8; 256], name: &[u8]) {
    let Some((mut query, qlen)) = dns::mkquery(0, name, 1, RR_PTR, dns::query_id()) else {
        return;
    };
    // No need for the AD flag.
    set(&mut query, 3, 0);
    let mut reply = [0u8; 512];
    let rlen = resolver::res_send_from(src, query.get(..qlen).unwrap_or_default(), &mut reply);
    let Ok(rlen) = usize::try_from(rlen) else {
        return;
    };
    let packet = reply.get(..rlen.min(reply.len())).unwrap_or_default();
    let _ = dns::parse(packet, |rr, data, _| {
        if rr == RR_PTR && dns::expand(packet, data, buf).is_none() {
            set(buf, 0, 0);
        }
        0
    });
}

/// The `%scope` an IPv6 address's text ends with, and its length: the
/// interface's name for a link-local address unless `NI_NUMERICSCOPE`
/// forbids it, and the number otherwise.
fn scope_suffix(scopeid: u32, addr: &[u8; 16], flags: c_int) -> ([u8; 24], usize) {
    let mut out = [0u8; 24];
    let mut len = 0;
    put(&mut out, &mut len, b'%');
    let mut name = [0u8; IF_NAMESIZE + 1];
    let by_name =
        flags & NI_NUMERICSCOPE == 0 && (is_linklocal(addr) || is_mc_linklocal(addr)) && {
            // SAFETY: `name` has `IF_NAMESIZE` writable bytes and one more, which
            // is never written and keeps the name NUL-terminated.
            !unsafe { if_indextoname(scopeid, name.as_mut_ptr().cast()) }.is_null()
        };
    if by_name {
        let text = name.get(..c_len(&name)).unwrap_or_default();
        write_bytes(&mut out, &mut len, text);
    } else {
        decimal(&mut out, &mut len, scopeid);
    }
    (out, len)
}

/// Writes `target`'s address as text into `buf`, with its scope, as musl's
/// `getnameinfo` does when no name is found.
fn numeric_host(buf: &mut [u8; 256], target: &Target, flags: c_int) {
    // SAFETY: `addr` holds 16 bytes, more than either family reads, and `buf`
    // has room for `INET6_ADDRSTRLEN`.
    let _ = unsafe {
        inet_ntop(
            target.af,
            target.addr.as_ptr().cast(),
            buf.as_mut_ptr().cast(),
            INET6_ADDRSTRLEN,
        )
    };
    if target.scopeid == 0 {
        return;
    }
    let (suffix, suffix_len) = scope_suffix(target.scopeid, &target.addr, flags);
    let mut len = c_len(buf);
    write_bytes(buf, &mut len, suffix.get(..suffix_len).unwrap_or_default());
    put(buf, &mut len, 0);
}

/// The name the services file gives port `port`, as musl's
/// `reverse_services` finds it. `buf` is left as it was when there is none.
fn reverse_services(src: &Sources<'_>, buf: &mut [u8; 64], port: u16, dgram: bool) {
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
        let wanted: &[u8] = if dgram { b"/udp" } else { b"/tcp" };
        if !super::has_at(&line, z, wanted) || p > buf.len() {
            continue;
        }
        copy_c(buf, &line, 0);
        break;
    }
}

/// Names the address at `sa` and its port through `src`, as musl's
/// `getnameinfo` does. Returns 0, or an `EAI_` error.
///
/// # Safety
///
/// `sa` must point to a readable `struct sockaddr` of `sl` bytes, `node` be
/// null or valid for writes of `nodelen` bytes, and `serv` null or valid for
/// writes of `servlen`.
#[expect(
    clippy::too_many_arguments,
    reason = "AUDIT: the argument list is `getnameinfo`'s, which C fixes"
)]
pub unsafe fn getnameinfo_from(
    src: &Sources<'_>,
    sa: *const c_void,
    sl: c_uint,
    node: *mut c_char,
    nodelen: c_uint,
    serv: *mut c_char,
    servlen: c_uint,
    flags: c_int,
) -> c_int {
    // SAFETY: the caller passes a `struct sockaddr`, whose family is first.
    let af = c_int::from(unsafe { sa.cast::<u16>().read_unaligned() });
    let target = match af {
        AF_INET if sl >= SOCKADDR_IN_LEN => {
            // SAFETY: the caller passes at least `sizeof(struct sockaddr_in)`.
            let sin = unsafe { sa.cast::<SockaddrIn>().read_unaligned() };
            let [a, b, c, d] = sin.sin_addr;
            Target {
                af,
                addr: [a, b, c, d, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                scopeid: 0,
                port: sin.sin_port,
            }
        }
        AF_INET6 if sl >= SOCKADDR_IN6_LEN => {
            // SAFETY: the caller passes at least `sizeof(struct sockaddr_in6)`.
            let sin6 = unsafe { sa.cast::<SockaddrIn6>().read_unaligned() };
            Target {
                af,
                addr: sin6.sin6_addr,
                scopeid: sin6.sin6_scope_id,
                port: sin6.sin6_port,
            }
        }
        _ => return EAI_FAMILY,
    };

    if !node.is_null() && nodelen != 0 {
        let mut buf = [0u8; 256];
        if flags & NI_NUMERICHOST == 0 {
            reverse_hosts(src, &mut buf, &target);
            if at(&buf, 0) == 0 {
                reverse_dns(src, &mut buf, &reverse_name(&target));
            }
        }
        if at(&buf, 0) == 0 {
            if flags & NI_NAMEREQD != 0 {
                return EAI_NONAME;
            }
            numeric_host(&mut buf, &target, flags);
        }
        let len = c_len(&buf);
        if len >= nodelen as usize {
            return EAI_OVERFLOW;
        }
        // SAFETY: the caller passes `nodelen` writable bytes, more than `len`.
        unsafe { copy_out(node, &buf, len) };
    }

    if !serv.is_null() && servlen != 0 {
        let mut buf = [0u8; 64];
        let port = u16::from_be(target.port);
        if flags & NI_NUMERICSERV == 0 {
            reverse_services(src, &mut buf, port, flags & NI_DGRAM != 0);
        }
        if at(&buf, 0) == 0 {
            let mut len = 0;
            decimal(&mut buf, &mut len, u32::from(port));
        }
        let len = c_len(&buf);
        if len >= servlen as usize {
            return EAI_OVERFLOW;
        }
        // SAFETY: the caller passes `servlen` writable bytes, more than `len`.
        unsafe { copy_out(serv, &buf, len) };
    }
    0
}

/// Names the address at `sa` and its port. Returns 0, or an `EAI_` error.
///
/// # Safety
///
/// As [`getnameinfo_from`].
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
    // SAFETY: the caller's contract is `getnameinfo_from`'s.
    unsafe { getnameinfo_from(&SYSTEM, sa, sl, node, nodelen, serv, servlen, flags) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netdb::testing::{Paths, Record, Reply, Responder};
    use crate::netdb::{AI_NUMERICSERV, EAI_SERVICE, SOCK_STREAM};

    /// One result, read out of the list.
    #[derive(Debug, PartialEq, Eq)]
    struct Result {
        family: c_int,
        socktype: c_int,
        protocol: c_int,
        addrlen: c_uint,
        addr: SockaddrIn6,
        canon: Option<String>,
    }

    /// Resolves through `src`, and returns the code and the whole list.
    fn resolve(
        src: &Sources<'_>,
        host: Option<&core::ffi::CStr>,
        serv: Option<&core::ffi::CStr>,
        hint: Option<Addrinfo>,
    ) -> (c_int, Vec<Result>) {
        let mut list: *mut Addrinfo = null_mut();
        let hint_ptr = hint
            .as_ref()
            .map_or(core::ptr::null(), |hint| &raw const *hint);
        let host = host.map_or(core::ptr::null(), core::ffi::CStr::as_ptr);
        let serv = serv.map_or(core::ptr::null(), core::ffi::CStr::as_ptr);
        // SAFETY: the strings are NUL-terminated or null, the hint is a live
        // local or null, and `list` is a live local.
        let code = unsafe { getaddrinfo_from(src, host, serv, hint_ptr, &raw mut list) };
        let mut found = Vec::new();
        let mut entry = list;
        while !entry.is_null() {
            // SAFETY: the entry is one `getaddrinfo` returned.
            let ai = unsafe { entry.read() };
            // SAFETY: `ai_addr` points at the entry's own address.
            let addr = unsafe { ai.ai_addr.cast::<SockaddrIn6>().read() };
            let canon = (!ai.ai_canonname.is_null()).then(|| {
                // SAFETY: the name is NUL-terminated.
                unsafe { core::ffi::CStr::from_ptr(ai.ai_canonname) }
                    .to_string_lossy()
                    .into_owned()
            });
            found.push(Result {
                family: ai.ai_family,
                socktype: ai.ai_socktype,
                protocol: ai.ai_protocol,
                addrlen: ai.ai_addrlen,
                addr,
                canon,
            });
            entry = ai.ai_next;
        }
        if code == 0 {
            // SAFETY: the list is one `getaddrinfo` returned, freed once.
            unsafe { freeaddrinfo(list) };
        }
        (code, found)
    }

    /// Names `addr` through `src`.
    fn name(
        src: &Sources<'_>,
        addr: &SockaddrIn6,
        len: c_uint,
        flags: c_int,
    ) -> (c_int, String, String) {
        let mut node = [0 as c_char; 300];
        let mut serv = [0 as c_char; 64];
        // SAFETY: `addr` is a live local of at least `len` bytes, and the two
        // buffers have the lengths passed.
        let code = unsafe {
            getnameinfo_from(
                src,
                addr.as_ptr(),
                len,
                node.as_mut_ptr(),
                300,
                serv.as_mut_ptr(),
                64,
                flags,
            )
        };
        let read = |buf: &[c_char]| {
            // SAFETY: both buffers start zeroed and are NUL-terminated.
            unsafe { core::ffi::CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        (code, read(&node), read(&serv))
    }

    fn v6(groups: [u16; 8]) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (pair, group) in out.chunks_exact_mut(2).zip(groups) {
            pair.copy_from_slice(&group.to_be_bytes());
        }
        out
    }

    #[test]
    fn a_numeric_address_and_a_numeric_port_need_no_files() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let hint = Addrinfo {
            ai_flags: AI_NUMERICHOST | AI_NUMERICSERV,
            ai_family: AF_UNSPEC,
            ai_socktype: SOCK_STREAM,
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_addr: null_mut(),
            ai_canonname: null_mut(),
            ai_next: null_mut(),
        };
        let (code, found) = resolve(&src, Some(c"192.0.2.1"), Some(c"80"), Some(hint));
        assert_eq!((code, found.len()), (0, 1));
        assert_eq!(
            found.first().map(|result| (
                result.family,
                result.socktype,
                result.addrlen,
                result.addr
            )),
            Some((
                AF_INET,
                SOCK_STREAM,
                SOCKADDR_IN_LEN,
                SockaddrIn6::v4([192, 0, 2, 1], 80u16.to_be())
            ))
        );

        // Without a socket type, each port comes back twice.
        let (code, found) = resolve(&src, Some(c"2001:db8::1"), Some(c"443"), None);
        assert_eq!((code, found.len()), (0, 2));
        assert_eq!(
            found.first().map(|result| (result.family, result.addrlen)),
            Some((AF_INET6, SOCKADDR_IN6_LEN))
        );
        assert!(
            found
                .iter()
                .all(|result| result.addr.sin6_port == 443u16.to_be())
        );
        assert!(
            found
                .iter()
                .all(|result| result.addr.sin6_addr == v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]))
        );
        assert_eq!(
            found
                .iter()
                .map(|result| result.protocol)
                .collect::<Vec<_>>(),
            [6, 17]
        );
    }

    #[test]
    fn the_arguments_are_checked_as_musl_checks_them() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        assert_eq!(resolve(&src, None, None, None).0, EAI_NONAME);
        let bad_flags = Addrinfo {
            ai_flags: 0x8000,
            ai_family: AF_UNSPEC,
            ai_socktype: 0,
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_addr: null_mut(),
            ai_canonname: null_mut(),
            ai_next: null_mut(),
        };
        assert_eq!(
            resolve(&src, Some(c"127.0.0.1"), None, Some(bad_flags)).0,
            EAI_BADFLAGS
        );
        let bad_family = Addrinfo {
            ai_flags: 0,
            ai_family: 99,
            ..bad_flags
        };
        assert_eq!(
            resolve(&src, Some(c"127.0.0.1"), None, Some(bad_family)).0,
            EAI_FAMILY
        );
        // A name that is not numeric, with `AI_NUMERICHOST`.
        let numeric = Addrinfo {
            ai_flags: AI_NUMERICHOST,
            ai_family: AF_UNSPEC,
            ..bad_flags
        };
        assert_eq!(
            resolve(&src, Some(c"nothing.example.test"), None, Some(numeric)).0,
            EAI_NONAME
        );
        // A service name with `AI_NUMERICSERV`.
        let numeric_serv = Addrinfo {
            ai_flags: AI_NUMERICSERV,
            ai_family: AF_UNSPEC,
            ..bad_flags
        };
        assert_eq!(
            resolve(&src, Some(c"127.0.0.1"), Some(c"http"), Some(numeric_serv)).0,
            EAI_NONAME
        );
        assert_eq!(
            resolve(&src, Some(c"127.0.0.1"), Some(c"nosuchservice"), None).0,
            EAI_SERVICE
        );
        // `AI_ADDRCONFIG` on the loopback interface leaves both families.
        let configured = Addrinfo {
            ai_flags: AI_ADDRCONFIG,
            ai_family: AF_INET,
            ..bad_flags
        };
        let (code, found) = resolve(&src, Some(c"127.0.0.1"), Some(c"80"), Some(configured));
        assert_eq!((code, found.len()), (0, 2));
    }

    #[test]
    fn the_hosts_file_names_every_address_and_the_list_frees_from_any_entry() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let (code, found) = resolve(&src, Some(c"multi.example.test"), Some(c"http"), None);
        assert_eq!(code, 0);
        // Three addresses, two ports each.
        assert_eq!(found.len(), 6);
        assert!(
            found
                .iter()
                .all(|result| result.canon.as_deref() == Some("multi.example.test"))
        );
        assert!(
            found
                .iter()
                .all(|result| result.addr.sin6_port == 8080u16.to_be())
        );

        // Freeing from the second entry frees the whole allocation: the two
        // halves are freed separately, and the count makes the last free it.
        let mut list: *mut Addrinfo = null_mut();
        // SAFETY: the name is NUL-terminated and `list` is a live local.
        let code = unsafe {
            getaddrinfo_from(
                &src,
                c"multi.example.test".as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                &raw mut list,
            )
        };
        assert_eq!(code, 0);
        // SAFETY: the list has at least two entries.
        let second = unsafe { (*list).ai_next };
        assert!(!second.is_null());
        // SAFETY: the first entry is one `getaddrinfo` returned.
        let next = unsafe { &raw mut (*list).ai_next };
        // SAFETY: the list is cut short here, so the two pieces do not
        // overlap.
        unsafe { next.write(null_mut()) };
        // SAFETY: each piece is freed once.
        unsafe { freeaddrinfo(second) };
        // SAFETY: as above; this one frees the allocation.
        unsafe { freeaddrinfo(list) };
        // SAFETY: a null list is nothing to free.
        unsafe { freeaddrinfo(null_mut()) };
    }

    #[test]
    fn dns_answers_getaddrinfo_with_a_canonical_name() {
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
            ai_socktype: SOCK_STREAM,
            ai_protocol: 0,
            ai_addrlen: 0,
            ai_addr: null_mut(),
            ai_canonname: null_mut(),
            ai_next: null_mut(),
        };
        let (code, found) = resolve(&src, Some(c"www"), Some(c"domain"), Some(hint));
        assert_eq!((code, found.len()), (0, 1));
        assert_eq!(
            found.first().map(|result| (
                result.canon.clone(),
                result.addr.v4_addr(),
                result.addr.sin6_port
            )),
            Some((
                Some("real.search.test".into()),
                [198, 51, 100, 7],
                5353u16.to_be()
            ))
        );
    }

    #[test]
    fn getnameinfo_reads_the_hosts_and_services_files() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let addr = SockaddrIn6::v4([192, 0, 2, 10], 8080u16.to_be());
        assert_eq!(
            name(&src, &addr, SOCKADDR_IN_LEN, 0),
            (0, "server.example.test".into(), "http".into())
        );
        // A datagram port of the same number is a different service.
        assert_eq!(name(&src, &addr, SOCKADDR_IN_LEN, NI_DGRAM).2, "http");
        let syslog = SockaddrIn6::v4([192, 0, 2, 10], 514u16.to_be());
        assert_eq!(name(&src, &syslog, SOCKADDR_IN_LEN, NI_DGRAM).2, "syslog");
        // Over TCP there is no such service, so the number stands.
        assert_eq!(name(&src, &syslog, SOCKADDR_IN_LEN, 0).2, "514");
        // Numeric forms ask no file.
        assert_eq!(
            name(
                &src,
                &addr,
                SOCKADDR_IN_LEN,
                NI_NUMERICHOST | NI_NUMERICSERV
            ),
            (0, "192.0.2.10".into(), "8080".into())
        );
        // An IPv6 address the hosts file names.
        let six = SockaddrIn6::v6(v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 6]), 0, 0);
        assert_eq!(name(&src, &six, SOCKADDR_IN6_LEN, 0).1, "six.example.test");
        // One it does not: the text, and its scope.
        let scoped = SockaddrIn6::v6(v6([0xfe80, 0, 0, 0, 0, 0, 0, 1]), 0, 1);
        let (code, node, _) = name(&src, &scoped, SOCKADDR_IN6_LEN, NI_NUMERICHOST);
        assert_eq!(code, 0);
        assert!(node.starts_with("fe80::1%"), "{node}");
        let (_, node, _) = name(
            &src,
            &scoped,
            SOCKADDR_IN6_LEN,
            NI_NUMERICHOST | NI_NUMERICSCOPE,
        );
        assert_eq!(node, "fe80::1%1");
    }

    #[test]
    fn getnameinfo_checks_its_family_its_room_and_its_flags() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let addr = SockaddrIn6::v4([192, 0, 2, 10], 8080u16.to_be());
        // A length short of the family's structure.
        assert_eq!(name(&src, &addr, SOCKADDR_IN_LEN - 1, 0).0, EAI_FAMILY);
        let unknown = SockaddrIn6 {
            sin6_family: 99,
            ..SockaddrIn6::default()
        };
        assert_eq!(name(&src, &unknown, SOCKADDR_IN6_LEN, 0).0, EAI_FAMILY);

        let mut node = [0 as c_char; 4];
        // SAFETY: `addr` is a live local and `node` has four bytes.
        let code = unsafe {
            getnameinfo_from(
                &src,
                addr.as_ptr(),
                SOCKADDR_IN_LEN,
                node.as_mut_ptr(),
                4,
                null_mut(),
                0,
                NI_NUMERICHOST,
            )
        };
        assert_eq!(code, EAI_OVERFLOW);

        // Nothing names 192.0.2.200, and no name server answers on port 9.
        let none = SockaddrIn6::v4([192, 0, 2, 200], 0);
        assert_eq!(
            name(&src, &none, SOCKADDR_IN_LEN, NI_NAMEREQD).0,
            EAI_NONAME
        );
        assert_eq!(name(&src, &none, SOCKADDR_IN_LEN, 0).1, "192.0.2.200");
    }

    #[test]
    fn getnameinfo_asks_dns_when_the_hosts_file_is_silent() {
        let paths = Paths::new("resolv.conf");
        let responder = Responder::start(|name, _| match name {
            "7.100.51.198.in-addr.arpa" => Reply::Records(vec![Record::Ptr("named.search.test")]),
            "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.8.b.d.0.1.0.0.2.ip6.arpa" => {
                Reply::Records(vec![Record::Ptr("six.search.test")])
            }
            _ => Reply::Code(3),
        });
        let src = paths.sources(responder.port);
        let addr = SockaddrIn6::v4([198, 51, 100, 7], 0);
        assert_eq!(name(&src, &addr, SOCKADDR_IN_LEN, 0).1, "named.search.test");
        let six = SockaddrIn6::v6(v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]), 0, 0);
        assert_eq!(name(&src, &six, SOCKADDR_IN6_LEN, 0).1, "six.search.test");
        // An IPv4-mapped address is asked for under `in-addr.arpa`.
        let mapped = SockaddrIn6::v6(mapped([198, 51, 100, 7]), 0, 0);
        assert_eq!(
            name(&src, &mapped, SOCKADDR_IN6_LEN, 0).1,
            "named.search.test"
        );
        // A name server that answers nothing leaves the numeric form.
        let missing = SockaddrIn6::v4([198, 51, 100, 9], 0);
        assert_eq!(name(&src, &missing, SOCKADDR_IN_LEN, 0).1, "198.51.100.9");
    }
}
