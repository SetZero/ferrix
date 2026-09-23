//! `stdlib.h`: `getenv`, `secure_getenv` and `environ` beside them; the
//! radix-64 conversions `a64l` and `l64a`; and `getsubopt`.
//!
//! `exit`, `_Exit`, `atexit`, `quick_exit` and `at_quick_exit` are in
//! [`crate::exit`], and `abort` is in [`crate::signal`]. The radix-64 functions
//! and `getsubopt` follow musl 1.2.5's `misc/a64l.c` and `misc/getsubopt.c`
//! (MIT).

use core::cell::UnsafeCell;
use core::ffi::{CStr, c_char, c_int, c_long};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::auxv;
use crate::string::{strchr, strlen, strncmp};

/// `environ`, the process's environment, as `NAME=value` strings ending in a
/// null.
///
/// C declares it `char **environ` and may assign to it. `AtomicPtr` has the
/// same layout as a pointer, which is what lets it be read as one.
///
/// It has three names, as glibc gives it: `__environ`, and `environ` and
/// `_environ` as weak aliases at the same address. A program linked against
/// glibc may ask for any of them, and on x86-64 it asks for its own copy:
/// the linker made room for `__environ` in the program and the loader copies
/// the library's value there (`R_X86_64_COPY`), after which the program's
/// copy is the variable. That works because this library reaches it only
/// through its GOT, whose entry the loader points at the copy; rustc emits
/// that for any exported variable it cannot assume is its own. One name
/// could be a Rust `static`; three at one address are only possible in
/// assembly, below.
pub fn environ() -> &'static AtomicPtr<*mut c_char> {
    #[allow(unused_unsafe, reason = "the variable is a Rust static in unit tests")]
    // SAFETY: the variable lives as long as the process, and every access to
    // it is atomic.
    unsafe {
        &ENVIRON
    }
}

/// The variable, in unit tests: the test binary's own C library defines the
/// C names.
#[cfg(test)]
static ENVIRON: AtomicPtr<*mut c_char> = AtomicPtr::new(null_mut());

#[cfg(not(test))]
unsafe extern "C" {
    /// The variable, defined below.
    #[link_name = "__environ"]
    static ENVIRON: AtomicPtr<*mut c_char>;
}

// `__environ`, and its two weak aliases, in `.bss`.
#[cfg(all(not(test), target_pointer_width = "64"))]
core::arch::global_asm!(
    ".pushsection .bss.__environ,\"aw\",@nobits",
    ".p2align 3",
    ".globl __environ",
    ".type __environ, @object",
    ".size __environ, 8",
    ".weak environ",
    ".type environ, @object",
    ".size environ, 8",
    ".weak _environ",
    ".type _environ, @object",
    ".size _environ, 8",
    "__environ:",
    "environ:",
    "_environ:",
    ".zero 8",
    ".popsection",
);

// The same for a 32-bit pointer.
#[cfg(all(not(test), target_pointer_width = "32"))]
core::arch::global_asm!(
    ".pushsection .bss.__environ,\"aw\",%nobits",
    ".p2align 2",
    ".globl __environ",
    ".type __environ, %object",
    ".size __environ, 4",
    ".weak environ",
    ".type environ, %object",
    ".size environ, 4",
    ".weak _environ",
    ".type _environ, %object",
    ".size _environ, 4",
    "__environ:",
    "environ:",
    "_environ:",
    ".zero 4",
    ".popsection",
);

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
    let mut at = environ().load(Ordering::Relaxed);
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

/// `AT_SECURE`, from `linux/auxvec.h`: nonzero when the program runs with
/// privileges its caller does not have, as a set-user-ID program does.
const AT_SECURE: usize = 23;

/// [`getenv`], except that it gives null in a program running with more
/// privilege than its caller, so that the caller's environment cannot steer
/// it. musl decides the same from the same auxiliary vector entry.
///
/// # Safety
///
/// As for [`getenv`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn secure_getenv(name: *const c_char) -> *mut c_char {
    if auxv::get(AT_SECURE).is_some_and(|value| value != 0) {
        return null_mut();
    }
    // SAFETY: the caller's contract is `getenv`'s.
    unsafe { getenv(name) }
}

/// The digits of radix-64 notation, least significant first in a string.
const DIGITS: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// The 32-bit value that up to six radix-64 digits at `s` stand for, sign
/// extended from 32 bits. Reading stops at the end of the string, at a byte
/// that is not a digit, or after six digits.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn a64l(s: *const c_char) -> c_long {
    let mut value: u32 = 0;
    let mut shift = 0;
    let mut at = s;
    while shift < 36 {
        // SAFETY: the string has not ended before `at`, or the loop would have
        // stopped.
        let byte = unsafe { at.read() } as u8;
        let Some(digit) = DIGITS.iter().position(|&d| d == byte && byte != 0) else {
            break;
        };
        value |= (digit as u32) << shift;
        shift += 6;
        at = at.wrapping_add(1);
    }
    c_long::from(value as i32)
}

/// The buffer `l64a` returns.
#[derive(Debug)]
struct Radix64(UnsafeCell<[u8; 7]>);

// SAFETY: POSIX documents `l64a`'s result as static storage the next call
// overwrites, and the function as unsafe to call from two threads at once, as
// musl's is.
unsafe impl Sync for Radix64 {}

/// What `l64a` returns.
static RADIX64: Radix64 = Radix64(UnsafeCell::new([0; 7]));

/// The low 32 bits of `value` in radix-64 digits, least significant first,
/// in static storage the next call overwrites. Zero is the empty string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn l64a(value: c_long) -> *mut c_char {
    let mut rest = value as u32;
    let out = RADIX64.0.get().cast::<u8>();
    let mut len = 0;
    while rest != 0 && len < 6 {
        let digit = DIGITS.get((rest & 63) as usize).copied().unwrap_or(b'.');
        // SAFETY: `len` is below 6, inside the seven-byte buffer.
        unsafe { out.wrapping_add(len).write(digit) };
        rest >>= 6;
        len += 1;
    }
    // SAFETY: `len` is at most 6, inside the seven-byte buffer.
    unsafe { out.wrapping_add(len).write(0) };
    out.cast()
}

/// Takes the next comma-separated suboption from `*option`, as `mount -o`
/// options are written. `*option` moves past it, and a comma ending it is
/// overwritten with a NUL. Returns the index in `keys` of the name it matches
/// and sets `*value` to what follows its `=`, or to null; returns -1 for a
/// name not in `keys`.
///
/// # Safety
///
/// `option` and `value` must be valid, `*option` a writable NUL-terminated
/// string, and `keys` a null-terminated array of NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getsubopt(
    option: *mut *mut c_char,
    keys: *const *mut c_char,
    value: *mut *mut c_char,
) -> c_int {
    // SAFETY: the caller passes a valid option pointer.
    let start = unsafe { option.read() };
    // SAFETY: the caller passes a valid value pointer.
    unsafe { value.write(null_mut()) };
    // SAFETY: `start` is a NUL-terminated string.
    let comma = unsafe { strchr(start, c_int::from(b',')) };
    if comma.is_null() {
        // SAFETY: as above.
        let len = unsafe { strlen(start) };
        // SAFETY: the caller passes a valid option pointer.
        unsafe { option.write(start.wrapping_add(len)) };
    } else {
        // SAFETY: `strchr` found the comma inside the writable string.
        unsafe { comma.write(0) };
        // SAFETY: the caller passes a valid option pointer.
        unsafe { option.write(comma.wrapping_add(1)) };
    }
    let mut index = 0;
    loop {
        // SAFETY: `keys` is null-terminated, and the loop stops at the null.
        let key = unsafe { keys.wrapping_add(index).read() };
        if key.is_null() {
            return -1;
        }
        // SAFETY: each key is a NUL-terminated string.
        let len = unsafe { strlen(key) };
        // SAFETY: both are NUL-terminated strings.
        if unsafe { strncmp(key, start, len) } == 0 {
            // SAFETY: `start` matched `len` bytes without ending, so byte
            // `len` is still inside it.
            let after = unsafe { start.wrapping_add(len).read() } as u8;
            if after == b'=' {
                // SAFETY: the caller passes a valid value pointer.
                unsafe { value.write(start.wrapping_add(len + 1)) };
                return c_int::try_from(index).unwrap_or(c_int::MAX);
            }
            if after == 0 {
                return c_int::try_from(index).unwrap_or(c_int::MAX);
            }
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radix_64_round_trips_and_stops_where_musl_does() {
        let text = |value: c_long| {
            // SAFETY: `l64a` returns a NUL-terminated string.
            unsafe { CStr::from_ptr(l64a(value)) }.to_bytes().to_vec()
        };
        assert_eq!(text(0), b"");
        assert_eq!(text(1), b"/");
        assert_eq!(text(64), b"./");
        for value in [1, 63, 64, 12_345, 0x7fff_ffff, -1, -12_345] {
            let digits = l64a(value);
            // SAFETY: `l64a` returned a NUL-terminated string.
            assert_eq!(unsafe { a64l(digits) }, c_long::from(value as i32));
        }
        // SAFETY: a C string literal.
        assert_eq!(unsafe { a64l(c"./".as_ptr()) }, 64);
        // A byte that is no digit ends the number.
        // SAFETY: as above.
        assert_eq!(unsafe { a64l(c"/!/".as_ptr()) }, 1);
        // Digits past the sixth are ignored.
        // SAFETY: as above.
        assert_eq!(unsafe { a64l(c"zzzzzzzz".as_ptr()) }, -1);
    }

    #[test]
    fn getsubopt_splits_names_values_and_unknowns() {
        let mut text = *b"ro,size=64k,bogus,,mode\0";
        let mut option = text.as_mut_ptr().cast::<c_char>();
        let mut keys = [
            c"ro".as_ptr().cast_mut(),
            c"size".as_ptr().cast_mut(),
            c"mode".as_ptr().cast_mut(),
            null_mut(),
        ];
        let mut value = null_mut();
        let mut next = || {
            // SAFETY: `option` walks a writable string, `keys` is
            // null-terminated.
            let index = unsafe {
                getsubopt(
                    &raw mut option,
                    keys.as_mut_ptr().cast_const(),
                    &raw mut value,
                )
            };
            let value = (!value.is_null())
                // SAFETY: `getsubopt` points `value` into the string.
                .then(|| unsafe { CStr::from_ptr(value) }.to_bytes().to_vec());
            (index, value)
        };
        assert_eq!(next(), (0, None));
        assert_eq!(next(), (1, Some(b"64k".to_vec())));
        assert_eq!(next(), (-1, None));
        assert_eq!(next(), (-1, None));
        assert_eq!(next(), (2, None));
    }

    #[test]
    fn getenv_matches_whole_names_only() {
        let mut entries = [
            c"PATHS=no".as_ptr().cast_mut(),
            c"PATH=/bin".as_ptr().cast_mut(),
            null_mut(),
        ];
        environ().store(entries.as_mut_ptr(), Ordering::Relaxed);

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

        environ().store(null_mut(), Ordering::Relaxed);
    }
}
