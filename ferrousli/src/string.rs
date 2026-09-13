//! `string.h`: the memory and string functions.
//!
//! Every loop here is a `while` over raw pointers, with no iterator and no
//! slice. Two different things turn Rust code into calls to these very
//! functions:
//!
//! * LLVM recognises a byte-copying loop as `memcpy`. `#![no_builtins]` stops
//!   that.
//! * rustc copies any value too large for two registers by calling `memcpy`,
//!   and nothing stops that at `-O0`. A `zip` of two slice iterators is 48
//!   bytes. A `memcpy` built on one called itself until the stack ran out, in
//!   the debug build only, since optimisation had put the iterator in
//!   registers.
//!
//! So nothing in these functions holds a value larger than two words.
//! `tests/c_programs.rs` links C against the debug build to keep it that way.

use core::ffi::{c_char, c_int, c_void};

/// The length of the string `s`, not counting its NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strlen(s: *const c_char) -> usize {
    let mut len = 0;
    // SAFETY: the string is NUL-terminated and the loop stops at the NUL.
    while unsafe { s.wrapping_add(len).read() } != 0 {
        len += 1;
    }
    len
}

/// Compares two strings as unsigned bytes.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcmp(a: *const c_char, b: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `strncmp`'s with no bound.
    unsafe { strncmp(a, b, usize::MAX) }
}

/// Compares at most `n` bytes of two strings as unsigned bytes.
///
/// # Safety
///
/// Both must be NUL-terminated or at least `n` bytes long.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strncmp(a: *const c_char, b: *const c_char, n: usize) -> c_int {
    let mut i = 0;
    while i < n {
        // SAFETY: neither string has ended before `i`, or the loop would have
        // returned.
        let x = unsafe { a.wrapping_add(i).read() }.cast_unsigned();
        // SAFETY: as above.
        let y = unsafe { b.wrapping_add(i).read() }.cast_unsigned();
        if x != y || x == 0 {
            return c_int::from(x) - c_int::from(y);
        }
        i += 1;
    }
    0
}

/// Copies `n` bytes from `src` to `dest`, which must not overlap.
///
/// # Safety
///
/// Both must be valid for `n` bytes, and the ranges must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let to = dest.cast::<u8>();
    let from = src.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and the caller vouches for `n` readable bytes.
        let byte = unsafe { from.wrapping_add(i).read() };
        // SAFETY: `i < n`, and the caller vouches for `n` writable bytes.
        unsafe { to.wrapping_add(i).write(byte) };
        i += 1;
    }
    dest
}

/// Copies `n` bytes from `src` to `dest`, which may overlap.
///
/// # Safety
///
/// Both must be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let to = dest.cast::<u8>();
    let from = src.cast::<u8>();
    // Copying away from the overlap never reads a byte this call has already
    // overwritten: forwards when the destination is below the source,
    // backwards when it is above.
    if to.cast_const() <= from {
        let mut i = 0;
        while i < n {
            // SAFETY: `i < n`, and the caller vouches for `n` bytes.
            let byte = unsafe { from.wrapping_add(i).read() };
            // SAFETY: as above.
            unsafe { to.wrapping_add(i).write(byte) };
            i += 1;
        }
    } else {
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: `i < n`, and the caller vouches for `n` bytes.
            let byte = unsafe { from.wrapping_add(i).read() };
            // SAFETY: as above.
            unsafe { to.wrapping_add(i).write(byte) };
        }
    }
    dest
}

/// Fills `n` bytes at `s` with the low byte of `c`.
///
/// # Safety
///
/// `s` must be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memset(s: *mut c_void, c: c_int, n: usize) -> *mut c_void {
    // C passes the byte as an `int` and uses only its low eight bits.
    let byte = c as u8;
    let to = s.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and the caller vouches for `n` writable bytes.
        unsafe { to.wrapping_add(i).write(byte) };
        i += 1;
    }
    s
}

/// Compares `n` bytes as unsigned values.
///
/// # Safety
///
/// Both must be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    let a = a.cast::<u8>();
    let b = b.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and the caller vouches for `n` readable bytes.
        let x = unsafe { a.wrapping_add(i).read() };
        // SAFETY: as above.
        let y = unsafe { b.wrapping_add(i).read() };
        if x != y {
            return c_int::from(x) - c_int::from(y);
        }
        i += 1;
    }
    0
}

/// `memcmp` for equality only. LLVM emits calls to it for `==` on byte arrays,
/// so compiled Rust needs it as much as C does.
///
/// # Safety
///
/// Both must be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    // SAFETY: the same contract as `memcmp`.
    unsafe { memcmp(a, b, n) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strlen_counts_to_the_nul() {
        // SAFETY: C string literals are NUL-terminated.
        assert_eq!(unsafe { strlen(c"".as_ptr()) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { strlen(c"ferrousli".as_ptr()) }, 9);
    }

    #[test]
    fn string_comparisons_use_unsigned_bytes() {
        // SAFETY: C string literals are NUL-terminated.
        assert!(unsafe { strcmp(c"abc".as_ptr(), c"abd".as_ptr()) } < 0);
        // SAFETY: as above.
        assert!(unsafe { strcmp(c"\xff".as_ptr(), c"a".as_ptr()) } > 0);
        // SAFETY: as above.
        assert!(unsafe { strcmp(c"ab".as_ptr(), c"abc".as_ptr()) } < 0);
        // SAFETY: as above.
        assert_eq!(unsafe { strncmp(c"abcx".as_ptr(), c"abcy".as_ptr(), 3) }, 0);
    }

    #[test]
    fn memmove_copies_across_either_overlap() {
        let mut buf = *b"abcdefgh";
        let p = buf.as_mut_ptr().cast::<c_void>();
        // SAFETY: both ranges lie inside `buf`.
        let _ = unsafe { memmove(p.wrapping_add(2), p, 6) };
        assert_eq!(&buf, b"ababcdef");
        // SAFETY: as above.
        let _ = unsafe { memmove(p, p.wrapping_add(2), 6) };
        assert_eq!(&buf, b"abcdefef");
    }

    #[test]
    fn memcpy_memset_and_memcmp() {
        let mut buf = [0_u8; 4];
        let p = buf.as_mut_ptr().cast::<c_void>();
        // SAFETY: `buf` holds four bytes.
        let _ = unsafe { memset(p, 0x178, 4) };
        assert_eq!(buf, [0x78; 4]);
        // SAFETY: both hold four bytes and are distinct.
        let _ = unsafe { memcpy(p, b"wxyz".as_ptr().cast(), 4) };
        assert_eq!(&buf, b"wxyz");
        // SAFETY: both hold four bytes.
        assert!(unsafe { memcmp(p, c"wxz".as_ptr().cast(), 4) } < 0);
        // SAFETY: both hold one byte.
        assert!(unsafe { memcmp(b"\xff".as_ptr().cast(), b"a".as_ptr().cast(), 1) } > 0);
    }
}
