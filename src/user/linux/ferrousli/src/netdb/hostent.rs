//! The legacy `netdb.h` lookups: `gethostbyname` and its relatives,
//! `getservbyname` and `getservbyport`, and the `set*ent`, `get*ent` and
//! `end*ent` cursors over `/etc/hosts`, `/etc/networks`, `/etc/protocols` and
//! `/etc/services`.
//!
//! The reentrant lookups are ports of musl 1.2.5's `gethostbyname2_r.c`,
//! `gethostbyaddr_r.c`, `getservbyname_r.c` and `getservbyport_r.c` (MIT),
//! packing their answer into the caller's buffer byte for byte as musl does,
//! and the wrappers that return static storage are musl's `gethostbyname2.c`,
//! `gethostbyaddr.c`, `getservbyname.c` and `getservbyport.c`: a buffer that
//! doubles until the answer fits.
//!
//! The cursors are not musl's. musl's `ent.c`, `serv.c` and `netname.c`
//! answer every call with a null pointer, and its `proto.c` answers from a
//! table built into the library. A program that lists the hosts or the
//! services of the machine — `getent`, and busybox's `nslookup` when it
//! prints a service name — then sees an empty database. These read the files
//! instead, with the cursor semantics `getpwent` and `getgrent` already have
//! here and glibc documents for these:
//!
//! * `get*ent` reads the next entry, opening the file on the first call, and
//!   answers null at the end or when the file is not there.
//! * `set*ent` starts again from the first entry. Its `stayopen` argument is
//!   ignored, as the file is kept open until `end*ent` either way.
//! * `end*ent` closes the file.
//! * `get*by*` starts from the first entry and reads to the one it wants, so
//!   it disturbs no cursor a caller is holding; this is what glibc does.
//! * An entry's strings live in the line buffer, which the next call to any
//!   of the three overwrites. Like the other non-reentrant functions here,
//!   they are not safe to call from two threads at once, as in C.
//!
//! `getprotoent` keeps musl's table as well: `/etc/protocols` is read first,
//! and the table answers when the file is absent or empty. A program that
//! asks for `tcp` gets an answer on a machine that has no `/etc/protocols`,
//! which a Ferrix root filesystem may well not.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use super::addrinfo::getnameinfo_from;
use super::lookup::{
    Address, CANON, MAXADDRS, MAXSERVS, Service, ipliteral, lookup_name, lookup_serv,
};
use super::{
    AF_INET, AF_INET6, AF_UNSPEC, AI_CANONNAME, EAI_AGAIN, EAI_MEMORY, EAI_NODATA, EAI_NONAME,
    EAI_OVERFLOW, EAI_SYSTEM, HOST_NOT_FOUND, Hostent, IPPROTO_TCP, IPPROTO_UDP, NI_DGRAM, NO_DATA,
    NO_RECOVERY, SOCKADDR_IN_LEN, SOCKADDR_IN6_LEN, SYSTEM, Servent, SockaddrIn6, Sources,
    TRY_AGAIN, at, c_bytes, c_bytes_max, c_len, copy_bytes, copy_out, is_space, set_h_errno,
    strtoul,
};
use crate::errno;
use crate::malloc::{free, malloc};
use crate::pwd::{Line, Shared, last_errno, open_database};
use crate::stdio::file::File;
use crate::stdio::open::fclose;
use crate::string::strcmp;

/// The size of a pointer, which every one of these buffers is laid out in.
const PTR: usize = size_of::<*mut c_char>();

/// `"tcp"`, which a `servent` points at.
static TCP: &CStr = c"tcp";
/// `"udp"`, which a `servent` points at.
static UDP: &CStr = c"udp";

/// C's `struct netent`, from `include/netdb.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Netent {
    /// The official name.
    pub n_name: *mut c_char,
    /// The other names, ending with null.
    pub n_aliases: *mut *mut c_char,
    /// The address family, always `AF_INET`.
    pub n_addrtype: c_int,
    /// The network number, in host byte order.
    pub n_net: u32,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Netent>() == 24);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Netent, n_addrtype) == 8);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Netent, n_net) == 12);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Netent>() == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Netent, n_addrtype) == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Netent, n_net) == 20);

/// C's `struct protoent`, from `include/netdb.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Protoent {
    /// The official name.
    pub p_name: *mut c_char,
    /// The other names, ending with null.
    pub p_aliases: *mut *mut c_char,
    /// The protocol number.
    pub p_proto: c_int,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Protoent>() == 24);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(Protoent, p_proto) == 8);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Protoent>() == 12);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(Protoent, p_proto) == 16);

/// The pointer array at `buf`, which the caller has aligned to hold one.
///
/// Every caller is a `_r` lookup packing its answer into the caller's `char`
/// buffer the way musl does, and each computes the skew that aligns `buf` to
/// a pointer before calling this.
#[expect(
    clippy::cast_ptr_alignment,
    reason = "AUDIT: each caller aligns `buf` to a pointer first, as musl does"
)]
fn as_array(buf: *mut c_char) -> *mut *mut c_char {
    buf.cast::<*mut c_char>()
}

/// Stores the pointer `value` at `index` of the array at `array`.
///
/// # Safety
///
/// `array` must be valid for writes of `index + 1` pointers.
unsafe fn put_ptr(array: *mut *mut c_char, index: usize, value: *mut c_char) {
    // SAFETY: the caller vouches for the room.
    unsafe { array.wrapping_add(index).write(value) };
}

// ---------------------------------------------------------------------------
// The reentrant host lookups
// ---------------------------------------------------------------------------

/// The `h_errno` value and the return value an `EAI_` code gives a `_r`
/// lookup, as musl's `gethostbyname2_r` maps them.
fn host_error(code: c_int) -> (c_int, c_int) {
    match code {
        EAI_NONAME => (HOST_NOT_FOUND, 0),
        EAI_NODATA => (NO_DATA, 0),
        EAI_AGAIN => (TRY_AGAIN, errno::EAGAIN),
        EAI_SYSTEM => (NO_RECOVERY, last_errno()),
        _ => (NO_RECOVERY, errno::EBADMSG),
    }
}

/// Resolves `name` for the family `af` through `src` into `*h`, whose strings
/// and addresses go into the `buflen` bytes at `buf`. Returns 0 with `*res`
/// set, `ERANGE` if the buffer is too small, or the error number; `*err`
/// holds the `h_errno` value either way.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, `h` a writable `struct hostent`,
/// `buf` valid for writes of `buflen` bytes, and `res` and `err` writable.
#[expect(
    clippy::too_many_arguments,
    reason = "AUDIT: the argument list is `gethostbyname2_r`'s, which C fixes"
)]
pub unsafe fn gethostbyname2_r_from(
    src: &Sources<'_>,
    name: *const c_char,
    af: c_int,
    h: *mut Hostent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Hostent,
    err: *mut c_int,
) -> c_int {
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(null_mut()) };
    let mut addrs = [Address::default(); MAXADDRS];
    let mut canon = [0u8; CANON];
    // SAFETY: the caller passes a NUL-terminated string.
    let wanted = unsafe { c_bytes_max(name, 255) };
    let cnt = lookup_name(src, &mut addrs, &mut canon, Some(wanted), af, AI_CANONNAME);
    if cnt < 0 {
        let (herror, ret) = host_error(cnt);
        // SAFETY: the caller passes a writable pointer.
        unsafe { err.write(herror) };
        return ret;
    }

    let cnt = cnt as usize;
    let length = if af == AF_INET6 { 16 } else { 4 };
    let align = buf.addr().wrapping_neg() & (PTR - 1);
    let canon_len = c_len(&canon);
    let need = 4 * PTR + (cnt + 1) * (PTR + length) + wanted.len() + 1 + canon_len + 1 + align;
    if need > buflen {
        return errno::ERANGE;
    }

    // Three pointers of aliases, then one per address and a null.
    let aliases = as_array(buf.wrapping_add(align));
    let addr_list = as_array(buf.wrapping_add(align + 3 * PTR));
    let mut next = buf.wrapping_add(align + (4 + cnt) * PTR);
    for index in 0..cnt {
        let address = addrs.get(index).copied().unwrap_or_default();
        // SAFETY: the buffer holds `cnt + 1` address pointers, checked above.
        unsafe { put_ptr(addr_list, index, next) };
        // SAFETY: the buffer holds `length` bytes for each address.
        unsafe { copy_bytes(next, &address.addr, length) };
        next = next.wrapping_add(length);
    }
    // SAFETY: as above.
    unsafe { put_ptr(addr_list, cnt, null_mut()) };

    // The canonical name is the official one, and the name asked for is an
    // alias when it differs from it.
    let official = next;
    // SAFETY: the buffer holds the name and its NUL, checked above.
    unsafe { copy_out(official, &canon, canon_len) };
    next = next.wrapping_add(canon_len + 1);
    // SAFETY: three alias pointers were reserved.
    unsafe { put_ptr(aliases, 0, official) };
    if canon.get(..canon_len) == Some(wanted) {
        // SAFETY: as above.
        unsafe { put_ptr(aliases, 1, null_mut()) };
    } else {
        // SAFETY: as above.
        unsafe { put_ptr(aliases, 1, next) };
        // SAFETY: the buffer holds the name and its NUL, checked above.
        unsafe { copy_out(next, wanted, wanted.len()) };
    }
    // SAFETY: as above.
    unsafe { put_ptr(aliases, 2, null_mut()) };

    let entry = Hostent {
        h_name: official,
        h_aliases: aliases,
        h_addrtype: af,
        h_length: length as c_int,
        h_addr_list: addr_list,
    };
    // SAFETY: the caller passes a writable `struct hostent`.
    unsafe { h.write(entry) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(h) };
    0
}

/// Resolves `name` for the family `af` into `*h`. Returns 0 with `*res` set,
/// `ERANGE` if the buffer is too small, or the error number.
///
/// # Safety
///
/// As [`gethostbyname2_r_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyname2_r(
    name: *const c_char,
    af: c_int,
    h: *mut Hostent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Hostent,
    err: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract is `gethostbyname2_r_from`'s.
    unsafe { gethostbyname2_r_from(&SYSTEM, name, af, h, buf, buflen, res, err) }
}

/// `gethostbyname2_r` for `AF_INET`.
///
/// # Safety
///
/// As [`gethostbyname2_r`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyname_r(
    name: *const c_char,
    h: *mut Hostent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Hostent,
    err: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract is `gethostbyname2_r`'s.
    unsafe { gethostbyname2_r(name, AF_INET, h, buf, buflen, res, err) }
}

/// Names the `l`-byte address at `a` of family `af` into `*h`, through
/// `getnameinfo` over `src`. Returns 0 with `*res` set, `ERANGE` if the
/// buffer is too small, or the error number.
///
/// # Safety
///
/// `a` must be valid for reads of `l` bytes, `h` a writable `struct hostent`,
/// `buf` valid for writes of `buflen` bytes, and `res` and `err` writable.
#[expect(
    clippy::too_many_arguments,
    reason = "AUDIT: the argument list is `gethostbyaddr_r`'s, which C fixes"
)]
pub unsafe fn gethostbyaddr_r_from(
    src: &Sources<'_>,
    a: *const c_void,
    l: c_uint,
    af: c_int,
    h: *mut Hostent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Hostent,
    err: *mut c_int,
) -> c_int {
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(null_mut()) };
    let len = l as usize;
    if !((af == AF_INET6 && len == 16) || (af == AF_INET && len == 4)) {
        // SAFETY: the caller passes a writable pointer.
        unsafe { err.write(NO_RECOVERY) };
        return errno::EINVAL;
    }
    // SAFETY: the caller passes `l` readable bytes.
    let bytes = unsafe { core::slice::from_raw_parts(a.cast::<u8>(), len) };
    let sa = if af == AF_INET6 {
        let mut addr = [0u8; 16];
        for (slot, &byte) in addr.iter_mut().zip(bytes) {
            *slot = byte;
        }
        SockaddrIn6::v6(addr, 0, 0)
    } else {
        SockaddrIn6::v4([at(bytes, 0), at(bytes, 1), at(bytes, 2), at(bytes, 3)], 0)
    };
    let sl = if af == AF_INET6 {
        SOCKADDR_IN6_LEN
    } else {
        SOCKADDR_IN_LEN
    };

    // Two address pointers, two alias pointers, the address, then the name.
    let skew = buf.addr() & (PTR - 1);
    let skew = if skew == 0 { PTR } else { skew };
    let head = 5 * PTR - skew + len;
    if buflen <= head {
        return errno::ERANGE;
    }
    let start = buf.wrapping_add(PTR - skew);
    let addr_list = as_array(start);
    let aliases = as_array(start.wrapping_add(2 * PTR));
    let stored = start.wrapping_add(4 * PTR);
    let text = stored.wrapping_add(len);
    let room = buflen - head;
    // SAFETY: two address pointers were reserved.
    unsafe { put_ptr(addr_list, 0, stored) };
    // SAFETY: as above.
    unsafe { put_ptr(addr_list, 1, null_mut()) };
    // SAFETY: `len` bytes were reserved for the address.
    unsafe { copy_bytes(stored, bytes, len) };
    // SAFETY: two alias pointers were reserved.
    unsafe { put_ptr(aliases, 0, text) };
    // SAFETY: as above.
    unsafe { put_ptr(aliases, 1, null_mut()) };

    // SAFETY: `sa` is a live local of at least `sl` bytes, and `text` has
    // `room` writable bytes.
    let code =
        unsafe { getnameinfo_from(src, sa.as_ptr(), sl, text, room as c_uint, null_mut(), 0, 0) };
    match code {
        0 => {}
        EAI_OVERFLOW => return errno::ERANGE,
        EAI_AGAIN => {
            // SAFETY: the caller passes a writable pointer.
            unsafe { err.write(TRY_AGAIN) };
            return errno::EAGAIN;
        }
        EAI_SYSTEM => {
            // SAFETY: as above.
            unsafe { err.write(NO_RECOVERY) };
            return last_errno();
        }
        _ => {
            // SAFETY: as above.
            unsafe { err.write(NO_RECOVERY) };
            return errno::EBADMSG;
        }
    }

    let entry = Hostent {
        h_name: text,
        h_aliases: aliases,
        h_addrtype: af,
        h_length: l as c_int,
        h_addr_list: addr_list,
    };
    // SAFETY: the caller passes a writable `struct hostent`.
    unsafe { h.write(entry) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(h) };
    0
}

/// Names the `l`-byte address at `a` of family `af` into `*h`. Returns 0 with
/// `*res` set, `ERANGE` if the buffer is too small, or the error number.
///
/// # Safety
///
/// As [`gethostbyaddr_r_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyaddr_r(
    a: *const c_void,
    l: c_uint,
    af: c_int,
    h: *mut Hostent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Hostent,
    err: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract is `gethostbyaddr_r_from`'s.
    unsafe { gethostbyaddr_r_from(&SYSTEM, a, l, af, h, buf, buflen, res, err) }
}

/// The storage the three `gethostby*` functions return, grown until the
/// answer fits. musl keeps one such buffer per function; one for all three
/// costs a caller nothing, because each already invalidates the others.
static HOST: Shared<*mut Hostent> = Shared(UnsafeCell::new(null_mut()));

/// Calls `lookup` with a buffer that doubles until the answer fits, as musl's
/// `gethostbyname2` does, and sets `h_errno`.
fn grown(
    mut lookup: impl FnMut(*mut Hostent, *mut c_char, usize, *mut *mut Hostent, *mut c_int) -> c_int,
) -> *mut Hostent {
    // SAFETY: see `Shared`: C documents these as unsafe to call from two
    // threads at once, and this reaches the buffer only within one call.
    let slot = unsafe { &mut *HOST.0.get() };
    let mut size = 63usize;
    loop {
        // SAFETY: the buffer is null or an earlier `malloc`'s, and is
        // forgotten here.
        unsafe { free((*slot).cast()) };
        *slot = null_mut();
        size = size * 2 + 1;
        let h = malloc(size).cast::<Hostent>();
        if h.is_null() {
            set_h_errno(NO_RECOVERY);
            return null_mut();
        }
        *slot = h;
        let mut found: *mut Hostent = null_mut();
        let error = lookup(
            h,
            h.wrapping_add(1).cast::<c_char>(),
            size - size_of::<Hostent>(),
            &raw mut found,
            super::__h_errno_location(),
        );
        if error != errno::ERANGE {
            return found;
        }
    }
}

/// Resolves `name` for the family `af`, in static storage the next call to
/// any of these overwrites, or null with `h_errno` set.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyname2(name: *const c_char, af: c_int) -> *mut Hostent {
    grown(|h, buf, len, res, err| {
        // SAFETY: the caller passes a NUL-terminated string, and the buffer
        // is the allocation's, past the `struct hostent`.
        unsafe { gethostbyname2_r(name, af, h, buf, len, res, err) }
    })
}

/// `gethostbyname2` for `AF_INET`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyname(name: *const c_char) -> *mut Hostent {
    // SAFETY: the caller's contract is `gethostbyname2`'s.
    unsafe { gethostbyname2(name, AF_INET) }
}

/// Names the `l`-byte address at `a` of family `af`, in static storage the
/// next call to any of these overwrites, or null with `h_errno` set.
///
/// # Safety
///
/// `a` must be valid for reads of `l` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyaddr(a: *const c_void, l: c_uint, af: c_int) -> *mut Hostent {
    grown(|h, buf, len, res, err| {
        // SAFETY: the caller passes `l` readable bytes, and the buffer is the
        // allocation's, past the `struct hostent`.
        unsafe { gethostbyaddr_r(a, l, af, h, buf, len, res, err) }
    })
}

// ---------------------------------------------------------------------------
// The reentrant service lookups
// ---------------------------------------------------------------------------

/// The protocol number `prots` names: 0 for null, or `EINVAL`.
///
/// # Safety
///
/// `prots` must be null or a NUL-terminated string.
unsafe fn protocol_of(prots: *const c_char) -> Result<c_int, c_int> {
    if prots.is_null() {
        return Ok(0);
    }
    // SAFETY: the caller passes a NUL-terminated string.
    match unsafe { c_bytes(prots) } {
        b"tcp" => Ok(IPPROTO_TCP),
        b"udp" => Ok(IPPROTO_UDP),
        _ => Err(errno::EINVAL),
    }
}

/// Looks up the service `name` for the protocol `prots` in `src` into `*se`.
/// Returns 0 with `*res` set, or the error number. A name that is a number is
/// not a service record and gives `ENOENT`, as musl's does.
///
/// # Safety
///
/// `name` must be a NUL-terminated string that outlives `*se`, which points
/// into it, `prots` null or a NUL-terminated string, `se` a writable
/// `struct servent`, `buf` valid for writes of `buflen` bytes, and `res`
/// writable.
pub unsafe fn getservbyname_r_from(
    src: &Sources<'_>,
    name: *const c_char,
    prots: *const c_char,
    se: *mut Servent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Servent,
) -> c_int {
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(null_mut()) };
    // SAFETY: the caller passes a NUL-terminated string.
    let wanted = unsafe { c_bytes(name) };
    // A number is a port, not a service record.
    let (_, end) = strtoul(wanted, 0, 10);
    if at(wanted, end) == 0 {
        return errno::ENOENT;
    }
    let align = buf.addr().wrapping_neg() & (PTR - 1);
    if buflen < 2 * PTR + align {
        return errno::ERANGE;
    }
    let aliases = as_array(buf.wrapping_add(align));
    // SAFETY: the caller passes null or a NUL-terminated string.
    let proto = match unsafe { protocol_of(prots) } {
        Ok(proto) => proto,
        Err(error) => return error,
    };

    let mut servs = [Service::default(); MAXSERVS];
    let cnt = lookup_serv(src, &mut servs, Some(wanted), proto, 0, 0);
    if cnt < 0 {
        return match cnt {
            EAI_MEMORY | EAI_SYSTEM => errno::ENOMEM,
            _ => errno::ENOENT,
        };
    }
    let first = servs.first().copied().unwrap_or_default();
    // SAFETY: two alias pointers were reserved, checked above.
    unsafe { put_ptr(aliases, 0, name.cast_mut()) };
    // SAFETY: as above.
    unsafe { put_ptr(aliases, 1, null_mut()) };
    let entry = Servent {
        s_name: name.cast_mut(),
        s_aliases: aliases,
        s_port: c_int::from(first.port.to_be()),
        s_proto: proto_name(c_int::from(first.proto)),
    };
    // SAFETY: the caller passes a writable `struct servent`.
    unsafe { se.write(entry) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(se) };
    0
}

/// Looks up the service `name` for the protocol `prots` into `*se`. Returns 0
/// with `*res` set, or the error number.
///
/// # Safety
///
/// As [`getservbyname_r_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyname_r(
    name: *const c_char,
    prots: *const c_char,
    se: *mut Servent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Servent,
) -> c_int {
    // SAFETY: the caller's contract is `getservbyname_r_from`'s.
    unsafe { getservbyname_r_from(&SYSTEM, name, prots, se, buf, buflen, res) }
}

/// `"tcp"` or `"udp"`, in static storage, for the protocol number `proto`.
fn proto_name(proto: c_int) -> *mut c_char {
    if proto == IPPROTO_TCP { TCP } else { UDP }
        .as_ptr()
        .cast_mut()
}

/// Names the service on `port`, given in network byte order, for the protocol
/// `prots` in `src`, or for TCP and then UDP if it is null. Returns 0 with
/// `*res` set, or the error number.
///
/// # Safety
///
/// `prots` must be null or a NUL-terminated string that outlives `*se`, which
/// points at it, `se` a writable `struct servent`, `buf` valid for writes of
/// `buflen` bytes, and `res` writable.
pub unsafe fn getservbyport_r_from(
    src: &Sources<'_>,
    port: c_int,
    prots: *const c_char,
    se: *mut Servent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Servent,
) -> c_int {
    if prots.is_null() {
        // SAFETY: the caller's contract, with a protocol of this function's.
        let tcp = unsafe { getservbyport_r_from(src, port, TCP.as_ptr(), se, buf, buflen, res) };
        if tcp == 0 {
            return 0;
        }
        // SAFETY: as above.
        return unsafe { getservbyport_r_from(src, port, UDP.as_ptr(), se, buf, buflen, res) };
    }
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(null_mut()) };
    // SAFETY: the caller passes a NUL-terminated string.
    let dgram = match unsafe { c_bytes(prots) } {
        b"tcp" => false,
        b"udp" => true,
        _ => return errno::EINVAL,
    };

    let skew = buf.addr() & (PTR - 1);
    let skew = if skew == 0 { PTR } else { skew };
    if buflen <= 3 * PTR - skew {
        return errno::ERANGE;
    }
    let start = buf.wrapping_add(PTR - skew);
    let aliases = as_array(start);
    let text = start.wrapping_add(2 * PTR);
    let room = buflen - (PTR - skew) - 2 * PTR;
    // SAFETY: two alias pointers were reserved, checked above.
    unsafe { put_ptr(aliases, 0, text) };
    // SAFETY: as above.
    unsafe { put_ptr(aliases, 1, null_mut()) };

    let sa = SockaddrIn6::v4([0, 0, 0, 0], port as u16);
    let flags = if dgram { NI_DGRAM } else { 0 };
    // SAFETY: `sa` is a live local, and `text` has `room` writable bytes.
    let code = unsafe {
        getnameinfo_from(
            src,
            sa.as_ptr(),
            SOCKADDR_IN_LEN,
            null_mut(),
            0,
            text,
            room as c_uint,
            flags,
        )
    };
    match code {
        0 => {}
        EAI_MEMORY | EAI_SYSTEM => return errno::ENOMEM,
        EAI_OVERFLOW => return errno::ERANGE,
        _ => return errno::ENOENT,
    }
    // A numeric port string is not a service record.
    // SAFETY: `getnameinfo` left a NUL-terminated string.
    let named = unsafe { c_bytes(text) };
    let (value, end) = strtoul(named, 0, 10);
    if end != 0 && value == u64::from(u16::from_be(port as u16)) {
        return errno::ENOENT;
    }

    let entry = Servent {
        s_name: text,
        s_aliases: aliases,
        s_port: port,
        s_proto: prots.cast_mut(),
    };
    // SAFETY: the caller passes a writable `struct servent`.
    unsafe { se.write(entry) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(se) };
    0
}

/// Names the service on `port`, given in network byte order, for the protocol
/// `prots`. Returns 0 with `*res` set, or the error number.
///
/// # Safety
///
/// As [`getservbyport_r_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyport_r(
    port: c_int,
    prots: *const c_char,
    se: *mut Servent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Servent,
) -> c_int {
    // SAFETY: the caller's contract is `getservbyport_r_from`'s.
    unsafe { getservbyport_r_from(&SYSTEM, port, prots, se, buf, buflen, res) }
}

/// What `getservbyname` and `getservbyport` return: the entry, and the two
/// alias pointers and the name it points at.
#[derive(Debug)]
struct ServStorage {
    /// The entry returned.
    entry: Servent,
    /// The buffer the `_r` forms lay out, pointer-aligned.
    buf: [usize; 8],
}

/// The storage `getservbyname` and `getservbyport` return.
static SERV: Shared<ServStorage> = Shared(UnsafeCell::new(ServStorage {
    entry: Servent {
        s_name: null_mut(),
        s_aliases: null_mut(),
        s_port: 0,
        s_proto: null_mut(),
    },
    buf: [0; 8],
}));

/// The service storage, for the length of one call.
fn serv_storage() -> &'static mut ServStorage {
    // SAFETY: see `Shared`.
    unsafe { &mut *SERV.0.get() }
}

/// Looks up the service `name` for the protocol `prots`, in static storage
/// the next call overwrites, or null.
///
/// # Safety
///
/// `name` must be a NUL-terminated string that outlives the answer, which
/// points into it, and `prots` null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyname(name: *const c_char, prots: *const c_char) -> *mut Servent {
    let storage = serv_storage();
    let mut found: *mut Servent = null_mut();
    // SAFETY: the caller's contract, with the static entry and buffer.
    let error = unsafe {
        getservbyname_r(
            name,
            prots,
            &raw mut storage.entry,
            storage.buf.as_mut_ptr().cast(),
            size_of::<[usize; 8]>(),
            &raw mut found,
        )
    };
    if error == 0 { found } else { null_mut() }
}

/// Names the service on `port`, given in network byte order, for the protocol
/// `prots`, in static storage the next call overwrites, or null.
///
/// # Safety
///
/// `prots` must be null or a NUL-terminated string that outlives the answer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyport(port: c_int, prots: *const c_char) -> *mut Servent {
    let storage = serv_storage();
    let mut found: *mut Servent = null_mut();
    // SAFETY: the caller's contract, with the static entry and buffer.
    let error = unsafe {
        getservbyport_r(
            port,
            prots,
            &raw mut storage.entry,
            storage.buf.as_mut_ptr().cast(),
            size_of::<[usize; 8]>(),
            &raw mut found,
        )
    };
    if error == 0 { found } else { null_mut() }
}

// ---------------------------------------------------------------------------
// The cursors
// ---------------------------------------------------------------------------

/// How many white-space separated fields of a line an entry keeps: an entry
/// with more aliases than this loses the ones past it, as glibc's fixed
/// `getent` buffers lose them.
const FIELDS: usize = 16;

/// Splits `line`, which ends in a NUL, into white-space separated fields,
/// each ended with a NUL, and records where each starts. A `#` and what
/// follows it is a comment. Returns how many fields there are.
fn split(line: &mut [u8], starts: &mut [usize; FIELDS]) -> usize {
    if let Some(slot) = line
        .iter()
        .position(|&byte| byte == b'#')
        .and_then(|hash| line.get_mut(hash))
    {
        *slot = 0;
    }
    let mut count = 0;
    let mut index = 0;
    while at(line, index) != 0 {
        if is_space(at(line, index)) {
            if let Some(slot) = line.get_mut(index) {
                *slot = 0;
            }
            index += 1;
            continue;
        }
        let Some(slot) = starts.get_mut(count) else {
            break;
        };
        *slot = index;
        count += 1;
        while at(line, index) != 0 && !is_space(at(line, index)) {
            index += 1;
        }
    }
    count
}

/// A database file read entry by entry.
#[derive(Debug)]
struct Cursor {
    /// The file, or null before the first read and after `end*ent`.
    stream: *mut File,
    /// The line the entry's strings are in.
    line: Line,
    /// Where each field of that line starts.
    starts: [usize; FIELDS],
    /// How many fields it has.
    count: usize,
    /// The alias array an entry points at, ending with null.
    aliases: [*mut c_char; FIELDS],
}

impl Cursor {
    /// Nothing read yet.
    const EMPTY: Self = Self {
        stream: null_mut(),
        line: Line::EMPTY,
        starts: [0; FIELDS],
        count: 0,
        aliases: [null_mut(); FIELDS],
    };

    /// Reads the next line of `path` with at least `least` fields, splitting
    /// it. `false` at the end, or when the file is not there.
    fn next(&mut self, path: &CStr, least: usize) -> bool {
        if self.stream.is_null() {
            match open_database(path) {
                Ok(Some(stream)) => self.stream = stream,
                Ok(None) => return false,
                Err(error) => {
                    errno::set(error);
                    return false;
                }
            }
        }
        let stream = self.stream;
        loop {
            // SAFETY: the stream is the file opened here or by an earlier
            // call, and is live until `close`.
            let Ok(Some(len)) = (unsafe { self.line.read(stream) }) else {
                return false;
            };
            // SAFETY: the line holds `len` bytes and a NUL.
            let bytes = unsafe { self.line.bytes(len) };
            self.count = split(bytes, &mut self.starts);
            if self.count >= least {
                return true;
            }
        }
    }

    /// Closes the file, so that the next read starts again.
    fn close(&mut self) {
        if !self.stream.is_null() {
            // SAFETY: the stream is live, and forgotten here.
            let _ = unsafe { fclose(self.stream) };
            self.stream = null_mut();
        }
    }

    /// Field `index` of the line, or null.
    fn field(&self, index: usize) -> *mut c_char {
        match self.starts.get(index) {
            Some(&start) if index < self.count => self.line.ptr.wrapping_add(start),
            _ => null_mut(),
        }
    }

    /// Field `index`'s bytes, without its NUL.
    fn field_bytes(&self, index: usize) -> &[u8] {
        let field = self.field(index);
        if field.is_null() {
            return &[];
        }
        // SAFETY: the field is a NUL-terminated string in the line buffer,
        // which lives until the next read.
        unsafe { c_bytes(field) }
    }

    /// The alias array over the fields from `from`, ending with null.
    fn alias_array(&mut self, from: usize) -> *mut *mut c_char {
        let base = self.line.ptr;
        let mut slot = 0;
        for index in from..self.count {
            let start = self.starts.get(index).copied().unwrap_or(0);
            let Some(entry) = self.aliases.get_mut(slot) else {
                break;
            };
            *entry = base.wrapping_add(start);
            slot += 1;
        }
        if let Some(entry) = self.aliases.get_mut(slot) {
            *entry = null_mut();
        }
        self.aliases.as_mut_ptr()
    }
}

/// The state `sethostent`, `gethostent` and `endhostent` share.
#[derive(Debug)]
struct HostState {
    cursor: Cursor,
    entry: Hostent,
    /// The address the entry's one address pointer points at.
    addr: [u8; 16],
    /// That pointer, and the null after it.
    addr_list: [*mut c_char; 2],
}

/// The state of the hosts cursor.
static HOSTS: Shared<HostState> = Shared(UnsafeCell::new(HostState {
    cursor: Cursor::EMPTY,
    entry: Hostent {
        h_name: null_mut(),
        h_aliases: null_mut(),
        h_addrtype: 0,
        h_length: 0,
        h_addr_list: null_mut(),
    },
    addr: [0; 16],
    addr_list: [null_mut(); 2],
}));

/// The hosts cursor, for the length of one call.
fn hosts() -> &'static mut HostState {
    // SAFETY: see `Shared`.
    unsafe { &mut *HOSTS.0.get() }
}

/// Starts `gethostent` again from the first entry of `/etc/hosts`. The
/// `stayopen` argument is ignored.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sethostent(stayopen: c_int) {
    let _ = stayopen;
    hosts().cursor.close();
}

/// Closes `/etc/hosts`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endhostent() {
    hosts().cursor.close();
}

/// The next entry of `src`'s hosts file: an address and the names on its
/// line, in static storage the next call overwrites. Null at the end, or when
/// the file is not there.
pub fn gethostent_from(src: &Sources<'_>) -> *mut Hostent {
    let state = hosts();
    // An entry is an address and at least one name.
    while state.cursor.next(src.hosts, 2) {
        let mut iplit = Address::default();
        if ipliteral(&mut iplit, state.cursor.field_bytes(0), AF_UNSPEC) <= 0 {
            continue;
        }
        state.addr = iplit.addr;
        state.addr_list = [state.addr.as_mut_ptr().cast(), null_mut()];
        state.entry = Hostent {
            h_name: state.cursor.field(1),
            h_aliases: state.cursor.alias_array(2),
            h_addrtype: iplit.family,
            h_length: if iplit.family == AF_INET6 { 16 } else { 4 },
            h_addr_list: state.addr_list.as_mut_ptr(),
        };
        return &raw mut state.entry;
    }
    null_mut()
}

/// The next entry of `/etc/hosts`, in static storage the next call
/// overwrites. Null at the end, or when the file is not there.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gethostent() -> *mut Hostent {
    gethostent_from(&SYSTEM)
}

/// The network number `text` names, as `inet_network` reads it: up to four
/// parts separated by dots, each in decimal, octal or hexadecimal, packed
/// into the low bytes without the shift `inet_addr` applies.
fn network_number(text: &[u8]) -> Option<u32> {
    let mut value: u32 = 0;
    let mut parts = 0;
    let mut from = 0;
    loop {
        let (part, end) = strtoul(text, from, 0);
        if end == from || part > 255 || parts == 4 {
            return None;
        }
        value = (value << 8) | part as u32;
        parts += 1;
        from = end;
        if at(text, from) != b'.' {
            break;
        }
        from += 1;
    }
    (at(text, from) == 0).then_some(value)
}

/// The state `setnetent`, `getnetent` and `endnetent` share.
#[derive(Debug)]
struct NetState {
    cursor: Cursor,
    entry: Netent,
}

/// The state of the networks cursor.
static NETS: Shared<NetState> = Shared(UnsafeCell::new(NetState {
    cursor: Cursor::EMPTY,
    entry: Netent {
        n_name: null_mut(),
        n_aliases: null_mut(),
        n_addrtype: 0,
        n_net: 0,
    },
}));

/// The networks cursor, for the length of one call.
fn nets() -> &'static mut NetState {
    // SAFETY: see `Shared`.
    unsafe { &mut *NETS.0.get() }
}

/// Starts `getnetent` again from the first entry of `/etc/networks`. The
/// `stayopen` argument is ignored.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setnetent(stayopen: c_int) {
    let _ = stayopen;
    nets().cursor.close();
}

/// Closes `/etc/networks`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endnetent() {
    nets().cursor.close();
}

/// The next entry of `src`'s networks file, in static storage the next call
/// overwrites. Null at the end, or when the file is not there.
pub fn getnetent_from(src: &Sources<'_>) -> *mut Netent {
    let state = nets();
    // An entry is a name and a network number.
    while state.cursor.next(src.networks, 2) {
        let Some(net) = network_number(state.cursor.field_bytes(1)) else {
            continue;
        };
        state.entry = Netent {
            n_name: state.cursor.field(0),
            n_aliases: state.cursor.alias_array(2),
            n_addrtype: AF_INET,
            n_net: net,
        };
        return &raw mut state.entry;
    }
    null_mut()
}

/// The next entry of `/etc/networks`, in static storage the next call
/// overwrites. Null at the end, or when the file is not there.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getnetent() -> *mut Netent {
    getnetent_from(&SYSTEM)
}

/// The entry of `src`'s networks file named `name`, by its official name or
/// one of its aliases, or null. It starts from the first entry and closes the
/// file, so it disturbs no cursor `getnetent` is holding.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
pub unsafe fn getnetbyname_from(src: &Sources<'_>, name: *const c_char) -> *mut Netent {
    endnetent();
    loop {
        let entry = getnetent_from(src);
        if entry.is_null() {
            return null_mut();
        }
        // SAFETY: the entry is the static one `getnetent` filled.
        let found = unsafe { entry.read() };
        // SAFETY: both are NUL-terminated strings, and the caller's outlives
        // the call.
        if unsafe { names_match(name, found.n_name, found.n_aliases) } {
            return entry;
        }
    }
}

/// The entry of `/etc/networks` named `name`, or null.
///
/// # Safety
///
/// As [`getnetbyname_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getnetbyname(name: *const c_char) -> *mut Netent {
    // SAFETY: the caller's contract is `getnetbyname_from`'s.
    unsafe { getnetbyname_from(&SYSTEM, name) }
}

/// The entry of `src`'s networks file whose number is `net`, of the address
/// type `kind`, or null.
pub fn getnetbyaddr_from(src: &Sources<'_>, net: u32, kind: c_int) -> *mut Netent {
    endnetent();
    loop {
        let entry = getnetent_from(src);
        if entry.is_null() {
            return null_mut();
        }
        // SAFETY: the entry is the static one `getnetent` filled.
        let found = unsafe { entry.read() };
        if found.n_net == net && found.n_addrtype == kind {
            return entry;
        }
    }
}

/// The entry of `/etc/networks` whose number is `net`, of the address type
/// `kind`, or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getnetbyaddr(net: u32, kind: c_int) -> *mut Netent {
    getnetbyaddr_from(&SYSTEM, net, kind)
}

/// Whether `name` is `official` or one of the null-ended `aliases`.
///
/// # Safety
///
/// All three must be NUL-terminated strings, and `aliases` a null-ended array
/// of them.
unsafe fn names_match(
    name: *const c_char,
    official: *mut c_char,
    aliases: *mut *mut c_char,
) -> bool {
    // SAFETY: both are NUL-terminated strings.
    if !official.is_null() && unsafe { strcmp(name, official) } == 0 {
        return true;
    }
    let mut index = 0;
    loop {
        // SAFETY: the array ends with a null pointer.
        let alias = unsafe { aliases.wrapping_add(index).read() };
        if alias.is_null() {
            return false;
        }
        // SAFETY: both are NUL-terminated strings.
        if unsafe { strcmp(name, alias) } == 0 {
            return true;
        }
        index += 1;
    }
}

/// The state `setservent`, `getservent` and `endservent` share.
#[derive(Debug)]
struct ServState {
    cursor: Cursor,
    entry: Servent,
}

/// The state of the services cursor.
static SERVS: Shared<ServState> = Shared(UnsafeCell::new(ServState {
    cursor: Cursor::EMPTY,
    entry: Servent {
        s_name: null_mut(),
        s_aliases: null_mut(),
        s_port: 0,
        s_proto: null_mut(),
    },
}));

/// The services cursor, for the length of one call.
fn servs() -> &'static mut ServState {
    // SAFETY: see `Shared`.
    unsafe { &mut *SERVS.0.get() }
}

/// Starts `getservent` again from the first entry of `/etc/services`. The
/// `stayopen` argument is ignored.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setservent(stayopen: c_int) {
    let _ = stayopen;
    servs().cursor.close();
}

/// Closes `/etc/services`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endservent() {
    servs().cursor.close();
}

/// The next entry of `src`'s services file, in static storage the next call
/// overwrites. Null at the end, or when the file is not there.
pub fn getservent_from(src: &Sources<'_>) -> *mut Servent {
    let state = servs();
    // An entry is a name and a `port/protocol`.
    while state.cursor.next(src.services, 2) {
        let port_field = state.cursor.field_bytes(1);
        let (port, end) = strtoul(port_field, 0, 10);
        if end == 0 || port > 65535 || at(port_field, end) != b'/' {
            continue;
        }
        let proto = state.cursor.field(1).wrapping_add(end + 1);
        // The slash becomes the port's NUL, so the protocol stands alone.
        // SAFETY: the slash is inside the line buffer, which is writable.
        unsafe { state.cursor.field(1).wrapping_add(end).write(0) };
        state.entry = Servent {
            s_name: state.cursor.field(0),
            s_aliases: state.cursor.alias_array(2),
            s_port: c_int::from((port as u16).to_be()),
            s_proto: proto,
        };
        return &raw mut state.entry;
    }
    null_mut()
}

/// The next entry of `/etc/services`, in static storage the next call
/// overwrites. Null at the end, or when the file is not there.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getservent() -> *mut Servent {
    getservent_from(&SYSTEM)
}

/// musl's table of protocol numbers and names, from its `proto.c`, which
/// answers when `/etc/protocols` is not there. Each entry is the number, the
/// name, and a NUL.
static PROTOS: &[u8] = b"\x00ip\0\x01icmp\0\x02igmp\0\x03ggp\0\x04ipencap\0\x05st\0\x06tcp\0\
\x08egp\0\x0cpup\0\x11udp\0\x14hmp\0\x16xns-idp\0\x1brdp\0\x1diso-tp4\0\x24xtp\0\x25ddp\0\
\x26idpr-cmtp\0\x29ipv6\0\x2bipv6-route\0\x2cipv6-frag\0\x2didrp\0\x2ersvp\0\x2fgre\0\x32esp\0\
\x33ah\0\x39skip\0\x3aipv6-icmp\0\x3bipv6-nonxt\0\x3cipv6-opts\0\x49rspf\0\x51vmtp\0\x59ospf\0\
\x5eipip\0\x62encap\0\x67pim\0\xffraw\0";

/// The state `setprotoent`, `getprotoent` and `endprotoent` share.
#[derive(Debug)]
struct ProtoState {
    cursor: Cursor,
    entry: Protoent,
    /// Where the built-in table has got to, once the file is exhausted.
    index: usize,
    /// An entry from the table has no aliases.
    none: [*mut c_char; 1],
}

/// The state of the protocols cursor.
static PROTOENT: Shared<ProtoState> = Shared(UnsafeCell::new(ProtoState {
    cursor: Cursor::EMPTY,
    entry: Protoent {
        p_name: null_mut(),
        p_aliases: null_mut(),
        p_proto: 0,
    },
    index: 0,
    none: [null_mut(); 1],
}));

/// The protocols cursor, for the length of one call.
fn protos() -> &'static mut ProtoState {
    // SAFETY: see `Shared`.
    unsafe { &mut *PROTOENT.0.get() }
}

/// Starts `getprotoent` again from the first entry. The `stayopen` argument
/// is ignored.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setprotoent(stayopen: c_int) {
    let _ = stayopen;
    endprotoent();
}

/// Closes `/etc/protocols` and starts the built-in table again.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endprotoent() {
    let state = protos();
    state.cursor.close();
    state.index = 0;
}

/// The next entry of `src`'s protocols file, or of the built-in table once
/// the file has none, in static storage the next call overwrites. Null at the
/// end.
pub fn getprotoent_from(src: &Sources<'_>) -> *mut Protoent {
    let state = protos();
    // An entry is a name and a number.
    while state.cursor.next(src.protocols, 2) {
        let (proto, end) = strtoul(state.cursor.field_bytes(1), 0, 10);
        if end == 0 || proto > 255 {
            continue;
        }
        state.entry = Protoent {
            p_name: state.cursor.field(0),
            p_aliases: state.cursor.alias_array(2),
            p_proto: proto as c_int,
        };
        return &raw mut state.entry;
    }
    // The table, as musl's `getprotoent` walks it.
    let Some(&proto) = PROTOS.get(state.index) else {
        return null_mut();
    };
    let name = PROTOS.get(state.index + 1..).unwrap_or_default();
    let len = c_len(name);
    state.none = [null_mut(); 1];
    state.entry = Protoent {
        p_name: PROTOS
            .as_ptr()
            .wrapping_add(state.index + 1)
            .cast_mut()
            .cast(),
        p_aliases: state.none.as_mut_ptr(),
        p_proto: c_int::from(proto),
    };
    state.index += len + 2;
    &raw mut state.entry
}

/// The next entry of `/etc/protocols`, or of the built-in table once the file
/// has none, in static storage the next call overwrites. Null at the end.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getprotoent() -> *mut Protoent {
    getprotoent_from(&SYSTEM)
}

/// The protocol of `src` named `name`, by its official name or one of its
/// aliases, or null. It starts from the first entry, as musl's does.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
pub unsafe fn getprotobyname_from(src: &Sources<'_>, name: *const c_char) -> *mut Protoent {
    endprotoent();
    loop {
        let entry = getprotoent_from(src);
        if entry.is_null() {
            return null_mut();
        }
        // SAFETY: the entry is the static one `getprotoent` filled.
        let found = unsafe { entry.read() };
        // SAFETY: the caller passes a NUL-terminated string, and the entry's
        // names are NUL-terminated.
        if unsafe { names_match(name, found.p_name, found.p_aliases) } {
            return entry;
        }
    }
}

/// The protocol named `name`, or null.
///
/// # Safety
///
/// As [`getprotobyname_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getprotobyname(name: *const c_char) -> *mut Protoent {
    // SAFETY: the caller's contract is `getprotobyname_from`'s.
    unsafe { getprotobyname_from(&SYSTEM, name) }
}

/// The protocol of `src` numbered `num`, or null. It starts from the first
/// entry, as musl's does.
pub fn getprotobynumber_from(src: &Sources<'_>, num: c_int) -> *mut Protoent {
    endprotoent();
    loop {
        let entry = getprotoent_from(src);
        if entry.is_null() {
            return null_mut();
        }
        // SAFETY: the entry is the static one `getprotoent` filled.
        let found = unsafe { entry.read() };
        if found.p_proto == num {
            return entry;
        }
    }
}

/// The protocol numbered `num`, or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getprotobynumber(num: c_int) -> *mut Protoent {
    getprotobynumber_from(&SYSTEM, num)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netdb::testing::{Paths, fixture};
    use crate::netdb::{NO_DATA, h_errno};

    /// The strings of a null-ended array of C strings.
    ///
    /// # Safety
    ///
    /// `array` must be null or a null-ended array of NUL-terminated strings.
    unsafe fn strings(array: *mut *mut c_char) -> Vec<String> {
        let mut out = Vec::new();
        if array.is_null() {
            return out;
        }
        let mut index = 0;
        loop {
            // SAFETY: the array ends with a null pointer.
            let entry = unsafe { array.wrapping_add(index).read() };
            if entry.is_null() {
                return out;
            }
            // SAFETY: each entry is a NUL-terminated string.
            out.push(
                unsafe { CStr::from_ptr(entry) }
                    .to_string_lossy()
                    .into_owned(),
            );
            index += 1;
        }
    }

    /// A C string's bytes.
    ///
    /// # Safety
    ///
    /// `text` must be null or a NUL-terminated string.
    unsafe fn text(text: *mut c_char) -> String {
        if text.is_null() {
            return String::new();
        }
        // SAFETY: the caller passes a NUL-terminated string.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }

    /// The name, aliases and addresses of a `struct hostent`.
    ///
    /// # Safety
    ///
    /// `h` must be a filled `struct hostent`.
    unsafe fn host(h: *mut Hostent) -> (String, Vec<String>, c_int, Vec<Vec<u8>>) {
        // SAFETY: the caller passes a filled entry.
        let entry = unsafe { h.read() };
        let length = entry.h_length as usize;
        let mut addrs = Vec::new();
        let mut index = 0;
        loop {
            // SAFETY: the list ends with a null pointer.
            let address = unsafe { entry.h_addr_list.wrapping_add(index).read() };
            if address.is_null() {
                break;
            }
            // SAFETY: each address is `h_length` bytes.
            let bytes = unsafe { core::slice::from_raw_parts(address.cast::<u8>(), length) };
            addrs.push(bytes.to_vec());
            index += 1;
        }
        (
            // SAFETY: the name is a NUL-terminated string.
            unsafe { text(entry.h_name) },
            // SAFETY: the aliases end with a null pointer.
            unsafe { strings(entry.h_aliases) },
            entry.h_addrtype,
            addrs,
        )
    }

    #[test]
    fn a_host_lookup_packs_its_answer_into_the_buffer() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut entry = Hostent {
            h_name: null_mut(),
            h_aliases: null_mut(),
            h_addrtype: 0,
            h_length: 0,
            h_addr_list: null_mut(),
        };
        let mut buf = [0 as c_char; 512];
        let mut found: *mut Hostent = null_mut();
        let mut err = 0;
        // SAFETY: the name is NUL-terminated, and the rest are live locals.
        let code = unsafe {
            gethostbyname2_r_from(
                &src,
                c"multi.example.test".as_ptr(),
                AF_INET,
                &raw mut entry,
                buf.as_mut_ptr(),
                512,
                &raw mut found,
                &raw mut err,
            )
        };
        assert_eq!((code, found), (0, &raw mut entry));
        // SAFETY: the entry was filled.
        let (name, aliases, family, addrs) = unsafe { host(found) };
        assert_eq!(name, "multi.example.test");
        // The name asked for is the canonical name, so there is no alias.
        assert_eq!(aliases, ["multi.example.test"]);
        assert_eq!(family, AF_INET);
        assert_eq!(addrs, [vec![192, 0, 2, 20], vec![192, 0, 2, 21]]);

        // A name the hosts file gives another canonical name keeps both.
        // SAFETY: as above.
        let code = unsafe {
            gethostbyname2_r_from(
                &src,
                c"alias".as_ptr(),
                AF_INET,
                &raw mut entry,
                buf.as_mut_ptr(),
                512,
                &raw mut found,
                &raw mut err,
            )
        };
        assert_eq!(code, 0);
        // SAFETY: the entry was filled.
        let (name, aliases, _, _) = unsafe { host(found) };
        assert_eq!(name, "server.example.test");
        assert_eq!(aliases, ["server.example.test", "alias"]);

        // A buffer too small says so, and a name nothing knows says why.
        // SAFETY: as above, with 16 bytes of buffer.
        let code = unsafe {
            gethostbyname2_r_from(
                &src,
                c"multi.example.test".as_ptr(),
                AF_INET,
                &raw mut entry,
                buf.as_mut_ptr(),
                16,
                &raw mut found,
                &raw mut err,
            )
        };
        assert_eq!(code, errno::ERANGE);
        // SAFETY: as above.
        let code = unsafe {
            gethostbyname2_r_from(
                &src,
                c"six.example.test".as_ptr(),
                AF_INET,
                &raw mut entry,
                buf.as_mut_ptr(),
                512,
                &raw mut found,
                &raw mut err,
            )
        };
        assert_eq!((code, err, found), (0, NO_DATA, null_mut()));
    }

    #[test]
    fn the_static_host_lookups_grow_their_buffer() {
        // SAFETY: the name is NUL-terminated.
        let entry = unsafe { gethostbyname(c"127.0.0.1".as_ptr()) };
        assert!(!entry.is_null());
        // SAFETY: the entry was filled.
        let (name, _, family, addrs) = unsafe { host(entry) };
        assert_eq!((name.as_str(), family), ("127.0.0.1", AF_INET));
        assert_eq!(addrs, [vec![127, 0, 0, 1]]);
        // SAFETY: as above.
        let entry = unsafe { gethostbyname2(c"localhost".as_ptr(), AF_INET6) };
        assert!(!entry.is_null());
        // SAFETY: the entry was filled.
        let (name, _, family, addrs) = unsafe { host(entry) };
        assert_eq!((name.as_str(), family), ("localhost", AF_INET6));
        assert_eq!(
            addrs,
            [vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]]
        );
        // A name that resolves to nothing sets `h_errno` and answers null.
        // SAFETY: as above.
        let entry = unsafe { gethostbyname2(c"".as_ptr(), AF_INET) };
        assert!(entry.is_null());
        assert_eq!(h_errno(), HOST_NOT_FOUND);
    }

    #[test]
    fn an_address_lookup_names_it_from_the_hosts_file() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut entry = Hostent {
            h_name: null_mut(),
            h_aliases: null_mut(),
            h_addrtype: 0,
            h_length: 0,
            h_addr_list: null_mut(),
        };
        let mut buf = [0 as c_char; 256];
        let mut found: *mut Hostent = null_mut();
        let mut err = 0;
        let address = [192u8, 0, 2, 10];
        // SAFETY: the address has four bytes and the rest are live locals.
        let code = unsafe {
            gethostbyaddr_r_from(
                &src,
                address.as_ptr().cast(),
                4,
                AF_INET,
                &raw mut entry,
                buf.as_mut_ptr(),
                256,
                &raw mut found,
                &raw mut err,
            )
        };
        assert_eq!(code, 0);
        // SAFETY: the entry was filled.
        let (name, aliases, family, addrs) = unsafe { host(found) };
        assert_eq!(name, "server.example.test");
        assert_eq!(aliases, ["server.example.test"]);
        assert_eq!((family, addrs), (AF_INET, vec![vec![192, 0, 2, 10]]));

        // A length that does not match the family is refused.
        // SAFETY: as above.
        let code = unsafe {
            gethostbyaddr_r_from(
                &src,
                address.as_ptr().cast(),
                4,
                AF_INET6,
                &raw mut entry,
                buf.as_mut_ptr(),
                256,
                &raw mut found,
                &raw mut err,
            )
        };
        assert_eq!((code, err, found), (errno::EINVAL, NO_RECOVERY, null_mut()));
        // So is a buffer with no room for the name.
        // SAFETY: as above, with 48 bytes of buffer.
        let code = unsafe {
            gethostbyaddr_r_from(
                &src,
                address.as_ptr().cast(),
                4,
                AF_INET,
                &raw mut entry,
                buf.as_mut_ptr(),
                6 * size_of::<usize>(),
                &raw mut found,
                &raw mut err,
            )
        };
        assert_eq!(code, errno::ERANGE);
    }

    #[test]
    fn the_service_lookups_read_the_services_file() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut entry = Servent {
            s_name: null_mut(),
            s_aliases: null_mut(),
            s_port: 0,
            s_proto: null_mut(),
        };
        let mut buf = [0usize; 8];
        let mut found: *mut Servent = null_mut();
        let by_name = |name: &CStr,
                       prots: *const c_char,
                       entry: &mut Servent,
                       buf: &mut [usize; 8],
                       found: &mut *mut Servent| {
            // SAFETY: the strings are NUL-terminated and the rest are live.
            unsafe {
                getservbyname_r_from(
                    &src,
                    name.as_ptr(),
                    prots,
                    &raw mut *entry,
                    buf.as_mut_ptr().cast(),
                    size_of::<[usize; 8]>(),
                    &raw mut *found,
                )
            }
        };
        assert_eq!(
            by_name(c"http", TCP.as_ptr(), &mut entry, &mut buf, &mut found),
            0
        );
        assert_eq!(entry.s_port, c_int::from(8080u16.to_be()));
        // SAFETY: the entry was filled.
        assert_eq!(unsafe { text(entry.s_proto) }, "tcp");
        // SAFETY: as above.
        assert_eq!(unsafe { text(entry.s_name) }, "http");
        // SAFETY: as above.
        assert_eq!(unsafe { strings(entry.s_aliases) }, ["http"]);

        // A number is a port, not a service; an unknown protocol is refused;
        // and a name nothing knows is not found.
        assert_eq!(
            by_name(c"80", TCP.as_ptr(), &mut entry, &mut buf, &mut found),
            errno::ENOENT
        );
        assert_eq!(
            by_name(c"http", c"sctp".as_ptr(), &mut entry, &mut buf, &mut found),
            errno::EINVAL
        );
        assert_eq!(
            by_name(c"nothing", null_mut(), &mut entry, &mut buf, &mut found),
            errno::ENOENT
        );
        // syslog is in the file for UDP alone.
        assert_eq!(
            by_name(c"syslog", TCP.as_ptr(), &mut entry, &mut buf, &mut found),
            errno::ENOENT
        );
        assert_eq!(
            by_name(c"syslog", UDP.as_ptr(), &mut entry, &mut buf, &mut found),
            0
        );
        assert_eq!(entry.s_port, c_int::from(514u16.to_be()));
    }

    #[test]
    fn a_port_is_named_for_the_protocol_asked_for() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut entry = Servent {
            s_name: null_mut(),
            s_aliases: null_mut(),
            s_port: 0,
            s_proto: null_mut(),
        };
        let mut buf = [0usize; 8];
        let mut found: *mut Servent = null_mut();
        let by_port = |port: u16,
                       prots: *const c_char,
                       entry: &mut Servent,
                       buf: &mut [usize; 8],
                       found: &mut *mut Servent| {
            // SAFETY: the strings are NUL-terminated or null, and the rest
            // are live locals.
            unsafe {
                getservbyport_r_from(
                    &src,
                    c_int::from(port.to_be()),
                    prots,
                    &raw mut *entry,
                    buf.as_mut_ptr().cast(),
                    size_of::<[usize; 8]>(),
                    &raw mut *found,
                )
            }
        };
        assert_eq!(
            by_port(8080, TCP.as_ptr(), &mut entry, &mut buf, &mut found),
            0
        );
        // SAFETY: the entry was filled.
        assert_eq!(unsafe { text(entry.s_name) }, "http");
        // SAFETY: as above.
        assert_eq!(unsafe { text(entry.s_proto) }, "tcp");
        assert_eq!(entry.s_port, c_int::from(8080u16.to_be()));
        // Without a protocol, TCP is tried and then UDP.
        assert_eq!(
            by_port(514, null_mut(), &mut entry, &mut buf, &mut found),
            0
        );
        // SAFETY: as above.
        assert_eq!(unsafe { text(entry.s_proto) }, "udp");
        // A port the file does not name is not a service.
        assert_eq!(
            by_port(9999, TCP.as_ptr(), &mut entry, &mut buf, &mut found),
            errno::ENOENT
        );
        assert_eq!(
            by_port(8080, c"sctp".as_ptr(), &mut entry, &mut buf, &mut found),
            errno::EINVAL
        );
    }

    #[test]
    fn the_hosts_cursor_reads_every_line_in_turn() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        endhostent();
        let mut seen = Vec::new();
        loop {
            let entry = gethostent_from(&src);
            if entry.is_null() {
                break;
            }
            // SAFETY: the entry is the static one the cursor filled.
            let (name, aliases, family, addrs) = unsafe { host(entry) };
            seen.push((name, aliases, family, addrs));
        }
        endhostent();
        assert_eq!(seen.len(), 5);
        assert_eq!(
            seen.first().map(|entry| (entry.0.clone(), entry.1.clone())),
            Some(("server.example.test".into(), vec!["alias".to_owned()]))
        );
        assert_eq!(
            seen.last().map(|entry| (entry.0.clone(), entry.2)),
            Some(("six.example.test".into(), AF_INET6))
        );
        // Starting again gives the first entry once more.
        sethostent(1);
        let entry = gethostent_from(&src);
        // SAFETY: the entry is the static one the cursor filled.
        assert_eq!(unsafe { host(entry) }.0, "server.example.test");
        endhostent();
        // A file that is not there is an empty database.
        let absent = Paths {
            hosts: fixture("absent"),
            ..Paths::new("resolv.conf")
        };
        assert!(gethostent_from(&absent.sources(9)).is_null());
        endhostent();
    }

    #[test]
    fn the_networks_cursor_reads_names_and_numbers() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        endnetent();
        let mut seen = Vec::new();
        loop {
            let entry = getnetent_from(&src);
            if entry.is_null() {
                break;
            }
            // SAFETY: the entry is the static one the cursor filled.
            let found = unsafe { entry.read() };
            // SAFETY: the name is NUL-terminated and the aliases end with null.
            seen.push((unsafe { text(found.n_name) }, found.n_net, unsafe {
                strings(found.n_aliases)
            }));
        }
        endnetent();
        assert_eq!(
            seen,
            [
                ("loopback".into(), 127, Vec::new()),
                (
                    "test-net".into(),
                    0x00c0_0002,
                    vec!["documentation".to_owned(), "example".to_owned()]
                ),
            ]
        );
        // SAFETY: the name is NUL-terminated.
        let entry = unsafe { getnetbyname_from(&src, c"example".as_ptr()) };
        assert!(!entry.is_null());
        // SAFETY: the entry is the static one the cursor filled.
        assert_eq!(unsafe { entry.read() }.n_net, 0x00c0_0002);
        let entry = getnetbyaddr_from(&src, 127, AF_INET);
        assert!(!entry.is_null());
        // SAFETY: as above.
        // SAFETY: as above.
        let found = unsafe { entry.read() };
        // SAFETY: the name is NUL-terminated.
        assert_eq!(unsafe { text(found.n_name) }, "loopback");
        assert!(getnetbyaddr_from(&src, 127, AF_INET6).is_null());
        // SAFETY: the name is NUL-terminated.
        assert!(unsafe { getnetbyname_from(&src, c"nothing".as_ptr()) }.is_null());
        endnetent();
    }

    #[test]
    fn the_services_cursor_splits_the_port_from_the_protocol() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        endservent();
        let mut seen = Vec::new();
        loop {
            let entry = getservent_from(&src);
            if entry.is_null() {
                break;
            }
            // SAFETY: the entry is the static one the cursor filled.
            let found = unsafe { entry.read() };
            seen.push((
                // SAFETY: the name is NUL-terminated.
                unsafe { text(found.s_name) },
                u16::from_be(found.s_port as u16),
                // SAFETY: the protocol is NUL-terminated.
                unsafe { text(found.s_proto) },
                // SAFETY: the aliases end with a null pointer.
                unsafe { strings(found.s_aliases) },
            ));
        }
        endservent();
        assert_eq!(
            seen,
            [
                ("http".into(), 8080, "tcp".into(), vec!["www".to_owned()]),
                ("http".into(), 8080, "udp".into(), vec!["www".to_owned()]),
                ("domain".into(), 5353, "tcp".into(), Vec::new()),
                ("domain".into(), 5353, "udp".into(), Vec::new()),
                ("syslog".into(), 514, "udp".into(), Vec::new()),
            ]
        );
    }

    #[test]
    fn the_protocols_cursor_prefers_the_file_and_falls_back_to_the_table() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        // SAFETY: the name is NUL-terminated.
        let entry = unsafe { getprotobyname_from(&src, c"hopopt".as_ptr()) };
        assert!(!entry.is_null());
        // SAFETY: the entry is the static one the cursor filled.
        assert_eq!(unsafe { entry.read() }.p_proto, 0);
        // The file's number for `tcp` is not the real one, so an answer of
        // 200 can only have come from the file.
        // SAFETY: as above.
        let entry = unsafe { getprotobyname_from(&src, c"TCP".as_ptr()) };
        assert!(!entry.is_null());
        // SAFETY: as above.
        assert_eq!(unsafe { entry.read() }.p_proto, 200);
        let entry = getprotobynumber_from(&src, 17);
        assert!(!entry.is_null());
        // SAFETY: as above.
        // SAFETY: as above.
        let found = unsafe { entry.read() };
        // SAFETY: the name is NUL-terminated.
        assert_eq!(unsafe { text(found.p_name) }, "udp");

        // Without a file, the table built into the library answers.
        let absent = Paths {
            protocols: fixture("absent"),
            ..Paths::new("resolv.conf")
        };
        let src = absent.sources(9);
        // SAFETY: the name is NUL-terminated.
        let entry = unsafe { getprotobyname_from(&src, c"tcp".as_ptr()) };
        assert!(!entry.is_null());
        // SAFETY: as above.
        assert_eq!(unsafe { entry.read() }.p_proto, 6);
        let entry = getprotobynumber_from(&src, 255);
        assert!(!entry.is_null());
        // SAFETY: as above.
        // SAFETY: as above.
        let found = unsafe { entry.read() };
        // SAFETY: the name is NUL-terminated.
        assert_eq!(unsafe { text(found.p_name) }, "raw");
        // SAFETY: as above.
        assert!(unsafe { getprotobyname_from(&src, c"nothing".as_ptr()) }.is_null());
        assert!(getprotobynumber_from(&src, 254).is_null());

        // The file's entries come first, then the table's.
        let src = paths.sources(9);
        endprotoent();
        let mut count = 0;
        while !getprotoent_from(&src).is_null() {
            count += 1;
        }
        endprotoent();
        assert_eq!(count, 3 + 36);
    }

    #[test]
    fn a_network_number_reads_as_inet_network_reads_it() {
        assert_eq!(network_number(b"127"), Some(127));
        assert_eq!(network_number(b"192.0.2"), Some(0x00c0_0002));
        assert_eq!(network_number(b"10.1.2.3"), Some(0x0a01_0203));
        assert_eq!(network_number(b"0x7f"), Some(127));
        assert_eq!(network_number(b"0177"), Some(127));
        assert_eq!(network_number(b""), None);
        assert_eq!(network_number(b"256"), None);
        assert_eq!(network_number(b"1.2.3.4.5"), None);
        assert_eq!(network_number(b"1."), None);
        assert_eq!(network_number(b"1x"), None);
    }
}
