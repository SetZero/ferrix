//! glibc's resolver state: `struct __res_state`, `__res_state`, the
//! `res_n*` calls over a state the caller passes, and the reentrant
//! protocol lookups. Chrome reads the name servers out of a state
//! `__res_ninit` filled, and GLib asks `res_nquery`.
//!
//! The state has glibc's layout, which its public `<resolv.h>` gives and
//! programs built against it read directly. `__res_ninit` fills what this
//! library's own resolver reads from `/etc/resolv.conf`: the name servers,
//! IPv4 ones in `nsaddr_list` and IPv6 ones through `_u._ext.nsaddrs` as
//! glibc keeps them, the timeout and attempts, `ndots`, and the search
//! list. The queries themselves go through this library's resolver, which
//! reads the file afresh each time, as musl's does, so a state changes
//! nothing about how they are made.
//!
//! glibc's `_res` is per thread; `__res_state` here answers one state for
//! the process.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use core::mem::size_of;
use core::ptr::null_mut;

use super::hostent::{Protoent, getprotobyname, getprotobynumber};
use super::lookup::Address;
use super::resolver::{get_resolv_conf, res_query, res_search, res_send};
use super::{SYSTEM, SockaddrIn, SockaddrIn6, h_errno};
use crate::errno;
use crate::lock::SpinLock;
use crate::malloc::{free, malloc};
use crate::string::strlen;

/// `MAXNS`: the most name servers a state holds.
const MAXNS: usize = 3;
/// `MAXDNSRCH`: the most search domains.
const MAXDNSRCH: usize = 6;
/// `MAXRESOLVSORT`.
const MAXRESOLVSORT: usize = 10;
/// `RES_INIT`: the state was filled.
const RES_INIT: c_ulong = 0x1;
/// `RES_DEFAULT`: `RES_RECURSE | RES_DEFNAMES | RES_DNSRCH`.
const RES_DEFAULT: c_ulong = 0x40 | 0x80 | 0x200;
/// `AF_INET`.
const AF_INET: c_int = 2;
/// `AF_INET6`.
const AF_INET6: c_int = 10;

/// `_u._ext`, glibc's extension for IPv6 name servers.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ResExt {
    /// How many name servers the extension describes.
    pub nscount: u16,
    /// Which of them each slot is.
    pub nsmap: [u16; MAXNS],
    /// Sockets, unused here.
    pub nssocks: [c_int; MAXNS],
    /// How many are IPv6.
    pub nscount6: u16,
    /// Whether `nsaddrs` was set up.
    pub nsinit: u16,
    /// Each IPv6 name server, from `malloc`, or null.
    pub nsaddrs: [*mut SockaddrIn6; MAXNS],
    /// Reserved.
    pub reserved: [c_uint; 2],
}

/// The union at the end of the state.
#[repr(C)]
#[derive(Clone, Copy)]
pub union ResUnion {
    /// Its size.
    pub pad: [c_char; 52],
    /// The extension.
    pub ext: ResExt,
}

/// glibc's `struct __res_state`.
#[repr(C)]
pub struct ResState {
    /// The retransmission interval, in seconds.
    pub retrans: c_int,
    /// How many times to send.
    pub retry: c_int,
    /// `RES_*` options.
    pub options: c_ulong,
    /// How many name servers.
    pub nscount: c_int,
    /// The IPv4 name servers; an IPv6 one's slot has family 0.
    pub nsaddr_list: [SockaddrIn; MAXNS],
    /// The current message id.
    pub id: u16,
    /// The search domains, into `defdname`, then null.
    pub dnsrch: [*mut c_char; MAXDNSRCH + 1],
    /// The search domains, each ending in a NUL.
    pub defdname: [c_char; 256],
    /// `RES_PRF_*` flags.
    pub pfcode: c_ulong,
    /// `ndots` in the low 4 bits, then `nsort`, `ipv6_unavail`, unused.
    pub bits: c_uint,
    /// The sort list, unused here.
    pub sort_list: [[u32; 2]; MAXRESOLVSORT],
    /// Unused hooks.
    pub hooks: [*mut c_void; 2],
    /// The last error of a query through this state.
    pub res_h_errno: c_int,
    /// A socket, unused here.
    pub vcsock: c_int,
    /// Internal flags.
    pub flags: c_uint,
    /// The IPv6 extension.
    pub u: ResUnion,
}

impl core::fmt::Debug for ResExt {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ResExt")
            .field("nscount", &self.nscount)
            .field("nsaddrs", &self.nsaddrs)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for ResUnion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ResUnion").finish_non_exhaustive()
    }
}

impl core::fmt::Debug for ResState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ResState")
            .field("options", &self.options)
            .field("nscount", &self.nscount)
            .finish_non_exhaustive()
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<ResState>() == 568);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<ResState>() == 512);

/// The process's state, zeroed until `__res_ninit` fills it.
struct Shared(UnsafeCell<[u64; size_of::<ResState>() / 8]>);

// SAFETY: C's `_res` is shared state that a program uses as it chooses.
unsafe impl Sync for Shared {}

/// The state [`__res_state`] answers.
static RES: Shared = Shared(UnsafeCell::new([0; size_of::<ResState>() / 8]));

/// The state `_res` names: glibc's `__res_state`, which its headers call
/// for `_res`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __res_state() -> *mut ResState {
    RES.0.get().cast()
}

/// Frees the IPv6 name servers `__res_ninit` allocated for `state`.
fn free_extension(state: &mut ResState) {
    // SAFETY: the union's extension is what `__res_ninit` wrote, or zeros.
    let ext = unsafe { &mut state.u.ext };
    for address in &mut ext.nsaddrs {
        // SAFETY: null, or from `malloc` below.
        unsafe { free(address.cast()) };
        *address = null_mut();
    }
    ext.nsinit = 0;
}

/// Fills `*state` from `/etc/resolv.conf`: glibc's `__res_ninit`, which
/// its headers call for `res_ninit`. Returns 0, or -1 if the file cannot be
/// read.
///
/// # Safety
///
/// `state` must be valid for reads and writes of a `struct __res_state`,
/// zeroed or filled by an earlier call.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __res_ninit(state: *mut ResState) -> c_int {
    // SAFETY: the caller vouches for `state`.
    let state = unsafe { &mut *state };
    if state.options & RES_INIT != 0 {
        free_extension(state);
    }
    let mut search = [0_u8; 256];
    let Ok(conf) = get_resolv_conf(&SYSTEM, Some(&mut search)) else {
        return -1;
    };
    state.retrans = conf.timeout as c_int;
    state.retry = conf.attempts as c_int;
    state.options = RES_INIT | RES_DEFAULT;
    state.bits = (state.bits & !0xf) | conf.ndots.min(15);
    state.res_h_errno = 0;

    // SAFETY: zeros are a valid extension.
    let mut ext: ResExt = unsafe { core::mem::zeroed() };
    let servers = conf.ns.iter().take(conf.nns);
    let slots = state
        .nsaddr_list
        .iter_mut()
        .zip(ext.nsaddrs.iter_mut())
        .zip(ext.nsmap.iter_mut());
    let mut count = 0;
    for (((list, v6), map), address) in slots.zip(servers) {
        *list = ipv4(address, conf.port);
        if address.family == AF_INET6 {
            let copy = malloc(size_of::<SockaddrIn6>()).cast::<SockaddrIn6>();
            if !copy.is_null() {
                let sa = SockaddrIn6 {
                    sin6_family: AF_INET6 as u16,
                    sin6_port: conf.port.to_be(),
                    sin6_addr: address.addr,
                    sin6_scope_id: address.scopeid,
                    ..SockaddrIn6::default()
                };
                // SAFETY: fresh memory for one `struct sockaddr_in6`.
                unsafe { copy.write(sa) };
                ext.nscount6 += 1;
            }
            *v6 = copy;
        }
        *map = count as u16;
        count += 1;
    }
    state.nscount = count;
    ext.nscount = count as u16;
    ext.nsinit = 1;
    state.u = ResUnion { ext };

    // The search list, split into `defdname` with a NUL after each.
    state.defdname = [0; 256];
    state.dnsrch = [null_mut(); MAXDNSRCH + 1];
    let text = search.split(|&b| b == 0).next().unwrap_or_default();
    let mut at = 0;
    let base = state.defdname.as_mut_ptr();
    let domains = text
        .split(u8::is_ascii_whitespace)
        .filter(|domain| !domain.is_empty());
    for (pointer, domain) in state.dnsrch.iter_mut().take(MAXDNSRCH).zip(domains) {
        if at + domain.len() + 1 > state.defdname.len() {
            break;
        }
        *pointer = base.wrapping_add(at);
        for (slot, &byte) in state.defdname.iter_mut().skip(at).zip(domain) {
            *slot = byte as c_char;
        }
        at += domain.len() + 1;
    }
    0
}

/// `address` as a `struct sockaddr_in` for `nsaddr_list`, or family 0 for
/// an IPv6 one, as glibc leaves those slots.
fn ipv4(address: &Address, port: u16) -> SockaddrIn {
    let v4 = address.family == AF_INET;
    let sin_addr = match address.addr.first_chunk::<4>() {
        Some(first) if v4 => *first,
        _ => [0; 4],
    };
    let family = if v4 { AF_INET as u16 } else { 0 };
    SockaddrIn {
        sin_family: family,
        sin_port: port.to_be(),
        sin_addr,
        sin_zero: [0; 8],
    }
}

/// Releases what `__res_ninit` allocated for `*state`: glibc's
/// `__res_nclose`. As glibc's, it leaves `RES_INIT` set.
///
/// # Safety
///
/// As [`__res_ninit`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __res_nclose(state: *mut ResState) {
    // SAFETY: the caller vouches for `state`.
    let state = unsafe { &mut *state };
    free_extension(state);
}

/// Records `ret`'s error in `*state`, as glibc's `res_n*` calls do.
///
/// # Safety
///
/// As [`__res_ninit`].
unsafe fn note(state: *mut ResState, ret: c_int) -> c_int {
    if ret < 0 && !state.is_null() {
        // SAFETY: the caller vouches for `state`.
        unsafe { (*state).res_h_errno = h_errno() };
    }
    ret
}

/// [`res_query`] through a state: glibc's `res_nquery`.
///
/// # Safety
///
/// As `res_query`, and `state` as [`__res_ninit`] or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_nquery(
    state: *mut ResState,
    name: *const c_char,
    class: c_int,
    kind: c_int,
    answer: *mut u8,
    anslen: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `res_query`'s.
    let ret = unsafe { res_query(name, class, kind, answer, anslen) };
    // SAFETY: as above.
    unsafe { note(state, ret) }
}

/// [`res_search`] through a state: glibc's `res_nsearch`.
///
/// # Safety
///
/// As [`res_nquery`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_nsearch(
    state: *mut ResState,
    name: *const c_char,
    class: c_int,
    kind: c_int,
    answer: *mut u8,
    anslen: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `res_search`'s.
    let ret = unsafe { res_search(name, class, kind, answer, anslen) };
    // SAFETY: as above.
    unsafe { note(state, ret) }
}

/// [`res_send`] through a state: glibc's `res_nsend`.
///
/// # Safety
///
/// As `res_send`, and `state` as [`__res_ninit`] or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_nsend(
    state: *mut ResState,
    msg: *const u8,
    msglen: c_int,
    answer: *mut u8,
    anslen: c_int,
) -> c_int {
    // SAFETY: the caller's contract is `res_send`'s.
    let ret = unsafe { res_send(msg, msglen, answer, anslen) };
    // SAFETY: as above.
    unsafe { note(state, ret) }
}

/// Guards the protocol table's static entry while a reentrant lookup copies
/// it out.
static PROTO_LOCK: SpinLock = SpinLock::new();

/// Copies the protocol entry `found` into `*result_buf`, its strings and
/// alias array into the `buflen` bytes at `buf`, as the `_r` lookups
/// return it: 0 with `*result` set, 0 with `*result` null when nothing was
/// found, or `ERANGE` when the buffer is too small, as glibc answers.
///
/// # Safety
///
/// `found` must be null or the static entry the lookups fill, and the rest
/// as the `_r` functions say.
unsafe fn copy_protoent(
    found: *mut Protoent,
    result_buf: *mut Protoent,
    buf: *mut c_char,
    buflen: usize,
    result: *mut *mut Protoent,
) -> c_int {
    // SAFETY: the caller passes a writable pointer.
    unsafe { result.write(null_mut()) };
    if found.is_null() {
        return 0;
    }
    // SAFETY: the static entry the lookup filled.
    let entry = unsafe { found.read() };
    let mut aliases = 0;
    // SAFETY: the alias array ends in a null.
    while !unsafe { entry.p_aliases.wrapping_add(aliases).read() }.is_null() {
        aliases += 1;
    }
    let pointer = size_of::<*mut c_char>();
    let skew = buf.addr() % pointer;
    let align = if skew == 0 { 0 } else { pointer - skew };
    let mut need = align + (aliases + 1) * pointer;
    // SAFETY: the name is a NUL-terminated string.
    need += unsafe { strlen(entry.p_name) } + 1;
    for i in 0..aliases {
        // SAFETY: each alias is a NUL-terminated string.
        let alias = unsafe { entry.p_aliases.wrapping_add(i).read() };
        // SAFETY: as above.
        need += unsafe { strlen(alias) } + 1;
    }
    if need > buflen {
        return errno::ERANGE;
    }
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "`align` moved the pointer to a multiple of a pointer's size"
    )]
    let array = buf.wrapping_add(align).cast::<*mut c_char>();
    let mut text = array.wrapping_add(aliases + 1).cast::<c_char>();
    let mut copy = |from: *const c_char| {
        let at = text;
        // SAFETY: `need` counted this string's bytes and its NUL.
        let len = unsafe { strlen(from) } + 1;
        // SAFETY: as above, inside the caller's buffer.
        let _ = unsafe { crate::string::memcpy(at.cast(), from.cast(), len) };
        text = text.wrapping_add(len);
        at
    };
    let name = copy(entry.p_name);
    for i in 0..aliases {
        // SAFETY: as above.
        let alias = copy(unsafe { entry.p_aliases.wrapping_add(i).read() });
        // SAFETY: the array has room for every alias and the null.
        unsafe { array.wrapping_add(i).write(alias) };
    }
    // SAFETY: as above.
    unsafe { array.wrapping_add(aliases).write(null_mut()) };
    let copied = Protoent {
        p_name: name,
        p_aliases: array,
        p_proto: entry.p_proto,
    };
    // SAFETY: the caller passes a writable `struct protoent`.
    unsafe { result_buf.write(copied) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { result.write(result_buf) };
    0
}

/// The protocol named `name`, copied into `*result_buf` and `buf`: glibc's
/// reentrant `getprotobyname_r`, which NSPR calls.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, `result_buf` and `result`
/// writable, and `buf` valid for writes of `buflen` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getprotobyname_r(
    name: *const c_char,
    result_buf: *mut Protoent,
    buf: *mut c_char,
    buflen: usize,
    result: *mut *mut Protoent,
) -> c_int {
    let _guard = PROTO_LOCK.lock();
    // SAFETY: the caller passes a NUL-terminated string.
    let found = unsafe { getprotobyname(name) };
    // SAFETY: the entry is the static one, held by the lock.
    unsafe { copy_protoent(found, result_buf, buf, buflen, result) }
}

/// The protocol numbered `num`, copied as [`getprotobyname_r`] copies it.
///
/// # Safety
///
/// As [`getprotobyname_r`], but for `name`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getprotobynumber_r(
    num: c_int,
    result_buf: *mut Protoent,
    buf: *mut c_char,
    buflen: usize,
    result: *mut *mut Protoent,
) -> c_int {
    let _guard = PROTO_LOCK.lock();
    let found = getprotobynumber(num);
    // SAFETY: the entry is the static one, held by the lock.
    unsafe { copy_protoent(found, result_buf, buf, buflen, result) }
}
