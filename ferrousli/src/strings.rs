//! `strings.h`: the BSD memory and string functions, case-blind comparison,
//! and finding the lowest set bit.
//!
//! `bcmp` is in [`crate::string`], beside `memcmp`.

use core::ffi::{c_char, c_int, c_long, c_longlong, c_void};

use crate::string::{memmove, memset, strchr, strrchr};

/// Copies `n` bytes from `src` to `dest`, which may overlap. The arguments are
/// in the other order from `memmove`'s.
///
/// # Safety
///
/// Both must be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bcopy(src: *const c_void, dest: *mut c_void, n: usize) {
    // SAFETY: the same contract as `memmove`.
    let _ = unsafe { memmove(dest, src, n) };
}

/// Sets `n` bytes at `s` to zero.
///
/// # Safety
///
/// `s` must be valid for writing `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bzero(s: *mut c_void, n: usize) {
    // SAFETY: the same contract as `memset`.
    let _ = unsafe { memset(s, 0, n) };
}

/// `strchr` under its BSD name.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn index(s: *const c_char, c: c_int) -> *mut c_char {
    // SAFETY: the same contract as `strchr`.
    unsafe { strchr(s, c) }
}

/// `strrchr` under its BSD name.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn rindex(s: *const c_char, c: c_int) -> *mut c_char {
    // SAFETY: the same contract as `strrchr`.
    unsafe { strrchr(s, c) }
}

/// Compares two strings as unsigned bytes, with ASCII letters folded to lower
/// case, as in the C locale.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcasecmp(a: *const c_char, b: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `strncasecmp`'s with no bound.
    unsafe { strncasecmp(a, b, usize::MAX) }
}

/// Compares at most `n` bytes of two strings as [`strcasecmp`] does.
///
/// # Safety
///
/// Both must be NUL-terminated or at least `n` bytes long.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strncasecmp(a: *const c_char, b: *const c_char, n: usize) -> c_int {
    let a = a.cast::<u8>();
    let b = b.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: neither string ended before `i`, or the loop would have
        // returned, and `i < n`.
        let x = unsafe { a.wrapping_add(i).read() }.to_ascii_lowercase();
        // SAFETY: as above.
        let y = unsafe { b.wrapping_add(i).read() }.to_ascii_lowercase();
        if x != y || x == 0 {
            return c_int::from(x) - c_int::from(y);
        }
        i += 1;
    }
    0
}

/// The position of the lowest set bit of `i`, counting from 1, or 0 if none is.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ffs(i: c_int) -> c_int {
    if i == 0 {
        0
    } else {
        i.trailing_zeros() as c_int + 1
    }
}

/// [`ffs`] for a `long`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ffsl(i: c_long) -> c_int {
    if i == 0 {
        0
    } else {
        i.trailing_zeros() as c_int + 1
    }
}

/// [`ffs`] for a `long long`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ffsll(i: c_longlong) -> c_int {
    if i == 0 {
        0
    } else {
        i.trailing_zeros() as c_int + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_blind_comparison_folds_only_ascii_letters() {
        assert_eq!(
            // SAFETY: C string literals are NUL-terminated.
            unsafe { strcasecmp(c"Hello".as_ptr(), c"hELLO".as_ptr()) },
            0
        );
        // SAFETY: as above.
        assert!(unsafe { strcasecmp(c"abc".as_ptr(), c"ABD".as_ptr()) } < 0);
        // SAFETY: as above.
        assert!(unsafe { strcasecmp(c"ab".as_ptr(), c"AB c".as_ptr()) } < 0);
        // '[' sorts after 'Z' but before 'z', to which 'Z' folds.
        // SAFETY: as above.
        assert!(unsafe { strcasecmp(c"[".as_ptr(), c"Z".as_ptr()) } < 0);
        // SAFETY: as above.
        assert!(unsafe { strcasecmp(c"\xc0".as_ptr(), c"\xe0".as_ptr()) } < 0);
        // SAFETY: as above.
        assert!(unsafe { strcasecmp(c"\xff".as_ptr(), c"a".as_ptr()) } > 0);
        assert_eq!(
            // SAFETY: as above.
            unsafe { strncasecmp(c"abcX".as_ptr(), c"ABCy".as_ptr(), 3) },
            0
        );
        // SAFETY: nothing is read with a zero length.
        assert_eq!(unsafe { strncasecmp(c"a".as_ptr(), c"b".as_ptr(), 0) }, 0);
    }

    #[test]
    fn ffs_counts_from_one() {
        assert_eq!(ffs(0), 0);
        assert_eq!(ffs(1), 1);
        assert_eq!(ffs(0x18), 4);
        assert_eq!(ffs(c_int::MIN), 32);
        assert_eq!(ffs(-1), 1);
        assert_eq!(ffsl(0), 0);
        assert_eq!(ffsl(c_long::MIN), 64);
        assert_eq!(ffsll(1 << 40), 41);
    }

    #[test]
    fn bcopy_bzero_index_and_rindex() {
        let mut buf = *b"abcdef\0";
        let p = buf.as_mut_ptr();
        // SAFETY: both ranges lie inside `buf`.
        unsafe { bcopy(p.cast(), p.wrapping_add(1).cast(), 4) };
        assert_eq!(&buf, b"aabcdf\0");
        // SAFETY: as above.
        unsafe { bzero(p.wrapping_add(4).cast(), 2) };
        assert_eq!(&buf, b"aabc\0\0\0");
        let s = c"a-b-c";
        assert_eq!(
            // SAFETY: C string literals are NUL-terminated.
            unsafe { index(s.as_ptr(), c_int::from(b'-')) },
            s.as_ptr().wrapping_add(1).cast_mut()
        );
        assert_eq!(
            // SAFETY: as above.
            unsafe { rindex(s.as_ptr(), c_int::from(b'-')) },
            s.as_ptr().wrapping_add(3).cast_mut()
        );
    }
}
