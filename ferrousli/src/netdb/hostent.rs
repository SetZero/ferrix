//! The legacy lookups: `gethostbyname`, `gethostbyaddr`, `getservbyname`,
//! `getservbyport` and their `_r` forms.
//!
//! Ported from musl 1.2.5's `gethostbyname*.c`, `gethostbyaddr*.c`,
//! `getservbyname*.c` and `getservbyport*.c` (MIT). The `_r` forms lay their
//! results out in the caller's buffer, pointer arrays first; the others keep
//! one buffer of their own, growing it until the result fits, and are unsafe
//! to call from two threads at once, as in C.
//!
//! `gethostbyname` reports through `h_errno`, which every thread has its own
//! of, in its control block beside `errno`.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_uint};
use core::mem::{offset_of, size_of};
use core::ptr::null_mut;

use super::addrinfo::nameinfo;
use super::lookup::{Address, CANON, MAXADDRS, MAXSERVS, Service, lookup_name, lookup_serv};
use super::{
    __h_errno_location, AF_INET, AF_INET6, AI_CANONNAME, EAI_AGAIN, EAI_MEMORY, EAI_NODATA,
    EAI_NONAME, EAI_OVERFLOW, EAI_SYSTEM, HOST_NOT_FOUND, Hostent, IPPROTO_TCP, IPPROTO_UDP,
    NI_DGRAM, NO_DATA, NO_RECOVERY, SOCKADDR_IN_LEN, SOCKADDR_IN6_LEN, SYSTEM, Servent,
    SockaddrIn6, Sources, TRY_AGAIN, c_bytes, c_bytes_max, c_len, set_h_errno, strtoul,
};
use crate::errno;
use crate::malloc::{free, malloc};
use crate::pwd::{Shared, last_errno};

/// The size of a pointer in the caller's buffer.
const PTR: usize = size_of::<*mut c_char>();

/// Stores the pointer `value` at `at`.
///
/// # Safety
///
/// `at` must be valid for a write of a pointer.
#[allow(
    clippy::cast_ptr_alignment,
    reason = "the write is unaligned, so the buffer's alignment does not matter"
)]
unsafe fn put_ptr(at: *mut c_char, value: *mut c_char) {
    // SAFETY: the caller vouches for the room, and the write is unaligned.
    unsafe { at.cast::<*mut c_char>().write_unaligned(value) };
}

/// Copies the C string `bytes`, with a NUL, to `at`.
///
/// # Safety
///
/// `at` must be valid for writes of `bytes.len() + 1` bytes.
unsafe fn put_str(at: *mut c_char, bytes: &[u8]) {
    for (index, &byte) in bytes.iter().chain(&[0]).enumerate() {
        // SAFETY: the caller vouches for the room.
        unsafe { at.wrapping_add(index).write(byte as c_char) };
    }
}

/// Looks `name` up, as musl's `gethostbyname2_r` does, laying the result out
/// in the `buflen` bytes at `buf`. Returns 0 with `*res` set, `ERANGE` if the
/// buffer is too small, or another error number; `*err` holds the `h_errno`
/// value.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, `h` a writable `hostent`, `buf`
/// valid for writes of `buflen` bytes, and `res` and `err` writable.
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
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(null_mut()) };
    // SAFETY: the caller passes a NUL-terminated string.
    let wanted = unsafe { c_bytes_max(name, 255) };

    let mut addrs = [Address::default(); MAXADDRS];
    let mut canon = [0u8; CANON];
    let cnt = lookup_name(
        &SYSTEM,
        &mut addrs,
        &mut canon,
        Some(wanted),
        af,
        AI_CANONNAME,
    );
    if cnt < 0 {
        let (herrno, error) = match cnt {
            EAI_NONAME => (HOST_NOT_FOUND, 0),
            EAI_NODATA => (NO_DATA, 0),
            EAI_AGAIN => (TRY_AGAIN, errno::EAGAIN),
            EAI_SYSTEM => (NO_RECOVERY, last_errno()),
            _ => (NO_RECOVERY, errno::EBADMSG),
        };
        // SAFETY: the caller passes a writable pointer.
        unsafe { err.write(herrno) };
        return error;
    }
    let cnt = cnt as usize;
    let length = if af == AF_INET6 { 16 } else { 4 };
    // SAFETY: the caller passes a writable `hostent`.
    let addrtype = h
        .wrapping_byte_add(offset_of!(Hostent, h_addrtype))
        .cast::<c_int>();
    // SAFETY: the caller passes a writable `hostent`.
    unsafe { addrtype.write(af) };
    // SAFETY: as above.
    let host_length = h
        .wrapping_byte_add(offset_of!(Hostent, h_length))
        .cast::<c_int>();
    // SAFETY: as above.
    unsafe { host_length.write(length as c_int) };

    let align = buf.addr().wrapping_neg() & (PTR - 1);
    let canon_len = c_len(&canon);
    let need = 4 * PTR + (cnt + 1) * (PTR + length) + wanted.len() + 1 + canon_len + 1 + align;
    if need > buflen {
        return errno::ERANGE;
    }

    let aliases = buf.wrapping_add(align);
    let addr_list = aliases.wrapping_add(3 * PTR);
    let mut at = addr_list.wrapping_add((cnt + 1) * PTR);
    for (index, address) in addrs.iter().take(cnt).enumerate() {
        // SAFETY: the buffer holds the array, whose room was counted above.
        unsafe { put_ptr(addr_list.wrapping_add(index * PTR), at) };
        for (offset, &byte) in address.addr.iter().take(length).enumerate() {
            // SAFETY: as above, for this address's bytes.
            unsafe { at.wrapping_add(offset).write(byte as c_char) };
        }
        at = at.wrapping_add(length);
    }
    // SAFETY: as above.
    unsafe { put_ptr(addr_list.wrapping_add(cnt * PTR), null_mut()) };

    let official = at;
    // SAFETY: the canonical name and its NUL were counted above.
    unsafe { put_str(official, canon.get(..canon_len).unwrap_or_default()) };
    at = at.wrapping_add(canon_len + 1);
    // SAFETY: the caller passes a writable `hostent`.
    let host_name = h
        .wrapping_byte_add(offset_of!(Hostent, h_name))
        .cast::<*mut c_char>();
    // SAFETY: the caller passes a writable `hostent`.
    unsafe { host_name.write(official) };
    // SAFETY: the alias array is in the buffer.
    unsafe { put_ptr(aliases, official) };
    if canon.get(..canon_len) == Some(wanted) {
        // SAFETY: as above.
        unsafe { put_ptr(aliases.wrapping_add(PTR), null_mut()) };
    } else {
        // SAFETY: the name and its NUL were counted above.
        unsafe { put_str(at, wanted) };
        // SAFETY: the alias array is in the buffer.
        unsafe { put_ptr(aliases.wrapping_add(PTR), at) };
    }
    // SAFETY: as above.
    unsafe { put_ptr(aliases.wrapping_add(2 * PTR), null_mut()) };

    // SAFETY: the caller passes a writable `hostent`.
    let host_aliases = h
        .wrapping_byte_add(offset_of!(Hostent, h_aliases))
        .cast::<*mut *mut c_char>();
    // SAFETY: the caller passes a writable `hostent`.
    unsafe { host_aliases.write(aliases.cast()) };
    // SAFETY: as above.
    let addresses = h
        .wrapping_byte_add(offset_of!(Hostent, h_addr_list))
        .cast::<*mut *mut c_char>();
    // SAFETY: as above.
    unsafe { addresses.write(addr_list.cast()) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(h) };
    0
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

/// The buffer `gethostbyname2` returns its result in, which the next call
/// frees.
static HOST_BY_NAME: Shared<*mut Hostent> = Shared(UnsafeCell::new(null_mut()));
/// The buffer `gethostbyaddr` returns its result in.
static HOST_BY_ADDR: Shared<*mut Hostent> = Shared(UnsafeCell::new(null_mut()));

/// Calls `lookup` with a buffer that grows until the result fits, as musl's
/// `gethostbyname2` and `gethostbyaddr` do, keeping it in `store`.
///
/// # Safety
///
/// `lookup` must write its result into the `hostent` and buffer it is given,
/// and return `ERANGE` when they are too small.
unsafe fn with_buffer(
    store: &Shared<*mut Hostent>,
    mut lookup: impl FnMut(*mut Hostent, *mut c_char, usize, *mut *mut Hostent) -> c_int,
) -> *mut Hostent {
    // SAFETY: see `Shared`: one call reaches this at a time, as in C.
    let slot = unsafe { &mut *store.0.get() };
    let mut size = 63;
    loop {
        // SAFETY: the buffer is this function's own, from `malloc`.
        unsafe { free((*slot).cast()) };
        size = size + size + 1;
        let h = malloc(size).cast::<Hostent>();
        *slot = h;
        if h.is_null() {
            set_h_errno(NO_RECOVERY);
            return null_mut();
        }
        let mut res = null_mut();
        let buf = h.wrapping_add(1).cast::<c_char>();
        let error = lookup(h, buf, size - size_of::<Hostent>(), &raw mut res);
        if error != errno::ERANGE {
            return res;
        }
    }
}

/// Looks `name` up, returning a `hostent` in storage the next call to
/// `gethostbyname`, `gethostbyname2` or `gethostbyaddr` reuses, or null with
/// `h_errno` set.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, and no other thread may be in one
/// of these functions.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyname2(name: *const c_char, af: c_int) -> *mut Hostent {
    let lookup = |h, buf, buflen, res| {
        // SAFETY: the caller passes a NUL-terminated string, and the buffer is
        // this call's own.
        unsafe { gethostbyname2_r(name, af, h, buf, buflen, res, __h_errno_location()) }
    };
    // SAFETY: the lookup writes into what it is given.
    unsafe { with_buffer(&HOST_BY_NAME, lookup) }
}

/// `gethostbyname2` for `AF_INET`.
///
/// # Safety
///
/// As [`gethostbyname2`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyname(name: *const c_char) -> *mut Hostent {
    // SAFETY: the caller's contract is `gethostbyname2`'s.
    unsafe { gethostbyname2(name, AF_INET) }
}

/// Names the address `a`, `l` bytes of family `af`, as musl's
/// `gethostbyaddr_r` does, laying the result out in the `buflen` bytes at
/// `buf`.
///
/// # Safety
///
/// `a` must be valid for reads of `l` bytes, `h` a writable `hostent`, `buf`
/// valid for writes of `buflen` bytes, and `res` and `err` writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyaddr_r(
    a: *const c_char,
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
    let (sa, sl) = match (af, l) {
        (AF_INET6, 16) => {
            // SAFETY: the caller passes 16 readable bytes.
            let addr = unsafe { a.cast::<[u8; 16]>().read_unaligned() };
            (SockaddrIn6::v6(addr, 0, 0), SOCKADDR_IN6_LEN)
        }
        (AF_INET, 4) => {
            // SAFETY: the caller passes 4 readable bytes.
            let addr = unsafe { a.cast::<[u8; 4]>().read_unaligned() };
            (SockaddrIn6::v4(addr, 0), SOCKADDR_IN_LEN)
        }
        _ => {
            // SAFETY: the caller passes a writable pointer.
            unsafe { err.write(NO_RECOVERY) };
            return errno::EINVAL;
        }
    };

    let l = l as usize;
    let mut i = buf.addr() & (PTR - 1);
    if i == 0 {
        i = PTR;
    }
    if buflen <= 5 * PTR - i + l {
        return errno::ERANGE;
    }
    let buf = buf.wrapping_add(PTR - i);
    let buflen = buflen - (5 * PTR - i + l);

    let addr_list = buf;
    let aliases = addr_list.wrapping_add(2 * PTR);
    let at = aliases.wrapping_add(2 * PTR);
    // SAFETY: the room for the arrays and the address was checked above.
    unsafe { put_ptr(addr_list, at) };
    for offset in 0..l {
        // SAFETY: the caller passes `l` readable bytes.
        let byte = unsafe { a.wrapping_add(offset).read() };
        // SAFETY: the caller passes `l` readable bytes, and the room was
        // counted above.
        unsafe { at.wrapping_add(offset).write(byte) };
    }
    let name = at.wrapping_add(l);
    // SAFETY: as above.
    unsafe { put_ptr(addr_list.wrapping_add(PTR), null_mut()) };
    // SAFETY: as above.
    unsafe { put_ptr(aliases, name) };
    // SAFETY: as above.
    unsafe { put_ptr(aliases.wrapping_add(PTR), null_mut()) };

    // SAFETY: the address is a live local, and `name` has `buflen` bytes.
    let named = unsafe {
        nameinfo(
            &SYSTEM,
            sa.as_ptr(),
            sl,
            name,
            c_uint::try_from(buflen).unwrap_or(c_uint::MAX),
            null_mut(),
            0,
            0,
        )
    };
    match named {
        0 => {}
        EAI_AGAIN => {
            // SAFETY: the caller passes a writable pointer.
            unsafe { err.write(TRY_AGAIN) };
            return errno::EAGAIN;
        }
        EAI_OVERFLOW => return errno::ERANGE,
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

    // SAFETY: the caller passes a writable `hostent`.
    let addrtype = h
        .wrapping_byte_add(offset_of!(Hostent, h_addrtype))
        .cast::<c_int>();
    // SAFETY: the caller passes a writable `hostent`.
    unsafe { addrtype.write(af) };
    // SAFETY: as above.
    let host_length = h
        .wrapping_byte_add(offset_of!(Hostent, h_length))
        .cast::<c_int>();
    // SAFETY: as above.
    unsafe { host_length.write(l as c_int) };
    // SAFETY: as above.
    let host_name = h
        .wrapping_byte_add(offset_of!(Hostent, h_name))
        .cast::<*mut c_char>();
    // SAFETY: as above.
    unsafe { host_name.write(name) };
    // SAFETY: as above.
    let host_aliases = h
        .wrapping_byte_add(offset_of!(Hostent, h_aliases))
        .cast::<*mut *mut c_char>();
    // SAFETY: as above.
    unsafe { host_aliases.write(aliases.cast()) };
    // SAFETY: as above.
    let addresses = h
        .wrapping_byte_add(offset_of!(Hostent, h_addr_list))
        .cast::<*mut *mut c_char>();
    // SAFETY: as above.
    unsafe { addresses.write(addr_list.cast()) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(h) };
    0
}

/// Names the address `a`, `l` bytes of family `af`, returning a `hostent` in
/// storage the next call reuses, or null with `h_errno` set.
///
/// # Safety
///
/// `a` must be valid for reads of `l` bytes, and no other thread may be in one
/// of these functions.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn gethostbyaddr(a: *const c_char, l: c_uint, af: c_int) -> *mut Hostent {
    let lookup = |h, buf, buflen, res| {
        // SAFETY: the caller passes `l` readable bytes, and the buffer is this
        // call's own.
        unsafe { gethostbyaddr_r(a, l, af, h, buf, buflen, res, __h_errno_location()) }
    };
    // SAFETY: the lookup writes into what it is given.
    unsafe { with_buffer(&HOST_BY_ADDR, lookup) }
}

/// `"tcp"` and `"udp"`, which a `servent`'s `s_proto` points at.
static TCP: &[u8] = b"tcp\0";
/// As [`TCP`].
static UDP: &[u8] = b"udp\0";

/// Looks the service `name` up, reading `src`'s files, as musl's
/// `getservbyname_r` does.
///
/// # Safety
///
/// As [`getservbyname_r`].
pub unsafe fn servbyname(
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
    if super::at(wanted, end) == 0 {
        return errno::ENOENT;
    }

    let align = buf.addr().wrapping_neg() & (PTR - 1);
    if buflen < 2 * PTR + align {
        return errno::ERANGE;
    }
    let buf = buf.wrapping_add(align);

    let proto = if prots.is_null() {
        0
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        match unsafe { c_bytes(prots) } {
            b"tcp" => IPPROTO_TCP,
            b"udp" => IPPROTO_UDP,
            _ => return errno::EINVAL,
        }
    };

    let mut servs = [Service::default(); MAXSERVS];
    let cnt = lookup_serv(src, &mut servs, Some(wanted), proto, 0, 0);
    if cnt < 0 {
        return match cnt {
            EAI_MEMORY | EAI_SYSTEM => errno::ENOMEM,
            _ => errno::ENOENT,
        };
    }
    let found = servs[0];

    // SAFETY: the caller passes a writable `servent`.
    let service_name = se
        .wrapping_byte_add(offset_of!(Servent, s_name))
        .cast::<*mut c_char>();
    // SAFETY: the caller passes a writable `servent`.
    unsafe { service_name.write(name.cast_mut()) };
    // SAFETY: as above.
    let aliases = se
        .wrapping_byte_add(offset_of!(Servent, s_aliases))
        .cast::<*mut *mut c_char>();
    // SAFETY: as above.
    unsafe { aliases.write(buf.cast()) };
    // SAFETY: the alias array's room was checked above.
    unsafe { put_ptr(buf, name.cast_mut()) };
    // SAFETY: as above.
    unsafe { put_ptr(buf.wrapping_add(PTR), null_mut()) };
    // SAFETY: the caller passes a writable `servent`.
    let service_port = se
        .wrapping_byte_add(offset_of!(Servent, s_port))
        .cast::<c_int>();
    // SAFETY: the caller passes a writable `servent`.
    unsafe { service_port.write(c_int::from(found.port.to_be())) };
    let proto = if c_int::from(found.proto) == IPPROTO_TCP {
        TCP
    } else {
        UDP
    };
    // SAFETY: as above; the protocol name is static storage.
    let service_protocol = se
        .wrapping_byte_add(offset_of!(Servent, s_proto))
        .cast::<*mut c_char>();
    // SAFETY: the caller passes a writable `servent`; the protocol name is
    // static storage.
    unsafe { service_protocol.write(proto.as_ptr().cast_mut().cast()) };
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(se) };
    0
}

/// Looks the service `name` up for the protocol `prots`, `"tcp"`, `"udp"` or
/// null, laying the result out in the `buflen` bytes at `buf`. Returns 0 with
/// `*res` set, or an error number.
///
/// # Safety
///
/// `name` must be a NUL-terminated string that outlives the result, `prots`
/// null or a NUL-terminated string, `se` a writable `servent`, `buf` valid for
/// writes of `buflen` bytes, and `res` writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyname_r(
    name: *const c_char,
    prots: *const c_char,
    se: *mut Servent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Servent,
) -> c_int {
    // SAFETY: the caller's contract is `servbyname`'s.
    unsafe { servbyname(&SYSTEM, name, prots, se, buf, buflen, res) }
}

/// What `getservbyname` and `getservbyport` return: the record and the alias
/// array beside it.
#[derive(Debug)]
struct ServentStore {
    /// The record.
    se: Servent,
    /// musl's buffer: two pointers for `getservbyname`, and four for
    /// `getservbyport`, which keeps the name there too.
    buf: [usize; 4],
}

/// What `getservbyname` returns.
static SERV_BY_NAME: Shared<ServentStore> = Shared(UnsafeCell::new(ServentStore {
    se: Servent {
        s_name: null_mut(),
        s_aliases: null_mut(),
        s_port: 0,
        s_proto: null_mut(),
    },
    buf: [0; 4],
}));
/// What `getservbyport` returns.
static SERV_BY_PORT: Shared<ServentStore> = Shared(UnsafeCell::new(ServentStore {
    se: Servent {
        s_name: null_mut(),
        s_aliases: null_mut(),
        s_port: 0,
        s_proto: null_mut(),
    },
    buf: [0; 4],
}));

/// Looks the service `name` up, returning a `servent` in static storage the
/// next call reuses, or null.
///
/// # Safety
///
/// `name` must be a NUL-terminated string that outlives the result, `prots`
/// null or a NUL-terminated string, and no other thread may be in one of these
/// functions.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyname(name: *const c_char, prots: *const c_char) -> *mut Servent {
    // SAFETY: see `Shared`: one call reaches this at a time, as in C.
    let store = unsafe { &mut *SERV_BY_NAME.0.get() };
    let mut res = null_mut();
    // musl's buffer here is two pointers.
    let buflen = 2 * PTR;
    // SAFETY: the caller passes NUL-terminated strings, and the record and
    // buffer are this function's own.
    let error = unsafe {
        getservbyname_r(
            name,
            prots,
            &raw mut store.se,
            store.buf.as_mut_ptr().cast(),
            buflen,
            &raw mut res,
        )
    };
    if error != 0 {
        return null_mut();
    }
    &raw mut store.se
}

/// Names the service on `port`, given in network byte order, reading `src`'s
/// files, as musl's `getservbyport_r` does.
///
/// # Safety
///
/// As [`getservbyport_r`].
pub unsafe fn servbyport(
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
        let error = unsafe { servbyport(src, port, TCP.as_ptr().cast(), se, buf, buflen, res) };
        if error == 0 {
            return 0;
        }
        // SAFETY: as above.
        return unsafe { servbyport(src, port, UDP.as_ptr().cast(), se, buf, buflen, res) };
    }
    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(null_mut()) };

    let mut i = buf.addr() & (PTR - 1);
    if i == 0 {
        i = PTR;
    }
    if buflen <= 3 * PTR - i {
        return errno::ERANGE;
    }
    let buf = buf.wrapping_add(PTR - i);
    let buflen = buflen - (PTR - i);

    // SAFETY: the caller passes a NUL-terminated string.
    let dgram = match unsafe { c_bytes(prots) } {
        b"tcp" => false,
        b"udp" => true,
        _ => return errno::EINVAL,
    };

    // SAFETY: the caller passes a writable `servent`.
    let service_port = se
        .wrapping_byte_add(offset_of!(Servent, s_port))
        .cast::<c_int>();
    // SAFETY: the caller passes a writable `servent`.
    unsafe { service_port.write(port) };
    // SAFETY: as above; the protocol string is the caller's.
    let service_protocol = se
        .wrapping_byte_add(offset_of!(Servent, s_proto))
        .cast::<*mut c_char>();
    // SAFETY: as above; the protocol string is the caller's.
    unsafe { service_protocol.write(prots.cast_mut()) };
    // SAFETY: as above.
    let aliases = se
        .wrapping_byte_add(offset_of!(Servent, s_aliases))
        .cast::<*mut *mut c_char>();
    // SAFETY: as above.
    unsafe { aliases.write(buf.cast()) };
    let name = buf.wrapping_add(2 * PTR);
    let buflen = buflen - 2 * PTR;
    // SAFETY: the alias array's room was checked above.
    unsafe { put_ptr(buf.wrapping_add(PTR), null_mut()) };
    // SAFETY: as above.
    unsafe { put_ptr(buf, name) };
    // SAFETY: the caller passes a writable `servent`.
    let service_name = se
        .wrapping_byte_add(offset_of!(Servent, s_name))
        .cast::<*mut c_char>();
    // SAFETY: the caller passes a writable `servent`.
    unsafe { service_name.write(name) };

    let sa = SockaddrIn6 {
        sin6_family: AF_INET as u16,
        sin6_port: port as u16,
        ..SockaddrIn6::default()
    };
    // SAFETY: the address is a live local, and `name` has `buflen` bytes.
    let named = unsafe {
        nameinfo(
            src,
            sa.as_ptr(),
            SOCKADDR_IN_LEN,
            null_mut(),
            0,
            name,
            c_uint::try_from(buflen).unwrap_or(c_uint::MAX),
            if dgram { NI_DGRAM } else { 0 },
        )
    };
    match named {
        0 => {}
        EAI_MEMORY | EAI_SYSTEM => return errno::ENOMEM,
        EAI_OVERFLOW => return errno::ERANGE,
        _ => return errno::ENOENT,
    }

    // A number is not a service record. As in musl, a name that is not a
    // number reads as zero, so port zero never has one.
    // SAFETY: `nameinfo` wrote a NUL-terminated name.
    let text = unsafe { c_bytes(name) };
    let (value, _) = strtoul(text, 0, 10);
    if value == u64::from(u16::from_be(port as u16)) {
        return errno::ENOENT;
    }

    // SAFETY: the caller passes a writable pointer.
    unsafe { res.write(se) };
    0
}

/// Names the service on `port`, given in network byte order, for the protocol
/// `prots`, `"tcp"`, `"udp"` or null, laying the result out in the `buflen`
/// bytes at `buf`. Returns 0 with `*res` set, or an error number.
///
/// # Safety
///
/// `prots` must be null or a NUL-terminated string that outlives the result,
/// `se` a writable `servent`, `buf` valid for writes of `buflen` bytes, and
/// `res` writable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyport_r(
    port: c_int,
    prots: *const c_char,
    se: *mut Servent,
    buf: *mut c_char,
    buflen: usize,
    res: *mut *mut Servent,
) -> c_int {
    // SAFETY: the caller's contract is `servbyport`'s.
    unsafe { servbyport(&SYSTEM, port, prots, se, buf, buflen, res) }
}

/// Names the service on `port`, given in network byte order, returning a
/// `servent` in static storage the next call reuses, or null.
///
/// # Safety
///
/// `prots` must be null or a NUL-terminated string that outlives the result,
/// and no other thread may be in one of these functions.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getservbyport(port: c_int, prots: *const c_char) -> *mut Servent {
    // SAFETY: see `Shared`: one call reaches this at a time, as in C.
    let store = unsafe { &mut *SERV_BY_PORT.0.get() };
    let mut res = null_mut();
    // musl's buffer here is 32 bytes: two pointers and the name.
    let buflen = size_of::<[usize; 4]>();
    // SAFETY: the caller passes a NUL-terminated string or null, and the
    // record and buffer are this function's own.
    let error = unsafe {
        getservbyport_r(
            port,
            prots,
            &raw mut store.se,
            store.buf.as_mut_ptr().cast(),
            buflen,
            &raw mut res,
        )
    };
    if error != 0 {
        return null_mut();
    }
    &raw mut store.se
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "a test that indexes out of range fails, which is what a test does"
)]
mod tests {
    use super::*;
    use crate::netdb::h_errno;
    use crate::netdb::testing::Paths;

    /// The name, aliases and addresses of a `hostent`.
    fn parts(h: *mut Hostent) -> (String, Vec<String>, Vec<Vec<u8>>) {
        // SAFETY: the caller passes a filled `hostent`.
        let h = unsafe { h.read() };
        // SAFETY: the name is a NUL-terminated string.
        let name = unsafe { core::ffi::CStr::from_ptr(h.h_name) }
            .to_string_lossy()
            .into_owned();
        let mut aliases = Vec::new();
        let mut index = 0;
        loop {
            // SAFETY: the array ends with a null pointer.
            let alias = unsafe { h.h_aliases.wrapping_add(index).read() };
            if alias.is_null() {
                break;
            }
            // SAFETY: each alias is a NUL-terminated string.
            aliases.push(
                unsafe { core::ffi::CStr::from_ptr(alias) }
                    .to_string_lossy()
                    .into_owned(),
            );
            index += 1;
        }
        let mut addresses = Vec::new();
        let mut index = 0;
        loop {
            // SAFETY: the array ends with a null pointer.
            let address = unsafe { h.h_addr_list.wrapping_add(index).read() };
            if address.is_null() {
                break;
            }
            let mut bytes = Vec::new();
            for offset in 0..h.h_length as usize {
                // SAFETY: each address has `h_length` bytes.
                bytes.push(unsafe { address.wrapping_add(offset).read() } as u8);
            }
            addresses.push(bytes);
            index += 1;
        }
        (name, aliases, addresses)
    }

    #[test]
    fn gethostbyname_reads_the_hosts_file_of_the_system() {
        // The system's hosts file always names the loopback address.
        // SAFETY: the name is NUL-terminated.
        let h = unsafe { gethostbyname(c"localhost".as_ptr()) };
        assert!(!h.is_null());
        let (name, _, addresses) = parts(h);
        assert_eq!(name, "localhost");
        assert_eq!(addresses, [vec![127, 0, 0, 1]]);
        // SAFETY: as above.
        let h = unsafe { gethostbyname(c"192.0.2.3".as_ptr()) };
        assert!(!h.is_null());
        assert_eq!(parts(h).2, [vec![192, 0, 2, 3]]);
        // SAFETY: as above; an IPv6 address is not of this family.
        let h = unsafe { gethostbyname(c"::1".as_ptr()) };
        assert!(h.is_null());
        assert_eq!(h_errno(), NO_DATA);
        // SAFETY: as above.
        let h = unsafe { gethostbyname2(c"::1".as_ptr(), AF_INET6) };
        assert!(!h.is_null());
        assert_eq!(
            parts(h).2,
            [vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]]
        );
        // SAFETY: as above. A name that resolves nowhere.
        let h = unsafe { gethostbyname(c"ferrousli.invalid".as_ptr()) };
        assert!(h.is_null());
        assert!(h_errno() == HOST_NOT_FOUND || h_errno() == NO_RECOVERY || h_errno() == TRY_AGAIN);
    }

    #[test]
    fn gethostbyname_r_reports_a_buffer_too_small() {
        let mut h = Hostent {
            h_name: null_mut(),
            h_aliases: null_mut(),
            h_addrtype: 0,
            h_length: 0,
            h_addr_list: null_mut(),
        };
        let mut buf = [0 as c_char; 256];
        let mut res = null_mut();
        let mut err = 0;
        // SAFETY: the name is NUL-terminated and the buffers are live locals.
        let r = unsafe {
            gethostbyname_r(
                c"127.0.0.1".as_ptr(),
                &raw mut h,
                buf.as_mut_ptr(),
                8,
                &raw mut res,
                &raw mut err,
            )
        };
        assert_eq!(r, errno::ERANGE);
        assert!(res.is_null());
        // SAFETY: as above, with room.
        let r = unsafe {
            gethostbyname_r(
                c"127.0.0.1".as_ptr(),
                &raw mut h,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
                &raw mut err,
            )
        };
        assert_eq!(r, 0);
        assert_eq!(res, &raw mut h);
        let (name, aliases, addresses) = parts(res);
        assert_eq!(name, "127.0.0.1");
        assert_eq!(aliases, ["127.0.0.1"]);
        assert_eq!(addresses, [vec![127, 0, 0, 1]]);
        // A name that does not resolve.
        // SAFETY: as above.
        let r = unsafe {
            gethostbyname_r(
                c"ferrousli.invalid".as_ptr(),
                &raw mut h,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
                &raw mut err,
            )
        };
        assert!(res.is_null());
        assert!(r == 0 || r == errno::EAGAIN || r == errno::EBADMSG);
    }

    #[test]
    fn gethostbyaddr_names_an_address() {
        let addr = [127u8, 0, 0, 1];
        // SAFETY: the address has four bytes.
        let h = unsafe { gethostbyaddr(addr.as_ptr().cast(), 4, AF_INET) };
        assert!(!h.is_null());
        let (name, aliases, addresses) = parts(h);
        assert_eq!(addresses, [vec![127, 0, 0, 1]]);
        assert_eq!(aliases.as_slice(), core::slice::from_ref(&name));
        assert!(!name.is_empty());
        // A length that does not match the family.
        let mut h = Hostent {
            h_name: null_mut(),
            h_aliases: null_mut(),
            h_addrtype: 0,
            h_length: 0,
            h_addr_list: null_mut(),
        };
        let mut buf = [0 as c_char; 256];
        let mut res = null_mut();
        let mut err = 0;
        // SAFETY: the address and buffers are live locals.
        let r = unsafe {
            gethostbyaddr_r(
                addr.as_ptr().cast(),
                3,
                AF_INET,
                &raw mut h,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
                &raw mut err,
            )
        };
        assert_eq!((r, err), (errno::EINVAL, NO_RECOVERY));
        // SAFETY: as above, with no room.
        let r = unsafe {
            gethostbyaddr_r(
                addr.as_ptr().cast(),
                4,
                AF_INET,
                &raw mut h,
                buf.as_mut_ptr(),
                8,
                &raw mut res,
                &raw mut err,
            )
        };
        assert_eq!(r, errno::ERANGE);
    }

    #[test]
    fn services_are_named_and_numbered_from_the_fixture() {
        let paths = Paths::new("resolv.conf");
        let src = paths.sources(9);
        let mut se = Servent {
            s_name: null_mut(),
            s_aliases: null_mut(),
            s_port: 0,
            s_proto: null_mut(),
        };
        let mut buf = [0 as c_char; 64];
        let mut res = null_mut();
        // SAFETY: the strings are NUL-terminated and the buffers live.
        let r = unsafe {
            servbyname(
                &src,
                c"http".as_ptr(),
                c"tcp".as_ptr(),
                &raw mut se,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
            )
        };
        assert_eq!(r, 0);
        assert_eq!(se.s_port, c_int::from(8080u16.to_be()));
        // SAFETY: the protocol is a NUL-terminated string.
        assert_eq!(unsafe { core::ffi::CStr::from_ptr(se.s_proto) }, c"tcp");
        // SAFETY: the alias array holds the name, then null.
        let alias = unsafe { se.s_aliases.read() };
        // SAFETY: the alias is the name given.
        assert_eq!(unsafe { core::ffi::CStr::from_ptr(alias) }, c"http");
        // A number is not a service name, and an unknown protocol is refused.
        // SAFETY: as above.
        let r = unsafe {
            servbyname(
                &src,
                c"80".as_ptr(),
                c"tcp".as_ptr(),
                &raw mut se,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
            )
        };
        assert_eq!(r, errno::ENOENT);
        // SAFETY: as above.
        let r = unsafe {
            servbyname(
                &src,
                c"http".as_ptr(),
                c"sctp".as_ptr(),
                &raw mut se,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
            )
        };
        assert_eq!(r, errno::EINVAL);
        // SAFETY: as above.
        let r = unsafe {
            servbyname(
                &src,
                c"nosuch".as_ptr(),
                null_mut(),
                &raw mut se,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
            )
        };
        assert_eq!(r, errno::ENOENT);

        // The other way round: a port gives its name.
        // SAFETY: as above.
        let r = unsafe {
            servbyport(
                &src,
                c_int::from(8080u16.to_be()),
                c"tcp".as_ptr(),
                &raw mut se,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
            )
        };
        assert_eq!(r, 0);
        // SAFETY: the name is a NUL-terminated string.
        assert_eq!(unsafe { core::ffi::CStr::from_ptr(se.s_name) }, c"http");
        // A port with no record gives its number, which is no record.
        // SAFETY: as above.
        let r = unsafe {
            servbyport(
                &src,
                c_int::from(4711u16.to_be()),
                c"tcp".as_ptr(),
                &raw mut se,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
            )
        };
        assert_eq!(r, errno::ENOENT);
        // Without a protocol, TCP then UDP.
        // SAFETY: as above.
        let r = unsafe {
            servbyport(
                &src,
                c_int::from(514u16.to_be()),
                null_mut(),
                &raw mut se,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut res,
            )
        };
        assert_eq!(r, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { core::ffi::CStr::from_ptr(se.s_name) }, c"syslog");
    }
}
