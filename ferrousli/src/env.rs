//! Changing the environment: `setenv`, `unsetenv`, `putenv` and `clearenv`.
//!
//! The environment starts as the kernel's array on the initial stack, which
//! the library does not own. The first change copies that array into memory
//! from `malloc`, and later changes resize the copy. A program may also point
//! `environ` at an array of its own. The library then copies again rather than
//! resizing memory it did not allocate.
//!
//! `setenv` allocates each `NAME=value` string it adds. `putenv` adds the
//! program's own string, which stays the program's. The library keeps a list
//! of the strings it allocated, so that a replaced or removed one can be freed
//! while the program's are never touched. The design, list included, is
//! musl's (MIT).
//!
//! None of these functions is thread-safe, and POSIX does not require them to
//! be.

use core::ffi::{c_char, c_int};
use core::mem::size_of;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::errno;
use crate::malloc::{free, malloc, realloc};
use crate::stdlib::{environ, getenv};
use crate::string::{memcpy, strlen, strncmp};

/// The size of one entry of an environment array.
const ENTRY: usize = size_of::<*mut c_char>();

/// The array the library last allocated for `environ`, or null.
static OWNED_ARRAY: AtomicPtr<*mut c_char> = AtomicPtr::new(null_mut());
/// The strings the library allocated that may still be in the environment,
/// in an array from `malloc`. A null entry is a free slot.
static ALLOCATED: AtomicPtr<*mut c_char> = AtomicPtr::new(null_mut());
/// How many entries `ALLOCATED` has.
static ALLOCATED_LEN: AtomicUsize = AtomicUsize::new(0);

/// The length of the name at the start of `s`: the bytes before its first `=`,
/// or before its end.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
unsafe fn name_len(s: *const c_char) -> usize {
    let mut len = 0;
    loop {
        // SAFETY: the string is NUL-terminated, and the loop stops at its end.
        let byte = unsafe { s.wrapping_add(len).read() };
        if byte == 0 || byte == b'=' as c_char {
            return len;
        }
        len += 1;
    }
}

/// The byte at `offset` in `s`.
///
/// # Safety
///
/// `s` must be readable at `offset`.
unsafe fn byte_at(s: *const c_char, offset: usize) -> c_char {
    // SAFETY: the caller vouches for the byte.
    unsafe { s.wrapping_add(offset).read() }
}

/// Records that the environment no longer holds `old` and now holds `new`.
/// Either may be null. `old` is freed if the library allocated it.
///
/// # Safety
///
/// A non-null `new` came from `malloc`. `old`, if the library allocated it, is
/// not used again.
unsafe fn replace_allocated(old: *mut c_char, new: *mut c_char) {
    let table = ALLOCATED.load(Ordering::Relaxed);
    let len = ALLOCATED_LEN.load(Ordering::Relaxed);
    let mut new = new;
    let mut i = 0;
    while i < len {
        let slot = table.wrapping_add(i);
        // SAFETY: `slot` is one of the table's `len` entries.
        let entry = unsafe { slot.read() };
        if !old.is_null() && entry == old {
            // SAFETY: as above.
            unsafe { slot.write(new) };
            // SAFETY: the library allocated `old`, and the environment no
            // longer holds it.
            unsafe { free(old.cast()) };
            return;
        }
        if entry.is_null() && !new.is_null() {
            // SAFETY: as above.
            unsafe { slot.write(new) };
            new = null_mut();
        }
        i += 1;
    }
    if new.is_null() {
        return;
    }
    let Some(bytes) = len.checked_add(1).and_then(|n| n.checked_mul(ENTRY)) else {
        return;
    };
    // SAFETY: the table is null or came from `malloc`, and nothing else holds
    // it.
    let grown = unsafe { realloc(table.cast(), bytes) }.cast::<*mut c_char>();
    if grown.is_null() {
        // The string stays in the environment; it just cannot be freed later.
        return;
    }
    // SAFETY: the grown table holds `len + 1` entries.
    unsafe { grown.wrapping_add(len).write(new) };
    ALLOCATED.store(grown, Ordering::Relaxed);
    ALLOCATED_LEN.store(len + 1, Ordering::Relaxed);
}

/// Puts `s`, whose name is `len` bytes long and followed by `=`, into the
/// environment, replacing an entry with the same name or adding one.
/// `allocated` is `s` when the library allocated it, and null otherwise.
///
/// # Safety
///
/// `s` is a NUL-terminated string that stays valid while it is in the
/// environment. `environ` is null or a null-terminated array of strings.
unsafe fn insert(s: *mut c_char, len: usize, allocated: *mut c_char) -> c_int {
    let env = environ.load(Ordering::Relaxed);
    let mut count = 0;
    if !env.is_null() {
        loop {
            let at = env.wrapping_add(count);
            // SAFETY: the array is null-terminated, and `at` has not passed
            // its end.
            let entry = unsafe { at.read() };
            if entry.is_null() {
                break;
            }
            // SAFETY: both are NUL-terminated strings. Comparing the `=` as
            // well makes a longer name with this prefix differ.
            if unsafe { strncmp(s, entry, len + 1) } == 0 {
                // SAFETY: as above, and the array is writable, as C's
                // `char **environ` says.
                unsafe { at.write(s) };
                // SAFETY: the entry is no longer in the environment.
                unsafe { replace_allocated(entry, allocated) };
                return 0;
            }
            count += 1;
        }
    }

    let Some(bytes) = count.checked_add(2).and_then(|n| n.checked_mul(ENTRY)) else {
        errno::set(errno::ENOMEM);
        return fail(allocated);
    };
    let owned = OWNED_ARRAY.load(Ordering::Relaxed);
    let array = if !owned.is_null() && env == owned {
        // SAFETY: the library allocated the array `environ` still names.
        unsafe { realloc(owned.cast(), bytes) }.cast::<*mut c_char>()
    } else {
        let fresh = malloc(bytes).cast::<*mut c_char>();
        if !fresh.is_null() && count > 0 {
            // SAFETY: the old array holds `count` entries, and the new one
            // room for `count + 2`.
            let _ = unsafe { memcpy(fresh.cast(), env.cast(), count * ENTRY) };
        }
        fresh
    };
    if array.is_null() {
        return fail(allocated);
    }
    if env != owned {
        // The program replaced `environ`, so the array the library allocated
        // before is no longer in use.
        // SAFETY: `owned` is null or came from `malloc`.
        unsafe { free(owned.cast()) };
    }
    // SAFETY: the array has room for `count + 2` entries.
    unsafe { array.wrapping_add(count).write(s) };
    // SAFETY: as above.
    unsafe { array.wrapping_add(count + 1).write(null_mut()) };
    environ.store(array, Ordering::Relaxed);
    OWNED_ARRAY.store(array, Ordering::Relaxed);
    if !allocated.is_null() {
        // SAFETY: `allocated` came from `malloc` and is now in the
        // environment.
        unsafe { replace_allocated(null_mut(), allocated) };
    }
    0
}

/// Gives up on adding `allocated`, which is null or the library's string.
fn fail(allocated: *mut c_char) -> c_int {
    // SAFETY: the string never reached the environment.
    unsafe { free(allocated.cast()) };
    -1
}

/// Sets the environment variable `name` to `value`, replacing an existing
/// value only if `overwrite` is not zero.
///
/// # Safety
///
/// `name` and `value` must be NUL-terminated strings, and `environ` null or a
/// null-terminated array of strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setenv(
    name: *const c_char,
    value: *const c_char,
    overwrite: c_int,
) -> c_int {
    if name.is_null() {
        errno::set(errno::EINVAL);
        return -1;
    }
    // SAFETY: the caller passes a NUL-terminated name.
    let len = unsafe { name_len(name) };
    // SAFETY: `name_len` stopped at a byte of the name.
    if len == 0 || unsafe { byte_at(name, len) } != 0 {
        errno::set(errno::EINVAL);
        return -1;
    }
    // SAFETY: as above.
    if overwrite == 0 && !unsafe { getenv(name) }.is_null() {
        return 0;
    }
    // SAFETY: the caller passes a NUL-terminated value.
    let value_len = unsafe { strlen(value) };
    let Some(total) = len.checked_add(value_len).and_then(|n| n.checked_add(2)) else {
        errno::set(errno::ENOMEM);
        return -1;
    };
    let s = malloc(total).cast::<c_char>();
    if s.is_null() {
        return -1;
    }
    // SAFETY: `s` holds `len + value_len + 2` bytes, and the name `len`.
    let _ = unsafe { memcpy(s.cast(), name.cast(), len) };
    // SAFETY: as above.
    unsafe { s.wrapping_add(len).write(b'=' as c_char) };
    // SAFETY: as above, with the value's NUL.
    let _ = unsafe { memcpy(s.wrapping_add(len + 1).cast(), value.cast(), value_len + 1) };
    // SAFETY: `s` is a new string the environment now owns.
    unsafe { insert(s, len, s) }
}

/// Removes every entry for `name` from the environment.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, and `environ` null or a
/// null-terminated array of strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn unsetenv(name: *const c_char) -> c_int {
    if name.is_null() {
        errno::set(errno::EINVAL);
        return -1;
    }
    // SAFETY: the caller passes a NUL-terminated name.
    let len = unsafe { name_len(name) };
    // SAFETY: `name_len` stopped at a byte of the name.
    if len == 0 || unsafe { byte_at(name, len) } != 0 {
        errno::set(errno::EINVAL);
        return -1;
    }
    let env = environ.load(Ordering::Relaxed);
    if env.is_null() {
        return 0;
    }
    let mut read = env;
    let mut write = env;
    loop {
        // SAFETY: the array is null-terminated, and `read` has not passed its
        // end.
        let entry = unsafe { read.read() };
        if entry.is_null() {
            break;
        }
        // SAFETY: both are NUL-terminated strings. An entry that matches
        // `len` bytes is at least that long, so its byte at `len` is readable.
        let matches = unsafe { strncmp(name, entry, len) } == 0
            && unsafe { byte_at(entry, len) } == b'=' as c_char;
        if matches {
            // SAFETY: the entry leaves the environment.
            unsafe { replace_allocated(entry, null_mut()) };
        } else {
            if write != read {
                // SAFETY: `write` is behind `read`, inside the array.
                unsafe { write.write(entry) };
            }
            write = write.wrapping_add(1);
        }
        read = read.wrapping_add(1);
    }
    if write != read {
        // SAFETY: as above.
        unsafe { write.write(null_mut()) };
    }
    0
}

/// Adds `string`, of the form `NAME=value`, to the environment as it is: a
/// later change to the string changes the environment. A string with no `=`
/// removes the variable, as in glibc and musl.
///
/// # Safety
///
/// `string` must be a NUL-terminated string that stays valid while it is in
/// the environment, and `environ` null or a null-terminated array of strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn putenv(string: *mut c_char) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { name_len(string) };
    // SAFETY: `name_len` stopped at a byte of the string.
    if len == 0 || unsafe { byte_at(string, len) } == 0 {
        // SAFETY: the caller's contract covers `unsetenv`'s.
        return unsafe { unsetenv(string) };
    }
    // SAFETY: as above, and the program keeps the string.
    unsafe { insert(string, len, null_mut()) }
}

/// Empties the environment.
///
/// # Safety
///
/// `environ` must be null or a null-terminated array of strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clearenv() -> c_int {
    let env = environ.swap(null_mut(), Ordering::Relaxed);
    if env.is_null() {
        return 0;
    }
    let mut at = env;
    loop {
        // SAFETY: the array is null-terminated, and `at` has not passed its
        // end.
        let entry = unsafe { at.read() };
        if entry.is_null() {
            return 0;
        }
        // SAFETY: the entry has left the environment.
        unsafe { replace_allocated(entry, null_mut()) };
        at = at.wrapping_add(1);
    }
}
