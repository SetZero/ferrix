//! glibc's `argz.h`: a vector of strings kept as one buffer, each string
//! ending in a NUL, with its length in bytes beside it. The Steam Runtime's
//! launcher interface builds its command lines with `argz_add`,
//! `argz_add_sep`, `argz_count` and `argz_extract`, and the scout runtime's
//! libltdl imports `argz_append`, `argz_create_sep`, `argz_insert` and
//! `argz_stringify`. Those eight are here, the ones anything on Ferrix
//! imports. A buffer is the C library's heap's, grown with `realloc`; an
//! empty vector is a null pointer and length 0.

use core::ffi::{c_char, c_int, c_void};

use crate::errno;
use crate::malloc::realloc;
use crate::string::strlen;

/// Appends `len` bytes from `from`, then a NUL unless `terminated` says the
/// bytes already end with one, to the vector `*argz` of `*argz_len` bytes.
/// `ENOMEM`, with the vector unchanged, if it cannot grow.
///
/// # Safety
///
/// `argz` and `argz_len` must be valid for reads and writes and describe a
/// vector this module or `malloc` made; `from` must be valid for `len` reads.
unsafe fn append(
    argz: *mut *mut c_char,
    argz_len: *mut usize,
    from: *const c_char,
    len: usize,
    terminated: bool,
) -> c_int {
    // SAFETY: the caller vouches for `argz`.
    let old = unsafe { argz.read() };
    // SAFETY: the caller vouches for `argz_len`.
    let old_len = unsafe { argz_len.read() };
    let add = if terminated {
        len
    } else {
        len.saturating_add(1)
    };
    let Some(new_len) = old_len.checked_add(add) else {
        return errno::ENOMEM;
    };
    // SAFETY: `old` is null or the heap's, as the caller vouches.
    let grown = unsafe { realloc(old.cast::<c_void>(), new_len) }.cast::<c_char>();
    if grown.is_null() {
        return errno::ENOMEM;
    }
    let mut i = 0;
    while i < len {
        // SAFETY: `i < len` reads inside `from`.
        let byte = unsafe { from.wrapping_add(i).read() };
        // SAFETY: `old_len + i < new_len` writes inside the grown buffer.
        unsafe { grown.wrapping_add(old_len + i).write(byte) };
        i += 1;
    }
    if !terminated {
        // SAFETY: `old_len + len` is the last byte of the grown buffer.
        unsafe { grown.wrapping_add(old_len + len).write(0) };
    }
    // SAFETY: the caller vouches for `argz`.
    unsafe { argz.write(grown) };
    // SAFETY: the caller vouches for `argz_len`.
    unsafe { argz_len.write(new_len) };
    0
}

/// Appends the string `str` to the vector `*argz` of `*argz_len` bytes.
/// Returns 0, or `ENOMEM` with the vector unchanged.
///
/// # Safety
///
/// `argz` and `argz_len` must be valid for reads and writes and describe a
/// vector made by these functions or `malloc`, and `str` must be a
/// NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_add(
    argz: *mut *mut c_char,
    argz_len: *mut usize,
    str: *const c_char,
) -> c_int {
    // SAFETY: the string's bytes and its NUL are readable.
    let len = unsafe { strlen(str) }.saturating_add(1);
    // SAFETY: the caller's contract, with the string's NUL included.
    unsafe { append(argz, argz_len, str, len, true) }
}

/// Appends each piece of `string` between the separators `delim` to the vector
/// `*argz` of `*argz_len` bytes, as glibc's does: runs of separators, and
/// separators at either end, make no empty strings. Returns 0, or `ENOMEM`
/// with the vector unchanged.
///
/// # Safety
///
/// As [`argz_add`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_add_sep(
    argz: *mut *mut c_char,
    argz_len: *mut usize,
    string: *const c_char,
    delim: c_int,
) -> c_int {
    // SAFETY: the caller vouches for both.
    let before_len = unsafe { argz_len.read() };
    let mut at = string;
    loop {
        // Skip separators, then take one piece up to the next or the end.
        // SAFETY: the string is readable up to and including its NUL.
        while unsafe { at.read() } != 0 && c_int::from(unsafe { at.read() } as u8) == delim {
            at = at.wrapping_add(1);
        }
        // SAFETY: as above.
        if unsafe { at.read() } == 0 {
            return 0;
        }
        let start = at;
        let mut len = 0usize;
        // SAFETY: as above; the loop stops at the NUL.
        while unsafe { at.read() } != 0 && c_int::from(unsafe { at.read() } as u8) != delim {
            at = at.wrapping_add(1);
            len += 1;
        }
        // SAFETY: `start` holds `len` readable bytes; the vector is the
        // caller's.
        let e = unsafe { append(argz, argz_len, start, len, false) };
        if e != 0 {
            // The pieces appended before this one are dropped by the old
            // length; the buffer may have grown, which the caller frees as
            // it would have.
            // SAFETY: the caller vouches for `argz_len`.
            unsafe { argz_len.write(before_len) };
            return e;
        }
    }
}

/// How many strings the vector of `len` bytes at `argz` holds: its NULs.
///
/// # Safety
///
/// `argz` must be valid for reads of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_count(argz: *const c_char, len: usize) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < len {
        // SAFETY: `i < len`.
        if unsafe { argz.wrapping_add(i).read() } == 0 {
            count += 1;
        }
        i += 1;
    }
    count
}

/// Stores a pointer to each string of the vector of `len` bytes at `argz` in
/// `argv`, then a null.
///
/// # Safety
///
/// `argz` must be valid for reads of `len` bytes, and `argv` for writes of
/// [`argz_count`]` + 1` pointers.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_extract(argz: *const c_char, len: usize, argv: *mut *mut c_char) {
    let mut slot = argv;
    let mut start = 0;
    let mut i = 0;
    while i < len {
        // SAFETY: `i < len`.
        if unsafe { argz.wrapping_add(i).read() } == 0 {
            // SAFETY: one slot per NUL, which the caller made room for.
            unsafe { slot.write(argz.wrapping_add(start).cast_mut()) };
            slot = slot.wrapping_add(1);
            start = i + 1;
        }
        i += 1;
    }
    // SAFETY: the caller made room for the terminating null.
    unsafe { slot.write(core::ptr::null_mut()) };
}

/// Appends the vector of `buf_len` bytes at `buf` to the vector `*argz` of
/// `*argz_len` bytes. Returns 0, or `ENOMEM` with the vector unchanged.
///
/// # Safety
///
/// As [`argz_add`], with `buf` valid for reads of `buf_len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_append(
    argz: *mut *mut c_char,
    argz_len: *mut usize,
    buf: *const c_char,
    buf_len: usize,
) -> c_int {
    // SAFETY: the caller's contract; `buf` is a vector, so ends in its NUL.
    unsafe { append(argz, argz_len, buf, buf_len, true) }
}

/// Makes a new vector in `*argz` and `*argz_len` of the pieces of `string`
/// between the separators `sep`, as [`argz_add_sep`] adds them to an empty
/// one. Returns 0, or `ENOMEM` with an empty vector.
///
/// # Safety
///
/// `string` must be a NUL-terminated string, and `argz` and `argz_len`
/// valid for writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_create_sep(
    string: *const c_char,
    sep: c_int,
    argz: *mut *mut c_char,
    argz_len: *mut usize,
) -> c_int {
    let mut made: *mut c_char = core::ptr::null_mut();
    let mut len = 0usize;
    // SAFETY: an empty vector, and the caller's string.
    let e = unsafe { argz_add_sep(&raw mut made, &raw mut len, string, sep) };
    if e != 0 {
        // SAFETY: `made` is null or the heap's, and not handed out.
        unsafe { crate::malloc::free(made.cast::<c_void>()) };
        made = core::ptr::null_mut();
        len = 0;
    }
    // SAFETY: the caller vouches for `argz`.
    unsafe { argz.write(made) };
    // SAFETY: the caller vouches for `argz_len`.
    unsafe { argz_len.write(len) };
    e
}

/// Inserts the string `entry` into the vector `*argz` of `*argz_len` bytes
/// before the string `before` points into, backing `before` up to that
/// string's start as glibc does; a null `before` appends, as [`argz_add`].
/// Returns 0, `EINVAL` if `before` is not in the vector, or `ENOMEM` with
/// the vector unchanged.
///
/// # Safety
///
/// As [`argz_add`], with `before` null or a pointer into the vector.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_insert(
    argz: *mut *mut c_char,
    argz_len: *mut usize,
    before: *mut c_char,
    entry: *const c_char,
) -> c_int {
    if before.is_null() {
        // SAFETY: the caller's contract is `argz_add`'s.
        return unsafe { argz_add(argz, argz_len, entry) };
    }
    // SAFETY: the caller vouches for `argz`.
    let old = unsafe { argz.read() };
    // SAFETY: the caller vouches for `argz_len`.
    let old_len = unsafe { argz_len.read() };
    let Some(mut at) = before.addr().checked_sub(old.addr()) else {
        return errno::EINVAL;
    };
    if old.is_null() || at >= old_len {
        return errno::EINVAL;
    }
    // SAFETY: `at - 1` is inside the vector.
    while at > 0 && unsafe { old.wrapping_add(at - 1).read() } != 0 {
        at -= 1;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let add = unsafe { strlen(entry) }.saturating_add(1);
    let Some(new_len) = old_len.checked_add(add) else {
        return errno::ENOMEM;
    };
    // SAFETY: `old` is the heap's, as the caller vouches.
    let grown = unsafe { realloc(old.cast::<c_void>(), new_len) }.cast::<c_char>();
    if grown.is_null() {
        return errno::ENOMEM;
    }
    // The strings from `at` on move up by `add`, last byte first.
    let mut i = old_len;
    while i > at {
        i -= 1;
        // SAFETY: `i` is inside the old vector's bytes.
        let byte = unsafe { grown.wrapping_add(i).read() };
        // SAFETY: `i + add < new_len`.
        unsafe { grown.wrapping_add(i + add).write(byte) };
    }
    let mut j = 0;
    while j < add {
        // SAFETY: `j` is inside the entry and its NUL.
        let byte = unsafe { entry.wrapping_add(j).read() };
        // SAFETY: `at + j < at + add <= new_len`.
        unsafe { grown.wrapping_add(at + j).write(byte) };
        j += 1;
    }
    // SAFETY: the caller vouches for `argz`.
    unsafe { argz.write(grown) };
    // SAFETY: the caller vouches for `argz_len`.
    unsafe { argz_len.write(new_len) };
    0
}

/// Turns the vector of `len` bytes at `argz` into one string by making each
/// NUL but the last the character `sep`.
///
/// # Safety
///
/// `argz` must be valid for reads and writes of `len` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn argz_stringify(argz: *mut c_char, len: usize, sep: c_int) {
    let mut i = 0;
    while i + 1 < len {
        // SAFETY: `i < len`.
        if unsafe { argz.wrapping_add(i).read() } == 0 {
            // SAFETY: as above.
            unsafe { argz.wrapping_add(i).write(sep as c_char) };
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::CStr;

    #[test]
    fn a_vector_is_made_from_pieces_inserted_into_appended_to_and_printed() {
        let mut argz: *mut c_char = core::ptr::null_mut();
        let mut len = 0usize;
        let comma = c_int::from(b',');
        // SAFETY: the string is a literal, and the outputs locals.
        let made =
            unsafe { argz_create_sep(c",a,,bb,".as_ptr(), comma, &raw mut argz, &raw mut len) };
        assert_eq!((made, len), (0, b"a\0bb\0".len()));
        // `before` in the middle of "bb" backs up to its start.
        let middle = argz.wrapping_add(3);
        // SAFETY: the vector is the one just made, and `middle` inside it.
        let inserted = unsafe { argz_insert(&raw mut argz, &raw mut len, middle, c"x".as_ptr()) };
        assert_eq!(inserted, 0);
        let more: [u8; 4] = [b'c', 0, b'd', 0];
        // SAFETY: the vector is the heap's, and `more` a local of 4 bytes.
        let appended = unsafe { argz_append(&raw mut argz, &raw mut len, more.as_ptr().cast(), 4) };
        assert_eq!(appended, 0);
        let outside = argz.wrapping_add(len);
        // SAFETY: `outside` is refused before it is read.
        let refused = unsafe { argz_insert(&raw mut argz, &raw mut len, outside, c"y".as_ptr()) };
        assert_eq!(refused, errno::EINVAL);
        // SAFETY: the vector holds `len` bytes.
        unsafe { argz_stringify(argz, len, c_int::from(b' ')) };
        // SAFETY: the last byte is still the NUL.
        assert_eq!(unsafe { CStr::from_ptr(argz) }.to_bytes(), b"a x bb c d");
        // SAFETY: the vector is the heap's.
        unsafe { crate::malloc::free(argz.cast::<c_void>()) };
    }

    #[test]
    fn a_vector_grows_by_strings_and_pieces_and_comes_apart_again() {
        let mut argz: *mut c_char = core::ptr::null_mut();
        let mut len = 0usize;
        // SAFETY: the vector starts empty, and the string is a literal.
        let added = unsafe { argz_add(&raw mut argz, &raw mut len, c"steam".as_ptr()) };
        assert_eq!(added, 0);
        let colon = c_int::from(b':');
        // SAFETY: the vector is the one just made, and the string a literal.
        let added =
            unsafe { argz_add_sep(&raw mut argz, &raw mut len, c"::-a::b:".as_ptr(), colon) };
        assert_eq!(added, 0);
        assert_eq!(len, b"steam\0-a\0b\0".len());
        // SAFETY: the vector holds `len` bytes.
        assert_eq!(unsafe { argz_count(argz, len) }, 3);
        let mut argv = [core::ptr::null_mut::<c_char>(); 4];
        // SAFETY: room for three strings and the null.
        unsafe { argz_extract(argz, len, argv.as_mut_ptr()) };
        let words: [&[u8]; 3] = [b"steam", b"-a", b"b"];
        for (slot, word) in argv.iter().zip(words) {
            // SAFETY: each slot points at a NUL-terminated string of the vector.
            assert_eq!(unsafe { CStr::from_ptr(*slot) }.to_bytes(), word);
        }
        assert!(argv[3].is_null());
        // SAFETY: the vector is the heap's.
        unsafe { crate::malloc::free(argz.cast::<c_void>()) };
    }
}
