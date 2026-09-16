//! Turning a host name into addresses and a service name into ports.
//!
//! Ported from musl 1.2.5's `lookup_name.c`, `lookup_serv.c` and
//! `lookup_ipliteral.c` (MIT). A host name is tried, in order, as no name at
//! all, a numeric address, a `localhost` name, a line of the hosts file, and
//! a DNS name with the search domains of `/etc/resolv.conf`. More than one
//! address of mixed families is then sorted by a subset of RFC 6724's rules,
//! which connect a UDP socket to each address to learn the source address it
//! would use.
//!
//! Two things differ from musl:
//!
//! * A name that is `localhost` or ends in `.localhost`, in any case and with
//!   or without a final dot, gives 127.0.0.1 and ::1 without reading the hosts
//!   file or asking DNS, as RFC 6761 section 6.3 asks of resolver libraries.
//!   musl 1.2.5 looks such names up like any other. The answer does not then
//!   depend on a hosts file being present, which on Ferrix it may not be, and
//!   a lookup of `localhost` never leaves the machine.
//! * When `/etc/resolv.conf` exists but cannot be read, the lookup fails with
//!   `EAI_SYSTEM` and `errno` from `fopen`. musl returns -1 from there, which
//!   is `EAI_BADFLAGS`.

use core::ffi::{c_int, c_uint};

use super::dns::{self, RR_A, RR_AAAA, RR_CNAME};
use super::resolver::{self, ResolvConf};
use super::{
    AF_INET, AF_INET6, AF_UNSPEC, AI_ALL, AI_NUMERICHOST, AI_NUMERICSERV, AI_PASSIVE,
    AI_V4MAPPED, Database, EAI_AGAIN, EAI_FAIL, EAI_NODATA, EAI_NONAME, EAI_SERVICE, EAI_SYSTEM,
    IPPROTO_TCP, IPPROTO_UDP, SOCK_CLOEXEC, SOCK_DGRAM, SOCK_STREAM, SockaddrIn6, Sources,
    V4MAPPED, at, c_len, copy_c, find, has_at, is_linklocal, is_mc_linklocal, is_space, strtoul,
};
use crate::cancel;
use crate::growable::sort_by;
use crate::inet;
use crate::socket::{connect, getsockname, socket};
use crate::unistd::close;

/// The most addresses a lookup gives: a bound on what one 512-byte packet of
/// IPv4 answers and one of IPv6 answers can hold.
pub const MAXADDRS: usize = 48;
/// The most ports a service lookup gives: one for TCP and one for UDP.
pub const MAXSERVS: usize = 2;
/// The size of a canonical name buffer.
pub const CANON: usize = 256;

/// An address a lookup found, as musl's `struct address`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Address {
    /// `AF_INET` or `AF_INET6`.
    pub family: c_int,
    /// The IPv6 scope, or 0.
    pub scopeid: c_uint,
    /// The address: 4 bytes for IPv4, 16 for IPv6.
    pub addr: [u8; 16],
    /// Where RFC 6724 sorts it: higher first.
    pub sortkey: c_int,
}

/// A port a service lookup found, as musl's `struct service`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Service {
    /// The port, in host byte order.
    pub port: u16,
    /// `IPPROTO_TCP` or `IPPROTO_UDP`.
    pub proto: u8,
    /// `SOCK_STREAM` or `SOCK_DGRAM`.
    pub socktype: u8,
}

/// An IPv4 address in the first four bytes of an [`Address`].
fn v4(addr: [u8; 4]) -> Address {
    let [a, b, c, d] = addr;
    Address {
        family: AF_INET,
        scopeid: 0,
        addr: [a, b, c, d, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        sortkey: 0,
    }
}

/// The first four bytes of `addr`.
pub(crate) fn first4(addr: &[u8; 16]) -> [u8; 4] {
    let [a, b, c, d, ..] = *addr;
    [a, b, c, d]
}

/// `addr`, an IPv4 address, mapped into IPv6.
pub(crate) fn mapped(addr: [u8; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (slot, byte) in out.iter_mut().zip(V4MAPPED.into_iter().chain(addr)) {
        *slot = byte;
    }
    out
}

/// Stores `value` at `index` of `buf`, if it is inside.
fn set(buf: &mut [u8], index: usize, value: u8) {
    if let Some(slot) = buf.get_mut(index) {
        *slot = value;
    }
}

/// Parses `name`, without its NUL, as a numeric address, as musl's
/// `__lookup_ipliteral` does: an IPv4 address in any of `inet_aton`'s forms,
/// or an IPv6 address with an optional `%` and a scope, numeric or an
/// interface name for a link-local address. Returns 1 with the address in
/// `out`, 0 if `name` is not numeric, `EAI_NODATA` if it is of the wrong
/// family, or `EAI_NONAME` for a scope that does not exist.
pub fn ipliteral(out: &mut Address, name: &[u8], family: c_int) -> c_int {
    if let Some(addr) = inet::parse_aton(name) {
        if family == AF_INET6 {
            return EAI_NODATA;
        }
        *out = v4(addr);
        return 1;
    }

    let percent = name.iter().position(|&byte| byte == b'%');
    let text = match percent {
        Some(p) if p < 64 => name.get(..p).unwrap_or_default(),
        _ => name,
    };
    let Some(addr) = inet::pton6(text) else {
        return 0;
    };
    if family == AF_INET {
        return EAI_NODATA;
    }
    *out = Address {
        family: AF_INET6,
        scopeid: 0,
        addr,
        sortkey: 0,
    };
    let mut scopeid: u64 = 0;
    if let Some(p) = percent {
        let after = p + 1;
        let z = if at(name, after).is_ascii_digit() {
            let (value, end) = strtoul(name, after, 10);
            scopeid = value;
            end
        } else {
            p
        };
        if at(name, z) != 0 {
            if !is_linklocal(&addr) && !is_mc_linklocal(&addr) {
                return EAI_NONAME;
            }
            let mut interface = [0u8; CANON];
            for (slot, &byte) in interface
                .iter_mut()
                .take(CANON - 1)
                .zip(name.get(after..).unwrap_or_default())
            {
                *slot = byte;
            }
            // SAFETY: the name is NUL-terminated: its last byte is never
            // written.
            scopeid = u64::from(unsafe { inet::if_nametoindex(interface.as_ptr().cast()) });
            if scopeid == 0 {
                return EAI_NONAME;
            }
        }
        if scopeid > u64::from(u32::MAX) {
            return EAI_NONAME;
        }
    }
    out.scopeid = scopeid as u32;
    1
}

/// Whether the C string at the start of `host` is a usable host name, as
/// musl's `is_valid_hostname` judges: 1 to 254 bytes, valid in the current
/// locale's multibyte encoding, of letters, digits, dots, hyphens and bytes
/// from 0x80.
pub fn is_valid_hostname(host: &[u8]) -> bool {
    let len = c_len(host);
    if len == host.len() || len.wrapping_sub(1) >= 254 {
        return false;
    }
    // SAFETY: `host` holds a NUL within its bytes, found above.
    let chars = unsafe {
        crate::multibyte::mbstowcs(core::ptr::null_mut(), host.as_ptr().cast(), 0)
    };
    if chars == usize::MAX {
        return false;
    }
    host.iter()
        .take(len)
        .all(|&byte| byte >= 0x80 || byte == b'.' || byte == b'-' || byte.is_ascii_alphanumeric())
}

/// The addresses of no name: the wildcard addresses for `AI_PASSIVE`, the
/// loopback addresses otherwise.
fn name_from_null(buf: &mut [Address], family: c_int, flags: c_int) -> c_int {
    let mut found = [Address::default(); 2];
    let mut cnt = 0;
    let passive = flags & AI_PASSIVE != 0;
    if family != AF_INET6 {
        found[cnt] = v4(if passive { [0; 4] } else { [127, 0, 0, 1] });
        cnt += 1;
    }
    if family != AF_INET {
        let mut addr = [0u8; 16];
        if !passive {
            addr[15] = 1;
        }
        if let Some(slot) = found.get_mut(cnt) {
            *slot = Address {
                family: AF_INET6,
                addr,
                ..Address::default()
            };
        }
        cnt += 1;
    }
    for (slot, address) in buf.iter_mut().zip(found.iter().take(cnt)) {
        *slot = *address;
    }
    cnt as c_int
}

/// The loopback addresses, for a `localhost` name. See the module's
/// documentation.
fn name_from_localhost(buf: &mut [Address], name: &[u8], family: c_int) -> c_int {
    let name = name.strip_suffix(b".").unwrap_or(name);
    let suffix = b".localhost";
    let local = name.eq_ignore_ascii_case(b"localhost")
        || (name.len() > suffix.len()
            && name
                .get(name.len() - suffix.len()..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix)));
    if !local {
        return 0;
    }
    let mut loopback6 = [0u8; 16];
    loopback6[15] = 1;
    name_from_null(buf, family, 0) + 0 * loopback6.len() as c_int
}

/// The addresses the hosts file gives `name`, and its canonical name, as
/// musl's `name_from_hosts` finds them: each line that holds `name` between
/// white space contributes its address, and the first such line with a valid
/// name gives the canonical name.
fn name_from_hosts(
    src: &Sources<'_>,
    buf: &mut [Address; MAXADDRS],
    canon: &mut [u8; CANON],
    name: &[u8],
    family: c_int,
) -> c_int {
    let mut db = match Database::open(src.hosts) {
        Ok(Some(db)) => db,
        Ok(None) => return 0,
        Err(error) => {
            crate::errno::set(error);
            return EAI_SYSTEM;
        }
    };
    let l = name.len();
    let mut cnt = 0;
    let mut badfam = 0;
    let mut have_canon = false;
    let mut line = [0u8; 512];
    while let Some(len) = db.line(&mut line) {
        if cnt >= MAXADDRS {
            break;
        }
        if let Some(hash) = line.iter().take(len).position(|&byte| byte == b'#') {
            set(&mut line, hash, b'\n');
            set(&mut line, hash + 1, 0);
        }
        let mut from = 1;
        let found = loop {
            match find(&line, from, name) {
                Some(p) if !is_space(at(&line, p - 1)) || !is_space(at(&line, p + l)) => {
                    from = p + 1;
                }
                other => break other,
            }
        };
        if found.is_none() {
            continue;
        }

        let mut p = 0;
        while at(&line, p) != 0 && !is_space(at(&line, p)) {
            p += 1;
        }
        set(&mut line, p, 0);
        p += 1;
        let address = line.get(..c_len(&line)).unwrap_or_default();
        let Some(slot) = buf.get_mut(cnt) else {
            break;
        };
        match ipliteral(slot, address, family) {
            1 => cnt += 1,
            0 => continue,
            _ => badfam = EAI_NODATA,
        }

        if have_canon {
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
        if is_valid_hostname(line.get(p..).unwrap_or_default()) {
            have_canon = true;
            copy_c(canon, &line, p);
        }
    }
    if cnt > 0 { cnt as c_int } else { badfam }
}

/// Where a DNS answer's records go.
#[derive(Debug)]
struct Answers<'a> {
    addrs: &'a mut [Address; MAXADDRS],
    canon: &'a mut [u8; CANON],
    cnt: usize,
    rrtype: c_int,
}

impl Answers<'_> {
    /// Takes one record of the response `packet`, as musl's
    /// `dns_parse_callback` does: a CNAME renames the result, and an address
    /// of the type asked for is kept.
    fn take(&mut self, packet: &[u8], rr: c_int, data: usize, len: usize) -> c_int {
        if rr == RR_CNAME {
            let mut tmp = [0u8; CANON];
            if dns::expand(packet, data, &mut tmp).is_some() && is_valid_hostname(&tmp) {
                copy_c(self.canon, &tmp, 0);
            }
            return 0;
        }
        if self.cnt >= MAXADDRS || rr != self.rrtype {
            return 0;
        }
        let family = match rr {
            RR_A if len == 4 => AF_INET,
            RR_AAAA if len == 16 => AF_INET6,
            RR_A | RR_AAAA => return -1,
            _ => return 0,
        };
        let mut address = Address {
            family,
            ..Address::default()
        };
        for (slot, &byte) in address
            .addr
            .iter_mut()
            .zip(packet.get(data..data + len).unwrap_or_default())
        {
            *slot = byte;
        }
        if let Some(slot) = self.addrs.get_mut(self.cnt) {
            *slot = address;
            self.cnt += 1;
        }
        0
    }
}

/// The size of each DNS answer buffer.
const ABUF_SIZE: usize = 4800;

/// Asks DNS for `name`'s addresses of `family`, as musl's `name_from_dns`
/// does: an A query unless only IPv6 is wanted and an AAAA query unless only
/// IPv4 is, sent together.
fn name_from_dns(
    src: &Sources<'_>,
    buf: &mut [Address; MAXADDRS],
    canon: &mut [u8; CANON],
    name: &[u8],
    family: c_int,
    conf: &ResolvConf,
) -> c_int {
    let mut qbuf = [[0u8; dns::QUERY_MAX]; 2];
    let mut qlens = [0usize; 2];
    let mut qtypes = [0; 2];
    let mut nq = 0;
    for (af, rr) in [(AF_INET6, RR_A), (AF_INET, RR_AAAA)] {
        if family == af {
            continue;
        }
        let Some((mut query, len)) = dns::mkquery(0, name, 1, rr, dns::query_id()) else {
            return 0;
        };
        // No need for the AD flag.
        set(&mut query, 3, 0);
        // Keep the ids distinct.
        if nq > 0 && at(&query, 0) == at(&qbuf[0], 0) {
            set(&mut query, 0, at(&query, 0).wrapping_add(1));
        }
        if let (Some(q), Some(l), Some(t)) =
            (qbuf.get_mut(nq), qlens.get_mut(nq), qtypes.get_mut(nq))
        {
            *q = query;
            *l = len;
            *t = rr;
            nq += 1;
        }
    }

    let [q0, q1] = &qbuf;
    let [l0, l1] = qlens;
    let queries: [&[u8]; 2] = [q0.get(..l0).unwrap_or_default(), q1.get(..l1).unwrap_or_default()];
    let mut abuf = [[0u8; ABUF_SIZE]; 2];
    let [a0, a1] = &mut abuf;
    let mut answers: [&mut [u8]; 2] = [a0, a1];
    let mut alens = [0isize; 2];
    let _ = src;
    if resolver::msend_rc(
        queries.get(..nq).unwrap_or_default(),
        answers.get_mut(..nq).unwrap_or_default(),
        &mut alens,
        conf,
    )
    .is_err()
    {
        return EAI_SYSTEM;
    }

    for i in 0..nq {
        let answer = answers.get(i).map_or(&[][..], |answer| &answer[..]);
        let rcode = at(answer, 3) & 15;
        if alens.get(i).copied().unwrap_or(0) < 4 || rcode == 2 {
            return EAI_AGAIN;
        }
        if rcode == 3 {
            return 0;
        }
        if rcode != 0 {
            return EAI_FAIL;
        }
    }

    let mut ctx = Answers {
        addrs: buf,
        canon,
        cnt: 0,
        rrtype: 0,
    };
    for i in (0..nq).rev() {
        ctx.rrtype = qtypes.get(i).copied().unwrap_or(0);
        let len = usize::try_from(alens.get(i).copied().unwrap_or(0))
            .unwrap_or(0)
            .min(ABUF_SIZE);
        let packet = answers
            .get(i)
            .and_then(|answer| answer.get(..len))
            .unwrap_or_default();
        let _ = dns::parse(packet, |rr, data, len| ctx.take(packet, rr, data, len));
    }
    if ctx.cnt > 0 {
        ctx.cnt as c_int
    } else {
        EAI_NODATA
    }
}

/// Asks DNS for `name`, first with each search domain appended when it has
/// fewer dots than `ndots` and no final dot, as musl's `name_from_dns_search`
/// does. The name asked for becomes the canonical name unless an answer
/// renames it.
fn name_from_dns_search(
    src: &Sources<'_>,
    buf: &mut [Address; MAXADDRS],
    canon: &mut [u8; CANON],
    name: &[u8],
    family: c_int,
) -> c_int {
    let mut search = [0u8; CANON];
    let conf = match resolver::get_resolv_conf(src, Some(&mut search)) {
        Ok(conf) => conf,
        Err(error) => {
            crate::errno::set(error);
            return EAI_SYSTEM;
        }
    };

    let mut l = name.len();
    let dots = name.iter().filter(|&&byte| byte == b'.').count();
    let last_dot = l > 0 && at(name, l - 1) == b'.';
    if dots >= conf.ndots as usize || last_dot {
        search[0] = 0;
    }
    if last_dot {
        l -= 1;
    }
    if l == 0 || at(name, l - 1) == b'.' || l >= CANON {
        return EAI_NONAME;
    }

    for (slot, &byte) in canon.iter_mut().zip(name.iter().take(l)) {
        *slot = byte;
    }
    set(canon, l, b'.');

    let mut p = 0;
    loop {
        while is_space(at(&search, p)) {
            p += 1;
        }
        let mut z = p;
        while at(&search, z) != 0 && !is_space(at(&search, z)) {
            z += 1;
        }
        if z == p {
            break;
        }
        if z - p < CANON - l - 1 {
            for (offset, index) in (p..z).enumerate() {
                set(canon, l + 1 + offset, at(&search, index));
            }
            set(canon, z - p + 1 + l, 0);
            let mut full = [0u8; CANON];
            copy_c(&mut full, canon, 0);
            let full = full.get(..c_len(&full)).unwrap_or_default();
            let cnt = name_from_dns(src, buf, canon, full, family, &conf);
            if cnt != 0 {
                return cnt;
            }
        }
        p = z;
    }

    set(canon, l, 0);
    name_from_dns(src, buf, canon, name, family, &conf)
}

/// A row of RFC 6724's policy table, as musl's `struct policy`.
#[derive(Debug, Clone, Copy)]
struct Policy {
    addr: [u8; 16],
    len: usize,
    mask: u8,
    prec: c_int,
    label: c_int,
}

/// musl's default policy table. The last row matches every address.
const DEFPOLICY: [Policy; 6] = [
    Policy {
        addr: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        len: 15,
        mask: 0xff,
        prec: 50,
        label: 0,
    },
    Policy {
        addr: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0],
        len: 11,
        mask: 0xff,
        prec: 35,
        label: 4,
    },
    Policy {
        addr: [0x20, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        len: 1,
        mask: 0xff,
        prec: 30,
        label: 2,
    },
    Policy {
        addr: [0x20, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        len: 3,
        mask: 0xff,
        prec: 5,
        label: 5,
    },
    Policy {
        addr: [0xfc, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        len: 0,
        mask: 0xfe,
        prec: 3,
        label: 13,
    },
    Policy {
        addr: [0; 16],
        len: 0,
        mask: 0,
        prec: 40,
        label: 1,
    },
];

/// The policy row for `a`.
fn policyof(a: &[u8; 16]) -> Policy {
    let last = Policy {
        addr: [0; 16],
        len: 0,
        mask: 0,
        prec: 40,
        label: 1,
    };
    DEFPOLICY
        .iter()
        .find(|policy| {
            a.get(..policy.len) == policy.addr.get(..policy.len)
                && at(a, policy.len) & policy.mask == at(&policy.addr, policy.len)
        })
        .copied()
        .unwrap_or(last)
}

/// The scope of `a`, as musl's `scopeof` gives it.
fn scopeof(a: &[u8; 16]) -> c_int {
    if a[0] == 0xff {
        return c_int::from(a[1] & 15);
    }
    if is_linklocal(a) {
        return 2;
    }
    if a[..15].iter().all(|&byte| byte == 0) && a[15] == 1 {
        return 2;
    }
    if a[0] == 0xfe && (a[1] & 0xc0) == 0xc0 {
        return 5;
    }
    14
}

/// How many leading bits `s` and `d` share.
fn prefixmatch(s: &[u8; 16], d: &[u8; 16]) -> c_int {
    let mut i = 0;
    while i < 128 && (at(s, i / 8) ^ at(d, i / 8)) & (128 >> (i % 8)) == 0 {
        i += 1;
    }
    i as c_int
}

const DAS_USABLE: c_int = 0x4000_0000;
const DAS_MATCHINGSCOPE: c_int = 0x2000_0000;
const DAS_MATCHINGLABEL: c_int = 0x1000_0000;
const DAS_PREC_SHIFT: c_int = 20;
const DAS_SCOPE_SHIFT: c_int = 16;
const DAS_PREFIX_SHIFT: c_int = 8;

/// Gives each of the addresses a sort key, from RFC 6724's rules 1, 2, 5, 6,
/// 8, 9 and 10 as musl applies them, and sorts them by it.
fn sort(buf: &mut [Address]) {
    let state = cancel::set_state(cancel::DISABLE);
    for (i, address) in buf.iter_mut().enumerate() {
        let family = address.family;
        let mut key = 0;
        let mut sa6 = SockaddrIn6::default();
        let mut da6 = SockaddrIn6::v6([0; 16], 65535, address.scopeid);
        let (da, dalen, mut salen) = if family == AF_INET6 {
            da6.sin6_addr = address.addr;
            (da6, super::SOCKADDR_IN6_LEN, super::SOCKADDR_IN6_LEN)
        } else {
            sa6.sin6_addr = mapped([0; 4]);
            let v4 = first4(&address.addr);
            da6.sin6_addr = mapped(v4);
            (
                SockaddrIn6::v4(v4, 65535),
                super::SOCKADDR_IN_LEN,
                super::SOCKADDR_IN_LEN,
            )
        };
        let dpolicy = policyof(&da6.sin6_addr);
        let dscope = scopeof(&da6.sin6_addr);
        let mut prefixlen = 0;
        let fd = socket(family, SOCK_DGRAM | SOCK_CLOEXEC, IPPROTO_UDP);
        if fd >= 0 {
            // SAFETY: `da` is a live local of `dalen` bytes.
            if unsafe { connect(fd, da.as_ptr(), dalen) } == 0 {
                key |= DAS_USABLE;
                let mut sa = SockaddrIn6::default();
                // SAFETY: `sa` is a live local of 28 bytes, at least `salen`.
                if unsafe { getsockname(fd, sa.as_mut_ptr(), &raw mut salen) } == 0 {
                    if family == AF_INET {
                        sa6.sin6_addr = mapped(sa.v4_addr());
                    } else {
                        sa6.sin6_addr = sa.sin6_addr;
                    }
                    if dscope == scopeof(&sa6.sin6_addr) {
                        key |= DAS_MATCHINGSCOPE;
                    }
                    if dpolicy.label == policyof(&sa6.sin6_addr).label {
                        key |= DAS_MATCHINGLABEL;
                    }
                    prefixlen = prefixmatch(&sa6.sin6_addr, &da6.sin6_addr);
                }
            }
            let _ = close(fd);
        }
        key |= dpolicy.prec << DAS_PREC_SHIFT;
        key |= (15 - dscope) << DAS_SCOPE_SHIFT;
        key |= prefixlen << DAS_PREFIX_SHIFT;
        key |= (MAXADDRS - i) as c_int;
        address.sortkey = key;
    }
    sort_by(buf, |a, b| b.sortkey.cmp(&a.sortkey));
    let _ = cancel::set_state(state);
}

/// Looks up `name`, at most 255 bytes of a C string, or no name, as musl's
/// `__lookup_name` does. Returns how many addresses it stored in `buf`, with
/// the canonical name in `canon`, or an `EAI_` error.
pub fn lookup_name(
    src: &Sources<'_>,
    buf: &mut [Address; MAXADDRS],
    canon: &mut [u8; CANON],
    name: Option<&[u8]>,
    family: c_int,
    flags: c_int,
) -> c_int {
    let mut family = family;
    let mut flags = flags;
    canon[0] = 0;
    if let Some(name) = name {
        // An empty name, or one too long for the buffers, is refused.
        let l = name.len();
        if l.wrapping_sub(1) >= 254 {
            return EAI_NONAME;
        }
        for (slot, &byte) in canon.iter_mut().zip(name) {
            *slot = byte;
        }
        set(canon, l, 0);
    }

    // Asking for IPv6 with IPv4-mapped addresses is asking for either family,
    // then mapping.
    if flags & AI_V4MAPPED != 0 {
        if family == AF_INET6 {
            family = AF_UNSPEC;
        } else {
            flags -= AI_V4MAPPED;
        }
    }

    let mut cnt = match name {
        None => name_from_null(buf, family, flags),
        Some(name) => ipliteral(&mut buf[0], name, family),
    };
    if let Some(name) = name
        && cnt == 0
        && flags & AI_NUMERICHOST == 0
    {
        cnt = name_from_localhost(buf, name, family);
        if cnt == 0 {
            cnt = name_from_hosts(src, buf, canon, name, family);
        }
        if cnt == 0 {
            cnt = name_from_dns_search(src, buf, canon, name, family);
        }
    }
    if cnt <= 0 {
        return if cnt != 0 { cnt } else { EAI_NONAME };
    }
    let mut cnt = cnt as usize;

    if flags & AI_V4MAPPED != 0 {
        if flags & AI_ALL == 0 && buf.iter().take(cnt).any(|a| a.family == AF_INET6) {
            // Some IPv6 results: drop the IPv4 ones.
            let mut j = 0;
            for i in 0..cnt {
                if buf[i].family == AF_INET6 {
                    buf[j] = buf[i];
                    j += 1;
                }
            }
            cnt = j;
        }
        for address in buf.iter_mut().take(cnt) {
            if address.family == AF_INET {
                address.addr = mapped(first4(&address.addr));
                address.family = AF_INET6;
            }
        }
    }

    if cnt < 2 || family == AF_INET || buf.iter().take(cnt).all(|a| a.family == AF_INET) {
        return cnt as c_int;
    }
    if let Some(found) = buf.get_mut(..cnt) {
        sort(found);
    }
    cnt as c_int
}

/// Looks up the service `name`, a C string without its NUL, or no service, for
/// `proto` and `socktype`, as musl's `__lookup_serv` does: a number is a port
/// for TCP, UDP or both, and a name is looked for in the services file unless
/// `AI_NUMERICSERV` forbids it. Returns how many ports it stored in `buf`, or
/// an `EAI_` error.
pub fn lookup_serv(
    src: &Sources<'_>,
    buf: &mut [Service; MAXSERVS],
    name: Option<&[u8]>,
    proto: c_int,
    socktype: c_int,
    flags: c_int,
) -> c_int {
    let mut proto = proto;
    match socktype {
        SOCK_STREAM => match proto {
            0 | IPPROTO_TCP => proto = IPPROTO_TCP,
            _ => return EAI_SERVICE,
        },
        SOCK_DGRAM => match proto {
            0 | IPPROTO_UDP => proto = IPPROTO_UDP,
            _ => return EAI_SERVICE,
        },
        0 => {}
        _ => {
            if name.is_some() {
                return EAI_SERVICE;
            }
            buf[0] = Service {
                port: 0,
                proto: proto as u8,
                socktype: socktype as u8,
            };
            return 1;
        }
    }

    let tcp = |port: u64| Service {
        port: port as u16,
        proto: IPPROTO_TCP as u8,
        socktype: SOCK_STREAM as u8,
    };
    let udp = |port: u64| Service {
        port: port as u16,
        proto: IPPROTO_UDP as u8,
        socktype: SOCK_DGRAM as u8,
    };

    let (port, numeric) = match name {
        None => (0, true),
        Some(name) => {
            if name.is_empty() {
                return EAI_SERVICE;
            }
            let (port, end) = strtoul(name, 0, 10);
            (port, at(name, end) == 0)
        }
    };
    let mut cnt = 0;
    if numeric {
        if port > 65535 {
            return EAI_SERVICE;
        }
        if proto != IPPROTO_UDP {
            buf[cnt] = tcp(port);
            cnt += 1;
        }
        if proto != IPPROTO_TCP {
            if let Some(slot) = buf.get_mut(cnt) {
                *slot = udp(port);
            }
            cnt += 1;
        }
        return cnt as c_int;
    }
    let Some(name) = name else {
        return EAI_SERVICE;
    };

    if flags & AI_NUMERICSERV != 0 {
        return EAI_NONAME;
    }

    let mut db = match Database::open(src.services) {
        Ok(Some(db)) => db,
        Ok(None) => return EAI_SERVICE,
        Err(error) => {
            crate::errno::set(error);
            return EAI_SYSTEM;
        }
    };
    let l = name.len();
    let mut line = [0u8; 128];
    while let Some(len) = db.line(&mut line) {
        if cnt >= MAXSERVS {
            break;
        }
        if let Some(hash) = line.iter().take(len).position(|&byte| byte == b'#') {
            set(&mut line, hash, b'\n');
            set(&mut line, hash + 1, 0);
        }

        let mut from = 0;
        let found = loop {
            match find(&line, from, name) {
                Some(p)
                    if (p > 0 && !is_space(at(&line, p - 1)))
                        || (at(&line, p + l) != 0 && !is_space(at(&line, p + l))) =>
                {
                    from = p + 1;
                }
                other => break other,
            }
        };
        if found.is_none() {
            continue;
        }

        let mut p = 0;
        while at(&line, p) != 0 && !is_space(at(&line, p)) {
            p += 1;
        }
        let (port, z) = strtoul(&line, p, 10);
        if port > 65535 || z == p {
            continue;
        }
        if has_at(&line, z, b"/udp") {
            if proto == IPPROTO_TCP {
                continue;
            }
            if let Some(slot) = buf.get_mut(cnt) {
                *slot = udp(port);
                cnt += 1;
            }
        }
        if has_at(&line, z, b"/tcp") {
            if proto == IPPROTO_UDP {
                continue;
            }
            if let Some(slot) = buf.get_mut(cnt) {
                *slot = tcp(port);
                cnt += 1;
            }
        }
    }
    if cnt > 0 { cnt as c_int } else { EAI_SERVICE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netdb::testing::{Paths, Record, Reply, Responder};

    fn lookup(src: &Sources<'_>, name: &str, family: c_int, flags: c_int) -> (c_int, Vec<Address>, String) {
        let mut buf = [Address::default(); MAXADDRS];
        let mut canon = [0u8; CANON];
        let cnt = lookup_name(src, &mut buf, &mut canon, Some(name.as_bytes()), family, flags);
        let found = buf.iter().take(usize::try_from(cnt).unwrap_or(0)).copied().collect();
        let canon = String::from_utf8_lossy(canon.get(..c_len(&canon)).unwrap_or_default()).into_owned();
        (cnt, found, canon)
    }

    fn v6(groups: [u16; 8]) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (pair, group) in out.chunks_exact_mut(2).zip(groups) {
            pair.copy_from_slice(&group.to_be_bytes());
        }
        out
    }

    #[test]
    fn numeric_addresses_parse_with_their_scopes() {
        let mut out = Address::default();
        assert_eq!(ipliteral(&mut out, b"127.1", AF_UNSPEC), 1);
        assert_eq!((out.family, first4(&out.addr)), (AF_INET, [127, 0, 0, 1]));
        assert_eq!(ipliteral(&mut out, b"10.0.0.1", AF_INET6), EAI_NODATA);
        assert_eq!(ipliteral(&mut out, b"::1", AF_INET), EAI_NODATA);
        assert_eq!(ipliteral(&mut out, b"fe80::1%7", AF_INET6), 1);
        assert_eq!((out.family, out.scopeid), (AF_INET6, 7));
        assert_eq!(ipliteral(&mut out, b"fe80::1%lo", AF_UNSPEC), 1);
        assert!(out.scopeid > 0);
        assert_eq!(ipliteral(&mut out, b"fe80::1%ferrousli-none", AF_UNSPEC), EAI_NONAME);
        assert_eq!(ipliteral(&mut out, b"2001:db8::1%lo", AF_UNSPEC), EAI_NONAME);
        assert_eq!(ipliteral(&mut out, b"::1%4294967296", AF_UNSPEC), EAI_NONAME);
        assert_eq!(ipliteral(&mut out, b"::1%4294967295", AF_UNSPEC), 1);
        assert_eq!(ipliteral(&mut out, b"example", AF_UNSPEC), 0);
        assert_eq!(ipliteral(&mut out, b"1.2.3.4.5", AF_UNSPEC), 0);
    }

    #[test]
    fn host_names_are_judged_as_musl_does() {
        assert!(is_valid_hostname(b"a-b.example\0"));
        assert!(is_valid_hostname(b"xn--\x80\0"));
        assert!(!is_valid_hostname(b"\0"));
        assert!(!is_valid_hostname(b"a_b\0"));
        assert!(!is_valid_hostname(b"a b\0"));
        assert!(!is_valid_hostname(b"no-nul"));
        let mut long = vec![b'a'; 254];
        long.push(0);
        assert!(is_valid_hostname(&long));
        long.insert(0, b'a');
        assert!(!is_valid_hostname(&long));
    }

    #[test]
    fn no_name_and_localhost_give_the_loopback_addresses() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut buf = [Address::default(); MAXADDRS];
        let mut canon = [0u8; CANON];
        let cnt = lookup_name(&src, &mut buf, &mut canon, None, AF_UNSPEC, AI_PASSIVE);
        assert_eq!(cnt, 2);
        assert!(buf.iter().take(2).all(|a| a.addr == [0; 16]));
        let cnt = lookup_name(&src, &mut buf, &mut canon, None, AF_INET, 0);
        assert_eq!((cnt, first4(&buf[0].addr)), (1, [127, 0, 0, 1]));

        for name in ["localhost", "LocalHost.", "a.localhost", "b.LOCALHOST."] {
            let (cnt, found, canon) = lookup(&src, name, AF_INET, AI_NUMERICSERV);
            assert_eq!(cnt, 1, "{name}");
            assert_eq!(first4(&found[0].addr), [127, 0, 0, 1]);
            assert_eq!(canon, name);
        }
        let (cnt, found, _) = lookup(&src, "localhost", AF_INET6, 0);
        assert_eq!((cnt, found[0].addr), (1, v6([0, 0, 0, 0, 0, 0, 0, 1])));
        let (cnt, found, _) = lookup(&src, "localhost", AF_UNSPEC, 0);
        assert_eq!(cnt, 2);
        assert!(found.iter().any(|a| a.family == AF_INET6));
        // Not a localhost name, and not in the hosts file: numeric only fails.
        assert_eq!(lookup(&src, "localhostx", AF_INET, AI_NUMERICHOST).0, EAI_NONAME);
        assert_eq!(lookup(&src, "localhost", AF_INET, AI_NUMERICHOST).0, EAI_NONAME);
    }

    #[test]
    fn the_hosts_file_gives_addresses_and_a_canonical_name() {
        let paths = Paths::new("resolv.conf");
        // No name server listens on port 9: DNS is never reached here.
        let src = paths.sources(9);
        let (cnt, found, canon) = lookup(&src, "alias", AF_INET, 0);
        assert_eq!(cnt, 1);
        assert_eq!(first4(&found[0].addr), [192, 0, 2, 10]);
        assert_eq!(canon, "server.example.test");
        let (cnt, found, canon) = lookup(&src, "multi.example.test", AF_UNSPEC, 0);
        assert_eq!(cnt, 3);
        assert_eq!(canon, "multi.example.test");
        assert_eq!(found.iter().filter(|a| a.family == AF_INET).count(), 2);
        assert_eq!(found.iter().filter(|a| a.family == AF_INET6).count(), 1);
        // Only IPv6 in the file for this name.
        assert_eq!(lookup(&src, "six.example.test", AF_INET, 0).0, EAI_NODATA);
        let (cnt, found, _) = lookup(&src, "six.example.test", AF_INET6, 0);
        assert_eq!((cnt, found[0].addr), (1, v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 6])));
        // A comment does not name a host, and a name must stand alone.
        assert_eq!(lookup(&src, "commented", AF_INET, AI_NUMERICHOST).0, EAI_NONAME);
        let (cnt, found, _) = lookup(&src, "six.example.test", AF_INET, AI_V4MAPPED);
        assert_eq!((cnt, found[0].family), (EAI_NODATA, 0));
        let (cnt, found, _) = lookup(&src, "server.example.test", AF_INET6, AI_V4MAPPED);
        assert_eq!(cnt, 1);
        assert_eq!(found[0].addr, mapped([192, 0, 2, 10]));
    }

    #[test]
    fn a_missing_hosts_file_is_an_empty_one() {
        let paths = Paths {
            hosts: crate::netdb::testing::fixture("absent"),
            ..Paths::new("resolv.conf")
        };
        let responder = Responder::start(|_, _| Reply::Code(3));
        let src = paths.sources(responder.port);
        assert_eq!(lookup(&src, "alias", AF_INET, 0).0, EAI_NONAME);
    }

    #[test]
    fn dns_answers_with_search_domains_and_canonical_names() {
        let paths = Paths::new("resolv.conf");
        let responder = Responder::start(|name, _| match name {
            "www.search.test" => Reply::Records(vec![
                Record::Cname("real.search.test"),
                Record::A([198, 51, 100, 7]),
                Record::Aaaa(v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 7])),
            ]),
            "dotted.name" => Reply::Records(vec![Record::A([198, 51, 100, 8])]),
            "fail.search.test" | "fail" => Reply::Code(2),
            "broken.search.test" | "broken" => Reply::Code(5),
            "empty.search.test" | "empty" => Reply::Records(vec![]),
            _ => Reply::Code(3),
        });
        let src = paths.sources(responder.port);

        let (cnt, found, canon) = lookup(&src, "www", AF_INET, 0);
        assert_eq!(cnt, 1);
        assert_eq!(first4(&found[0].addr), [198, 51, 100, 7]);
        assert_eq!(canon, "real.search.test");
        let (cnt, found, _) = lookup(&src, "www", AF_UNSPEC, 0);
        assert_eq!(cnt, 2);
        assert!(found.iter().any(|a| a.family == AF_INET6));
        // A dot, with ndots 1, skips the search domains.
        let (cnt, _, canon) = lookup(&src, "dotted.name", AF_INET, 0);
        assert_eq!((cnt, canon.as_str()), (1, "dotted.name"));
        assert_eq!(lookup(&src, "nothing", AF_INET, 0).0, EAI_NONAME);
        assert_eq!(lookup(&src, "fail", AF_INET, 0).0, EAI_AGAIN);
        assert_eq!(lookup(&src, "broken", AF_INET, 0).0, EAI_FAIL);
        assert_eq!(lookup(&src, "empty", AF_INET, 0).0, EAI_NODATA);
        assert_eq!(lookup(&src, "trailing..", AF_INET, 0).0, EAI_NONAME);
    }

    #[test]
    fn a_truncated_answer_is_asked_again_over_tcp() {
        let paths = Paths::new("resolv.conf");
        let responder = Responder::start(|_, _| {
            Reply::Truncated(vec![Record::A([203, 0, 113, 5])])
        });
        let src = paths.sources(responder.port);
        let (cnt, found, _) = lookup(&src, "big.test", AF_INET, 0);
        assert_eq!(cnt, 1);
        assert_eq!(first4(&found[0].addr), [203, 0, 113, 5]);
        assert!(responder.questions.load(std::sync::atomic::Ordering::Relaxed) >= 2);
    }

    #[test]
    fn a_silent_name_server_times_out() {
        let paths = Paths::new("resolv.conf");
        // Nothing answers on this port: its socket is bound and never read.
        let silent = std::net::UdpSocket::bind("127.0.0.1:0");
        let port = silent
            .as_ref()
            .ok()
            .and_then(|socket| socket.local_addr().ok())
            .map_or(9, |address| address.port());
        let src = paths.sources(port);
        let started = std::time::Instant::now();
        assert_eq!(lookup(&src, "quiet.test", AF_INET, 0).0, EAI_AGAIN);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn services_come_from_numbers_or_the_services_file() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut buf = [Service::default(); MAXSERVS];
        let serv = |buf: &mut [Service; MAXSERVS], name: Option<&str>, proto, socktype, flags| {
            lookup_serv(&src, buf, name.map(str::as_bytes), proto, socktype, flags)
        };
        assert_eq!(serv(&mut buf, Some("80"), 0, 0, 0), 2);
        assert_eq!(buf[0], Service { port: 80, proto: 6, socktype: 1 });
        assert_eq!(buf[1], Service { port: 80, proto: 17, socktype: 2 });
        assert_eq!(serv(&mut buf, Some("53"), 0, SOCK_DGRAM, 0), 1);
        assert_eq!(buf[0].proto, 17);
        assert_eq!(serv(&mut buf, Some("65536"), 0, 0, 0), EAI_SERVICE);
        assert_eq!(serv(&mut buf, Some(""), 0, 0, 0), EAI_SERVICE);
        assert_eq!(serv(&mut buf, None, 0, SOCK_STREAM, 0), 1);
        assert_eq!(serv(&mut buf, None, 0, 3, 0), 1);
        assert_eq!(buf[0], Service { port: 0, proto: 0, socktype: 3 });
        assert_eq!(serv(&mut buf, Some("http"), 0, 3, 0), EAI_SERVICE);
        assert_eq!(serv(&mut buf, Some("80"), IPPROTO_UDP, SOCK_STREAM, 0), EAI_SERVICE);

        assert_eq!(serv(&mut buf, Some("http"), 0, 0, 0), 2);
        assert_eq!(buf[0], Service { port: 8080, proto: 6, socktype: 1 });
        assert_eq!(buf[1].proto, 17);
        assert_eq!(serv(&mut buf, Some("www"), IPPROTO_TCP, 0, 0), 1);
        assert_eq!(buf[0].port, 8080);
        assert_eq!(serv(&mut buf, Some("domain"), 0, SOCK_STREAM, 0), 1);
        assert_eq!(buf[0].port, 5353);
        assert_eq!(serv(&mut buf, Some("syslog"), IPPROTO_TCP, 0, 0), EAI_SERVICE);
        assert_eq!(serv(&mut buf, Some("syslog"), 0, 0, 0), 1);
        assert_eq!(serv(&mut buf, Some("http"), 0, 0, AI_NUMERICSERV), EAI_NONAME);
        assert_eq!(serv(&mut buf, Some("htt"), 0, 0, 0), EAI_SERVICE);
        assert_eq!(serv(&mut buf, Some("commented"), 0, 0, 0), EAI_SERVICE);
    }

    #[test]
    fn mixed_families_sort_loopback_first() {
        let mut buf = [
            v4([192, 0, 2, 1]),
            Address {
                family: AF_INET6,
                addr: v6([0, 0, 0, 0, 0, 0, 0, 1]),
                ..Address::default()
            },
        ];
        sort(&mut buf);
        assert_eq!(buf[0].family, AF_INET6);
        assert!(buf[0].sortkey > buf[1].sortkey);
        assert_eq!(scopeof(&v6([0xfe80, 0, 0, 0, 0, 0, 0, 1])), 2);
        assert_eq!(scopeof(&v6([0xff05, 0, 0, 0, 0, 0, 0, 1])), 5);
        assert_eq!(scopeof(&v6([0xfec0, 0, 0, 0, 0, 0, 0, 1])), 5);
        assert_eq!(scopeof(&v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1])), 14);
        assert_eq!(policyof(&v6([0x2002, 1, 0, 0, 0, 0, 0, 1])).label, 2);
        assert_eq!(policyof(&v6([0x2001, 0, 0, 0, 0, 0, 0, 1])).label, 5);
        assert_eq!(policyof(&v6([0x2001, 1, 0, 0, 0, 0, 0, 1])).label, 1);
        assert_eq!(policyof(&v6([0xfd00, 0, 0, 0, 0, 0, 0, 1])).label, 13);
        assert_eq!(policyof(&mapped([1, 2, 3, 4])).label, 4);
        assert_eq!(prefixmatch(&[0; 16], &[0; 16]), 128);
        assert_eq!(prefixmatch(&v6([0x8000, 0, 0, 0, 0, 0, 0, 0]), &[0; 16]), 0);
        assert_eq!(prefixmatch(&v6([0x0100, 0, 0, 0, 0, 0, 0, 0]), &[0; 16]), 7);
    }
}
