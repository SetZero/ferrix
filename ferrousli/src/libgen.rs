//! `libgen.h`: POSIX's `basename` and `dirname`.
//!
//! Both may write NULs into the path they are given, and both may return a
//! pointer to a static string instead. Adapted from musl 1.2.5 (MIT),
//! `src/misc/basename.c` and `src/misc/dirname.c`.
//!
//! glibc has two `basename`s. The one in `<string.h>` is GNU's, which never
//! modifies its argument and returns an empty string for a path ending in
//! `/`. `<libgen.h>` renames calls to POSIX's as `__xpg_basename`. This library
//! exports POSIX's under both names, so a program built against glibc's
//! `<libgen.h>` finds it too.

use core::ffi::c_char;

use crate::string::strlen;

/// `"."`, returned for an empty or null path.
static DOT: [c_char; 2] = [b'.' as c_char, 0];

/// `"/"`, returned for a path of only slashes.
static SLASH: [c_char; 2] = [b'/' as c_char, 0];

/// A static string as C's `char *`. The functions' contract forbids writing
/// through it.
fn static_string(s: &'static [c_char; 2]) -> *mut c_char {
    s.as_ptr().cast_mut()
}

/// The byte at `i` in `s`.
///
/// # Safety
///
/// `s` must be readable at `i`.
unsafe fn at(s: *const c_char, i: usize) -> u8 {
    // SAFETY: the caller vouches for the byte.
    unsafe { s.wrapping_add(i).read() }.cast_unsigned()
}

/// The last component of `path`, with trailing slashes removed by writing
/// NULs over them.
///
/// # Safety
///
/// `path` must be null or a writable NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn basename(path: *mut c_char) -> *mut c_char {
    // SAFETY: the caller passes null or a string.
    if path.is_null() || unsafe { at(path, 0) } == 0 {
        return static_string(&DOT);
    }
    // SAFETY: as above; the string is not empty.
    let mut i = unsafe { strlen(path) } - 1;
    // SAFETY: `i` stays within the string.
    while i > 0 && unsafe { at(path, i) } == b'/' {
        // SAFETY: as above, and the caller lets the string be written.
        unsafe { path.wrapping_add(i).write(0) };
        i -= 1;
    }
    // SAFETY: as above.
    while i > 0 && unsafe { at(path, i - 1) } != b'/' {
        i -= 1;
    }
    path.wrapping_add(i)
}

/// POSIX's `basename` under the name glibc's `<libgen.h>` calls.
///
/// # Safety
///
/// As [`basename`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __xpg_basename(path: *mut c_char) -> *mut c_char {
    // SAFETY: the same contract.
    unsafe { basename(path) }
}

/// Everything in `path` before its last component, ending the string there
/// with a NUL.
///
/// # Safety
///
/// `path` must be null or a writable NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dirname(path: *mut c_char) -> *mut c_char {
    // SAFETY: the caller passes null or a string.
    if path.is_null() || unsafe { at(path, 0) } == 0 {
        return static_string(&DOT);
    }
    // SAFETY: as above; the string is not empty.
    let mut i = unsafe { strlen(path) } - 1;
    // Trailing slashes.
    // SAFETY: `i` stays within the string in all three loops.
    while unsafe { at(path, i) } == b'/' {
        if i == 0 {
            return static_string(&SLASH);
        }
        i -= 1;
    }
    // The last component.
    // SAFETY: as above.
    while unsafe { at(path, i) } != b'/' {
        if i == 0 {
            return static_string(&DOT);
        }
        i -= 1;
    }
    // The slashes before it.
    // SAFETY: as above.
    while unsafe { at(path, i) } == b'/' {
        if i == 0 {
            return static_string(&SLASH);
        }
        i -= 1;
    }
    // SAFETY: `i + 1` is within the string, which the caller lets be written.
    unsafe { path.wrapping_add(i + 1).write(0) };
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::CStr;

    /// Runs `f` on a writable copy of `path` and returns the result.
    fn run(f: unsafe extern "C" fn(*mut c_char) -> *mut c_char, path: &str) -> String {
        let mut buf = path.as_bytes().to_vec();
        buf.push(0);
        // SAFETY: `buf` is a writable NUL-terminated string.
        let out = unsafe { f(buf.as_mut_ptr().cast()) };
        // SAFETY: the result points into `buf` or at a static string.
        unsafe { CStr::from_ptr(out) }
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn basename_takes_the_last_component() {
        for (path, want) in [
            ("", "."),
            ("/usr/lib", "lib"),
            ("/usr/", "usr"),
            ("/", "/"),
            ("///", "/"),
            ("//usr//lib//", "lib"),
            ("a", "a"),
        ] {
            assert_eq!(run(basename, path), want, "basename({path:?})");
        }
    }

    #[test]
    fn dirname_drops_the_last_component() {
        for (path, want) in [
            ("", "."),
            ("/usr/lib", "/usr"),
            ("/usr/", "/"),
            ("usr", "."),
            ("//a//b//", "//a"),
            ("///", "/"),
            ("/a", "/"),
        ] {
            assert_eq!(run(dirname, path), want, "dirname({path:?})");
        }
    }
}
