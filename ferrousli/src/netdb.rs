//! `netdb.h` and `resolv.h`: name resolution, from `/etc/hosts`,
//! `/etc/services` and DNS.
//!
//! This is a port of musl 1.2.5's `src/network/` (MIT, copyright Rich Felker
//! and contributors): the lookups in `lookup_name.c`, `lookup_serv.c` and
//! `lookup_ipliteral.c`, `getaddrinfo`, `getnameinfo`, the legacy `hostent`
//! and `servent` functions, and the stub resolver in `resolvconf.c`,
//! `res_msend.c`, `res_mkquery.c`, `dns_parse.c`, `dn_expand.c` and
//! `ns_parse.c`. The parts:
//!
//! * [`lookup`]: turning a host name into addresses and a service name into
//!   ports, from a numeric address, the hosts file, or DNS.
//! * [`addrinfo`]: `getaddrinfo`, `freeaddrinfo` and `getnameinfo`.
//! * [`hostent`]: `gethostbyname` and its relatives, `getservbyname` and
//!   `getservbyport`, and the `set*ent`, `get*ent` and `end*ent` cursors.
//! * [`dns`]: building and parsing DNS packets.
//! * [`resolver`]: `/etc/resolv.conf`, and sending queries to its name servers
//!   over UDP, falling back to TCP for a truncated answer.
//!
//! Every lookup reads its files and asks its name servers through a
//! [`Sources`]. The C functions pass [`SYSTEM`], the standard paths and port
//! 53, as musl's do; the unit tests pass fixtures and a responder of their own.
//!
//! Where this differs from musl, the module that differs says so. The
//! differences in behaviour a program can see are three: `localhost` names
//! answer with the loopback addresses without a lookup, a `/etc/resolv.conf`
//! that cannot be read gives `EAI_SYSTEM` rather than `EAI_BADFLAGS`, and
//! `freeaddrinfo(NULL)` does nothing.
//!
//! # Without sockets
//!
//! Numeric addresses, `localhost`, the hosts file and the services file need
//! no socket. A lookup that reaches DNS first opens an `AF_INET` or `AF_INET6`
//! datagram socket; where the kernel refuses it, the lookup fails as musl's
//! does: `getaddrinfo` with `EAI_SYSTEM` and `errno` from `socket`, the
//! `hostent` functions with `h_errno` set to `NO_RECOVERY` or `TRY_AGAIN`, and
//! `getnameinfo` by falling back to the numeric form unless `NI_NAMEREQD`
//! asks for a name.

pub mod addrinfo;
pub mod dns;
pub mod hostent;
pub mod lookup;
pub mod resolver;

use core::ffi::{CStr, c_char, c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};

use crate::stdio::file::{self, File};
use crate::stdio::io::{feof, ferror, fgets, getc};
use crate::stdio::open::{fclose, fopen};
use crate::stdio::printf::{FileSink, Sink};
use crate::string::{strlen, strnlen};
use crate::{errno, scan, strtol};

pub use crate::inet::{AF_INET, AF_INET6};

/// `AF_UNSPEC`, from `include/sys/socket.h`.
pub const AF_UNSPEC: c_int = 0;
/// `SOCK_STREAM`, from `include/sys/socket.h`.
pub const SOCK_STREAM: c_int = 1;
/// `SOCK_DGRAM`, from `include/sys/socket.h`.
pub const SOCK_DGRAM: c_int = 2;
/// `SOCK_CLOEXEC`, from `include/sys/socket.h`.
pub const SOCK_CLOEXEC: c_int = 0o2_000_000;
/// `SOCK_NONBLOCK`, from `include/sys/socket.h`.
pub const SOCK_NONBLOCK: c_int = 0o4000;
/// `IPPROTO_TCP`, from `include/netinet/in.h`.
pub const IPPROTO_TCP: c_int = 6;
/// `IPPROTO_UDP`, from `include/netinet/in.h`.
pub const IPPROTO_UDP: c_int = 17;

/// `AI_PASSIVE`, from `include/netdb.h`.
pub const AI_PASSIVE: c_int = 0x01;
/// `AI_CANONNAME`, from `include/netdb.h`.
pub const AI_CANONNAME: c_int = 0x02;
/// `AI_NUMERICHOST`, from `include/netdb.h`.
pub const AI_NUMERICHOST: c_int = 0x04;
/// `AI_V4MAPPED`, from `include/netdb.h`.
pub const AI_V4MAPPED: c_int = 0x08;
/// `AI_ALL`, from `include/netdb.h`.
pub const AI_ALL: c_int = 0x10;
/// `AI_ADDRCONFIG`, from `include/netdb.h`.
pub const AI_ADDRCONFIG: c_int = 0x20;
/// `AI_NUMERICSERV`, from `include/netdb.h`.
pub const AI_NUMERICSERV: c_int = 0x400;

/// `NI_NUMERICHOST`, from `include/netdb.h`.
pub const NI_NUMERICHOST: c_int = 0x01;
/// `NI_NUMERICSERV`, from `include/netdb.h`.
pub const NI_NUMERICSERV: c_int = 0x02;
/// `NI_NAMEREQD`, from `include/netdb.h`.
pub const NI_NAMEREQD: c_int = 0x08;
/// `NI_DGRAM`, from `include/netdb.h`.
pub const NI_DGRAM: c_int = 0x10;
/// `NI_NUMERICSCOPE`, from `include/netdb.h`.
pub const NI_NUMERICSCOPE: c_int = 0x100;

/// `EAI_BADFLAGS`, from `include/netdb.h`.
pub const EAI_BADFLAGS: c_int = -1;
/// `EAI_NONAME`, from `include/netdb.h`.
pub const EAI_NONAME: c_int = -2;
/// `EAI_AGAIN`, from `include/netdb.h`.
pub const EAI_AGAIN: c_int = -3;
/// `EAI_FAIL`, from `include/netdb.h`.
pub const EAI_FAIL: c_int = -4;
/// `EAI_NODATA`, from `include/netdb.h`.
pub const EAI_NODATA: c_int = -5;
/// `EAI_FAMILY`, from `include/netdb.h`.
pub const EAI_FAMILY: c_int = -6;
/// `EAI_SERVICE`, from `include/netdb.h`.
pub const EAI_SERVICE: c_int = -8;
/// `EAI_MEMORY`, from `include/netdb.h`.
pub const EAI_MEMORY: c_int = -10;
/// `EAI_SYSTEM`, from `include/netdb.h`.
pub const EAI_SYSTEM: c_int = -11;
/// `EAI_OVERFLOW`, from `include/netdb.h`.
pub const EAI_OVERFLOW: c_int = -12;

/// `HOST_NOT_FOUND`, from `include/netdb.h`.
pub const HOST_NOT_FOUND: c_int = 1;
/// `TRY_AGAIN`, from `include/netdb.h`.
pub const TRY_AGAIN: c_int = 2;
/// `NO_RECOVERY`, from `include/netdb.h`.
pub const NO_RECOVERY: c_int = 3;
/// `NO_DATA`, from `include/netdb.h`.
pub const NO_DATA: c_int = 4;

/// C's `struct addrinfo`, from `include/netdb.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Addrinfo {
    /// The `AI_` flags.
    pub ai_flags: c_int,
    /// The address family.
    pub ai_family: c_int,
    /// The socket type.
    pub ai_socktype: c_int,
    /// The protocol.
    pub ai_protocol: c_int,
    /// The size of `ai_addr`.
    pub ai_addrlen: c_uint,
    /// The address.
    pub ai_addr: *mut c_void,
    /// The canonical name, or null.
    pub ai_canonname: *mut c_char,
    /// The next result, or null.
    pub ai_next: *mut Addrinfo,
}

const _: () = assert!(size_of::<Addrinfo>() == 48);
const _: () = assert!(offset_of!(Addrinfo, ai_addrlen) == 16);
const _: () = assert!(offset_of!(Addrinfo, ai_addr) == 24);
const _: () = assert!(offset_of!(Addrinfo, ai_canonname) == 32);
const _: () = assert!(offset_of!(Addrinfo, ai_next) == 40);

/// C's `struct hostent`, from `include/netdb.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Hostent {
    /// The official name.
    pub h_name: *mut c_char,
    /// The other names, ending with null.
    pub h_aliases: *mut *mut c_char,
    /// The address family.
    pub h_addrtype: c_int,
    /// The size of each address.
    pub h_length: c_int,
    /// The addresses, ending with null.
    pub h_addr_list: *mut *mut c_char,
}

const _: () = assert!(size_of::<Hostent>() == 32);
const _: () = assert!(offset_of!(Hostent, h_addrtype) == 16);
const _: () = assert!(offset_of!(Hostent, h_length) == 20);
const _: () = assert!(offset_of!(Hostent, h_addr_list) == 24);

/// C's `struct servent`, from `include/netdb.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Servent {
    /// The official name.
    pub s_name: *mut c_char,
    /// The other names, ending with null.
    pub s_aliases: *mut *mut c_char,
    /// The port, in network byte order.
    pub s_port: c_int,
    /// The protocol, `tcp` or `udp`.
    pub s_proto: *mut c_char,
}

const _: () = assert!(size_of::<Servent>() == 32);
const _: () = assert!(offset_of!(Servent, s_port) == 16);
const _: () = assert!(offset_of!(Servent, s_proto) == 24);

/// C's `struct sockaddr_in`, from `include/netinet/in.h`. It is here for its
/// layout: addresses are kept in a [`SockaddrIn6`], which is large enough for
/// either.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockaddrIn {
    /// `AF_INET`.
    pub sin_family: u16,
    /// The port, in network byte order.
    pub sin_port: u16,
    /// The address.
    pub sin_addr: [u8; 4],
    /// Zeros.
    pub sin_zero: [u8; 8],
}

/// C's `struct sockaddr_in6`, from `include/netinet/in.h`, which also holds a
/// `struct sockaddr_in` in its first 16 bytes, as musl's `union` of the two
/// does: the IPv4 address lies where `sin6_flowinfo` is.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SockaddrIn6 {
    /// `AF_INET6`, or `AF_INET`.
    pub sin6_family: u16,
    /// The port, in network byte order.
    pub sin6_port: u16,
    /// The flow information, or an IPv4 address.
    pub sin6_flowinfo: u32,
    /// The address.
    pub sin6_addr: [u8; 16],
    /// The scope.
    pub sin6_scope_id: u32,
}

const _: () = assert!(size_of::<SockaddrIn>() == 16);
const _: () = assert!(offset_of!(SockaddrIn, sin_port) == 2);
const _: () = assert!(offset_of!(SockaddrIn, sin_addr) == 4);
const _: () = assert!(size_of::<SockaddrIn6>() == 28);
const _: () = assert!(offset_of!(SockaddrIn6, sin6_port) == 2);
const _: () = assert!(offset_of!(SockaddrIn6, sin6_flowinfo) == offset_of!(SockaddrIn, sin_addr));
const _: () = assert!(offset_of!(SockaddrIn6, sin6_addr) == 8);
const _: () = assert!(offset_of!(SockaddrIn6, sin6_scope_id) == 24);

/// `sizeof(struct sockaddr_in)`.
pub const SOCKADDR_IN_LEN: c_uint = size_of::<SockaddrIn>() as c_uint;
/// `sizeof(struct sockaddr_in6)`.
pub const SOCKADDR_IN6_LEN: c_uint = size_of::<SockaddrIn6>() as c_uint;

impl SockaddrIn6 {
    /// A `struct sockaddr_in` for `addr` and `port`, given in network byte
    /// order.
    pub const fn v4(addr: [u8; 4], port: u16) -> Self {
        Self {
            sin6_family: AF_INET as u16,
            sin6_port: port,
            sin6_flowinfo: u32::from_ne_bytes(addr),
            sin6_addr: [0; 16],
            sin6_scope_id: 0,
        }
    }

    /// A `struct sockaddr_in6` for `addr`, `port` in network byte order, and
    /// `scope`.
    pub const fn v6(addr: [u8; 16], port: u16, scope: u32) -> Self {
        Self {
            sin6_family: AF_INET6 as u16,
            sin6_port: port,
            sin6_flowinfo: 0,
            sin6_addr: addr,
            sin6_scope_id: scope,
        }
    }

    /// The IPv4 address, when this holds a `struct sockaddr_in`.
    pub const fn v4_addr(&self) -> [u8; 4] {
        self.sin6_flowinfo.to_ne_bytes()
    }

    /// The structure's bytes, as `memcmp` sees them. It has no padding.
    pub fn bytes(&self) -> [u8; 28] {
        let mut out = [0u8; 28];
        let fields = self
            .sin6_family
            .to_ne_bytes()
            .into_iter()
            .chain(self.sin6_port.to_ne_bytes())
            .chain(self.sin6_flowinfo.to_ne_bytes())
            .chain(self.sin6_addr)
            .chain(self.sin6_scope_id.to_ne_bytes());
        for (slot, byte) in out.iter_mut().zip(fields) {
            *slot = byte;
        }
        out
    }

    /// A pointer to hand the kernel or a C caller.
    pub fn as_ptr(&self) -> *const c_void {
        (&raw const *self).cast()
    }

    /// A pointer for the kernel to write through.
    pub fn as_mut_ptr(&mut self) -> *mut c_void {
        (&raw mut *self).cast()
    }
}

/// Where a lookup reads its files and finds its name servers.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// The hosts file.
    pub hosts: &'a CStr,
    /// The networks file, which only the `getnetent` cursor reads.
    pub networks: &'a CStr,
    /// The protocols file, which only the `getprotoent` cursor reads.
    pub protocols: &'a CStr,
    /// The services file.
    pub services: &'a CStr,
    /// The resolver's configuration.
    pub resolv_conf: &'a CStr,
    /// The port name servers listen on.
    pub port: u16,
}

/// What the C functions read: the standard files, and name servers on port 53.
pub const SYSTEM: Sources<'static> = Sources {
    hosts: c"/etc/hosts",
    networks: c"/etc/networks",
    protocols: c"/etc/protocols",
    services: c"/etc/services",
    resolv_conf: c"/etc/resolv.conf",
    port: 53,
};

#[cfg(test)]
std::thread_local! {
    static H_ERRNO: core::cell::Cell<c_int> = const { core::cell::Cell::new(0) };
}

/// Where the calling thread's `h_errno` lives. `<netdb.h>` defines `h_errno`
/// as `*__h_errno_location()`. Each thread has its own, in its control block
/// beside `errno`; musl keeps the main thread's in a global, which comes to
/// the same.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __h_errno_location() -> *mut c_int {
    crate::thread::h_errno_location()
}

/// Where the calling thread's `h_errno` lives: in unit tests, a Rust
/// thread-local.
#[cfg(test)]
pub extern "C" fn __h_errno_location() -> *mut c_int {
    H_ERRNO.with(core::cell::Cell::as_ptr)
}

/// Sets the calling thread's `h_errno`.
pub fn set_h_errno(value: c_int) {
    // SAFETY: the pointer is the calling thread's `h_errno`, which lives as
    // long as the thread.
    unsafe { __h_errno_location().write(value) }
}

/// The calling thread's `h_errno`.
pub fn h_errno() -> c_int {
    // SAFETY: as in `set_h_errno`.
    unsafe { __h_errno_location().read() }
}

/// The text for the `h_errno` value `ecode`.
pub fn hstrerror_text(ecode: c_int) -> &'static CStr {
    match ecode {
        HOST_NOT_FOUND => c"Host not found",
        TRY_AGAIN => c"Try again",
        NO_RECOVERY => c"Non-recoverable error",
        NO_DATA => c"Address not available",
        _ => c"Unknown error",
    }
}

/// A description of the `h_errno` value `ecode`, in static storage.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn hstrerror(ecode: c_int) -> *const c_char {
    hstrerror_text(ecode).as_ptr()
}

/// Writes `msg`, a colon and a space if `msg` is not null, then the text for
/// `h_errno` and a newline, to standard error.
///
/// # Safety
///
/// `msg` must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn herror(msg: *const c_char) {
    let text = hstrerror_text(h_errno());
    let prefix = if msg.is_null() {
        None
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        Some(unsafe { CStr::from_ptr(msg) }.to_bytes())
    };
    let err: *mut File = file::stderr.load(core::sync::atomic::Ordering::Relaxed);
    // SAFETY: standard error is a static stream.
    unsafe {
        file::locked(err, |inner| {
            let mut sink = FileSink::new(inner);
            if let Some(prefix) = prefix {
                sink.write(prefix);
                sink.write(b": ");
            }
            sink.write(text.to_bytes());
            sink.write(b"\n");
            sink.flush();
        });
    }
}

/// The text for the `getaddrinfo` error `ecode`.
pub fn gai_strerror_text(ecode: c_int) -> &'static CStr {
    match ecode {
        EAI_BADFLAGS => c"Invalid flags",
        EAI_NONAME => c"Name does not resolve",
        EAI_AGAIN => c"Try again",
        EAI_FAIL => c"Non-recoverable error",
        EAI_NODATA => c"Name has no usable address",
        EAI_FAMILY => c"Unrecognized address family or invalid length",
        -7 => c"Unrecognized socket type",
        EAI_SERVICE => c"Unrecognized service",
        EAI_MEMORY => c"Out of memory",
        EAI_SYSTEM => c"System error",
        EAI_OVERFLOW => c"Overflow",
        _ => c"Unknown error",
    }
}

/// A description of the `getaddrinfo` error `ecode`, in static storage.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gai_strerror(ecode: c_int) -> *const c_char {
    gai_strerror_text(ecode).as_ptr()
}

/// Whether `byte` is white space in the C locale, as `isspace` is.
pub(crate) const fn is_space(byte: u8) -> bool {
    scan::is_space(byte)
}

/// The byte at `index`, or NUL past the end, as a C string reads.
pub(crate) fn at(text: &[u8], index: usize) -> u8 {
    text.get(index).copied().unwrap_or(0)
}

/// The length of the C string at the start of `text`: up to its first NUL, or
/// all of it.
pub(crate) fn c_len(text: &[u8]) -> usize {
    text.iter()
        .position(|&byte| byte == 0)
        .unwrap_or(text.len())
}

/// Copies `len` bytes of `text`, and a NUL after them, to `dest`.
///
/// # Safety
///
/// `dest` must be valid for writes of `len + 1` bytes.
pub(crate) unsafe fn copy_out(dest: *mut c_char, text: &[u8], len: usize) {
    for index in 0..len {
        let byte = at(text, index);
        // SAFETY: the caller vouches for `len + 1` writable bytes.
        unsafe { dest.wrapping_add(index).write(byte as c_char) };
    }
    // SAFETY: as above.
    unsafe { dest.wrapping_add(len).write(0) };
}

/// Copies `len` bytes of `text` to `dest`, without a NUL.
///
/// # Safety
///
/// `dest` must be valid for writes of `len` bytes.
pub(crate) unsafe fn copy_bytes(dest: *mut c_char, text: &[u8], len: usize) {
    for index in 0..len {
        let byte = at(text, index);
        // SAFETY: the caller vouches for `len` writable bytes.
        unsafe { dest.wrapping_add(index).write(byte as c_char) };
    }
}

/// The bytes of the C string `s`, without its NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated string that outlives the slice.
pub(crate) unsafe fn c_bytes<'a>(s: *const c_char) -> &'a [u8] {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(s) };
    // SAFETY: `strlen` found `len` readable bytes before the NUL.
    unsafe { core::slice::from_raw_parts(s.cast::<u8>(), len) }
}

/// The bytes of the C string `s`, without its NUL, but no more than `max`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string that outlives the slice.
pub(crate) unsafe fn c_bytes_max<'a>(s: *const c_char, max: usize) -> &'a [u8] {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strnlen(s, max) };
    // SAFETY: `strnlen` found `len` readable bytes.
    unsafe { core::slice::from_raw_parts(s.cast::<u8>(), len) }
}

/// `strtoul(text + from, &end, base)`: the value, saturated to `ULONG_MAX` and
/// negated after a minus sign as C does, and the index `end` is left at, which
/// is `from` when nothing converts.
pub(crate) fn strtoul(text: &[u8], from: usize, base: c_int) -> (u64, usize) {
    let rest = text.get(from..).unwrap_or_default();
    match strtol::scan(rest, base, false) {
        Some(scanned) => {
            let end = from + scanned.end;
            (strtol::to_unsigned(scanned, u64::MAX).0, end)
        }
        None => (0, from),
    }
}

/// Where `needle` first occurs in the C string `text` at or after `from`, as
/// `strstr(text + from, needle)` finds it.
pub(crate) fn find(text: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let hay = text.get(..c_len(text))?;
    let mut start = from;
    while start.checked_add(needle.len())? <= hay.len() {
        if hay.get(start..start + needle.len()) == Some(needle) {
            return Some(start);
        }
        start += 1;
    }
    None
}

/// Whether the C string `text` has `prefix` at `index`, as
/// `!strncmp(text + index, prefix, strlen(prefix))` does.
pub(crate) fn has_at(text: &[u8], index: usize, prefix: &[u8]) -> bool {
    prefix
        .iter()
        .enumerate()
        .all(|(offset, &byte)| at(text, index + offset) == byte && byte != 0)
}

/// Copies the C string at `from` in `text`, its NUL included, over the start
/// of `to`, as much as fits.
pub(crate) fn copy_c(to: &mut [u8], text: &[u8], from: usize) {
    let source = text.get(from..).unwrap_or_default();
    let len = c_len(source);
    for (index, slot) in to.iter_mut().enumerate().take(len + 1) {
        *slot = at(source, index);
    }
}

/// A database file, read a line at a time into a buffer of the caller's, as
/// musl's `fgets` loops read `/etc/hosts`, `/etc/services` and
/// `/etc/resolv.conf`. A line longer than the buffer comes in pieces.
#[derive(Debug)]
pub(crate) struct Database {
    /// The open stream.
    stream: *mut File,
}

impl Database {
    /// Opens `path`. `Ok(None)` if it does not exist or may not be read,
    /// which musl treats as an empty file, and `Err` with the error number for
    /// any other failure.
    pub(crate) fn open(path: &CStr) -> Result<Option<Self>, c_int> {
        // SAFETY: both strings are NUL-terminated.
        let stream = unsafe { fopen(path.as_ptr(), c"rbe".as_ptr()) };
        if !stream.is_null() {
            return Ok(Some(Self { stream }));
        }
        // SAFETY: the pointer is this thread's `errno`.
        match unsafe { errno::__errno_location().read() } {
            errno::ENOENT | errno::ENOTDIR | errno::EACCES => Ok(None),
            error => Err(error),
        }
    }

    /// Reads the next line, or as much of it as fits with a NUL, into `line`,
    /// and returns its length up to the first NUL. `None` at the end.
    pub(crate) fn line(&mut self, line: &mut [u8]) -> Option<usize> {
        let size = c_int::try_from(line.len()).unwrap_or(c_int::MAX);
        // SAFETY: the stream is open, and `line` has `size` writable bytes.
        let got = unsafe { fgets(line.as_mut_ptr().cast(), size, self.stream) };
        if got.is_null() {
            return None;
        }
        Some(c_len(line))
    }

    /// Whether the end of the file has been reached.
    pub(crate) fn at_end(&mut self) -> bool {
        // SAFETY: the stream is open.
        unsafe { feof(self.stream) != 0 }
    }

    /// Reads one byte, or `EOF`.
    pub(crate) fn byte(&mut self) -> c_int {
        // SAFETY: the stream is open.
        unsafe { getc(self.stream) }
    }

    /// Whether a read failed, rather than reaching the end. A directory
    /// opens as a file and fails on its first read, which is how the
    /// resolver's configuration reports `EISDIR`.
    pub(crate) fn failed(&mut self) -> bool {
        // SAFETY: the stream is open.
        unsafe { ferror(self.stream) != 0 }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // SAFETY: the stream is open, and not used again.
        let _ = unsafe { fclose(self.stream) };
    }
}

/// `IN6_IS_ADDR_LINKLOCAL`, from `include/netinet/in.h`.
pub(crate) const fn is_linklocal(a: &[u8; 16]) -> bool {
    a[0] == 0xfe && (a[1] & 0xc0) == 0x80
}

/// `IN6_IS_ADDR_MC_LINKLOCAL`, from `include/netinet/in.h`.
pub(crate) const fn is_mc_linklocal(a: &[u8; 16]) -> bool {
    a[0] == 0xff && (a[1] & 0xf) == 0x2
}

/// The prefix of an IPv4-mapped IPv6 address.
pub(crate) const V4MAPPED: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];

#[cfg(test)]
pub(crate) mod testing {
    //! What the lookup tests share: fixture paths and a DNS responder.

    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "a test reports failure by panicking"
    )]

    use std::ffi::CString;
    use std::net::{TcpListener, UdpSocket};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread::JoinHandle;

    use super::Sources;

    /// A fixture file under `tests/data/netdb`, as a C path.
    pub(crate) fn fixture(name: &str) -> CString {
        CString::new(format!(
            "{}/tests/data/netdb/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// The paths a test lookup reads, kept alive for its [`Sources`].
    #[derive(Debug)]
    pub(crate) struct Paths {
        pub(crate) hosts: CString,
        pub(crate) networks: CString,
        pub(crate) protocols: CString,
        pub(crate) services: CString,
        pub(crate) resolv_conf: CString,
    }

    impl Paths {
        /// The fixtures, with `resolv_conf` naming the configuration file.
        pub(crate) fn new(resolv_conf: &str) -> Self {
            Self {
                hosts: fixture("hosts"),
                networks: fixture("networks"),
                protocols: fixture("protocols"),
                services: fixture("services"),
                resolv_conf: fixture(resolv_conf),
            }
        }

        /// Sources reading these files, with name servers on `port`.
        pub(crate) fn sources(&self, port: u16) -> Sources<'_> {
            Sources {
                hosts: &self.hosts,
                networks: &self.networks,
                protocols: &self.protocols,
                services: &self.services,
                resolv_conf: &self.resolv_conf,
                port,
            }
        }
    }

    /// One resource record an answer carries.
    #[derive(Debug, Clone)]
    pub(crate) enum Record {
        /// An IPv4 address.
        A([u8; 4]),
        /// An IPv6 address.
        Aaaa([u8; 16]),
        /// A canonical name.
        Cname(&'static str),
        /// A pointer to a name.
        Ptr(&'static str),
    }

    /// What the responder does with a question.
    #[derive(Debug, Clone)]
    pub(crate) enum Reply {
        /// Answers with these records, those whose type was asked for, and a
        /// canonical name always.
        Records(Vec<Record>),
        /// Answers with this response code and nothing else.
        Code(u8),
        /// Answers over UDP with the truncation bit and no records, and over
        /// TCP with these records.
        Truncated(Vec<Record>),
    }

    /// Encodes `name` in DNS labels.
    pub(crate) fn labels(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for label in name.split('.').filter(|label| !label.is_empty()) {
            out.push(u8::try_from(label.len()).unwrap());
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
        out
    }

    /// The question's name, as text, its type, and where it ends.
    fn question(packet: &[u8]) -> (String, u16, usize) {
        let mut at = 12;
        let mut name = Vec::new();
        while packet[at] != 0 {
            let len = usize::from(packet[at]);
            if !name.is_empty() {
                name.push(b'.');
            }
            name.extend_from_slice(&packet[at + 1..at + 1 + len]);
            at += 1 + len;
        }
        let kind = u16::from_be_bytes([packet[at + 1], packet[at + 2]]);
        (String::from_utf8(name).unwrap(), kind, at + 5)
    }

    /// Builds the answer to `query` that `reply` describes.
    pub(crate) fn answer(query: &[u8], reply: &Reply, tcp: bool) -> Vec<u8> {
        let (_, kind, end) = question(query);
        let mut out = query[..end].to_vec();
        out[2] |= 0x80;
        out[3] = 0;
        let records = match reply {
            Reply::Code(code) => {
                out[3] = *code;
                return out;
            }
            Reply::Truncated(_) if !tcp => {
                out[2] |= 0x02;
                return out;
            }
            Reply::Records(records) | Reply::Truncated(records) => records,
        };
        let mut count = 0u16;
        for record in records {
            let (rtype, data) = match record {
                Record::A(addr) => (1u16, addr.to_vec()),
                Record::Aaaa(addr) => (28, addr.to_vec()),
                Record::Cname(name) => (5, labels(name)),
                Record::Ptr(name) => (12, labels(name)),
            };
            if rtype != kind && rtype != 5 {
                continue;
            }
            // The name is a pointer to the question's.
            out.extend_from_slice(&[0xc0, 12]);
            out.extend_from_slice(&rtype.to_be_bytes());
            out.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
            out.extend_from_slice(&u16::try_from(data.len()).unwrap().to_be_bytes());
            out.extend_from_slice(&data);
            count += 1;
        }
        out[6..8].copy_from_slice(&count.to_be_bytes());
        out
    }

    /// A DNS server on 127.0.0.1, answering over UDP and TCP on one port
    /// until it is dropped.
    #[derive(Debug)]
    pub(crate) struct Responder {
        pub(crate) port: u16,
        /// How many questions arrived, over either transport.
        pub(crate) questions: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        threads: Vec<JoinHandle<()>>,
    }

    /// Answers each question that arrives over `udp` until `stop` is set.
    fn serve_udp(
        udp: &UdpSocket,
        stop: &AtomicBool,
        questions: &AtomicUsize,
        reply: fn(&str, u16) -> Reply,
    ) {
        let mut buf = [0u8; 512];
        while !stop.load(Ordering::Relaxed) {
            let Ok((len, from)) = udp.recv_from(&mut buf) else {
                continue;
            };
            let _ = questions.fetch_add(1, Ordering::Relaxed);
            let query = &buf[..len];
            let (name, kind, _) = question(query);
            let out = answer(query, &reply(&name, kind), false);
            let _ = udp.send_to(&out, from);
        }
    }

    /// Answers one question per connection to `tcp` until `stop` is set.
    fn serve_tcp(
        tcp: &TcpListener,
        stop: &AtomicBool,
        questions: &AtomicUsize,
        reply: fn(&str, u16) -> Reply,
    ) {
        while !stop.load(Ordering::Relaxed) {
            let Ok((stream, _)) = tcp.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            };
            answer_one(stream, questions, reply);
        }
    }

    /// Reads one length-framed question from `stream` and answers it.
    fn answer_one(
        mut stream: std::net::TcpStream,
        questions: &AtomicUsize,
        reply: fn(&str, u16) -> Reply,
    ) {
        use std::io::{Read, Write};
        stream.set_nonblocking(false).unwrap();
        let mut len = [0u8; 2];
        if stream.read_exact(&mut len).is_err() {
            return;
        }
        let mut query = vec![0u8; usize::from(u16::from_be_bytes(len))];
        if stream.read_exact(&mut query).is_err() {
            return;
        }
        let _ = questions.fetch_add(1, Ordering::Relaxed);
        let (name, kind, _) = question(&query);
        let out = answer(&query, &reply(&name, kind), true);
        let mut framed = u16::try_from(out.len()).unwrap().to_be_bytes().to_vec();
        framed.extend_from_slice(&out);
        let _ = stream.write_all(&framed);
    }

    impl Responder {
        /// Starts a responder that answers each question with
        /// `reply(name, type)`.
        pub(crate) fn start(reply: fn(&str, u16) -> Reply) -> Self {
            // The TCP listener must get the UDP socket's port; try until one
            // that is free for both comes up.
            let (udp, tcp) = loop {
                let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
                let port = udp.local_addr().unwrap().port();
                if let Ok(tcp) = TcpListener::bind(("127.0.0.1", port)) {
                    break (udp, tcp);
                }
            };
            let port = udp.local_addr().unwrap().port();
            udp.set_read_timeout(Some(std::time::Duration::from_millis(50)))
                .unwrap();
            tcp.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let questions = Arc::new(AtomicUsize::new(0));
            let mut threads = Vec::new();
            {
                let stop = Arc::clone(&stop);
                let questions = Arc::clone(&questions);
                threads.push(std::thread::spawn(move || {
                    serve_udp(&udp, &stop, &questions, reply);
                }));
            }
            {
                let stop = Arc::clone(&stop);
                let questions = Arc::clone(&questions);
                threads.push(std::thread::spawn(move || {
                    serve_tcp(&tcp, &stop, &questions, reply);
                }));
            }
            Self {
                port,
                questions,
                stop,
                threads,
            }
        }
    }

    impl Drop for Responder {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            for thread in self.threads.drain(..) {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_texts_are_musls() {
        assert_eq!(hstrerror_text(HOST_NOT_FOUND), c"Host not found");
        assert_eq!(hstrerror_text(NO_DATA), c"Address not available");
        assert_eq!(hstrerror_text(0), c"Unknown error");
        assert_eq!(hstrerror_text(5), c"Unknown error");
        assert_eq!(hstrerror_text(-1), c"Unknown error");
        assert_eq!(gai_strerror_text(EAI_BADFLAGS), c"Invalid flags");
        assert_eq!(gai_strerror_text(-7), c"Unrecognized socket type");
        assert_eq!(gai_strerror_text(-9), c"Unknown error");
        assert_eq!(gai_strerror_text(EAI_OVERFLOW), c"Overflow");
        assert_eq!(gai_strerror_text(-13), c"Unknown error");
        assert_eq!(gai_strerror_text(0), c"Unknown error");
        assert_eq!(gai_strerror_text(1), c"Unknown error");
    }

    #[test]
    fn h_errno_is_kept_per_thread() {
        set_h_errno(TRY_AGAIN);
        let other = std::thread::spawn(|| {
            let before = h_errno();
            set_h_errno(NO_DATA);
            before
        })
        .join()
        .unwrap_or(-1);
        assert_eq!(other, 0);
        assert_eq!(h_errno(), TRY_AGAIN);
    }

    #[test]
    fn the_c_string_helpers_stop_where_c_does() {
        assert_eq!(strtoul(b"  +80x", 0, 10), (80, 5));
        assert_eq!(strtoul(b"x80", 0, 10), (0, 0));
        assert_eq!(strtoul(b"a:-1", 2, 10), (u64::MAX, 4));
        assert_eq!(strtoul(b"99999999999999999999999", 0, 10), (u64::MAX, 23));
        assert_eq!(strtoul(b"0x", 0, 16), (0, 1));
        assert_eq!(find(b"a b\0 b", 0, b"b"), Some(2));
        assert_eq!(find(b"a b\0 b", 3, b"b"), None);
        assert_eq!(find(b"ab", 1, b"abc"), None);
        assert!(has_at(b"x/udp", 1, b"/udp"));
        assert!(!has_at(b"x/ud", 1, b"/udp"));
        let mut to = [9u8; 4];
        copy_c(&mut to, b"ab\0cd", 0);
        assert_eq!(to, [b'a', b'b', 0, 9]);
    }

    #[test]
    fn a_sockaddr_in_lies_in_the_first_bytes_of_a_sockaddr_in6() {
        let sin = SockaddrIn6::v4([127, 0, 0, 1], 53u16.to_be());
        let bytes = sin.bytes();
        assert_eq!(bytes.get(..8), Some(&[2, 0, 0, 53, 127, 0, 0, 1][..]));
        assert_eq!(sin.v4_addr(), [127, 0, 0, 1]);
    }
}
