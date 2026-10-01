//! `stdlib.h`: the environment and the small historical conversions and
//! option parser that do not have a more specific module.
//!
//! `exit`, `_Exit` and `atexit` are in [`crate::exit`], and `abort` is in
//! [`crate::signal`].

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int, c_long};
use core::ptr::{null_mut, read};
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::auxv::{self, AT_SECURE};

/// `environ`, the process's environment, as `NAME=value` strings ending in a
/// null.
///
/// C declares it `char **environ` and may assign to it. `AtomicPtr` has the
/// same layout as a pointer and makes it a safe `static`.
#[cfg_attr(not(test), unsafe(no_mangle))]
#[allow(non_upper_case_globals, reason = "C names it")]
pub static environ: AtomicPtr<*mut c_char> = AtomicPtr::new(null_mut());

/// The value of the environment variable `name`, or null.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, and `environ` must be null or a
/// null-terminated array of NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getenv(name: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    // No entry can match a name containing `=`: the name would end earlier.
    if name.is_empty() || name.contains(&b'=') {
        return null_mut();
    }
    let mut at = environ.load(Ordering::Relaxed);
    if at.is_null() {
        return null_mut();
    }
    loop {
        // SAFETY: the array is null-terminated, and `at` has not passed the
        // null.
        let entry = unsafe { at.read() };
        if entry.is_null() {
            return null_mut();
        }
        // SAFETY: each entry is a NUL-terminated string.
        let bytes = unsafe { CStr::from_ptr(entry) }.to_bytes();
        if let Some(rest) = bytes.strip_prefix(name)
            && rest.first() == Some(&b'=')
        {
            return entry.wrapping_add(name.len() + 1);
        }
        at = at.wrapping_add(1);
    }
}

/// The value of `name`, except that privileged execution cannot read values
/// supplied by an untrusted environment.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, and `environ` must satisfy the
/// requirements of [`getenv`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn secure_getenv(name: *const c_char) -> *mut c_char {
    if auxv::get(AT_SECURE).unwrap_or(0) != 0 {
        null_mut()
    } else {
        // SAFETY: this function has the same pointer contract as `getenv`.
        unsafe { getenv(name) }
    }
}

/// Converts one character from the historical `./0-9A-Za-z` alphabet.
fn base64_digit(byte: u8) -> Option<u32> {
    match byte {
        b'.' => Some(0),
        b'/' => Some(1),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 2),
        b'A'..=b'Z' => Some(u32::from(byte - b'A') + 12),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 38),
        _ => None,
    }
}

/// Converts at most six radix-64 characters, least-significant group first,
/// to the signed 32-bit value the historical interface defines.
///
/// # Safety
///
/// `text` must point to a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn a64l(text: *const c_char) -> c_long {
    let mut value = 0_u32;
    for index in 0..6_u32 {
        // SAFETY: the caller promises a C string and no read follows its NUL.
        let byte = unsafe { read(text.wrapping_add(index as usize)).cast_unsigned() };
        if byte == 0 {
            break;
        }
        let Some(digit) = base64_digit(byte) else {
            break;
        };
        value |= digit << (index * 6);
    }
    c_long::from(value.cast_signed())
}

const BASE64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

struct L64aBuffer(UnsafeCell<[u8; 7]>);

// SAFETY: POSIX explicitly permits `l64a` to use one shared static buffer and
// does not require the function to be thread-safe.
unsafe impl Sync for L64aBuffer {}

static L64A_BUFFER: L64aBuffer = L64aBuffer(UnsafeCell::new([0; 7]));

/// Converts the low 32 bits of `value` to at most six radix-64 characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn l64a(value: c_long) -> *mut c_char {
    // SAFETY: this is the documented static result buffer. A later call may
    // overwrite it, and callers must provide any synchronization they need.
    let out = unsafe { &mut *L64A_BUFFER.0.get() };
    let mut rest = value as u32;
    let mut written = 0;
    while rest != 0 && written < 6 {
        out[written] = BASE64[(rest & 63) as usize];
        written += 1;
        rest >>= 6;
    }
    out[written] = 0;
    out.as_mut_ptr().cast()
}

/// Parses the next comma-separated suboption in `*optionp`.
///
/// # Safety
///
/// `optionp` and `valuep` must be writable pointers, `*optionp` must point to
/// a writable C string, and `tokens` must be a null-terminated array of C
/// string pointers.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getsubopt(
    optionp: *mut *mut c_char,
    tokens: *const *mut c_char,
    valuep: *mut *mut c_char,
) -> c_int {
    // SAFETY: the caller supplies writable pointer objects.
    let option = unsafe { optionp.read() };
    let mut end = option;
    loop {
        // SAFETY: `end` walks the writable C string through its terminator.
        match unsafe { end.read().cast_unsigned() } {
            0 => break,
            b',' => {
                // SAFETY: the comma is part of the caller's writable string.
                unsafe { end.write(0) };
                // SAFETY: write back the start of the following suboption.
                unsafe { optionp.write(end.wrapping_add(1)) };
                break;
            }
            _ => end = end.wrapping_add(1),
        }
    }
    // A final token advances to its terminating NUL.
    // SAFETY: `optionp` is writable.
    if unsafe { optionp.read() } == option {
        unsafe { optionp.write(end) };
    }

    let bytes = unsafe { CStr::from_ptr(option) }.to_bytes();
    let (name, value) = match bytes.iter().position(|byte| *byte == b'=') {
        Some(equal) => (&bytes[..equal], option.wrapping_add(equal + 1)),
        None => (bytes, null_mut()),
    };

    let mut index = 0_usize;
    loop {
        // SAFETY: `tokens` is a null-terminated pointer array.
        let token = unsafe { tokens.wrapping_add(index).read() };
        if token.is_null() {
            // An unknown suboption returns the whole token, including any '='.
            unsafe { valuep.write(option) };
            return -1;
        }
        // SAFETY: every non-null entry is a C string.
        if unsafe { CStr::from_ptr(token) }.to_bytes() == name {
            unsafe { valuep.write(value) };
            return c_int::try_from(index).unwrap_or(-1);
        }
        index = index.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getenv_matches_whole_names_only() {
        let mut entries = [
            c"PATHS=no".as_ptr().cast_mut(),
            c"PATH=/bin".as_ptr().cast_mut(),
            null_mut(),
        ];
        environ.store(entries.as_mut_ptr(), Ordering::Relaxed);

        // SAFETY: the name is a C string literal and `environ` points at a
        // null-terminated array of them.
        let path = unsafe { getenv(c"PATH".as_ptr()) };
        assert!(!path.is_null());
        // SAFETY: `getenv` returned a pointer into a C string literal.
        assert_eq!(unsafe { CStr::from_ptr(path) }, c"/bin");
        // SAFETY: as above.
        assert!(unsafe { getenv(c"PAT".as_ptr()) }.is_null());
        // SAFETY: as above.
        assert!(unsafe { getenv(c"PATH=".as_ptr()) }.is_null());
        // SAFETY: as above.
        assert!(unsafe { getenv(c"".as_ptr()) }.is_null());

        environ.store(null_mut(), Ordering::Relaxed);
    }
}
