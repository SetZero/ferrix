//! `arpa/inet.h`'s address conversions, `netinet/in.h`'s byte order
//! functions, and `net/if.h`'s `if_nametoindex` and `if_indextoname`.
//!
//! The conversions follow musl's `network/inet_aton.c`, `inet_addr.c`,
//! `inet_ntoa.c`, `inet_ntop.c` and `inet_pton.c`, over byte slices rather
//! than C strings. They differ from musl in what a failure leaves behind:
//! `inet_ntop` writes nothing when the text does not fit, where musl's
//! `snprintf` leaves as much as fits, and `inet_pton` writes nothing for text
//! it refuses, where musl may have written part of an IPv6 address. Both
//! return the same results as musl.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_uint, c_void};
use core::mem::size_of;
use core::ptr::null;

use crate::errno;
use crate::string::strlen;
use crate::syscall::{self, nr};

/// `AF_UNIX`, from `include/sys/socket.h`.
const AF_UNIX: c_int = 1;
/// `AF_INET`, from `include/sys/socket.h`.
pub const AF_INET: c_int = 2;
/// `AF_INET6`, from `include/sys/socket.h`.
pub const AF_INET6: c_int = 10;
/// `SOCK_DGRAM | SOCK_CLOEXEC`, from `include/sys/socket.h`.
const DGRAM_CLOEXEC: usize = 2 | 0o2_000_000;
/// `SIOCGIFINDEX`, from `include/sys/ioctl.h`.
const SIOCGIFINDEX: usize = 0x8933;
/// `SIOCGIFNAME`, from `include/sys/ioctl.h`.
const SIOCGIFNAME: usize = 0x8910;
/// `IFNAMSIZ`, from `include/net/if.h`.
const IFNAMSIZ: usize = 16;
/// The size of `struct ifreq`, from `include/net/if.h`: the name, then a
/// 24-byte union.
const IFREQ_SIZE: usize = 40;
/// Where `struct ifreq` keeps `ifr_ifindex`, the union's `int`.
const IFREQ_INDEX: usize = IFNAMSIZ;

/// C's `struct in_addr`: an IPv4 address in network byte order.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InAddr {
    /// The address, its most significant byte first in memory.
    pub s_addr: u32,
}

const _: () = assert!(size_of::<InAddr>() == 4);

/// Converts a 32-bit value from host to network byte order.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn htonl(host: u32) -> u32 {
    host.to_be()
}

/// Converts a 16-bit value from host to network byte order.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn htons(host: u16) -> u16 {
    host.to_be()
}

/// Converts a 32-bit value from network to host byte order.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ntohl(net: u32) -> u32 {
    u32::from_be(net)
}

/// Converts a 16-bit value from network to host byte order.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ntohs(net: u16) -> u16 {
    u16::from_be(net)
}

/// The bytes of the C string `s`, without its NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated string that outlives the slice.
unsafe fn bytes<'a>(s: *const c_char) -> &'a [u8] {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(s) };
    // SAFETY: `strlen` found `len` readable bytes before the NUL.
    unsafe { core::slice::from_raw_parts(s.cast::<u8>(), len) }
}

/// The byte at `index`, or 0 past the end, as a C string reads.
fn at(text: &[u8], index: usize) -> u8 {
    text.get(index).copied().unwrap_or(0)
}

/// Reads the number at the start of `text` as `strtoul(text, &end, 0)` does
/// for text that starts with a digit: hexadecimal after `0x`, octal after a
/// leading `0`, and decimal otherwise. Returns it, saturated, and the number of
/// bytes read.
fn number(text: &[u8]) -> (u64, usize) {
    let (radix, mut len) = match text {
        [b'0', b'x' | b'X', digit, ..] if digit.is_ascii_hexdigit() => (16, 2),
        [b'0', ..] => (8, 1),
        _ => (10, 0),
    };
    let mut value: u64 = 0;
    while let Some(digit) = char::from(at(text, len)).to_digit(radix) {
        value = value
            .saturating_mul(u64::from(radix))
            .saturating_add(u64::from(digit));
        len += 1;
    }
    (value, len)
}

/// Parses an IPv4 address in any of `inet_aton`'s forms: `a.b.c.d`, `a.b.c`
/// with a 16-bit last part, `a.b` with a 24-bit one, or a single 32-bit `a`,
/// each part in decimal, octal or hexadecimal.
pub(crate) fn parse_aton(text: &[u8]) -> Option<[u8; 4]> {
    let mut parts = [0u64; 4];
    let mut count = 0;
    let mut rest = text;
    loop {
        if !rest.first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        let (value, len) = number(rest);
        *parts.get_mut(count)? = value;
        count += 1;
        match rest.get(len) {
            None => break,
            Some(b'.') if count < 4 => rest = rest.get(len + 1..)?,
            Some(_) => return None,
        }
    }
    let [mut a, mut b, mut c, mut d] = parts;
    if count == 1 {
        b = a & 0xff_ffff;
        a >>= 24;
    }
    if count <= 2 {
        c = b & 0xffff;
        b >>= 16;
    }
    if count <= 3 {
        d = c & 0xff;
        c >>= 8;
    }
    Some([
        u8::try_from(a).ok()?,
        u8::try_from(b).ok()?,
        u8::try_from(c).ok()?,
        u8::try_from(d).ok()?,
    ])
}

/// Stores the IPv4 address `s` in `*dest`, returning 1, or returns 0 if `s`
/// is not one.
///
/// # Safety
///
/// `s` must be a NUL-terminated string, and `dest` valid for a write of a
/// `struct in_addr`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn inet_aton(s: *const c_char, dest: *mut InAddr) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { bytes(s) };
    let Some(address) = parse_aton(text) else {
        return 0;
    };
    // SAFETY: the caller passes a `struct in_addr` to write.
    unsafe {
        dest.write(InAddr {
            s_addr: u32::from_ne_bytes(address),
        });
    }
    1
}

/// The IPv4 address `s` in network byte order, or `INADDR_NONE`, all ones, if
/// `s` is not one.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn inet_addr(s: *const c_char) -> u32 {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { bytes(s) };
    parse_aton(text).map_or(u32::MAX, u32::from_ne_bytes)
}

/// Stores `byte` at `*len` in `out`, if it fits, and counts it.
fn put(out: &mut [u8], len: &mut usize, byte: u8) {
    if let Some(slot) = out.get_mut(*len) {
        *slot = byte;
    }
    *len += 1;
}

/// Writes `value` in decimal.
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

/// Writes `value` in lower-case hexadecimal, without leading zeros.
fn hex(out: &mut [u8], len: &mut usize, value: u32) {
    if value >= 16 {
        hex(out, len, value / 16);
    }
    put(
        out,
        len,
        b"0123456789abcdef"
            .get((value % 16) as usize)
            .copied()
            .unwrap_or(b'0'),
    );
}

/// Writes `address` in dotted decimal at `*len` in `out`.
fn dotted(out: &mut [u8], len: &mut usize, address: [u8; 4]) {
    for (index, byte) in address.into_iter().enumerate() {
        if index > 0 {
            put(out, len, b'.');
        }
        decimal(out, len, u32::from(byte));
    }
}

/// Writes the IPv6 `address` into `out` as `inet_ntop` shows it, and returns
/// the length. `out` starts zeroed, so the text is NUL-terminated.
///
/// This is musl's: eight groups in hexadecimal, or six and a dotted IPv4
/// address for an IPv4-mapped one, then the first longest run of two or more
/// zero groups replaced with `::`.
fn ipv6_text(out: &mut [u8; 100], address: [u8; 16]) -> usize {
    let [
        p0,
        p1,
        p2,
        p3,
        p4,
        p5,
        p6,
        p7,
        p8,
        p9,
        p10,
        p11,
        v0,
        v1,
        v2,
        v3,
    ] = address;
    let mapped = [p0, p1, p2, p3, p4, p5, p6, p7, p8, p9, p10, p11]
        == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];
    let groups = if mapped { 6 } else { 8 };
    let mut len = 0;
    for (index, pair) in address.chunks_exact(2).take(groups).enumerate() {
        if index > 0 {
            put(out, &mut len, b':');
        }
        let value = pair
            .iter()
            .fold(0_u32, |value, &byte| value * 256 + u32::from(byte));
        hex(out, &mut len, value);
    }
    if mapped {
        put(out, &mut len, b':');
        dotted(out, &mut len, [v0, v1, v2, v3]);
    }

    // Find the run, as musl does: from the start or from each colon, the
    // longest span of colons and zeros longer than two.
    let mut best = 0;
    let mut max = 2;
    for start in 0..len {
        if start > 0 && at(out, start) != b':' {
            continue;
        }
        let span = out.get(start..len).map_or(0, |rest| {
            rest.iter()
                .take_while(|&&byte| byte == b':' || byte == b'0')
                .count()
        });
        if span > max {
            best = start;
            max = span;
        }
    }
    if max <= 3 {
        return len;
    }
    // `::` replaces the span. The text after it moves back, NUL included.
    put(out, &mut best.clone(), b':');
    put(out, &mut (best + 1), b':');
    let mut from = best + max;
    let mut to = best + 2;
    while from <= len {
        let byte = at(out, from);
        put(out, &mut to, byte);
        from += 1;
    }
    len - max + 2
}

/// Writes the address at `src`, of family `af`, as text into the `size`
/// bytes at `dst`, and returns `dst`. Returns null with `ENOSPC` if the text
/// and its NUL do not fit, or `EAFNOSUPPORT` if `af` is neither `AF_INET` nor
/// `AF_INET6`.
///
/// # Safety
///
/// `src` must hold 4 bytes for `AF_INET` or 16 for `AF_INET6`, and `dst` be
/// valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn inet_ntop(
    af: c_int,
    src: *const c_void,
    dst: *mut c_char,
    size: c_uint,
) -> *const c_char {
    let mut text = [0u8; 100];
    let len = match af {
        AF_INET => {
            // SAFETY: the caller passes an IPv4 address.
            let address = unsafe { src.cast::<[u8; 4]>().read_unaligned() };
            let mut len = 0;
            dotted(&mut text, &mut len, address);
            len
        }
        AF_INET6 => {
            // SAFETY: the caller passes an IPv6 address.
            let address = unsafe { src.cast::<[u8; 16]>().read_unaligned() };
            ipv6_text(&mut text, address)
        }
        _ => {
            errno::set(errno::EAFNOSUPPORT);
            return null();
        }
    };
    if len >= size as usize {
        errno::set(errno::ENOSPC);
        return null();
    }
    for (offset, &byte) in text.iter().take(len + 1).enumerate() {
        // SAFETY: `len + 1` is at most `size`, which the caller vouches for.
        unsafe { dst.cast::<u8>().wrapping_add(offset).write(byte) };
    }
    dst.cast_const()
}

/// Parses a dotted decimal IPv4 address as `inet_pton` does: exactly four
/// parts, each 0 to 255 in up to three digits, without a leading zero.
fn pton4(mut text: &[u8]) -> Option<[u8; 4]> {
    let mut address = [0u8; 4];
    for (index, slot) in address.iter_mut().enumerate() {
        let mut value = 0_u32;
        let mut len = 0;
        while len < 3 && at(text, len).is_ascii_digit() {
            value = value * 10 + u32::from(at(text, len) - b'0');
            len += 1;
        }
        if len == 0 || (len > 1 && at(text, 0) == b'0') || value > 255 {
            return None;
        }
        *slot = u8::try_from(value).ok()?;
        if at(text, len) == 0 && index == 3 {
            return Some(address);
        }
        if at(text, len) != b'.' {
            return None;
        }
        text = text.get(len + 1..)?;
    }
    None
}

/// Parses an IPv6 address as `inet_pton` does, following musl's
/// `inet_pton.c` step for step: up to eight groups of up to four hexadecimal
/// digits, one `::` standing for as many zero groups as are missing, and an
/// IPv4 address in place of the last two groups.
pub(crate) fn pton6(text: &[u8]) -> Option<[u8; 16]> {
    let mut s = text;
    let mut groups = [0u16; 8];
    let mut gap: Option<usize> = None;
    let mut ipv4 = false;
    if at(s, 0) == b':' {
        s = s.get(1..)?;
        if at(s, 0) != b':' {
            return None;
        }
    }
    let mut index = 0;
    loop {
        if at(s, 0) == b':' && gap.is_none() {
            gap = Some(index);
            *groups.get_mut(index)? = 0;
            s = s.get(1..)?;
            if at(s, 0) == 0 {
                break;
            }
            if index == 7 {
                return None;
            }
            index += 1;
            continue;
        }
        let mut value = 0_u32;
        let mut len = 0;
        while len < 4 {
            let Some(digit) = char::from(at(s, len)).to_digit(16) else {
                break;
            };
            value = value * 16 + digit;
            len += 1;
        }
        if len == 0 {
            return None;
        }
        *groups.get_mut(index)? = u16::try_from(value).ok()?;
        if at(s, len) == 0 && (gap.is_some() || index == 7) {
            break;
        }
        if index == 7 {
            return None;
        }
        if at(s, len) != b':' {
            if at(s, len) != b'.' || (index < 6 && gap.is_none()) {
                return None;
            }
            ipv4 = true;
            index += 1;
            *groups.get_mut(index)? = 0;
            break;
        }
        s = s.get(len + 1..)?;
        index += 1;
    }
    if let Some(gap) = gap {
        // The groups from the gap on move to the end, and zeros fill in.
        let mut moved = [0u16; 8];
        for position in 0..gap {
            *moved.get_mut(position)? = *groups.get(position)?;
        }
        for position in gap..=index {
            *moved.get_mut(position + 7 - index)? = *groups.get(position)?;
        }
        groups = moved;
    }
    let mut address = [0u8; 16];
    for (pair, group) in address.chunks_exact_mut(2).zip(groups) {
        if let [high, low] = pair {
            [*high, *low] = group.to_be_bytes();
        }
    }
    if ipv4 {
        let [.., a, b, c, d] = &mut address;
        [*a, *b, *c, *d] = pton4(s)?;
    }
    Some(address)
}

/// Stores the address `src`, text of family `af`, at `dst`, returning 1.
/// Returns 0 if `src` is not an address of that family, or -1 with
/// `EAFNOSUPPORT` if `af` is neither `AF_INET` nor `AF_INET6`.
///
/// # Safety
///
/// `src` must be a NUL-terminated string, and `dst` valid for writes of 4
/// bytes for `AF_INET` or 16 for `AF_INET6`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn inet_pton(af: c_int, src: *const c_char, dst: *mut c_void) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { bytes(src) };
    match af {
        AF_INET => match pton4(text) {
            Some(address) => {
                // SAFETY: the caller passes 4 writable bytes.
                unsafe { dst.cast::<[u8; 4]>().write_unaligned(address) };
                1
            }
            None => 0,
        },
        AF_INET6 => match pton6(text) {
            Some(address) => {
                // SAFETY: the caller passes 16 writable bytes.
                unsafe { dst.cast::<[u8; 16]>().write_unaligned(address) };
                1
            }
            None => 0,
        },
        _ => {
            errno::set(errno::EAFNOSUPPORT);
            -1
        }
    }
}

/// The buffer `inet_ntoa` returns.
#[derive(Debug)]
struct NtoaBuffer(UnsafeCell<[u8; 16]>);

// SAFETY: C documents `inet_ntoa`'s result as static storage that the next
// call overwrites, and the function as unsafe to call from two threads at
// once, as musl's is. Only `inet_ntoa` writes it.
unsafe impl Sync for NtoaBuffer {}

/// What `inet_ntoa` returns.
static NTOA: NtoaBuffer = NtoaBuffer(UnsafeCell::new([0; 16]));

/// The IPv4 address `address` in dotted decimal, in static storage the next
/// call overwrites.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn inet_ntoa(address: InAddr) -> *mut c_char {
    let mut text = [0u8; 16];
    let mut len = 0;
    dotted(&mut text, &mut len, address.s_addr.to_ne_bytes());
    let buffer = NTOA.0.get();
    // SAFETY: see `NtoaBuffer`. The longest address and its NUL are 16 bytes.
    unsafe { buffer.write(text) };
    buffer.cast()
}

/// The index of the network interface named `name`, or 0 with `errno` set if
/// there is none.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn if_nametoindex(name: *const c_char) -> c_uint {
    // SAFETY: `socket` reads no memory.
    let ret = unsafe { syscall::syscall3(nr::SOCKET, AF_UNIX as usize, DGRAM_CLOEXEC, 0) };
    let fd = match errno::decode(ret) {
        Ok(fd) => fd,
        Err(error) => {
            errno::set(error);
            return 0;
        }
    };
    // As musl's `strncpy` does, a name of `IFNAMSIZ` bytes or more is cut
    // there without a NUL, and the kernel ends it.
    let mut request = [0u8; IFREQ_SIZE];
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { bytes(name) };
    for (slot, &byte) in request.iter_mut().zip(text).take(IFNAMSIZ) {
        *slot = byte;
    }
    // SAFETY: the kernel reads and writes the request, a `struct ifreq`.
    let ret =
        unsafe { syscall::syscall3(nr::IOCTL, fd, SIOCGIFINDEX, request.as_mut_ptr().addr()) };
    // SAFETY: `close` reads no memory.
    let _ = unsafe { syscall::syscall2(nr::CLOSE, fd, 0) };
    if let Err(error) = errno::decode(ret) {
        errno::set(error);
        return 0;
    }
    request
        .get(IFREQ_INDEX..IFREQ_INDEX + 4)
        .and_then(|index| <[u8; 4]>::try_from(index).ok())
        .map_or(0, |index| c_int::from_ne_bytes(index).cast_unsigned())
}

/// Writes the name of the network interface with index `index` into `name`,
/// and returns `name`. Returns null with `errno` set if there is none: `ENXIO`
/// where the kernel says `ENODEV`, as musl's `if_indextoname.c` does.
///
/// # Safety
///
/// `name` must be valid for writes of `IF_NAMESIZE` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn if_indextoname(index: c_uint, name: *mut c_char) -> *mut c_char {
    // SAFETY: `socket` reads no memory.
    let ret = unsafe { syscall::syscall3(nr::SOCKET, AF_UNIX as usize, DGRAM_CLOEXEC, 0) };
    let fd = match errno::decode(ret) {
        Ok(fd) => fd,
        Err(error) => {
            errno::set(error);
            return core::ptr::null_mut();
        }
    };
    let mut request = [0u8; IFREQ_SIZE];
    for (slot, byte) in request
        .iter_mut()
        .skip(IFREQ_INDEX)
        .zip(index.to_ne_bytes())
    {
        *slot = byte;
    }
    // SAFETY: the kernel reads and writes the request, a `struct ifreq`.
    let ret =
        unsafe { syscall::syscall3(nr::IOCTL, fd, SIOCGIFNAME, request.as_mut_ptr().addr()) };
    // SAFETY: `close` reads no memory.
    let _ = unsafe { syscall::syscall2(nr::CLOSE, fd, 0) };
    if let Err(error) = errno::decode(ret) {
        errno::set(if error == errno::ENODEV {
            errno::ENXIO
        } else {
            error
        });
        return core::ptr::null_mut();
    }
    // As `strncpy` does: the name, then NULs to `IF_NAMESIZE` bytes.
    let mut ended = false;
    for (offset, &byte) in request.iter().take(IFNAMSIZ).enumerate() {
        ended |= byte == 0;
        // SAFETY: the caller passes `IF_NAMESIZE` writable bytes.
        unsafe { name.wrapping_add(offset).write(if ended { 0 } else { byte as c_char }) };
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_indextoname_names_the_loopback_interface() {
        // SAFETY: the name is NUL-terminated.
        let index = unsafe { if_nametoindex(c"lo".as_ptr()) };
        let mut name = [0x55 as c_char; 16];
        // SAFETY: the buffer has `IF_NAMESIZE` bytes.
        let got = unsafe { if_indextoname(index, name.as_mut_ptr()) };
        assert_eq!(got, name.as_mut_ptr());
        // SAFETY: `if_indextoname` wrote a NUL-terminated name.
        assert_eq!(unsafe { core::ffi::CStr::from_ptr(got) }, c"lo");
        // SAFETY: as above.
        let got = unsafe { if_indextoname(u32::MAX, name.as_mut_ptr()) };
        assert!(got.is_null());
        assert_eq!(last_errno(), errno::ENXIO);
    }

    fn last_errno() -> c_int {
        // SAFETY: the pointer is this thread's errno.
        unsafe { errno::__errno_location().read() }
    }

    fn ntop6(address: [u8; 16]) -> String {
        let mut out = [0u8; 100];
        let len = ipv6_text(&mut out, address);
        assert_eq!(at(&out, len), 0);
        String::from_utf8_lossy(out.get(..len).unwrap_or_default()).into_owned()
    }

    fn groups(groups: [u16; 8]) -> [u8; 16] {
        let mut address = [0u8; 16];
        for (pair, group) in address.chunks_exact_mut(2).zip(groups) {
            if let [high, low] = pair {
                [*high, *low] = group.to_be_bytes();
            }
        }
        address
    }

    #[test]
    fn byte_order_puts_the_most_significant_byte_first() {
        assert_eq!(htonl(0x0102_0304).to_ne_bytes(), [1, 2, 3, 4]);
        assert_eq!(htons(0x0102).to_ne_bytes(), [1, 2]);
        assert_eq!(ntohl(u32::from_ne_bytes([1, 2, 3, 4])), 0x0102_0304);
        assert_eq!(ntohs(u16::from_ne_bytes([1, 2])), 0x0102);
    }

    #[test]
    fn inet_aton_takes_every_classic_form() {
        let accepted: &[(&str, [u8; 4])] = &[
            ("127.0.0.1", [127, 0, 0, 1]),
            ("127.1", [127, 0, 0, 1]),
            ("0x7f.1", [127, 0, 0, 1]),
            ("0177.0.0.01", [127, 0, 0, 1]),
            ("1.2.65535", [1, 2, 255, 255]),
            ("1.16777215", [1, 255, 255, 255]),
            ("16909060", [1, 2, 3, 4]),
            ("0", [0, 0, 0, 0]),
        ];
        for &(text, address) in accepted {
            assert_eq!(parse_aton(text.as_bytes()), Some(address), "{text}");
        }
        // strtoul reads "0x" with no hexadecimal digit as 0 and stops at the
        // x, which is not a dot, so "0x" is refused after all.
        assert_eq!(parse_aton(b"0x"), None);
        for text in [
            "",
            " 1.2.3.4",
            "1.2.3.4 ",
            "1.2.3.4.",
            "1.2.3.4.5",
            "256.0.0.1",
            "1.2.3.256",
            "1.16777216",
            "1..2",
            "08",
            "-1",
            "a.b.c.d",
        ] {
            assert_eq!(parse_aton(text.as_bytes()), None, "{text}");
        }
    }

    #[test]
    fn inet_addr_and_inet_aton_store_network_order() {
        let mut address = InAddr::default();
        // SAFETY: the string is NUL-terminated and the address a live local.
        let stored = unsafe { inet_aton(c"10.0.0.2".as_ptr(), &raw mut address) };
        assert_eq!(stored, 1);
        assert_eq!(address.s_addr.to_ne_bytes(), [10, 0, 0, 2]);
        // SAFETY: as above.
        let stored = unsafe { inet_aton(c"10.0.0.256".as_ptr(), &raw mut address) };
        assert_eq!(stored, 0);
        // SAFETY: the string is NUL-terminated.
        let value = unsafe { inet_addr(c"1.2.3.4".as_ptr()) };
        assert_eq!(value, u32::from_ne_bytes([1, 2, 3, 4]));
        // SAFETY: as above.
        let value = unsafe { inet_addr(c"255.255.255.256".as_ptr()) };
        assert_eq!(value, u32::MAX);
        let text = inet_ntoa(InAddr {
            s_addr: u32::from_ne_bytes([255, 255, 255, 255]),
        });
        // SAFETY: `inet_ntoa` returns a NUL-terminated string.
        let text = unsafe { core::ffi::CStr::from_ptr(text) };
        assert_eq!(text, c"255.255.255.255");
    }

    #[test]
    fn inet_pton_reads_dotted_decimal_strictly() {
        assert_eq!(pton4(b"1.2.3.4"), Some([1, 2, 3, 4]));
        assert_eq!(pton4(b"255.255.255.255"), Some([255; 4]));
        assert_eq!(pton4(b"0.0.0.0"), Some([0; 4]));
        for text in [
            "256.1.1.1",
            "01.1.1.1",
            "1.2.3",
            "1.2.3.4.",
            "1.2.3.4x",
            "",
            "1..2.3",
            "1234.1.1.1",
        ] {
            assert_eq!(pton4(text.as_bytes()), None, "{text}");
        }
    }

    #[test]
    fn inet_pton_reads_ipv6_with_a_gap_and_a_trailing_ipv4_address() {
        let accepted: &[(&str, [u16; 8])] = &[
            ("::", [0; 8]),
            ("::1", [0, 0, 0, 0, 0, 0, 0, 1]),
            ("1::", [1, 0, 0, 0, 0, 0, 0, 0]),
            ("1::8", [1, 0, 0, 0, 0, 0, 0, 8]),
            ("1:2:3:4:5:6:7:8", [1, 2, 3, 4, 5, 6, 7, 8]),
            ("1:2:3:4:5:6:7::", [1, 2, 3, 4, 5, 6, 7, 0]),
            ("::2:3:4:5:6:7:8", [0, 2, 3, 4, 5, 6, 7, 8]),
            ("fe80::ABCD:ef01", [0xfe80, 0, 0, 0, 0, 0, 0xabcd, 0xef01]),
            ("::ffff:1.2.3.4", [0, 0, 0, 0, 0, 0xffff, 0x0102, 0x0304]),
            ("1:2:3:4:5:6:1.2.3.4", [1, 2, 3, 4, 5, 6, 0x0102, 0x0304]),
            ("1::1.2.3.4", [1, 0, 0, 0, 0, 0, 0x0102, 0x0304]),
        ];
        for &(text, expected) in accepted {
            assert_eq!(pton6(text.as_bytes()), Some(groups(expected)), "{text}");
        }
        for text in [
            "",
            ":",
            ":1",
            "1:",
            "1:::2",
            "1::2::3",
            "1:2:3:4:5:6:7:8:9",
            "1:2:3:4:5:6:7",
            "12345::",
            "::1.2.3",
            "1:2:3:4:5:1.2.3.4",
            "::1.2.3.4:5",
            "g::",
            "1:2:3:4:5:6:7:8::",
        ] {
            assert_eq!(pton6(text.as_bytes()), None, "{text}");
        }
    }

    #[test]
    fn inet_ntop_shortens_the_first_longest_zero_run() {
        assert_eq!(ntop6([0; 16]), "::");
        assert_eq!(ntop6(groups([0, 0, 0, 0, 0, 0, 0, 1])), "::1");
        assert_eq!(ntop6(groups([1, 0, 0, 0, 0, 0, 0, 8])), "1::8");
        assert_eq!(ntop6(groups([1, 0, 0, 0, 0, 0, 0, 0])), "1::");
        assert_eq!(ntop6(groups([0, 0, 1, 0, 0, 0, 0, 0])), "0:0:1::");
        assert_eq!(ntop6(groups([1, 0, 2, 0, 0, 3, 0, 0])), "1:0:2::3:0:0");
        assert_eq!(ntop6(groups([1, 0, 2, 3, 4, 5, 6, 7])), "1:0:2:3:4:5:6:7");
        assert_eq!(
            ntop6(groups([0xfe80, 0, 0, 0, 0, 0, 0xabcd, 0xef01])),
            "fe80::abcd:ef01"
        );
        assert_eq!(
            ntop6(groups([0, 0, 0, 0, 0, 0xffff, 0x0102, 0x0304])),
            "::ffff:1.2.3.4"
        );
        assert_eq!(
            ntop6(groups([0xffff; 8])),
            "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"
        );
        for text in [
            "::",
            "::1",
            "1::8",
            "fe80::abcd:ef01",
            "::ffff:10.0.0.1",
            "1:2:3:4:5:6:7:8",
        ] {
            let address = pton6(text.as_bytes()).unwrap_or_default();
            assert_eq!(ntop6(address), text);
        }
    }

    #[test]
    fn inet_ntop_and_inet_pton_check_the_family_and_the_room() {
        let address = [255u8; 4];
        let mut out = [0x55u8; 16];
        // SAFETY: the address has 4 bytes and the buffer 15 of 16 are offered.
        let ret = unsafe {
            inet_ntop(
                AF_INET,
                address.as_ptr().cast(),
                out.as_mut_ptr().cast(),
                15,
            )
        };
        assert!(ret.is_null());
        assert_eq!(last_errno(), errno::ENOSPC);
        assert_eq!(out, [0x55; 16]);
        // SAFETY: as above, with the whole buffer.
        let ret = unsafe {
            inet_ntop(
                AF_INET,
                address.as_ptr().cast(),
                out.as_mut_ptr().cast(),
                16,
            )
        };
        assert_eq!(ret, out.as_ptr().cast());
        assert_eq!(out.as_slice(), b"255.255.255.255\0");
        // SAFETY: as above; the family is refused before either is read.
        let ret = unsafe { inet_ntop(99, address.as_ptr().cast(), out.as_mut_ptr().cast(), 16) };
        assert!(ret.is_null());
        assert_eq!(last_errno(), errno::EAFNOSUPPORT);
        let mut stored = [0u8; 16];
        // SAFETY: the string is NUL-terminated and 16 bytes are writable.
        let ret = unsafe { inet_pton(AF_INET6, c"::1".as_ptr(), stored.as_mut_ptr().cast()) };
        assert_eq!(ret, 1);
        assert_eq!(stored, groups([0, 0, 0, 0, 0, 0, 0, 1]));
        // SAFETY: as above.
        let ret = unsafe { inet_pton(AF_INET, c"1.2.3".as_ptr(), stored.as_mut_ptr().cast()) };
        assert_eq!(ret, 0);
        // SAFETY: as above.
        let ret = unsafe { inet_pton(1, c"1.2.3.4".as_ptr(), stored.as_mut_ptr().cast()) };
        assert_eq!(ret, -1);
        assert_eq!(last_errno(), errno::EAFNOSUPPORT);
    }

    #[test]
    fn if_nametoindex_finds_the_loopback_interface() {
        // SAFETY: the names are NUL-terminated.
        assert!(unsafe { if_nametoindex(c"lo".as_ptr()) } > 0);
        // SAFETY: as above.
        assert_eq!(unsafe { if_nametoindex(c"ferrousli-none".as_ptr()) }, 0);
        assert_eq!(last_errno(), errno::ENODEV);
    }
}
