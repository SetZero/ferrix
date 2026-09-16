//! `netinet/ether.h`: Ethernet addresses as text, and the `/etc/ethers`
//! database.
//!
//! `ether_aton_r` and `ether_ntoa_r` are ports of musl 1.2.5's `ether.c`
//! (MIT), and the two wrappers that return static storage are its too.
//! `ether_line`, `ether_ntohost` and `ether_hostton` are not: musl's three
//! return -1 without looking at anything, so a program that asks which host
//! owns a hardware address is told there is none. These read `/etc/ethers`,
//! the file ethers(5) describes and glibc reads: one address and one host
//! name to a line, with `#` starting a comment.
//!
//! musl's parser is kept as it is, lax ends and all: `ether_aton` takes any
//! form `strtoul` does for each byte, so `0x1:2:3:4:5:6` is an address and so
//! is `:::::`, which is six zeros. glibc refuses both. A program that checks
//! what it was given does not notice, and one that does not is no worse off
//! than on musl.

use core::ffi::{CStr, c_char, c_int};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::netdb::{Database, at, c_bytes, c_len, copy_out, is_space, strtoul};
use crate::pwd::Shared;
use core::cell::UnsafeCell;

/// `ETH_ALEN`, from `include/netinet/if_ether.h`.
const ETH_ALEN: usize = 6;

/// The longest text `ether_ntoa_r` writes, its NUL included.
const ETHER_TEXT: usize = 18;

/// The file `ether_ntohost` and `ether_hostton` read.
const ETHERS: &CStr = c"/etc/ethers";

/// C's `struct ether_addr`, from `include/net/ethernet.h`. glibc's is the
/// same.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EtherAddr {
    /// The six bytes of the address.
    pub ether_addr_octet: [u8; ETH_ALEN],
}

const _: () = assert!(size_of::<EtherAddr>() == ETH_ALEN);

/// Reads an Ethernet address from `text`, as musl's `ether_aton_r` reads one:
/// six byte values separated by colons, each in any form `strtoul` takes for
/// base 16. Returns the address and where it ends.
fn parse(text: &[u8]) -> Option<([u8; ETH_ALEN], usize)> {
    let mut octets = [0u8; ETH_ALEN];
    let mut index = 0;
    for (position, slot) in octets.iter_mut().enumerate() {
        if position != 0 {
            if at(text, index) != b':' {
                return None;
            }
            index += 1;
        }
        let (value, end) = strtoul(text, index, 16);
        if value > 0xff {
            return None;
        }
        *slot = value as u8;
        index = end;
    }
    Some((octets, index))
}

/// Writes `address` into `out` as `ether_ntoa_r` does: six two-digit
/// upper-case hexadecimal bytes separated by colons, and a NUL.
fn format(address: &[u8; ETH_ALEN], out: &mut [u8; ETHER_TEXT]) {
    let digits = b"0123456789ABCDEF";
    let digit = |value: u8| digits.get(usize::from(value)).copied().unwrap_or(b'0');
    let mut len = 0;
    for (position, &byte) in address.iter().enumerate() {
        if position != 0 {
            if let Some(slot) = out.get_mut(len) {
                *slot = b':';
            }
            len += 1;
        }
        if let Some(slot) = out.get_mut(len) {
            *slot = digit(byte >> 4);
        }
        if let Some(slot) = out.get_mut(len + 1) {
            *slot = digit(byte & 15);
        }
        len += 2;
    }
    if let Some(slot) = out.get_mut(len) {
        *slot = 0;
    }
}

/// Reads the Ethernet address `x` into `*p_a`, and returns `p_a`. Null if
/// `x` is not an address.
///
/// # Safety
///
/// `x` must be a NUL-terminated string and `p_a` a writable
/// `struct ether_addr`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_aton_r(x: *const c_char, p_a: *mut EtherAddr) -> *mut EtherAddr {
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { c_bytes(x) };
    let Some((octets, end)) = parse(text) else {
        return null_mut();
    };
    if at(text, end) != 0 {
        return null_mut();
    }
    // SAFETY: the caller passes a writable `struct ether_addr`.
    unsafe {
        p_a.write(EtherAddr {
            ether_addr_octet: octets,
        })
    };
    p_a
}

/// The address `ether_aton` returns, which the next call overwrites.
static ATON: Shared<EtherAddr> = Shared(UnsafeCell::new(EtherAddr {
    ether_addr_octet: [0; ETH_ALEN],
}));

/// The text `ether_ntoa` returns, which the next call overwrites.
static NTOA: Shared<[u8; ETHER_TEXT]> = Shared(UnsafeCell::new([0; ETHER_TEXT]));

/// Reads the Ethernet address `x` into static storage the next call
/// overwrites, and returns it. Null if `x` is not an address.
///
/// # Safety
///
/// `x` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_aton(x: *const c_char) -> *mut EtherAddr {
    // SAFETY: see `Shared`: C documents this as unsafe to call from two
    // threads at once, and this reaches the storage only within one call.
    let slot = unsafe { &mut *ATON.0.get() };
    // SAFETY: the caller passes a NUL-terminated string, and `slot` is the
    // static address.
    unsafe { ether_aton_r(x, &raw mut *slot) }
}

/// Writes `p_a` as text into `x`, which needs 18 bytes, and returns `x`.
///
/// # Safety
///
/// `p_a` must be a readable `struct ether_addr` and `x` valid for writes of
/// 18 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_ntoa_r(p_a: *const EtherAddr, x: *mut c_char) -> *mut c_char {
    // SAFETY: the caller passes a readable address.
    let address = unsafe { p_a.read() };
    let mut text = [0u8; ETHER_TEXT];
    format(&address.ether_addr_octet, &mut text);
    // SAFETY: the caller passes 18 writable bytes, and the text is 17 and a
    // NUL.
    unsafe { copy_out(x, &text, ETHER_TEXT - 1) };
    x
}

/// `p_a` as text, in static storage the next call overwrites.
///
/// # Safety
///
/// `p_a` must be a readable `struct ether_addr`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_ntoa(p_a: *const EtherAddr) -> *mut c_char {
    // SAFETY: see `Shared`.
    let slot = unsafe { &mut *NTOA.0.get() };
    // SAFETY: the caller passes a readable address, and `slot` has 18 bytes.
    unsafe { ether_ntoa_r(p_a, slot.as_mut_ptr().cast()) }
}

/// One line of `/etc/ethers`, split: the address, and where its host name
/// starts and ends. `None` for a line that is a comment, is empty, or is not
/// an address and a name.
fn split(line: &[u8]) -> Option<([u8; ETH_ALEN], usize, usize)> {
    // A comment ends the line, as does its NUL.
    let text = line.get(..c_len(line))?;
    let end = text
        .iter()
        .position(|&byte| byte == b'#')
        .unwrap_or(text.len());
    let text = text.get(..end)?;
    let mut index = 0;
    while is_space(at(text, index)) {
        index += 1;
    }
    let (octets, used) = parse(text.get(index..)?)?;
    index += used;
    if !is_space(at(text, index)) {
        return None;
    }
    while is_space(at(text, index)) {
        index += 1;
    }
    let start = index;
    while at(text, index) != 0 && !is_space(at(text, index)) {
        index += 1;
    }
    (index > start).then_some((octets, start, index))
}

/// Looks `/etc/ethers` up at `path`, calling `each` with every line's address
/// and host name until it answers `true`. Whether it did.
fn search(path: &CStr, mut each: impl FnMut(&[u8; ETH_ALEN], &[u8]) -> bool) -> bool {
    let Ok(Some(mut db)) = Database::open(path) else {
        return false;
    };
    let mut line = [0u8; 256];
    while db.line(&mut line).is_some() {
        let Some((octets, start, end)) = split(&line) else {
            continue;
        };
        let name = line.get(start..end).unwrap_or_default();
        if each(&octets, name) {
            return true;
        }
    }
    false
}

/// Reads one line of `/etc/ethers` from `l`: an address into `*e`, and the
/// host name into `hostname`, which needs as many bytes as the line. Returns
/// 0, or -1 for a line that is a comment or is not an address and a name.
///
/// # Safety
///
/// `l` must be a NUL-terminated string, `e` a writable `struct ether_addr`,
/// and `hostname` valid for writes of as many bytes as `l` has.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_line(
    l: *const c_char,
    e: *mut EtherAddr,
    hostname: *mut c_char,
) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let line = unsafe { c_bytes(l) };
    let Some((octets, start, end)) = split(line) else {
        return -1;
    };
    // SAFETY: the caller passes a writable `struct ether_addr`.
    unsafe {
        e.write(EtherAddr {
            ether_addr_octet: octets,
        })
    };
    let name = line.get(start..end).unwrap_or_default();
    // SAFETY: the caller passes as many bytes as the line, which is more than
    // the name and its NUL.
    unsafe { copy_out(hostname, name, name.len()) };
    0
}

/// The host name `path` gives the address `e`, into `hostname`. Returns 0, or
/// -1 if there is none.
///
/// # Safety
///
/// `hostname` must be valid for writes of as many bytes as the longest line
/// of the file, and `e` be a readable `struct ether_addr`.
unsafe fn ntohost_from(path: &CStr, hostname: *mut c_char, e: *const EtherAddr) -> c_int {
    // SAFETY: the caller passes a readable address.
    let wanted = unsafe { e.read() }.ether_addr_octet;
    let mut found = false;
    let _ = search(path, |octets, name| {
        if *octets != wanted {
            return false;
        }
        // SAFETY: the caller passes room for a whole line's host name.
        unsafe { copy_out(hostname, name, name.len()) };
        found = true;
        true
    });
    if found { 0 } else { -1 }
}

/// The host name `/etc/ethers` gives the address `e`, into `hostname`.
/// Returns 0, or -1 if there is none.
///
/// # Safety
///
/// As [`ntohost_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_ntohost(hostname: *mut c_char, e: *const EtherAddr) -> c_int {
    // SAFETY: the caller's contract is `ntohost_from`'s.
    unsafe { ntohost_from(ETHERS, hostname, e) }
}

/// The address `path` gives the host `hostname`, into `*e`. Returns 0, or -1
/// if there is none.
///
/// # Safety
///
/// `hostname` must be a NUL-terminated string and `e` a writable
/// `struct ether_addr`.
unsafe fn hostton_from(path: &CStr, hostname: *const c_char, e: *mut EtherAddr) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let wanted = unsafe { c_bytes(hostname) };
    let mut address = None;
    let _ = search(path, |octets, name| {
        if name != wanted {
            return false;
        }
        address = Some(*octets);
        true
    });
    let Some(octets) = address else {
        return -1;
    };
    // SAFETY: the caller passes a writable `struct ether_addr`.
    unsafe {
        e.write(EtherAddr {
            ether_addr_octet: octets,
        })
    };
    0
}

/// The address `/etc/ethers` gives the host `hostname`, into `*e`. Returns 0,
/// or -1 if there is none.
///
/// # Safety
///
/// As [`hostton_from`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_hostton(hostname: *const c_char, e: *mut EtherAddr) -> c_int {
    // SAFETY: the caller's contract is `hostton_from`'s.
    unsafe { hostton_from(ETHERS, hostname, e) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ethers fixture, as a C path.
    fn fixture(name: &str) -> std::ffi::CString {
        std::ffi::CString::new(format!(
            "{}/tests/data/ether/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_default()
    }

    /// The address `text` names, through the C function.
    fn aton(text: &CStr) -> Option<[u8; ETH_ALEN]> {
        let mut address = EtherAddr::default();
        // SAFETY: the string is NUL-terminated and `address` is a live local.
        let got = unsafe { ether_aton_r(text.as_ptr(), &raw mut address) };
        (!got.is_null()).then_some(address.ether_addr_octet)
    }

    #[test]
    fn addresses_read_and_write_as_musl_reads_and_writes_them() {
        assert_eq!(
            aton(c"00:11:22:33:44:55"),
            Some([0, 0x11, 0x22, 0x33, 0x44, 0x55])
        );
        assert_eq!(aton(c"0:0:0:0:0:0"), Some([0; ETH_ALEN]));
        assert_eq!(aton(c"ff:FF:ff:ff:ff:ff"), Some([0xff; ETH_ALEN]));
        // musl takes any `strtoul` form for base 16, and a missing byte as 0.
        assert_eq!(aton(c"0x1:2:3:4:5:6"), Some([1, 2, 3, 4, 5, 6]));
        assert_eq!(aton(c":::::"), Some([0; ETH_ALEN]));
        // Too few bytes, too many, a byte that does not fit, and trailing text.
        assert_eq!(aton(c"1:2:3:4:5"), None);
        assert_eq!(aton(c"1:2:3:4:5:6:7"), None);
        assert_eq!(aton(c"1:2:3:4:5:100"), None);
        assert_eq!(aton(c"1:2:3:4:5:6 "), None);
        assert_eq!(aton(c""), None);

        let address = EtherAddr {
            ether_addr_octet: [0, 0x11, 0xab, 0x33, 0x44, 0x55],
        };
        let mut text = [0 as c_char; ETHER_TEXT];
        // SAFETY: `address` is a live local and `text` has 18 bytes.
        let got = unsafe { ether_ntoa_r(&raw const address, text.as_mut_ptr()) };
        assert_eq!(got, text.as_mut_ptr());
        // SAFETY: `ether_ntoa_r` ended the text.
        let written = unsafe { CStr::from_ptr(text.as_ptr()) };
        assert_eq!(written, c"00:11:AB:33:44:55");
        // The static forms answer the same way.
        // SAFETY: `address` is a live local.
        let shared = unsafe { ether_ntoa(&raw const address) };
        // SAFETY: the storage is NUL-terminated.
        assert_eq!(unsafe { CStr::from_ptr(shared) }, c"00:11:AB:33:44:55");
        // SAFETY: the string is NUL-terminated.
        let parsed = unsafe { ether_aton(c"00:11:AB:33:44:55".as_ptr()) };
        assert!(!parsed.is_null());
        // SAFETY: `ether_aton` filled its static address.
        assert_eq!(unsafe { parsed.read() }, address);
        // SAFETY: as above.
        assert!(unsafe { ether_aton(c"nonsense".as_ptr()) }.is_null());
    }

    #[test]
    fn a_line_of_the_ethers_file_splits_into_an_address_and_a_name() {
        let line = |text: &CStr| {
            let mut address = EtherAddr::default();
            let mut name = [0 as c_char; 64];
            // SAFETY: the string is NUL-terminated, and the two buffers are
            // live locals larger than any line here.
            let code = unsafe { ether_line(text.as_ptr(), &raw mut address, name.as_mut_ptr()) };
            // SAFETY: `ether_line` ended the name, or left it as it was.
            let name = unsafe { CStr::from_ptr(name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            (code, address.ether_addr_octet, name)
        };
        assert_eq!(
            line(c"  0:0:5e:0:53:1\thost.example.test  "),
            (0, [0, 0, 0x5e, 0, 0x53, 1], "host.example.test".into())
        );
        assert_eq!(
            line(c"0:0:5e:0:53:1 host # a comment"),
            (0, [0, 0, 0x5e, 0, 0x53, 1], "host".into())
        );
        assert_eq!(line(c"# only a comment").0, -1);
        assert_eq!(line(c"0:0:5e:0:53:1").0, -1);
        assert_eq!(line(c"").0, -1);
        assert_eq!(line(c"not-an-address host").0, -1);
    }

    #[test]
    fn the_ethers_file_maps_each_way() {
        let path = fixture("ethers");
        let mut name = [0 as c_char; 64];
        let address = EtherAddr {
            ether_addr_octet: [0, 0, 0x5e, 0, 0x53, 2],
        };
        // SAFETY: `name` has 64 bytes, more than any line of the fixture, and
        // `address` is a live local.
        let code = unsafe { ntohost_from(&path, name.as_mut_ptr(), &raw const address) };
        // SAFETY: the name was written and ended.
        let found = unsafe { CStr::from_ptr(name.as_ptr()) };
        assert_eq!((code, found), (0, c"second.example.test"));

        let mut found = EtherAddr::default();
        // SAFETY: the name is NUL-terminated and `found` is a live local.
        let code = unsafe { hostton_from(&path, c"first.example.test".as_ptr(), &raw mut found) };
        assert_eq!(
            (code, found.ether_addr_octet),
            (0, [0, 0, 0x5e, 0, 0x53, 1])
        );

        // A commented line names nothing, and a line without a name is not an
        // entry.
        // SAFETY: as above.
        let code =
            unsafe { hostton_from(&path, c"commented.example.test".as_ptr(), &raw mut found) };
        assert_eq!(code, -1);
        let missing = EtherAddr {
            ether_addr_octet: [0, 0, 0x5e, 0, 0x53, 4],
        };
        // SAFETY: as above.
        let code = unsafe { ntohost_from(&path, name.as_mut_ptr(), &raw const missing) };
        assert_eq!(code, -1);
        // A file that is not there answers nothing.
        // SAFETY: as above.
        let code = unsafe {
            hostton_from(
                &fixture("absent"),
                c"first.example.test".as_ptr(),
                &raw mut found,
            )
        };
        assert_eq!(code, -1);
    }
}
