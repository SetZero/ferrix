//! `stdlib.h`: `getenv`, and `environ` beside it.
//!
//! `exit`, `_Exit` and `atexit` are in [`crate::exit`], and `abort` is in
//! [`crate::signal`].

use core::ffi::{CStr, c_char};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

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
