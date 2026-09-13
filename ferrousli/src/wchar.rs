//! `wchar.h`'s wide string and memory functions.
//!
//! They follow musl 1.2.5 (MIT), from `src/string/` and `src/locale/`.
//! Characters are compared as `wchar_t`, a signed 32-bit integer on x86-64,
//! so a negative value sorts below every character. Collation is by code
//! point in every locale, so `wcscoll` is `wcscmp` and `wcsxfrm` a copy.
//!
//! The multibyte conversions are in [`crate::multibyte`], and the wide
//! character classes in [`crate::wctype`].

use core::ffi::c_int;
use core::mem::size_of;
use core::ptr::null_mut;

use crate::errno;
use crate::locale::Locale;
use crate::malloc;
use crate::multibyte::WChar;
use crate::wctype::towlower;

/// The character `i` places past `p`.
///
/// # Safety
///
/// That character must be readable.
unsafe fn at(p: *const WChar, i: usize) -> WChar {
    // SAFETY: the caller vouches for the character.
    unsafe { p.wrapping_add(i).read() }
}

/// Writes `wc` `i` places past `p`.
///
/// # Safety
///
/// That place must be writable.
unsafe fn put(p: *mut WChar, i: usize, wc: WChar) {
    // SAFETY: the caller vouches for the place.
    unsafe { p.wrapping_add(i).write(wc) }
}

/// -1, 0 or 1 as `a` is below, equal to or above `b`.
fn order(a: WChar, b: WChar) -> c_int {
    c_int::from(a > b) - c_int::from(a < b)
}

/// The length of the wide string `s`, not counting its NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcslen(s: *const WChar) -> usize {
    let mut i = 0;
    // SAFETY: no NUL came before `i`, so it is within the string.
    while unsafe { at(s, i) } != 0 {
        i += 1;
    }
    i
}

/// The length of `s`, or `n` if none of its first `n` characters is the NUL.
///
/// # Safety
///
/// `s` must be NUL-terminated or valid for `n` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsnlen(s: *const WChar, n: usize) -> usize {
    let mut i = 0;
    // SAFETY: `i < n`, and no NUL came before `i`.
    while i < n && unsafe { at(s, i) } != 0 {
        i += 1;
    }
    i
}

/// Copies the wide string `src` and its NUL to `dest`, and returns `dest`.
///
/// # Safety
///
/// `src` must be a NUL-terminated wide string, and `dest` have room for it.
/// They must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscpy(dest: *mut WChar, src: *const WChar) -> *mut WChar {
    // SAFETY: the caller's contract is `wcpcpy`'s.
    let _ = unsafe { wcpcpy(dest, src) };
    dest
}

/// Copies the wide string `src` and its NUL to `dest`, and returns the
/// address of the NUL written.
///
/// # Safety
///
/// As [`wcscpy`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcpcpy(dest: *mut WChar, src: *const WChar) -> *mut WChar {
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i` in `src`.
        let wc = unsafe { at(src, i) };
        // SAFETY: `dest` has room for the string and its NUL.
        unsafe { put(dest, i, wc) };
        if wc == 0 {
            return dest.wrapping_add(i);
        }
        i += 1;
    }
}

/// Copies at most `n` characters of `src` to `dest`, filling the rest of the
/// `n` with NULs, and returns the address after the last character copied
/// that was not a NUL.
///
/// # Safety
///
/// `src` must be NUL-terminated or valid for `n` characters, and `dest` valid
/// for writing `n`. They must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcpncpy(dest: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and no NUL came before `i`.
        let wc = unsafe { at(src, i) };
        if wc == 0 {
            break;
        }
        // SAFETY: `i < n`.
        unsafe { put(dest, i, wc) };
        i += 1;
    }
    let end = i;
    while i < n {
        // SAFETY: `i < n`.
        unsafe { put(dest, i, 0) };
        i += 1;
    }
    dest.wrapping_add(end)
}

/// [`wcpncpy`], returning `dest`. The `n` characters are not NUL-terminated
/// if `src` is at least that long.
///
/// # Safety
///
/// As [`wcpncpy`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsncpy(dest: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    // SAFETY: the caller's contract is `wcpncpy`'s.
    let _ = unsafe { wcpncpy(dest, src, n) };
    dest
}

/// Appends the wide string `src` to `dest`, and returns `dest`.
///
/// # Safety
///
/// Both must be NUL-terminated, and `dest` have room for both. They must not
/// overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscat(dest: *mut WChar, src: *const WChar) -> *mut WChar {
    // SAFETY: the caller passes a NUL-terminated `dest`.
    let end = dest.wrapping_add(unsafe { wcslen(dest) });
    // SAFETY: `dest` has room after its NUL for `src`.
    let _ = unsafe { wcpcpy(end, src) };
    dest
}

/// Appends at most `n` characters of `src`, and a NUL, to `dest`, and returns
/// `dest`.
///
/// # Safety
///
/// `dest` must be NUL-terminated with room for `n + 1` more characters, and
/// `src` NUL-terminated or valid for `n`. They must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsncat(dest: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    // SAFETY: the caller passes a NUL-terminated `dest`.
    let end = dest.wrapping_add(unsafe { wcslen(dest) });
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and no NUL came before `i`.
        let wc = unsafe { at(src, i) };
        if wc == 0 {
            break;
        }
        // SAFETY: `dest` has room for `n + 1` more.
        unsafe { put(end, i, wc) };
        i += 1;
    }
    // SAFETY: as above.
    unsafe { put(end, i, 0) };
    dest
}

/// Compares two wide strings as `wchar_t` values: -1, 0 or 1.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscmp(a: *const WChar, b: *const WChar) -> c_int {
    // SAFETY: the caller's contract is `wcsncmp`'s with no bound.
    unsafe { wcsncmp(a, b, usize::MAX) }
}

/// Compares at most `n` characters of two wide strings: -1, 0 or 1.
///
/// # Safety
///
/// Both must be NUL-terminated or valid for `n` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsncmp(a: *const WChar, b: *const WChar, n: usize) -> c_int {
    let mut i = 0;
    while i < n {
        // SAFETY: neither string ended before `i`.
        let x = unsafe { at(a, i) };
        // SAFETY: as above.
        let y = unsafe { at(b, i) };
        if x != y || x == 0 {
            return order(x, y);
        }
        i += 1;
    }
    0
}

/// Compares two wide strings ignoring case, as `towlower` maps it.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscasecmp(a: *const WChar, b: *const WChar) -> c_int {
    // SAFETY: the caller's contract is `wcsncasecmp`'s with no bound.
    unsafe { wcsncasecmp(a, b, usize::MAX) }
}

/// Compares at most `n` characters of two wide strings ignoring case. Returns
/// the difference of the lower-case forms of the first pair that differ, as
/// musl does.
///
/// # Safety
///
/// Both must be NUL-terminated or valid for `n` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsncasecmp(a: *const WChar, b: *const WChar, n: usize) -> c_int {
    if n == 0 {
        return 0;
    }
    let mut i = 0;
    loop {
        // SAFETY: neither string ended before `i`, and `i < n`.
        let x = unsafe { at(a, i) };
        // SAFETY: as above.
        let y = unsafe { at(b, i) };
        let (lx, ly) = (towlower(x.cast_unsigned()), towlower(y.cast_unsigned()));
        if x == 0 || y == 0 || i + 1 == n || (x != y && lx != ly) {
            return lx.wrapping_sub(ly).cast_signed();
        }
        i += 1;
    }
}

/// [`wcscasecmp`]: case does not depend on the locale.
///
/// # Safety
///
/// As [`wcscasecmp`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscasecmp_l(
    a: *const WChar,
    b: *const WChar,
    locale: *mut Locale,
) -> c_int {
    let _ = locale;
    // SAFETY: the caller's contract is `wcscasecmp`'s.
    unsafe { wcscasecmp(a, b) }
}

/// [`wcsncasecmp`]: case does not depend on the locale.
///
/// # Safety
///
/// As [`wcsncasecmp`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsncasecmp_l(
    a: *const WChar,
    b: *const WChar,
    n: usize,
    locale: *mut Locale,
) -> c_int {
    let _ = locale;
    // SAFETY: the caller's contract is `wcsncasecmp`'s.
    unsafe { wcsncasecmp(a, b, n) }
}

/// Compares two wide strings in the current locale's order, which is always
/// code point order.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscoll(a: *const WChar, b: *const WChar) -> c_int {
    // SAFETY: the same contract as `wcscmp`.
    unsafe { wcscmp(a, b) }
}

/// [`wcscoll`] in the locale `locale`, which does not change the order.
///
/// # Safety
///
/// As [`wcscoll`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscoll_l(a: *const WChar, b: *const WChar, locale: *mut Locale) -> c_int {
    let _ = locale;
    // SAFETY: the same contract as `wcscmp`.
    unsafe { wcscmp(a, b) }
}

/// Transforms `src` for [`wcscmp`] to order as [`wcscoll`] does, which is a
/// copy, into `dest` of `n` characters. Returns the length of `src`. If it
/// does not fit, as in musl, the first `n - 1` characters are copied and
/// terminated.
///
/// # Safety
///
/// `src` must be a NUL-terminated wide string, and `dest` valid for writing
/// `n` characters. They must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsxfrm(dest: *mut WChar, src: *const WChar, n: usize) -> usize {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { wcslen(src) };
    if len < n {
        // SAFETY: the string and its NUL fit in `n`.
        let _ = unsafe { wmemcpy(dest, src, len + 1) };
    } else if n > 0 {
        // SAFETY: `n - 1` characters of `src` exist, and `dest` holds `n`.
        let _ = unsafe { wmemcpy(dest, src, n - 1) };
        // SAFETY: `n - 1 < n`.
        unsafe { put(dest, n - 1, 0) };
    }
    len
}

/// [`wcsxfrm`] in the locale `locale`, which does not change the result.
///
/// # Safety
///
/// As [`wcsxfrm`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsxfrm_l(
    dest: *mut WChar,
    src: *const WChar,
    n: usize,
    locale: *mut Locale,
) -> usize {
    let _ = locale;
    // SAFETY: the caller's contract is `wcsxfrm`'s.
    unsafe { wcsxfrm(dest, src, n) }
}

/// The first `c` in the wide string `s`, or null. The NUL itself can be found.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcschr(s: *const WChar, c: WChar) -> *mut WChar {
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i`.
        let wc = unsafe { at(s, i) };
        if wc == c {
            return s.wrapping_add(i).cast_mut();
        }
        if wc == 0 {
            return null_mut();
        }
        i += 1;
    }
}

/// The last `c` in the wide string `s`, or null. The NUL itself can be found.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsrchr(s: *const WChar, c: WChar) -> *mut WChar {
    // SAFETY: the caller passes a NUL-terminated string.
    let mut i = unsafe { wcslen(s) };
    loop {
        // SAFETY: `i` is at most the NUL's index.
        if unsafe { at(s, i) } == c {
            return s.wrapping_add(i).cast_mut();
        }
        if i == 0 {
            return null_mut();
        }
        i -= 1;
    }
}

/// Whether `wc` is one of the characters of the wide string `set`, not
/// counting its NUL.
///
/// # Safety
///
/// `set` must be a NUL-terminated wide string.
unsafe fn in_set(set: *const WChar, wc: WChar) -> bool {
    // SAFETY: the caller passes a NUL-terminated string.
    wc != 0 && !unsafe { wcschr(set, wc) }.is_null()
}

/// The length of the prefix of `s` made only of characters in `accept`.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsspn(s: *const WChar, accept: *const WChar) -> usize {
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i`.
        let wc = unsafe { at(s, i) };
        // SAFETY: `accept` is NUL-terminated.
        if !unsafe { in_set(accept, wc) } {
            return i;
        }
        i += 1;
    }
}

/// The length of the prefix of `s` with no character in `reject`.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcscspn(s: *const WChar, reject: *const WChar) -> usize {
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i`.
        let wc = unsafe { at(s, i) };
        // SAFETY: `reject` is NUL-terminated.
        if wc == 0 || unsafe { in_set(reject, wc) } {
            return i;
        }
        i += 1;
    }
}

/// The first character of `s` that is in `accept`, or null.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcspbrk(s: *const WChar, accept: *const WChar) -> *mut WChar {
    // SAFETY: the caller's contract is `wcscspn`'s.
    let i = unsafe { wcscspn(s, accept) };
    // SAFETY: `i` is at most the NUL's index.
    if unsafe { at(s, i) } == 0 {
        null_mut()
    } else {
        s.wrapping_add(i).cast_mut()
    }
}

/// The first `c` among the `n` characters at `s`, or null.
///
/// # Safety
///
/// `s` must be valid for `n` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wmemchr(s: *const WChar, c: WChar, n: usize) -> *mut WChar {
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`.
        if unsafe { at(s, i) } == c {
            return s.wrapping_add(i).cast_mut();
        }
        i += 1;
    }
    null_mut()
}

/// Compares `n` wide characters as `wchar_t` values: -1, 0 or 1.
///
/// # Safety
///
/// Both must be valid for `n` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wmemcmp(a: *const WChar, b: *const WChar, n: usize) -> c_int {
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`.
        let x = unsafe { at(a, i) };
        // SAFETY: as above.
        let y = unsafe { at(b, i) };
        if x != y {
            return order(x, y);
        }
        i += 1;
    }
    0
}

/// Copies `n` wide characters from `src` to `dest`, which must not overlap,
/// and returns `dest`.
///
/// # Safety
///
/// Both must be valid for `n` characters, and must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wmemcpy(dest: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`.
        let wc = unsafe { at(src, i) };
        // SAFETY: as above.
        unsafe { put(dest, i, wc) };
        i += 1;
    }
    dest
}

/// Copies `n` wide characters from `src` to `dest`, which may overlap, and
/// returns `dest`.
///
/// # Safety
///
/// Both must be valid for `n` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wmemmove(dest: *mut WChar, src: *const WChar, n: usize) -> *mut WChar {
    if dest.cast_const() == src {
        return dest;
    }
    // Copying backward is needed only when `dest` starts inside `src`.
    if dest.addr().wrapping_sub(src.addr()) < n.wrapping_mul(size_of::<WChar>()) {
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: `i < n`.
            let wc = unsafe { at(src, i) };
            // SAFETY: as above.
            unsafe { put(dest, i, wc) };
        }
    } else {
        // SAFETY: copying forward never reads a character already written.
        let _ = unsafe { wmemcpy(dest, src, n) };
    }
    dest
}

/// Sets `n` wide characters at `s` to `c`, and returns `s`.
///
/// # Safety
///
/// `s` must be valid for writing `n` characters.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wmemset(s: *mut WChar, c: WChar, n: usize) -> *mut WChar {
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`.
        unsafe { put(s, i, c) };
        i += 1;
    }
    s
}

/// A copy of the wide string `s` from `malloc`, or null with `errno` set to
/// `ENOMEM`.
///
/// # Safety
///
/// `s` must be a NUL-terminated wide string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsdup(s: *const WChar) -> *mut WChar {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { wcslen(s) };
    let Some(size) = (len + 1).checked_mul(size_of::<WChar>()) else {
        errno::set(errno::ENOMEM);
        return null_mut();
    };
    let copy = malloc::malloc(size).cast::<WChar>();
    if copy.is_null() {
        return null_mut();
    }
    // SAFETY: the new block holds `len + 1` characters, and is aligned for
    // anything.
    unsafe { wmemcpy(copy, s, len + 1) }
}

/// Splits the wide string `s` into tokens separated by characters in `sep`,
/// keeping its place in `*p`. A null `s` continues from `*p`.
///
/// # Safety
///
/// `s`, or `*p` when `s` is null, must be null or a NUL-terminated writable
/// wide string, `sep` a NUL-terminated wide string, and `p` valid.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcstok(
    s: *mut WChar,
    sep: *const WChar,
    p: *mut *mut WChar,
) -> *mut WChar {
    let mut s = s;
    if s.is_null() {
        // SAFETY: the caller passes a valid `p`.
        s = unsafe { p.read() };
        if s.is_null() {
            return null_mut();
        }
    }
    // SAFETY: `s` and `sep` are NUL-terminated.
    s = s.wrapping_add(unsafe { wcsspn(s, sep) });
    // SAFETY: `s` is at most at the NUL.
    if unsafe { s.read() } == 0 {
        // SAFETY: the caller passes a valid `p`.
        unsafe { p.write(null_mut()) };
        return null_mut();
    }
    // SAFETY: as above.
    let end = s.wrapping_add(unsafe { wcscspn(s, sep) });
    // SAFETY: `end` is at most at the NUL.
    let next = if unsafe { end.read() } != 0 {
        // SAFETY: `end` is within the writable string.
        unsafe { end.write(0) };
        end.wrapping_add(1)
    } else {
        null_mut()
    };
    // SAFETY: the caller passes a valid `p`.
    unsafe { p.write(next) };
    s
}

/// musl's two-way search for `n`, at least two characters long, in `h`.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
unsafe fn two_way(h: *const WChar, n: *const WChar) -> *mut WChar {
    let mut h = h;
    // The needle's length, and whether the haystack is at least as long.
    let mut l = 0;
    // SAFETY: neither string ended before `l`.
    while unsafe { at(n, l) } != 0 && unsafe { at(h, l) } != 0 {
        l += 1;
    }
    // SAFETY: `l` is at most the needle's NUL.
    if unsafe { at(n, l) } != 0 {
        return null_mut();
    }

    // The maximal suffix for each ordering, with `ip` starting at -1.
    let suffix = |greater: bool| {
        let (mut ip, mut jp, mut k, mut p) = (usize::MAX, 0_usize, 1_usize, 1_usize);
        while jp + k < l {
            // SAFETY: `ip + k` and `jp + k` are below `l`.
            let a = unsafe { at(n, ip.wrapping_add(k)) };
            // SAFETY: as above.
            let b = unsafe { at(n, jp + k) };
            if a == b {
                if k == p {
                    jp += p;
                    k = 1;
                } else {
                    k += 1;
                }
            } else if (a > b) == greater {
                jp += k;
                k = 1;
                p = jp.wrapping_sub(ip);
            } else {
                ip = jp;
                jp += 1;
                k = 1;
                p = 1;
            }
        }
        (ip, p)
    };
    let (mut ms, p0) = suffix(true);
    let (ip, mut p) = suffix(false);
    if ip.wrapping_add(1) > ms.wrapping_add(1) {
        ms = ip;
    } else {
        p = p0;
    }

    // Whether the needle is periodic.
    // SAFETY: `p + ms + 1 <= l`, within the needle.
    let mem0 = if unsafe { wmemcmp(n, n.wrapping_add(p), ms.wrapping_add(1)) } != 0 {
        p = ms.max(l.wrapping_sub(ms).wrapping_sub(1)).wrapping_add(1);
        0
    } else {
        l - p
    };
    let mut mem = 0;

    // How far the haystack is known to have no NUL.
    let mut z = h;
    loop {
        if z.addr().wrapping_sub(h.addr()) / size_of::<WChar>() < l {
            let grow = l | 63;
            // SAFETY: `wmemchr` stops at the NUL, which ends the haystack.
            let nul = unsafe { wmemchr(z, 0, grow) };
            if nul.is_null() {
                z = z.wrapping_add(grow);
            } else {
                z = nul;
                if z.addr().wrapping_sub(h.addr()) / size_of::<WChar>() < l {
                    return null_mut();
                }
            }
        }

        // The right half.
        let mut k = ms.wrapping_add(1).max(mem);
        loop {
            // SAFETY: `k <= l`.
            let wc = unsafe { at(n, k) };
            // SAFETY: `k < l` once `wc` is not the NUL, and the haystack has
            // `l` characters from `h`.
            if wc == 0 || wc != unsafe { at(h, k) } {
                break;
            }
            k += 1;
        }
        // SAFETY: as above.
        if unsafe { at(n, k) } != 0 {
            h = h.wrapping_add(k.wrapping_sub(ms));
            mem = 0;
            continue;
        }
        // The left half.
        let mut k = ms.wrapping_add(1);
        while k > mem {
            // SAFETY: `k - 1 < l`.
            let wc = unsafe { at(n, k - 1) };
            // SAFETY: as above, and the haystack has `l` characters from `h`.
            if wc != unsafe { at(h, k - 1) } {
                break;
            }
            k -= 1;
        }
        if k <= mem {
            return h.cast_mut();
        }
        h = h.wrapping_add(p);
        mem = mem0;
    }
}

/// The first occurrence of the wide string `needle` in `haystack`, or null.
/// An empty needle is found at the start.
///
/// # Safety
///
/// Both must be NUL-terminated wide strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcsstr(haystack: *const WChar, needle: *const WChar) -> *mut WChar {
    // SAFETY: the needle is NUL-terminated.
    let first = unsafe { needle.read() };
    if first == 0 {
        return haystack.cast_mut();
    }
    // SAFETY: the haystack is NUL-terminated.
    if unsafe { haystack.read() } == 0 {
        return null_mut();
    }
    // SAFETY: as above.
    let h = unsafe { wcschr(haystack, first) };
    // SAFETY: the needle has a second character or its NUL.
    if h.is_null() || unsafe { at(needle, 1) } == 0 {
        return h;
    }
    // SAFETY: `h` is a non-NUL character of the haystack.
    if unsafe { at(h, 1) } == 0 {
        return null_mut();
    }
    // SAFETY: both are NUL-terminated.
    unsafe { two_way(h, needle) }
}

/// [`wcsstr`], under its old X/Open name.
///
/// # Safety
///
/// As [`wcsstr`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn wcswcs(haystack: *const WChar, needle: *const WChar) -> *mut WChar {
    // SAFETY: the caller's contract is `wcsstr`'s.
    unsafe { wcsstr(haystack, needle) }
}
