//! Ethernet address text conversion and the `/etc/ethers` database.
//!
//! The conversion functions follow the traditional BSD interface declared by
//! `netinet/ether.h`. The database functions accept the usual one-address,
//! one-host-name-per-line file, with blank lines and comments ignored.

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::netdb::{Database, c_bytes, is_space};

/// C's `struct ether_addr`, from `net/ethernet.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EtherAddr {
    /// The six octets in network order.
    pub ether_addr_octet: [u8; 6],
}

const _: () = assert!(size_of::<EtherAddr>() == 6);
const ETHERS: &CStr = c"/etc/ethers";
const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Parses six hexadecimal octets separated by colons.
fn parse_address(text: &[u8]) -> Option<EtherAddr> {
    let mut octets = [0u8; 6];
    let mut rest = text;
    for (index, octet) in octets.iter_mut().enumerate() {
        let separator = rest.iter().position(|&byte| byte == b':');
        let end = separator.unwrap_or(rest.len());
        let part = rest.get(..end)?;
        if part.is_empty() || part.len() > 2 {
            return None;
        }
        let mut value = 0u8;
        for &byte in part {
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return None,
            };
            value = value.checked_mul(16)?.checked_add(digit)?;
        }
        *octet = value;
        if index == 5 {
            if separator.is_some() {
                return None;
            }
        } else {
            let _ = separator?;
            rest = rest.get(end + 1..)?;
        }
    }
    Some(EtherAddr {
        ether_addr_octet: octets,
    })
}

/// Writes an address as 17 uppercase hexadecimal characters and a NUL.
///
/// # Safety
///
/// `address` must point to an address, and `text` to at least 18 writable
/// bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_ntoa_r(address: *const EtherAddr, text: *mut c_char) -> *mut c_char {
    if address.is_null() || text.is_null() {
        return null_mut();
    }
    // SAFETY: the caller passes an address to read.
    let octets = unsafe { (*address).ether_addr_octet };
    for (index, octet) in octets.into_iter().enumerate() {
        let offset = index * 3;
        let high = HEX.get(usize::from(octet >> 4)).copied().unwrap_or(b'0');
        let low = HEX.get(usize::from(octet & 0xf)).copied().unwrap_or(b'0');
        // SAFETY: the caller passes at least 18 writable bytes; each offset is
        // at most 15 and the writes end at offset 17 below.
        unsafe {
            text.wrapping_add(offset)
                .write(c_char::from_ne_bytes([high]))
        };
        // SAFETY: as above.
        unsafe {
            text.wrapping_add(offset + 1)
                .write(c_char::from_ne_bytes([low]))
        };
        // SAFETY: as above.
        unsafe {
            text.wrapping_add(offset + 2)
                .write(if index == 5 { 0 } else { b':' as c_char })
        };
    }
    text
}

/// Parses an Ethernet address into caller-provided storage.
///
/// # Safety
///
/// `text` must be a NUL-terminated string, and `address` must point to
/// writable storage for an [`EtherAddr`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_aton_r(
    text: *const c_char,
    address: *mut EtherAddr,
) -> *mut EtherAddr {
    if text.is_null() || address.is_null() {
        return null_mut();
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let text = unsafe { c_bytes(text) };
    let Some(parsed) = parse_address(text) else {
        return null_mut();
    };
    // SAFETY: the caller passes writable storage for one address.
    unsafe { address.write(parsed) };
    address
}

/// A static value returned by `ether_aton`.
#[derive(Debug)]
struct AddressBuffer(UnsafeCell<EtherAddr>);

// SAFETY: C specifies static storage overwritten by the next call. Concurrent
// calls are therefore not supported, and only `ether_aton` writes the value.
unsafe impl Sync for AddressBuffer {}

static ADDRESS: AddressBuffer = AddressBuffer(UnsafeCell::new(EtherAddr {
    ether_addr_octet: [0; 6],
}));

/// Parses an Ethernet address into static storage overwritten by the next
/// call.
///
/// # Safety
///
/// `text` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_aton(text: *const c_char) -> *mut EtherAddr {
    // SAFETY: the caller passes a NUL-terminated string, and `ADDRESS` is the
    // static storage reserved for this function.
    unsafe { ether_aton_r(text, ADDRESS.0.get()) }
}

/// A static text buffer returned by `ether_ntoa`.
#[derive(Debug)]
struct TextBuffer(UnsafeCell<[c_char; 18]>);

// SAFETY: as for `AddressBuffer`; only `ether_ntoa` writes this buffer.
unsafe impl Sync for TextBuffer {}

static TEXT: TextBuffer = TextBuffer(UnsafeCell::new([0; 18]));

/// Formats an Ethernet address in static storage overwritten by the next
/// call.
///
/// # Safety
///
/// `address` must point to an [`EtherAddr`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_ntoa(address: *const EtherAddr) -> *mut c_char {
    // SAFETY: the caller passes an address to read, and `TEXT` is the static
    // storage reserved for this function.
    unsafe { ether_ntoa_r(address, TEXT.0.get().cast()) }
}

/// Parses one non-comment line from an ethers database.
fn parse_line(line: &[u8]) -> Option<(EtherAddr, &[u8])> {
    let start = line.iter().position(|&byte| !is_space(byte))?;
    let line = line.get(start..)?;
    if line.first() == Some(&b'#') {
        return None;
    }
    let address_end = line.iter().position(|&byte| is_space(byte))?;
    let address = parse_address(line.get(..address_end)?)?;
    let after_address = line.get(address_end..)?;
    let name_start = after_address.iter().position(|&byte| !is_space(byte))?;
    let name = after_address.get(name_start..)?;
    let name_end = name
        .iter()
        .position(|&byte| is_space(byte) || byte == b'#')
        .unwrap_or(name.len());
    let name = name.get(..name_end)?;
    if name.is_empty() {
        return None;
    }
    Some((address, name))
}

/// Parses an ethers database line, storing its address and host name.
///
/// # Safety
///
/// `line` must be a NUL-terminated string, `address` must point to writable
/// storage for an [`EtherAddr`], and `host` must have room for the host name
/// and its NUL.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_line(
    line: *const c_char,
    address: *mut EtherAddr,
    host: *mut c_char,
) -> c_int {
    if line.is_null() || address.is_null() || host.is_null() {
        return -1;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let line = unsafe { c_bytes(line) };
    let Some((parsed, name)) = parse_line(line) else {
        return -1;
    };
    // SAFETY: the caller passes storage for one address.
    unsafe { address.write(parsed) };
    for (index, &byte) in name.iter().enumerate() {
        // SAFETY: the caller promises room for the name and NUL.
        unsafe { host.wrapping_add(index).write(byte as c_char) };
    }
    // SAFETY: as above.
    unsafe { host.wrapping_add(name.len()).write(0) };
    0
}

/// Visits every complete line in an ethers database.
fn find_entry<T>(path: &CStr, mut select: impl FnMut(EtherAddr, &[u8]) -> Option<T>) -> Option<T> {
    let mut database = Database::open(path).ok().flatten()?;
    let mut line = [0u8; 512];
    while let Some(len) = database.line(&mut line) {
        let complete =
            len < line.len().saturating_sub(1) || (len != 0 && line.get(len - 1) == Some(&b'\n'));
        if complete {
            if let Some((address, host)) = parse_line(line.get(..len)?)
                && let Some(found) = select(address, host)
            {
                return Some(found);
            }
        } else {
            while !database.at_end() && database.byte() != c_int::from(b'\n') {}
        }
    }
    None
}

fn host_to_address(path: &CStr, host: &[u8]) -> Option<EtherAddr> {
    find_entry(path, |address, candidate| {
        (candidate == host).then_some(address)
    })
}

fn address_to_host<'a>(path: &CStr, address: EtherAddr, out: &'a mut [u8]) -> Option<&'a [u8]> {
    let len = find_entry(path, |candidate, host| {
        if candidate != address || host.len() >= out.len() {
            return None;
        }
        out.get_mut(..host.len())?.copy_from_slice(host);
        *out.get_mut(host.len())? = 0;
        Some(host.len())
    })?;
    out.get(..len)
}

/// Finds `host` in `/etc/ethers` and stores its address.
///
/// # Safety
///
/// `host` must be a NUL-terminated string, and `address` must point to
/// writable storage for an [`EtherAddr`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_hostton(host: *const c_char, address: *mut EtherAddr) -> c_int {
    if host.is_null() || address.is_null() {
        return -1;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let host = unsafe { c_bytes(host) };
    let Some(found) = host_to_address(ETHERS, host) else {
        return -1;
    };
    // SAFETY: the caller passes storage for one address.
    unsafe { address.write(found) };
    0
}

/// Finds `address` in `/etc/ethers` and stores its host name.
///
/// # Safety
///
/// `address` must point to an [`EtherAddr`], and `host` must have enough room
/// for the database's host name and its NUL.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ether_ntohost(host: *mut c_char, address: *const EtherAddr) -> c_int {
    if host.is_null() || address.is_null() {
        return -1;
    }
    // SAFETY: the caller passes an address to read.
    let address = unsafe { address.read() };
    let mut name = [0u8; 256];
    let Some(found) = address_to_host(ETHERS, address, &mut name) else {
        return -1;
    };
    for (index, &byte) in found.iter().enumerate() {
        // SAFETY: the caller promises room for the host name and NUL.
        unsafe { host.wrapping_add(index).write(byte as c_char) };
    }
    // SAFETY: as above.
    unsafe { host.wrapping_add(found.len()).write(0) };
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netdb::testing::fixture;

    #[test]
    fn parses_and_formats_addresses() {
        let mut address = EtherAddr::default();
        // SAFETY: the literal is terminated and `address` is writable.
        let parsed = unsafe { ether_aton_r(c"01:23:45:67:89:ab".as_ptr(), &raw mut address) };
        assert_eq!(parsed, &raw mut address);
        assert_eq!(address.ether_addr_octet, [1, 0x23, 0x45, 0x67, 0x89, 0xab]);
        let mut text = [0 as c_char; 18];
        // SAFETY: the pointers name an address and 18 writable bytes.
        let formatted = unsafe { ether_ntoa_r(&raw const address, text.as_mut_ptr()) };
        assert_eq!(formatted, text.as_mut_ptr());
        // SAFETY: the formatter wrote a NUL-terminated string.
        // SAFETY: the formatter wrote a NUL-terminated string.
        let formatted = unsafe { CStr::from_ptr(text.as_ptr()) };
        assert_eq!(formatted, c"01:23:45:67:89:AB");

        for invalid in [
            c"",
            c"1:2:3:4:5",
            c"1:2:3:4:5:6:7",
            c"1:2:3:4:5:gg",
            c"100:2:3:4:5:6",
        ] {
            // SAFETY: each literal is terminated and `address` is writable.
            assert!(unsafe { ether_aton_r(invalid.as_ptr(), &raw mut address) }.is_null());
        }
    }

    #[test]
    fn static_forms_use_static_storage() {
        // SAFETY: the literal is terminated.
        let address = unsafe { ether_aton(c"a:b:c:d:e:f".as_ptr()) };
        assert!(!address.is_null());
        // SAFETY: `ether_aton` returned an address in static storage.
        let text = unsafe { ether_ntoa(address) };
        // SAFETY: `ether_ntoa` returned a NUL-terminated static string.
        assert_eq!(unsafe { CStr::from_ptr(text) }, c"0A:0B:0C:0D:0E:0F");
    }

    #[test]
    fn parses_database_lines() {
        let mut address = EtherAddr::default();
        let mut host = [0 as c_char; 64];
        // SAFETY: the line is terminated and the outputs have enough room.
        let result = unsafe {
            ether_line(
                c"  52:54:00:12:34:56  ferrix # vm\n".as_ptr(),
                &raw mut address,
                host.as_mut_ptr(),
            )
        };
        assert_eq!(result, 0);
        assert_eq!(address.ether_addr_octet, [0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        // SAFETY: `ether_line` terminated the host name.
        assert_eq!(unsafe { CStr::from_ptr(host.as_ptr()) }, c"ferrix");
        // SAFETY: the inputs and outputs are valid.
        let result =
            unsafe { ether_line(c"# comment".as_ptr(), &raw mut address, host.as_mut_ptr()) };
        assert_eq!(result, -1);
    }

    #[test]
    fn searches_the_ethers_database_in_both_directions() {
        let path = fixture("ethers");
        let address = host_to_address(&path, b"router").expect("fixture has router");
        assert_eq!(address.ether_addr_octet, [0x02, 0, 0, 0, 0, 1]);
        let mut host = [0u8; 64];
        assert_eq!(
            address_to_host(&path, address, &mut host),
            Some(b"router".as_slice())
        );
        assert_eq!(host_to_address(&path, b"missing"), None);
    }
}
