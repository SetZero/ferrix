//! The stub resolver: `/etc/resolv.conf`, and sending queries to its name
//! servers.
//!
//! Ported from musl 1.2.5's `resolvconf.c`, `res_msend.c`, `res_send.c`,
//! `res_query.c` and `res_querydomain.c` (MIT). Every query goes to every name
//! server at once over UDP, again after each `timeout / attempts`, until each
//! has an answer that is a success or a name error, or the timeout passes. An
//! answer with the truncation bit is asked again over TCP, from the server
//! that sent it.
//!
//! Three things differ from musl:
//!
//! * Cancellation is disabled for the whole exchange. musl lets a thread be
//!   cancelled while it waits in `poll`, with a cleanup handler closing the
//!   sockets. `poll` is not a cancellation point in this library yet, so there
//!   is no wait to cancel.
//! * `options attempts:0` is taken as 1. musl divides the timeout by it.
//! * When the kernel refuses an IPv6 socket, musl means to give up if every
//!   name server is IPv6, but reads one entry past the list to decide. This
//!   looks at the list.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::size_of;

use super::dns::{self, QUERY_MAX};
use super::lookup::{Address, first4, ipliteral, mapped};
use super::{
    AF_INET, AF_INET6, AF_UNSPEC, Database, HOST_NOT_FOUND, IPPROTO_TCP, NO_DATA,
    SOCK_CLOEXEC, SOCK_DGRAM, SOCK_NONBLOCK, SOCK_STREAM, SOCKADDR_IN_LEN, SOCKADDR_IN6_LEN,
    SYSTEM, SockaddrIn6, Sources, TRY_AGAIN, at, c_bytes_max, c_len, find, has_at, is_space,
    set_h_errno, strtoul,
};
use crate::cancel;
use crate::errno;
use crate::poll::{Pollfd, poll};
use crate::socket::{Msghdr, bind, connect, recvmsg, sendmsg, sendto, setsockopt, socket};
use crate::time::{CLOCK_MONOTONIC, CLOCK_REALTIME, Timespec, clock_gettime};
use crate::uio::Iovec;
use crate::unistd::close;

/// `MAXNS`, from `include/resolv.h`: the most name servers used.
pub const MAXNS: usize = 3;
/// The most queries sent together: A and AAAA.
const MAX_QUERIES: usize = 2;

/// `IPPROTO_IPV6`, from `include/netinet/in.h`.
const IPPROTO_IPV6: c_int = 41;
/// `IPV6_V6ONLY`, from `include/netinet/in.h`.
const IPV6_V6ONLY: c_int = 26;
/// `TCP_FASTOPEN_CONNECT`, from `include/netinet/tcp.h`.
const TCP_FASTOPEN_CONNECT: c_int = 30;
/// `MSG_TRUNC`, from `include/sys/socket.h`.
const MSG_TRUNC: c_int = 0x0020;
/// `MSG_NOSIGNAL`, from `include/sys/socket.h`.
const MSG_NOSIGNAL: c_int = 0x4000;
/// `MSG_FASTOPEN`, from `include/sys/socket.h`.
const MSG_FASTOPEN: c_int = 0x2000_0000;
/// `POLLIN`, from `include/poll.h`.
const POLLIN: i16 = 0x001;
/// `POLLOUT`, from `include/poll.h`.
const POLLOUT: i16 = 0x004;
/// `EOF`.
const EOF: c_int = -1;

/// What `/etc/resolv.conf` says, as musl's `struct resolvconf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvConf {
    /// The name servers.
    pub ns: [Address; MAXNS],
    /// How many of them there are.
    pub nns: usize,
    /// How many times a query is sent within the timeout.
    pub attempts: c_uint,
    /// How many dots make a name absolute, so that the search domains are not
    /// tried.
    pub ndots: c_uint,
    /// Seconds to wait for all answers.
    pub timeout: c_uint,
    /// The port the name servers listen on.
    pub port: u16,
}

/// Reads the resolver configuration from `src`, and the `search` or `domain`
/// line into `search` if it is given, as musl's `__get_resolv_conf` does.
/// Without a file, or without a usable `nameserver` line, the name server is
/// 127.0.0.1. `Err` holds the error number if the file exists and cannot be
/// read.
pub fn get_resolv_conf(
    src: &Sources<'_>,
    mut search: Option<&mut [u8; 256]>,
) -> Result<ResolvConf, c_int> {
    let mut conf = ResolvConf {
        ns: [Address::default(); MAXNS],
        nns: 0,
        attempts: 2,
        ndots: 1,
        timeout: 5,
        port: src.port,
    };
    if let Some(search) = search.as_deref_mut() {
        search[0] = 0;
    }

    if let Some(mut db) = Database::open(src.resolv_conf)? {
        let mut line = [0u8; 256];
        while let Some(len) = db.line(&mut line) {
            if !line.iter().take(len).any(|&byte| byte == b'\n') && !db.at_end() {
                // A line longer than the buffer is skipped rather than read
                // in pieces that might mean something else.
                loop {
                    let c = db.byte();
                    if c == c_int::from(b'\n') || c == EOF {
                        break;
                    }
                }
                continue;
            }
            if has_at(&line, 0, b"options") && is_space(at(&line, 7)) {
                let option = |name: &[u8], extra: u8| {
                    let p = find(&line, 0, name)? + name.len();
                    let first = at(&line, p);
                    if !first.is_ascii_digit() && first != extra {
                        return None;
                    }
                    let (value, end) = strtoul(&line, p, 10);
                    (end != p).then_some(value)
                };
                if let Some(value) = option(b"ndots:", b'0') {
                    conf.ndots = value.min(15) as c_uint;
                }
                if let Some(value) = option(b"attempts:", b'0') {
                    conf.attempts = value.min(10) as c_uint;
                }
                if let Some(value) = option(b"timeout:", b'.') {
                    conf.timeout = value.min(60) as c_uint;
                }
                continue;
            }
            if has_at(&line, 0, b"nameserver") && is_space(at(&line, 10)) {
                let Some(slot) = conf.ns.get_mut(conf.nns) else {
                    continue;
                };
                let mut p = 11;
                while is_space(at(&line, p)) {
                    p += 1;
                }
                let mut z = p;
                while at(&line, z) != 0 && !is_space(at(&line, z)) {
                    z += 1;
                }
                if ipliteral(slot, line.get(p..z).unwrap_or_default(), AF_UNSPEC) > 0 {
                    conf.nns += 1;
                }
                continue;
            }

            let Some(search) = search.as_deref_mut() else {
                continue;
            };
            if (!has_at(&line, 0, b"domain") && !has_at(&line, 0, b"search"))
                || !is_space(at(&line, 6))
            {
                continue;
            }
            let mut p = 7;
            while is_space(at(&line, p)) {
                p += 1;
            }
            let l = c_len(line.get(p..).unwrap_or_default());
            if l >= search.len() {
                continue;
            }
            for (index, slot) in search.iter_mut().enumerate().take(l + 1) {
                *slot = at(&line, p + index);
            }
        }
    }

    if conf.nns == 0 {
        let _ = ipliteral(&mut conf.ns[0], b"127.0.0.1", AF_UNSPEC);
        conf.nns = 1;
    }
    Ok(conf)
}

/// Milliseconds on the monotonic clock, or the real-time one without it.
fn mtime() -> u64 {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live local.
    if unsafe { clock_gettime(CLOCK_MONOTONIC, &raw mut ts) } < 0
        && crate::pwd::last_errno() == errno::ENOSYS
    {
        // SAFETY: as above.
        let _ = unsafe { clock_gettime(CLOCK_REALTIME, &raw mut ts) };
    }
    (ts.tv_sec as u64)
        .wrapping_mul(1000)
        .wrapping_add(ts.tv_nsec.unsigned_abs() / 1_000_000)
}

/// The two-byte length TCP puts before a message.
const fn length_prefix(len: usize) -> [u8; 2] {
    [(len >> 8) as u8, len as u8]
}

/// Points the first of `iov` to skip `n` bytes, and returns the index of the
/// first buffer left, as musl's `step_mh` steps a message header.
fn step(iov: &mut [Iovec; 2], mut n: usize) -> usize {
    let mut first = 0;
    while let Some(buffer) = iov.get(first) {
        if n < buffer.iov_len {
            break;
        }
        n -= buffer.iov_len;
        first += 1;
    }
    if let Some(buffer) = iov.get_mut(first) {
        buffer.iov_base = buffer.iov_base.wrapping_byte_add(n);
        buffer.iov_len -= n;
    }
    first
}

/// A message header over the buffers of `iov` from `first`, to `name`.
fn header(iov: &mut [Iovec; 2], first: usize, name: *mut c_void, namelen: c_uint) -> Msghdr {
    Msghdr {
        msg_name: name,
        msg_namelen: namelen,
        msg_iov: iov.as_mut_ptr().wrapping_add(first).cast(),
        msg_iovlen: (2 - first.min(2)) as c_int,
        __pad1: 0,
        msg_control: core::ptr::null_mut(),
        msg_controllen: 0,
        __pad2: 0,
        msg_flags: 0,
    }
}

/// Opens a TCP connection to `sa` for the query `q` into `pfd`, sending what
/// it can with the connection if the kernel does TCP Fast Open, as musl's
/// `start_tcp` does. Returns how many bytes of the framed query went out, or
/// -1 if no connection could be started.
fn start_tcp(pfd: &mut Pollfd, family: c_int, sa: &SockaddrIn6, sl: c_uint, q: &[u8]) -> isize {
    let mut prefix = length_prefix(q.len());
    let mut iov = [
        Iovec {
            iov_base: prefix.as_mut_ptr().cast(),
            iov_len: 2,
        },
        Iovec {
            iov_base: q.as_ptr().cast_mut().cast(),
            iov_len: q.len(),
        },
    ];
    let mh = header(&mut iov, 0, sa.as_ptr().cast_mut(), sl);
    let fd = socket(family, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    pfd.fd = fd;
    pfd.events = POLLOUT;
    let one: c_int = 1;
    // SAFETY: `one` is a live `int`.
    let fastopen = unsafe {
        setsockopt(
            fd,
            IPPROTO_TCP,
            TCP_FASTOPEN_CONNECT,
            (&raw const one).cast(),
            size_of::<c_int>() as c_uint,
        )
    };
    if fastopen == 0 {
        // SAFETY: the header's buffers and address are live locals and `q`.
        let r = unsafe { sendmsg(fd, &raw const mh, MSG_FASTOPEN | MSG_NOSIGNAL) };
        if r >= 0 && r as usize == q.len() + 2 {
            pfd.events = POLLIN;
        }
        if r >= 0 {
            return r;
        }
        if crate::pwd::last_errno() == errno::EINPROGRESS {
            return 0;
        }
    }
    // SAFETY: `sa` is a live address of at least `sl` bytes.
    let r = unsafe { connect(fd, sa.as_ptr(), sl) };
    if r == 0 || crate::pwd::last_errno() == errno::EINPROGRESS {
        return 0;
    }
    let _ = close(fd);
    pfd.fd = -1;
    -1
}

/// Sends each of `queries` to `conf`'s name servers and waits for the
/// answers, as musl's `__res_msend_rc` does. Each answer goes into the slot of
/// `answers` of the same index, all of which must be the same size and at
/// least 512 bytes, and its length into `alens`, 0 for none. A TCP answer's
/// length may exceed its slot; only the slot's bytes were kept.
///
/// `Err` with `errno` set if no socket could be opened and bound; past that,
/// each query simply has an answer or not.
pub fn msend_rc(
    queries: &[&[u8]],
    answers: &mut [&mut [u8]],
    alens: &mut [isize; MAX_QUERIES],
    conf: &ResolvConf,
) -> Result<(), ()> {
    let nq = queries.len().min(answers.len()).min(MAX_QUERIES);
    let asize = answers.iter().map(|answer| answer.len()).min().unwrap_or(0);
    let state = cancel::set_state(cancel::DISABLE);
    let result = exchange(queries, answers, alens, nq, asize, conf);
    let _ = cancel::set_state(state);
    result
}

/// The body of [`msend_rc`], with cancellation disabled.
fn exchange(
    queries: &[&[u8]],
    answers: &mut [&mut [u8]],
    alens: &mut [isize; MAX_QUERIES],
    nq: usize,
    asize: usize,
    conf: &ResolvConf,
) -> Result<(), ()> {
    let query = |i: usize| queries.get(i).copied().unwrap_or_default();
    let timeout = 1000 * u64::from(conf.timeout);
    let attempts = u64::from(conf.attempts.max(1));
    let port = conf.port.to_be();

    let mut ns = [SockaddrIn6::default(); MAXNS];
    let mut sl = SOCKADDR_IN_LEN;
    let mut family = AF_INET;
    let nns = conf.nns.min(MAXNS);
    for (slot, iplit) in ns.iter_mut().zip(conf.ns.iter()).take(nns) {
        if iplit.family == AF_INET {
            *slot = SockaddrIn6::v4(first4(&iplit.addr), port);
        } else {
            sl = SOCKADDR_IN6_LEN;
            *slot = SockaddrIn6::v6(iplit.addr, port, iplit.scopeid);
            family = AF_INET6;
        }
    }

    let mut fd = socket(family, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if fd < 0 && family == AF_INET6 && crate::pwd::last_errno() == errno::EAFNOSUPPORT {
        // Without IPv6, go on with the IPv4 name servers, if there are any.
        if conf.ns.iter().take(nns).all(|iplit| iplit.family == AF_INET6) {
            return Err(());
        }
        fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
        family = AF_INET;
        sl = SOCKADDR_IN_LEN;
    }

    if fd >= 0 && family == AF_INET6 {
        let zero: c_int = 0;
        // SAFETY: `zero` is a live `int`.
        let _ = unsafe {
            setsockopt(
                fd,
                IPPROTO_IPV6,
                IPV6_V6ONLY,
                (&raw const zero).cast(),
                size_of::<c_int>() as c_uint,
            )
        };
        for server in ns.iter_mut().take(nns) {
            if c_int::from(server.sin6_family) == AF_INET {
                *server = SockaddrIn6::v6(mapped(server.v4_addr()), server.sin6_port, 0);
            }
        }
    }

    let mut sa = SockaddrIn6 {
        sin6_family: family as u16,
        ..SockaddrIn6::default()
    };
    // SAFETY: `sa` is a live local of at least `sl` bytes.
    if fd < 0 || unsafe { bind(fd, sa.as_ptr(), sl) } < 0 {
        if fd >= 0 {
            let _ = close(fd);
        }
        return Err(());
    }

    // Past here there are no errors: each query has an answer or not.
    let mut pfd = [Pollfd {
        fd: -1,
        events: 0,
        revents: 0,
    }; MAX_QUERIES + 1];
    if let Some(udp) = pfd.get_mut(nq) {
        *udp = Pollfd {
            fd,
            events: POLLIN,
            revents: 0,
        };
    }
    *alens = [0; MAX_QUERIES];
    let mut qpos = [0usize; MAX_QUERIES];
    let mut apos = [0usize; MAX_QUERIES];
    let mut alen_buf = [[0u8; 2]; MAX_QUERIES];

    let retry_interval = timeout / attempts;
    let mut servfail_retry = 0;
    let mut next = 0;
    let t0 = mtime();
    let mut t2 = t0;
    let mut t1 = t2.wrapping_sub(retry_interval);
    let mut first = true;

    'wait: loop {
        if !first {
            t2 = mtime();
        }
        first = false;
        if t2.wrapping_sub(t0) >= timeout {
            break;
        }
        if alens.iter().take(nq).all(|&alen| alen > 0) {
            break;
        }

        if t2.wrapping_sub(t1) >= retry_interval {
            // Ask every name server at once.
            for (i, &alen) in alens.iter().enumerate().take(nq) {
                if alen != 0 {
                    continue;
                }
                let q = query(i);
                for server in ns.iter().take(nns) {
                    // SAFETY: the query and the address are live.
                    let _ = unsafe {
                        sendto(fd, q.as_ptr().cast(), q.len(), MSG_NOSIGNAL, server.as_ptr(), sl)
                    };
                }
            }
            t1 = t2;
            servfail_retry = 2 * nq;
        }

        let wait = t1.wrapping_add(retry_interval).wrapping_sub(t2);
        let wait = c_int::try_from(wait).unwrap_or(c_int::MAX);
        // SAFETY: `pfd` holds `nq + 1` entries.
        if unsafe { poll(pfd.as_mut_ptr(), (nq + 1) as _, wait) } <= 0 {
            continue;
        }

        while next < nq {
            let Some(slot) = answers.get_mut(next) else {
                break;
            };
            let mut from = SockaddrIn6::default();
            let mut iov = [
                Iovec {
                    iov_base: slot.as_mut_ptr().cast(),
                    iov_len: asize,
                },
                Iovec {
                    iov_base: core::ptr::null_mut(),
                    iov_len: 0,
                },
            ];
            let mut mh = header(&mut iov, 0, from.as_mut_ptr(), sl);
            mh.msg_iovlen = 1;
            // SAFETY: the header's buffer is the answer slot of `asize` bytes,
            // and its address a live local of 28.
            let rlen = unsafe { recvmsg(fd, &raw mut mh, 0) };
            if rlen < 0 {
                break;
            }
            // Too short to identify.
            if rlen < 4 {
                continue;
            }
            let rlen_bytes = rlen as usize;
            // Only from an address the query went to.
            let from_bytes = from.bytes();
            let Some(j) = ns
                .iter()
                .take(nns)
                .position(|server| server.bytes().get(..sl as usize) == from_bytes.get(..sl as usize))
            else {
                continue;
            };
            let got = answers.get(next).map_or(&[][..], |answer| &**answer);
            let (id0, id1, flags, rcode) = (at(got, 0), at(got, 1), at(got, 2), at(got, 3) & 15);
            // The query this answers.
            let Some(i) = (next..nq).find(|&i| at(query(i), 0) == id0 && at(query(i), 1) == id1)
            else {
                continue;
            };
            if alens.get(i).copied().unwrap_or(1) != 0 {
                continue;
            }
            // Take successes and name errors; ask again at once after a
            // server failure; ignore refusals and the rest.
            match rcode {
                0 | 3 => {}
                2 => {
                    if servfail_retry != 0 {
                        servfail_retry -= 1;
                        let q = query(i);
                        let server = ns.get(j).copied().unwrap_or_default();
                        // SAFETY: the query and the address are live.
                        let _ = unsafe {
                            sendto(fd, q.as_ptr().cast(), q.len(), MSG_NOSIGNAL, server.as_ptr(), sl)
                        };
                    }
                    continue;
                }
                _ => continue,
            }

            if let Some(alen) = alens.get_mut(i) {
                *alen = rlen;
            }
            if i == next {
                while next < nq && alens.get(next).is_some_and(|&alen| alen != 0) {
                    next += 1;
                }
            } else if let Some((left, right)) = answers.split_at_mut_checked(i)
                && let (Some(source), Some(target)) = (left.get(next), right.first_mut())
            {
                for (to, &byte) in target.iter_mut().zip(source.iter()).take(rlen_bytes) {
                    *to = byte;
                }
            }

            if next == nq
                && let Some(udp) = pfd.get_mut(nq)
            {
                udp.events = 0;
            }

            // A truncated answer is asked again over TCP.
            if flags & 2 != 0 || mh.msg_flags & MSG_TRUNC != 0 {
                if let Some(alen) = alens.get_mut(i) {
                    *alen = -1;
                }
                let server = ns.get(j).copied().unwrap_or_default();
                if let Some(tcp) = pfd.get_mut(i) {
                    let r = start_tcp(tcp, family, &server, sl, query(i));
                    if let (Ok(r), Some(q), Some(a)) =
                        (usize::try_from(r), qpos.get_mut(i), apos.get_mut(i))
                    {
                        *q = r;
                        *a = 0;
                    }
                }
            }
        }

        for i in 0..nq {
            let Some(tcp) = pfd.get(i).copied() else {
                break;
            };
            if tcp.revents & POLLOUT == 0 {
                continue;
            }
            let q = query(i);
            let mut prefix = length_prefix(q.len());
            let mut iov = [
                Iovec {
                    iov_base: prefix.as_mut_ptr().cast(),
                    iov_len: 2,
                },
                Iovec {
                    iov_base: q.as_ptr().cast_mut().cast(),
                    iov_len: q.len(),
                },
            ];
            let sent = qpos.get(i).copied().unwrap_or(0);
            let first = step(&mut iov, sent);
            let mh = header(&mut iov, first, core::ptr::null_mut(), 0);
            // SAFETY: the header's buffers are live.
            let r = unsafe { sendmsg(tcp.fd, &raw const mh, MSG_NOSIGNAL) };
            let Ok(r) = usize::try_from(r) else {
                break 'wait;
            };
            if let Some(pos) = qpos.get_mut(i) {
                *pos += r;
                if *pos == q.len() + 2
                    && let Some(entry) = pfd.get_mut(i)
                {
                    entry.events = POLLIN;
                }
            }
        }

        for i in 0..nq {
            let Some(tcp) = pfd.get(i).copied() else {
                break;
            };
            if tcp.revents & POLLIN == 0 {
                continue;
            }
            let (Some(slot), Some(len_buf)) = (answers.get_mut(i), alen_buf.get_mut(i)) else {
                break;
            };
            let mut iov = [
                Iovec {
                    iov_base: len_buf.as_mut_ptr().cast(),
                    iov_len: 2,
                },
                Iovec {
                    iov_base: slot.as_mut_ptr().cast(),
                    iov_len: asize,
                },
            ];
            let received = apos.get(i).copied().unwrap_or(0);
            let first = step(&mut iov, received);
            let mut mh = header(&mut iov, first, core::ptr::null_mut(), 0);
            // SAFETY: the header's buffers are the length and the answer slot.
            let r = unsafe { recvmsg(tcp.fd, &raw mut mh, 0) };
            let Some(r) = usize::try_from(r).ok().filter(|&r| r > 0) else {
                break 'wait;
            };
            let Some(pos) = apos.get_mut(i) else {
                break;
            };
            *pos += r;
            let pos = *pos;
            if pos < 2 {
                continue;
            }
            let [high, low] = alen_buf.get(i).copied().unwrap_or_default();
            let alen = usize::from(high) * 256 + usize::from(low);
            if alen < 13 {
                break 'wait;
            }
            if pos < alen + 2 && pos < asize + 2 {
                continue;
            }
            let got = answers.get(i).map_or(&[][..], |answer| &**answer);
            let rcode = at(got, 3) & 15;
            if rcode != 0 && rcode != 3 {
                break 'wait;
            }
            // The answer is taken: close its connection now.
            if let Some(entry) = alens.get_mut(i) {
                *entry = alen as isize;
            }
            let _ = close(tcp.fd);
            if let Some(entry) = pfd.get_mut(i) {
                entry.fd = -1;
            }
        }
    }

    for entry in pfd.iter().take(nq + 1) {
        if entry.fd >= 0 {
            let _ = close(entry.fd);
        }
    }
    // An unfinished TCP answer is no answer.
    for alen in alens.iter_mut() {
        if *alen < 0 {
            *alen = 0;
        }
    }
    Ok(())
}

/// Sends the query `msg` to the name servers `src` configures and stores the
/// answer in `answer`, as musl's `__res_send` does. Returns the answer's
/// length, which for a TCP answer may exceed what was kept, or -1 with no
/// answer.
pub fn res_send_from(src: &Sources<'_>, msg: &[u8], answer: &mut [u8]) -> c_int {
    if answer.len() < 512 {
        let mut buf = [0u8; 512];
        let r = res_send_from(src, msg, &mut buf);
        if let Ok(len) = usize::try_from(r) {
            for (slot, &byte) in answer.iter_mut().zip(buf.iter()).take(len) {
                *slot = byte;
            }
        }
        return r;
    }
    let Ok(conf) = get_resolv_conf(src, None) else {
        return -1;
    };
    let mut alens = [0isize; MAX_QUERIES];
    let mut answers: [&mut [u8]; 1] = [answer];
    if msend_rc(&[msg], &mut answers, &mut alens, &conf).is_err() || alens[0] == 0 {
        return -1;
    }
    c_int::try_from(alens[0]).unwrap_or(c_int::MAX)
}

/// Sends the `msglen`-byte query at `msg` and stores up to `anslen` bytes of
/// the answer at `answer`. Returns the answer's length, or -1.
///
/// # Safety
///
/// `msg` must be valid for reads of `msglen` bytes and `answer` for writes of
/// `anslen`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_send(
    msg: *const u8,
    msglen: c_int,
    answer: *mut u8,
    anslen: c_int,
) -> c_int {
    let (Ok(msglen), Ok(anslen)) = (usize::try_from(msglen), usize::try_from(anslen)) else {
        return -1;
    };
    // SAFETY: the caller passes `msglen` readable bytes.
    let msg = unsafe { core::slice::from_raw_parts(msg, msglen) };
    // SAFETY: the caller passes `anslen` writable bytes.
    let answer = unsafe { core::slice::from_raw_parts_mut(answer, anslen) };
    res_send_from(&SYSTEM, msg, answer)
}

/// Asks for records of `class` and `type` for `name`, storing the answer in
/// `dest`, as musl's `res_query` does. Returns the answer's length, or -1 with
/// `h_errno` set: `TRY_AGAIN` without an answer, `HOST_NOT_FOUND` for a name
/// error, `NO_DATA` for a success with no answers.
pub fn query_from(src: &Sources<'_>, name: &[u8], class: c_int, kind: c_int, dest: &mut [u8]) -> c_int {
    let Some((q, ql)) = dns::mkquery(0, name, class, kind, dns::query_id()) else {
        return -1;
    };
    let r = res_send_from(src, q.get(..ql).unwrap_or_default(), dest);
    if r < 12 {
        set_h_errno(TRY_AGAIN);
        return -1;
    }
    let rcode = at(dest, 3) & 15;
    if rcode == 3 {
        set_h_errno(HOST_NOT_FOUND);
        return -1;
    }
    if rcode == 0 && at(dest, 6) == 0 && at(dest, 7) == 0 {
        set_h_errno(NO_DATA);
        return -1;
    }
    r
}

/// Asks for records of `class` and `type` for `name`, storing up to `len`
/// bytes of the answer at `dest`. Returns the answer's length, or -1 with
/// `h_errno` set.
///
/// # Safety
///
/// `name` must be a NUL-terminated string and `dest` valid for writes of `len`
/// bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_query(
    name: *const c_char,
    class: c_int,
    kind: c_int,
    dest: *mut u8,
    len: c_int,
) -> c_int {
    let Ok(len) = usize::try_from(len) else {
        set_h_errno(TRY_AGAIN);
        return -1;
    };
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { c_bytes_max(name, 255) };
    // SAFETY: the caller passes `len` writable bytes.
    let dest = unsafe { core::slice::from_raw_parts_mut(dest, len) };
    query_from(&SYSTEM, name, class, kind, dest)
}

/// `res_query`: as in musl, no search domains are applied.
///
/// # Safety
///
/// As `res_query`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_search(
    name: *const c_char,
    class: c_int,
    kind: c_int,
    dest: *mut u8,
    len: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `res_query`'s.
    unsafe { res_query(name, class, kind, dest, len) }
}

/// `res_query` for `name` in `domain`: the two joined by a dot. -1 if that is
/// longer than 253 bytes.
///
/// # Safety
///
/// As `res_query`, and `domain` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_querydomain(
    name: *const c_char,
    domain: *const c_char,
    class: c_int,
    kind: c_int,
    dest: *mut u8,
    len: c_int,
) -> c_int {
    // SAFETY: the caller passes NUL-terminated strings.
    let name = unsafe { c_bytes_max(name, 255) };
    // SAFETY: as above.
    let domain = unsafe { c_bytes_max(domain, 255) };
    if name.len() + domain.len() + 1 > 254 {
        return -1;
    }
    let mut tmp = [0u8; 255];
    for (slot, &byte) in tmp
        .iter_mut()
        .zip(name.iter().chain(b".").chain(domain.iter()))
    {
        *slot = byte;
    }
    // SAFETY: `tmp` ends in a NUL: at most 254 of its bytes were written.
    unsafe { res_query(tmp.as_ptr().cast(), class, kind, dest, len) }
}

/// Does nothing: the configuration is read afresh for each lookup. Returns 0.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn res_init() -> c_int {
    0
}

/// A query buffer's size, for callers building one.
pub const QUERY_BUF: usize = QUERY_MAX;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netdb::h_errno;
    use crate::netdb::testing::{Paths, Record, Reply, Responder, fixture};

    #[test]
    fn the_configuration_file_is_read_as_musl_reads_it() {
        let paths = Paths::new("resolv-options.conf");
        let src = paths.sources(5300);
        let mut search = [0u8; 256];
        let conf = get_resolv_conf(&src, Some(&mut search));
        assert!(conf.is_ok());
        let Ok(conf) = conf else { return };
        assert_eq!((conf.ndots, conf.attempts, conf.timeout), (15, 10, 60));
        assert_eq!(conf.nns, 3);
        assert_eq!(conf.port, 5300);
        assert_eq!(conf.ns[0].family, AF_INET);
        assert_eq!(first4(&conf.ns[0].addr), [192, 0, 2, 53]);
        assert_eq!(conf.ns[1].family, AF_INET6);
        assert_eq!(first4(&conf.ns[2].addr), [192, 0, 2, 55]);
        assert_eq!(search.get(..c_len(&search)), Some(&b"b.test c.test\n"[..]));

        let paths = Paths::new("absent");
        let conf = get_resolv_conf(&paths.sources(53), None);
        assert_eq!(conf.map(|conf| (conf.nns, conf.ndots, conf.timeout, first4(&conf.ns[0].addr))), Ok((1, 1, 5, [127, 0, 0, 1])));
        let paths = Paths {
            resolv_conf: fixture(""),
            ..Paths::new("resolv.conf")
        };
        // A directory cannot be read as a file.
        assert_eq!(get_resolv_conf(&paths.sources(53), None).map(|_| ()), Err(errno::EISDIR));
    }

    #[test]
    fn a_long_line_is_skipped_whole() {
        let paths = Paths::new("resolv-long.conf");
        let conf = get_resolv_conf(&paths.sources(53), None);
        assert_eq!(conf.map(|conf| (conf.nns, first4(&conf.ns[0].addr))), Ok((1, [192, 0, 2, 1])));
    }

    #[test]
    fn res_query_reports_through_h_errno() {
        let paths = Paths::new("resolv.conf");
        let responder = Responder::start(|name, _| match name {
            "found.test" => Reply::Records(vec![Record::A([192, 0, 2, 1])]),
            "empty.test" => Reply::Records(vec![]),
            _ => Reply::Code(3),
        });
        let src = paths.sources(responder.port);
        let mut dest = [0u8; 100];
        let r = query_from(&src, b"found.test", 1, dns::RR_A, &mut dest);
        assert_eq!(r, 12 + 16 + 16);
        assert_eq!(dest.get(r as usize - 4..r as usize), Some(&[192, 0, 2, 1][..]));
        assert_eq!(query_from(&src, b"empty.test", 1, dns::RR_A, &mut dest), -1);
        assert_eq!(h_errno(), NO_DATA);
        assert_eq!(query_from(&src, b"missing.test", 1, dns::RR_A, &mut dest), -1);
        assert_eq!(h_errno(), HOST_NOT_FOUND);
        assert_eq!(query_from(&src, b"a..b", 1, dns::RR_A, &mut dest), -1);
    }

    #[test]
    fn no_answer_is_try_again() {
        let paths = Paths::new("resolv.conf");
        let silent = std::net::UdpSocket::bind("127.0.0.1:0");
        let port = silent
            .as_ref()
            .ok()
            .and_then(|socket| socket.local_addr().ok())
            .map_or(9, |address| address.port());
        let mut dest = [0u8; 600];
        assert_eq!(query_from(&paths.sources(port), b"x.test", 1, 1, &mut dest), -1);
        assert_eq!(h_errno(), TRY_AGAIN);
    }

    #[test]
    fn steps_skip_whole_and_partial_buffers() {
        let mut bytes = [0u8; 8];
        let base = bytes.as_mut_ptr().cast::<c_void>();
        let fresh = |base: *mut c_void| {
            [
                Iovec { iov_base: base, iov_len: 2 },
                Iovec { iov_base: base.wrapping_byte_add(2), iov_len: 6 },
            ]
        };
        let mut iov = fresh(base);
        assert_eq!(step(&mut iov, 0), 0);
        assert_eq!(iov[0].iov_len, 2);
        let mut iov = fresh(base);
        assert_eq!(step(&mut iov, 3), 1);
        assert_eq!((iov[1].iov_base, iov[1].iov_len), (base.wrapping_byte_add(3), 5));
        let mut iov = fresh(base);
        assert_eq!(step(&mut iov, 8), 2);
        assert_eq!(header(&mut iov, 2, core::ptr::null_mut(), 0).msg_iovlen, 0);
    }
}
