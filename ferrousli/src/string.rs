//! `string.h`: the memory and string functions.
//!
//! `strerror`, `strerror_r` and `strsignal` are in [`crate::strerror`].
//!
//! # Code that must not call itself
//!
//! Every loop here is a `while` over raw pointers, with no iterator and no
//! slice method that copies. Two different things turn Rust code into calls to
//! `memcpy`, `memmove`, `memset`, `memcmp`, `bcmp` and `strlen`:
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
//! `tests/c_programs.rs` and `tests/c_string.rs` link C against the debug
//! build to keep it that way.
//!
//! # A word at a time
//!
//! [`strlen`], [`strchrnul`] (and so [`strchr`] and [`strstr`]) and [`memchr`]
//! look at a machine word at a time once their pointer is aligned, testing all
//! eight bytes for a zero or a match with the usual bit trick.
//!
//! `memchr` reads a whole word only while at least a word of its length is
//! left, so it never reads outside the bytes it was given.
//!
//! `strlen` and `strchrnul` do not know where the string ends, so their last
//! word can reach up to seven bytes past its NUL. That read cannot fault:
//!
//! * a word is read only from an aligned address, and only once every byte
//!   before that address was found not to be the NUL, so the word's first byte
//!   is part of the string and readable;
//! * the kernel maps memory in whole pages, a page begins at an address that is
//!   a multiple of 4096, and so a word read from an aligned address lies
//!   entirely within one page. If its first byte is mapped, all eight are.
//!
//! The extra bytes are never used: the result depends only on the bytes up to
//! the NUL. Rust's own rules know nothing of pages, so the unit tests keep the
//! strings they scan a word at a time in word-aligned buffers with room after
//! them.

use core::ffi::{c_char, c_int, c_void};
use core::mem::MaybeUninit;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::locale::Locale;
use crate::strings::strncasecmp;

/// A machine word's size, the step of the word-at-a-time loops.
const WORD: usize = size_of::<usize>();

/// A word with every byte 0x01.
const ONES: usize = usize::MAX / 0xff;

/// A word with every byte 0x80.
const HIGHS: usize = ONES * 0x80;

/// Whether any byte of `word` is zero.
///
/// Subtracting 1 from each byte sets its high bit only where the byte was 0 or
/// above 0x80, and `!word` rules out the second.
const fn has_zero_byte(word: usize) -> bool {
    word.wrapping_sub(ONES) & !word & HIGHS != 0
}

/// Whether `p` is aligned to a word.
fn word_aligned(p: *const u8) -> bool {
    p.addr().is_multiple_of(WORD)
}

/// Reads the word at `p`.
///
/// # Safety
///
/// `p` must be aligned to a word, and the byte at `p` must be readable. The
/// rest of the word then lies in the same page; see the module documentation.
#[allow(
    clippy::cast_ptr_alignment,
    reason = "the caller aligns the pointer first"
)]
unsafe fn read_word(p: *const u8) -> usize {
    // SAFETY: the caller vouches that `p` is aligned and its page is mapped.
    unsafe { p.cast::<usize>().read() }
}

/// The byte `i` bytes past `p`.
///
/// # Safety
///
/// That byte must be readable.
unsafe fn byte_at(p: *const u8, i: usize) -> u8 {
    // SAFETY: the caller vouches for the byte.
    unsafe { p.wrapping_add(i).read() }
}

/// The address of the NUL that ends the string at `s`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
unsafe fn find_nul(s: *const u8) -> *const u8 {
    let mut p = s;
    while !word_aligned(p) {
        // SAFETY: no NUL came before `p`, so `p` is still within the string.
        if unsafe { p.read() } == 0 {
            return p;
        }
        p = p.wrapping_add(1);
    }
    // SAFETY: `p` is aligned, and no NUL came before it, so its first byte is
    // within the string.
    while !has_zero_byte(unsafe { read_word(p) }) {
        p = p.wrapping_add(WORD);
    }
    // SAFETY: the word at `p` holds a NUL, and this stops at the first.
    while unsafe { p.read() } != 0 {
        p = p.wrapping_add(1);
    }
    p
}

/// The length of the string `s`, not counting its NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strlen(s: *const c_char) -> usize {
    let start = s.cast::<u8>();
    // SAFETY: the caller passes a NUL-terminated string.
    let end = unsafe { find_nul(start) };
    end.addr() - start.addr()
}

/// The length of the string `s`, or `n` if none of its first `n` bytes is the
/// NUL.
///
/// # Safety
///
/// `s` must be NUL-terminated or valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strnlen(s: *const c_char, n: usize) -> usize {
    // SAFETY: `memchr` stops at the first NUL, which the caller vouches for,
    // or after `n` bytes.
    let nul = unsafe { memchr(s.cast(), 0, n) };
    if nul.is_null() {
        n
    } else {
        nul.addr() - s.addr()
    }
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
        let x = unsafe { a.wrapping_add(i).read() } as u8;
        // SAFETY: as above.
        let y = unsafe { b.wrapping_add(i).read() } as u8;
        if x != y || x == 0 {
            return c_int::from(x) - c_int::from(y);
        }
        i += 1;
    }
    0
}

/// Compares two strings in the current locale's collation order. Only the C
/// locale exists so far, where that is [`strcmp`]'s order.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcoll(a: *const c_char, b: *const c_char) -> c_int {
    // SAFETY: the same contract as `strcmp`.
    unsafe { strcmp(a, b) }
}

/// [`strcoll`] in the locale `locale`. Every locale collates by byte value, as
/// in musl, so the locale does not change the order.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcoll_l(
    a: *const c_char,
    b: *const c_char,
    locale: *mut Locale,
) -> c_int {
    let _ = locale;
    // SAFETY: the same contract as `strcmp`.
    unsafe { strcmp(a, b) }
}

/// Transforms `src` into a string whose [`strcmp`] order is `src`'s
/// [`strcoll`] order, writing it to `dest` if it fits in `n` bytes with its
/// NUL. Returns the transformed length. In the C locale the transformation is
/// a copy.
///
/// # Safety
///
/// `src` must be a NUL-terminated string, and `dest` valid for writing `n`
/// bytes. They must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strxfrm(dest: *mut c_char, src: *const c_char, n: usize) -> usize {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(src) };
    if len < n {
        // SAFETY: the string and its NUL are `len + 1 <= n` bytes, which fit.
        let _ = unsafe { memcpy(dest.cast(), src.cast(), len + 1) };
    }
    len
}

/// [`strxfrm`] in the locale `locale`, which does not change the result.
///
/// # Safety
///
/// As [`strxfrm`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strxfrm_l(
    dest: *mut c_char,
    src: *const c_char,
    n: usize,
    locale: *mut Locale,
) -> usize {
    let _ = locale;
    // SAFETY: the caller's contract is `strxfrm`'s.
    unsafe { strxfrm(dest, src, n) }
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

/// [`memcpy`], returning the address just past the last byte written.
///
/// # Safety
///
/// As [`memcpy`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mempcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    // SAFETY: the same contract as `memcpy`.
    let _ = unsafe { memcpy(dest, src, n) };
    dest.wrapping_byte_add(n)
}

/// Copies bytes from `src` to `dest` up to and including the first byte equal
/// to `c`, or `n` bytes if none is. Returns the address after the copy of `c`
/// in `dest`, or null.
///
/// # Safety
///
/// Both must be valid for `n` bytes, or up to the first `c`, and must not
/// overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memccpy(
    dest: *mut c_void,
    src: *const c_void,
    c: c_int,
    n: usize,
) -> *mut c_void {
    let stop = c as u8;
    let to = dest.cast::<u8>();
    let from = src.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and no earlier byte was `c`.
        let byte = unsafe { from.wrapping_add(i).read() };
        // SAFETY: as above.
        unsafe { to.wrapping_add(i).write(byte) };
        i += 1;
        if byte == stop {
            return to.wrapping_add(i).cast();
        }
    }
    null_mut()
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

/// Sets `n` bytes at `s` to zero, in a way the compiler does not remove when
/// the memory is never read again, as it may a `memset`. For wiping secrets.
///
/// # Safety
///
/// `s` must be valid for writing `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn explicit_bzero(s: *mut c_void, n: usize) {
    let to = s.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and the caller vouches for `n` writable bytes.
        unsafe { to.wrapping_add(i).write_volatile(0) };
        i += 1;
    }
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

/// The first of the `n` bytes at `s` equal to the low byte of `c`, or null.
///
/// # Safety
///
/// `s` must be valid for `n` bytes, or up to the first match. A word is read
/// only while a whole word of the `n` bytes remains; see the module
/// documentation.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memchr(s: *const c_void, c: c_int, n: usize) -> *mut c_void {
    let wanted = c as u8;
    let mut p = s.cast::<u8>();
    let mut left = n;
    while left > 0 && !word_aligned(p) {
        // SAFETY: `left > 0`, so `p` is one of the caller's bytes.
        if unsafe { p.read() } == wanted {
            return p.cast_mut().cast();
        }
        p = p.wrapping_add(1);
        left -= 1;
    }
    let pattern = ONES * usize::from(wanted);
    while left >= WORD {
        // SAFETY: `p` is aligned, since `left` is still positive only if the
        // loop above stopped for alignment, and the whole word is among the
        // caller's bytes.
        if has_zero_byte(unsafe { read_word(p) } ^ pattern) {
            break;
        }
        p = p.wrapping_add(WORD);
        left -= WORD;
    }
    while left > 0 {
        // SAFETY: `left > 0`, so `p` is one of the caller's bytes.
        if unsafe { p.read() } == wanted {
            return p.cast_mut().cast();
        }
        p = p.wrapping_add(1);
        left -= 1;
    }
    null_mut()
}

/// The last of the `n` bytes at `s` equal to the low byte of `c`, or null.
///
/// # Safety
///
/// `s` must be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memrchr(s: *const c_void, c: c_int, n: usize) -> *mut c_void {
    let wanted = c as u8;
    let s = s.cast::<u8>();
    let mut i = n;
    while i > 0 {
        i -= 1;
        // SAFETY: `i < n`, and the caller vouches for `n` bytes.
        if unsafe { byte_at(s, i) } == wanted {
            return s.wrapping_add(i).cast_mut().cast();
        }
    }
    null_mut()
}

/// Copies the string `src` to `dest` and returns the address of the NUL
/// written.
///
/// # Safety
///
/// `src` must be a NUL-terminated string, `dest` must have room for it, and
/// they must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn stpcpy(dest: *mut c_char, src: *const c_char) -> *mut c_char {
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i`, so `i` is within the string.
        let byte = unsafe { src.wrapping_add(i).read() };
        // SAFETY: the caller vouches for room for the whole string.
        unsafe { dest.wrapping_add(i).write(byte) };
        if byte == 0 {
            return dest.wrapping_add(i);
        }
        i += 1;
    }
}

/// Copies the string `src` to `dest` and returns `dest`.
///
/// # Safety
///
/// As [`stpcpy`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcpy(dest: *mut c_char, src: *const c_char) -> *mut c_char {
    // SAFETY: the same contract as `stpcpy`.
    let _ = unsafe { stpcpy(dest, src) };
    dest
}

/// Copies at most `n` bytes of the string `src` to `dest`, and fills the rest
/// of the `n` bytes with NULs. Returns the address of the first NUL written,
/// or `dest + n` if the string filled all `n` bytes and has no NUL there.
///
/// # Safety
///
/// `src` must be NUL-terminated or valid for `n` bytes, `dest` valid for
/// writing `n` bytes, and they must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn stpncpy(dest: *mut c_char, src: *const c_char, n: usize) -> *mut c_char {
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and no NUL came before `i`.
        let byte = unsafe { src.wrapping_add(i).read() };
        if byte == 0 {
            break;
        }
        // SAFETY: `i < n`.
        unsafe { dest.wrapping_add(i).write(byte) };
        i += 1;
    }
    let end = dest.wrapping_add(i);
    while i < n {
        // SAFETY: `i < n`.
        unsafe { dest.wrapping_add(i).write(0) };
        i += 1;
    }
    end
}

/// [`stpncpy`], returning `dest`. The result has no NUL if `src` is `n` bytes
/// or longer.
///
/// # Safety
///
/// As [`stpncpy`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strncpy(dest: *mut c_char, src: *const c_char, n: usize) -> *mut c_char {
    // SAFETY: the same contract as `stpncpy`.
    let _ = unsafe { stpncpy(dest, src, n) };
    dest
}

/// Appends the string `src` to the string `dest`, and returns `dest`.
///
/// # Safety
///
/// Both must be NUL-terminated strings, `dest` must have room for both, and
/// they must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcat(dest: *mut c_char, src: *const c_char) -> *mut c_char {
    // SAFETY: `dest` is a NUL-terminated string.
    let len = unsafe { strlen(dest) };
    // SAFETY: the caller vouches for room after `dest`'s string.
    let _ = unsafe { stpcpy(dest.wrapping_add(len), src) };
    dest
}

/// Appends at most `n` bytes of the string `src` to the string `dest`, then a
/// NUL, and returns `dest`.
///
/// # Safety
///
/// `dest` must be a NUL-terminated string with room for `n + 1` more bytes,
/// or for all of `src`, `src` must be NUL-terminated or valid for `n` bytes,
/// and they must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strncat(dest: *mut c_char, src: *const c_char, n: usize) -> *mut c_char {
    // SAFETY: `dest` is a NUL-terminated string.
    let end = dest.wrapping_add(unsafe { strlen(dest) });
    let mut i = 0;
    while i < n {
        // SAFETY: `i < n`, and no NUL came before `i`.
        let byte = unsafe { src.wrapping_add(i).read() };
        if byte == 0 {
            break;
        }
        // SAFETY: the caller vouches for the room.
        unsafe { end.wrapping_add(i).write(byte) };
        i += 1;
    }
    // SAFETY: as above.
    unsafe { end.wrapping_add(i).write(0) };
    dest
}

/// Copies as much of the string `src` as fits into the `n` bytes at `dest`,
/// always NUL-terminated if `n` is not 0. Returns the length of `src`, so a
/// result of `n` or more means it was cut short.
///
/// # Safety
///
/// `src` must be a NUL-terminated string, and `dest` valid for writing `n`
/// bytes. They must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strlcpy(dest: *mut c_char, src: *const c_char, n: usize) -> usize {
    let Some(room) = n.checked_sub(1) else {
        // SAFETY: the caller passes a NUL-terminated string.
        return unsafe { strlen(src) };
    };
    let mut i = 0;
    while i < room {
        // SAFETY: no NUL came before `i`.
        let byte = unsafe { src.wrapping_add(i).read() };
        if byte == 0 {
            break;
        }
        // SAFETY: `i < n`.
        unsafe { dest.wrapping_add(i).write(byte) };
        i += 1;
    }
    // SAFETY: `i <= n - 1`.
    unsafe { dest.wrapping_add(i).write(0) };
    // SAFETY: `src + i` is within the string, since no NUL came before it.
    i + unsafe { strlen(src.wrapping_add(i)) }
}

/// Appends as much of the string `src` to the string in the `n` bytes at
/// `dest` as fits, NUL-terminated. Returns the length the whole result would
/// have had. If `dest` holds no NUL within `n` bytes, nothing is written and
/// the result is `n` plus the length of `src`.
///
/// # Safety
///
/// `src` must be a NUL-terminated string, and `dest` valid for `n` bytes, and
/// NUL-terminated if its string is shorter. They must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strlcat(dest: *mut c_char, src: *const c_char, n: usize) -> usize {
    // SAFETY: the caller vouches for `n` bytes or a NUL before them.
    let len = unsafe { strnlen(dest, n) };
    if len == n {
        // SAFETY: the caller passes a NUL-terminated string.
        return len + unsafe { strlen(src) };
    }
    // SAFETY: `len < n`, so `n - len` bytes from `dest + len` are the caller's.
    len + unsafe { strlcpy(dest.wrapping_add(len), src, n - len) }
}

/// The first byte of the string `s` equal to the low byte of `c`, or its NUL
/// if there is none. A `c` of 0 finds the NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strchrnul(s: *const c_char, c: c_int) -> *mut c_char {
    let wanted = c as u8;
    let mut p = s.cast::<u8>();
    if wanted == 0 {
        // SAFETY: the caller passes a NUL-terminated string.
        return unsafe { find_nul(p) }.cast_mut().cast();
    }
    while !word_aligned(p) {
        // SAFETY: no NUL came before `p`.
        let byte = unsafe { p.read() };
        if byte == 0 || byte == wanted {
            return p.cast_mut().cast();
        }
        p = p.wrapping_add(1);
    }
    let pattern = ONES * usize::from(wanted);
    loop {
        // SAFETY: `p` is aligned, and no NUL came before it.
        let word = unsafe { read_word(p) };
        if has_zero_byte(word) || has_zero_byte(word ^ pattern) {
            break;
        }
        p = p.wrapping_add(WORD);
    }
    loop {
        // SAFETY: the word at `p` holds a NUL or a match, and this stops at
        // the first.
        let byte = unsafe { p.read() };
        if byte == 0 || byte == wanted {
            return p.cast_mut().cast();
        }
        p = p.wrapping_add(1);
    }
}

/// The first byte of the string `s` equal to the low byte of `c`, or null. A
/// `c` of 0 finds the NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strchr(s: *const c_char, c: c_int) -> *mut c_char {
    // SAFETY: the same contract as `strchrnul`.
    let p = unsafe { strchrnul(s, c) };
    // SAFETY: `strchrnul` returns an address within the string.
    if unsafe { p.read() } as u8 == c as u8 {
        p
    } else {
        null_mut()
    }
}

/// The last byte of the string `s` equal to the low byte of `c`, or null. A
/// `c` of 0 finds the NUL.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strrchr(s: *const c_char, c: c_int) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(s) };
    // SAFETY: the string and its NUL are `len + 1` bytes.
    unsafe { memrchr(s.cast(), c, len + 1) }.cast()
}

/// A set of byte values, one bit each.
type ByteSet = [u64; 4];

/// Adds the bytes of the string `s` to `set`.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
unsafe fn add_bytes(set: &mut ByteSet, s: *const u8) {
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i`.
        let byte = unsafe { byte_at(s, i) };
        if byte == 0 {
            return;
        }
        add_byte(set, byte);
        i += 1;
    }
}

/// Adds `byte` to `set`.
fn add_byte(set: &mut ByteSet, byte: u8) {
    if let Some(bits) = set.get_mut(usize::from(byte >> 6)) {
        *bits |= 1 << (byte & 63);
    }
}

/// Whether `byte` is in `set`.
fn has_byte(set: &ByteSet, byte: u8) -> bool {
    set.get(usize::from(byte >> 6))
        .is_some_and(|bits| bits >> (byte & 63) & 1 != 0)
}

/// The length of the longest prefix of the string `s` made only of bytes in
/// the string `accept`.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strspn(s: *const c_char, accept: *const c_char) -> usize {
    let s = s.cast::<u8>();
    let mut set: ByteSet = [0, 0, 0, 0];
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { add_bytes(&mut set, accept.cast()) };
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i`; the NUL is never in the set.
        let byte = unsafe { byte_at(s, i) };
        if !has_byte(&set, byte) {
            return i;
        }
        i += 1;
    }
}

/// The length of the longest prefix of the string `s` with no byte in the
/// string `reject`.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcspn(s: *const c_char, reject: *const c_char) -> usize {
    let s = s.cast::<u8>();
    let reject = reject.cast::<u8>();
    // SAFETY: the caller passes a NUL-terminated string.
    let first = unsafe { reject.read() };
    // SAFETY: the string holds at least its first byte, and the second only
    // matters when the first is not the NUL.
    if first == 0 || unsafe { byte_at(reject, 1) } == 0 {
        // SAFETY: the caller passes a NUL-terminated string.
        let end = unsafe { strchrnul(s.cast(), c_int::from(first)) };
        return end.addr() - s.addr();
    }
    let mut set: ByteSet = [0, 0, 0, 0];
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { add_bytes(&mut set, reject) };
    // The NUL ends the span too.
    add_byte(&mut set, 0);
    let mut i = 0;
    loop {
        // SAFETY: no NUL came before `i`.
        let byte = unsafe { byte_at(s, i) };
        if has_byte(&set, byte) {
            return i;
        }
        i += 1;
    }
}

/// The first byte of the string `s` that is in the string `accept`, or null.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strpbrk(s: *const c_char, accept: *const c_char) -> *mut c_char {
    // SAFETY: the same contract as `strcspn`.
    let p = s.wrapping_add(unsafe { strcspn(s, accept) });
    // SAFETY: `strcspn` stops within the string, at its NUL at the latest.
    if unsafe { p.read() } == 0 {
        null_mut()
    } else {
        p.cast_mut()
    }
}

/// Splits the string `*stringp` at its first byte in `delim`, which it
/// overwrites with a NUL. Returns the old `*stringp` and moves it past the
/// split, or to null when there was nothing to split. Empty fields are
/// returned, unlike [`strtok`]'s.
///
/// # Safety
///
/// `stringp` must be valid, and `*stringp` null or a writable NUL-terminated
/// string. `delim` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strsep(stringp: *mut *mut c_char, delim: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a valid pointer.
    let s = unsafe { stringp.read() };
    if s.is_null() {
        return null_mut();
    }
    // SAFETY: both are NUL-terminated strings.
    let end = s.wrapping_add(unsafe { strcspn(s, delim) });
    // SAFETY: `strcspn` stops within the string.
    let next = if unsafe { end.read() } == 0 {
        null_mut()
    } else {
        // SAFETY: the string is writable.
        unsafe { end.write(0) };
        end.wrapping_add(1)
    };
    // SAFETY: the caller passes a valid pointer.
    unsafe { stringp.write(next) };
    s
}

/// Where [`strtok`] left off.
static STRTOK_NEXT: AtomicPtr<c_char> = AtomicPtr::new(null_mut());

/// Returns the next token of the string `s`, splitting it at bytes in
/// `delim`, and remembers where it stopped for a following call with a null
/// `s`. Not safe to use from two threads at once: see [`strtok_r`].
///
/// # Safety
///
/// As [`strtok_r`], with the saved position kept inside the library.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strtok(s: *mut c_char, delim: *const c_char) -> *mut c_char {
    let mut next = STRTOK_NEXT.load(Ordering::Relaxed);
    // SAFETY: the caller meets `strtok_r`'s contract, and `next` is a local.
    let token = unsafe { strtok_r(s, delim, &raw mut next) };
    STRTOK_NEXT.store(next, Ordering::Relaxed);
    token
}

/// Returns the next token of the string `s`, or of `*saveptr` if `s` is null,
/// splitting at bytes in `delim`. The byte ending the token is overwritten
/// with a NUL, and `*saveptr` is left after it. Runs of delimiters are skipped,
/// so no token is empty. Returns null when no token is left.
///
/// # Safety
///
/// `saveptr` must be valid. `s`, or `*saveptr` when `s` is null, must be null
/// or a writable NUL-terminated string. `delim` must be a NUL-terminated
/// string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strtok_r(
    s: *mut c_char,
    delim: *const c_char,
    saveptr: *mut *mut c_char,
) -> *mut c_char {
    let mut s = s;
    if s.is_null() {
        // SAFETY: the caller passes a valid pointer.
        s = unsafe { saveptr.read() };
        if s.is_null() {
            return null_mut();
        }
    }
    // SAFETY: both are NUL-terminated strings.
    s = s.wrapping_add(unsafe { strspn(s, delim) });
    // SAFETY: `strspn` stops within the string.
    if unsafe { s.read() } == 0 {
        // SAFETY: the caller passes a valid pointer.
        unsafe { saveptr.write(null_mut()) };
        return null_mut();
    }
    // SAFETY: both are NUL-terminated strings.
    let end = s.wrapping_add(unsafe { strcspn(s, delim) });
    // SAFETY: `strcspn` stops within the string.
    let next = if unsafe { end.read() } == 0 {
        null_mut()
    } else {
        // SAFETY: the string is writable.
        unsafe { end.write(0) };
        end.wrapping_add(1)
    };
    // SAFETY: the caller passes a valid pointer.
    unsafe { saveptr.write(next) };
    s
}

/// The first occurrence of the string `needle` in the string `haystack`, or
/// null. An empty needle is found at the start.
///
/// Needles of up to four bytes are matched by shifting bytes into a word;
/// longer ones with the two-way algorithm, in linear time and constant space.
/// Both are adapted from musl 1.2.5 (MIT), `src/string/strstr.c`.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char {
    let needle = needle.cast::<u8>();
    // SAFETY: the caller passes a NUL-terminated string.
    let first = unsafe { needle.read() };
    if first == 0 {
        return haystack.cast_mut();
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let h = unsafe { strchr(haystack, c_int::from(first)) }.cast::<u8>();
    if h.is_null() {
        return null_mut();
    }
    // Measure the needle, stopping early if the haystack from `h` is shorter.
    let mut len = 0;
    // SAFETY: no NUL came before `len` in the needle.
    while unsafe { byte_at(needle, len) } != 0 {
        // SAFETY: no NUL came before `len` in the haystack.
        if unsafe { byte_at(h, len) } == 0 {
            return null_mut();
        }
        len += 1;
    }
    // SAFETY: the needle is `len` bytes long.
    let needle = unsafe { core::slice::from_raw_parts(needle, len) };
    let found = if len <= 4 {
        // SAFETY: the haystack from `h` holds at least `len` bytes before its
        // NUL, as the loop above found.
        unsafe { short_search(h, None, needle) }
    } else {
        // SAFETY: as above.
        unsafe { two_way(h, None, needle) }
    };
    found.cast_mut().cast()
}

/// The first occurrence of the string `needle` in the string `haystack`,
/// ignoring the case of ASCII letters, or null.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcasestr(haystack: *const c_char, needle: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(needle) };
    let mut h = haystack;
    loop {
        // SAFETY: `strncasecmp` stops at either string's NUL.
        if unsafe { strncasecmp(h, needle, len) } == 0 {
            return h.cast_mut();
        }
        // SAFETY: `h` is within the haystack, whose NUL has not been passed.
        if unsafe { h.read() } == 0 {
            return null_mut();
        }
        h = h.wrapping_add(1);
    }
}

/// The first occurrence of the `needle_len` bytes at `needle` in the
/// `haystack_len` bytes at `haystack`, or null. An empty needle is found at
/// the start. Adapted from musl 1.2.5 (MIT), `src/string/memmem.c`.
///
/// # Safety
///
/// Each must be valid for its length.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memmem(
    haystack: *const c_void,
    haystack_len: usize,
    needle: *const c_void,
    needle_len: usize,
) -> *mut c_void {
    if needle_len == 0 {
        return haystack.cast_mut();
    }
    if haystack_len < needle_len {
        return null_mut();
    }
    let needle = needle.cast::<u8>();
    // SAFETY: the needle is at least one byte.
    let first = unsafe { needle.read() };
    // SAFETY: the haystack is `haystack_len` bytes.
    let h = unsafe { memchr(haystack, c_int::from(first), haystack_len) }.cast::<u8>();
    if h.is_null() || needle_len == 1 {
        return h.cast();
    }
    let left = haystack_len - (h.addr() - haystack.addr());
    if left < needle_len {
        return null_mut();
    }
    // SAFETY: the caller vouches for `needle_len` bytes.
    let needle = unsafe { core::slice::from_raw_parts(needle, needle_len) };
    let end = h.wrapping_add(left);
    let found = if needle_len <= 4 {
        // SAFETY: the `left` bytes from `h` are the caller's.
        unsafe { short_search(h, Some(end), needle) }
    } else {
        // SAFETY: as above.
        unsafe { two_way(h, Some(end), needle) }
    };
    found.cast_mut().cast()
}

/// Finds a needle of one to four bytes by shifting the haystack's bytes into
/// a word and comparing it with the needle's.
///
/// The haystack ends at `end` if one is given, and otherwise at its NUL.
///
/// # Safety
///
/// The haystack at `h` must be valid up to `end`, or be a NUL-terminated
/// string, and hold at least as many bytes as the needle before either.
unsafe fn short_search(h: *const u8, end: Option<*const u8>, needle: &[u8]) -> *const u8 {
    let len = needle.len();
    let mask = u32::MAX >> (8 * (4 - len));
    let mut want = 0_u32;
    let mut have = 0_u32;
    let mut i = 0;
    while i < len {
        let Some(&byte) = needle.get(i) else {
            return core::ptr::null();
        };
        want = want << 8 | u32::from(byte);
        // SAFETY: the haystack holds at least `len` bytes.
        have = have << 8 | u32::from(unsafe { byte_at(h, i) });
        i += 1;
    }
    loop {
        // `have` holds the `len` bytes before `i`.
        if have & mask == want {
            return h.wrapping_add(i - len);
        }
        if let Some(end) = end
            && h.wrapping_add(i) == end
        {
            return core::ptr::null();
        }
        // SAFETY: the haystack has not ended before `i`: not at `end`, and no
        // byte before `i` was the NUL.
        let byte = unsafe { byte_at(h, i) };
        if end.is_none() && byte == 0 {
            return core::ptr::null();
        }
        have = have << 8 | u32::from(byte);
        i += 1;
    }
}

/// The start of the needle's critical factorisation and its period, found by
/// computing the maximal suffix under one byte order, or under the reverse
/// order if `reverse`. A start of `usize::MAX` stands for -1.
fn maximal_suffix(needle: &[u8], reverse: bool) -> (usize, usize) {
    let len = needle.len();
    let mut ip = usize::MAX;
    let mut jp = 0;
    let mut k = 1;
    let mut period = 1;
    while jp + k < len {
        let (Some(&a), Some(&b)) = (needle.get(ip.wrapping_add(k)), needle.get(jp + k)) else {
            break;
        };
        if a == b {
            if k == period {
                jp += period;
                k = 1;
            } else {
                k += 1;
            }
        } else if (a > b) != reverse {
            jp += k;
            k = 1;
            period = jp.wrapping_sub(ip);
        } else {
            ip = jp;
            jp += 1;
            k = 1;
            period = 1;
        }
    }
    (ip, period)
}

/// Finds a needle of any length with the two-way algorithm (Crochemore and
/// Perrin), with a bad-character shift on the needle's last byte. Adapted from
/// musl 1.2.5 (MIT), `src/string/memmem.c` and `src/string/strstr.c`.
///
/// The haystack ends at `end` if one is given. Otherwise it is a string, and
/// its end is found as the search goes, a stretch at a time, so a match near
/// the start of a long string is found without measuring all of it.
///
/// # Safety
///
/// The haystack at `h` must be valid up to `end`, or be a NUL-terminated
/// string.
unsafe fn two_way(h: *const u8, end: Option<*const u8>, needle: &[u8]) -> *const u8 {
    let len = needle.len();
    let mut h = h;

    // Which bytes the needle holds, and for each, one past its last position.
    // A shift entry is written for every byte in the set and read only for
    // those, so the table needs no clearing.
    let mut present: ByteSet = [0, 0, 0, 0];
    let mut shift = [MaybeUninit::<usize>::uninit(); 256];
    let mut i = 0;
    while i < len {
        let Some(&byte) = needle.get(i) else {
            return core::ptr::null();
        };
        add_byte(&mut present, byte);
        if let Some(entry) = shift.get_mut(usize::from(byte)) {
            let _ = entry.write(i + 1);
        }
        i += 1;
    }

    let (forward, forward_period) = maximal_suffix(needle, false);
    let (backward, backward_period) = maximal_suffix(needle, true);
    let (ms, mut period) = if backward.wrapping_add(1) > forward.wrapping_add(1) {
        (backward, backward_period)
    } else {
        (forward, forward_period)
    };

    // Whether the needle's prefix before the factorisation repeats at
    // `period`. An out-of-range comparison counts as a mismatch, which picks
    // the other case: correct for any needle, if slower for a periodic one.
    let mut periodic = true;
    let mut i = 0;
    while i < ms.wrapping_add(1) {
        match (needle.get(i), needle.get(i + period)) {
            (Some(a), Some(b)) if a == b => {}
            _ => {
                periodic = false;
                break;
            }
        }
        i += 1;
    }
    let memory_after_shift = if periodic {
        len - period
    } else {
        period = ms.max(len.wrapping_sub(ms).wrapping_sub(1)).wrapping_add(1);
        0
    };
    let mut memory = 0;

    // How far the haystack is known to extend.
    let mut known_end = end.unwrap_or(h);
    loop {
        if known_end.addr().saturating_sub(h.addr()) < len {
            if end.is_some() {
                return core::ptr::null();
            }
            // Look for the NUL in the next stretch of at least `len` bytes.
            let stretch = len | 63;
            // SAFETY: `known_end` is within the string, and `memchr` stops at
            // the NUL.
            let nul = unsafe { memchr(known_end.cast(), 0, stretch) }.cast::<u8>();
            if nul.is_null() {
                known_end = known_end.wrapping_add(stretch);
            } else {
                known_end = nul;
                if known_end.addr() - h.addr() < len {
                    return core::ptr::null();
                }
            }
        }
        // From here the `len` bytes at `h` are all in the haystack.

        // SAFETY: `len - 1` is within those bytes.
        let last = unsafe { byte_at(h, len - 1) };
        if !has_byte(&present, last) {
            h = h.wrapping_add(len);
            memory = 0;
            continue;
        }
        let entry = shift.get(usize::from(last));
        // SAFETY: the byte is in the needle, so its entry was written.
        let skip = entry.map_or(0, |entry| len - unsafe { entry.assume_init_read() });
        if skip != 0 {
            h = h.wrapping_add(skip.max(memory));
            memory = 0;
            continue;
        }

        // Compare the right half.
        let mut k = ms.wrapping_add(1).max(memory);
        // SAFETY: `k < len`.
        while k < len && needle.get(k) == Some(&unsafe { byte_at(h, k) }) {
            k += 1;
        }
        if k < len {
            h = h.wrapping_add(k.wrapping_sub(ms));
            memory = 0;
            continue;
        }
        // Compare the left half.
        let mut k = ms.wrapping_add(1);
        // SAFETY: `k - 1 < len`.
        while k > memory && needle.get(k - 1) == Some(&unsafe { byte_at(h, k - 1) }) {
            k -= 1;
        }
        if k <= memory {
            return h;
        }
        h = h.wrapping_add(period);
        memory = memory_after_shift;
    }
}

/// Compares two strings as version numbers: runs of digits compare by value,
/// and a run with leading zeros sorts as a fraction, before one without.
/// Adapted from musl 1.2.5 (MIT), `src/string/strverscmp.c`.
///
/// # Safety
///
/// Both must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strverscmp(a: *const c_char, b: *const c_char) -> c_int {
    let l = a.cast::<u8>();
    let r = b.cast::<u8>();
    // The common prefix's length, where its last run of digits starts, and
    // whether that run is all zeros.
    let mut i = 0;
    let mut digits_start = 0;
    let mut zeros = true;
    loop {
        // SAFETY: the strings matched, without a NUL, before `i`.
        let x = unsafe { byte_at(l, i) };
        // SAFETY: as above.
        let y = unsafe { byte_at(r, i) };
        if x != y {
            break;
        }
        if x == 0 {
            return 0;
        }
        if !x.is_ascii_digit() {
            digits_start = i + 1;
            zeros = true;
        } else if x != b'0' {
            zeros = false;
        }
        i += 1;
    }
    // SAFETY: `digits_start <= i`, within both strings.
    let x = unsafe { byte_at(l, digits_start) };
    // SAFETY: as above.
    let y = unsafe { byte_at(r, digits_start) };
    // SAFETY: `i` is within both strings.
    let li = unsafe { byte_at(l, i) };
    // SAFETY: as above.
    let ri = unsafe { byte_at(r, i) };
    if (b'1'..=b'9').contains(&x) && (b'1'..=b'9').contains(&y) {
        // Runs without leading zeros: the longer is greater.
        let mut j = i;
        // SAFETY: both strings held digits, so no NUL, before `j`.
        while unsafe { byte_at(l, j) }.is_ascii_digit() {
            // SAFETY: as above.
            if !unsafe { byte_at(r, j) }.is_ascii_digit() {
                return 1;
            }
            j += 1;
        }
        // SAFETY: as above.
        if unsafe { byte_at(r, j) }.is_ascii_digit() {
            return -1;
        }
    } else if zeros && digits_start < i && (li.is_ascii_digit() || ri.is_ascii_digit()) {
        // After a common run of zeros, digits sort before anything else.
        return c_int::from(li.wrapping_sub(b'0')) - c_int::from(ri.wrapping_sub(b'0'));
    }
    c_int::from(li) - c_int::from(ri)
}

/// Copies `n` bytes from `src` to `dest`, exchanging each pair. A last odd
/// byte is not copied. `swab` is declared in `unistd.h`.
///
/// # Safety
///
/// Both must be valid for `n` bytes and must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn swab(src: *const c_void, dest: *mut c_void, n: isize) {
    let from = src.cast::<u8>();
    let to = dest.cast::<u8>();
    let mut i = 0;
    while n - i as isize > 1 {
        // SAFETY: `i + 1 < n`.
        let first = unsafe { byte_at(from, i) };
        // SAFETY: as above.
        let second = unsafe { byte_at(from, i + 1) };
        // SAFETY: as above.
        unsafe { to.wrapping_add(i).write(second) };
        // SAFETY: as above.
        unsafe { to.wrapping_add(i + 1).write(first) };
        i += 2;
    }
}

/// A copy of the string `s`, in memory from `malloc`. Null, with `errno` set
/// to `ENOMEM`, if there is no memory for it.
///
/// # Safety
///
/// `s` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strdup(s: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated string.
    let len = unsafe { strlen(s) };
    // SAFETY: `strlen` found `len` readable bytes before the NUL.
    unsafe { copy_out(s, len) }
}

/// A copy of at most `n` bytes of `s`, always NUL-terminated, in memory from
/// `malloc`. Null, with `errno` set to `ENOMEM`, if there is no memory for it.
///
/// # Safety
///
/// `s` must be NUL-terminated or readable for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strndup(s: *const c_char, n: usize) -> *mut c_char {
    // SAFETY: the caller's contract is `strnlen`'s.
    let len = unsafe { strnlen(s, n) };
    // SAFETY: `strnlen` found `len` readable bytes.
    unsafe { copy_out(s, len) }
}

/// `len` bytes of `s` followed by a NUL, in memory from `malloc`.
///
/// # Safety
///
/// `s` must be readable for `len` bytes.
unsafe fn copy_out(s: *const c_char, len: usize) -> *mut c_char {
    let Some(size) = len.checked_add(1) else {
        crate::errno::set(crate::errno::ENOMEM);
        return null_mut();
    };
    let copy = crate::malloc::malloc(size).cast::<c_char>();
    if copy.is_null() {
        return copy;
    }
    // SAFETY: the copy holds `len + 1` bytes, the source `len`, and they are
    // different allocations.
    let _ = unsafe { memcpy(copy.cast(), s.cast(), len) };
    // SAFETY: as above.
    unsafe { copy.wrapping_add(len).write(0) };
    copy
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::CStr;

    /// A word-aligned buffer, so the word-at-a-time functions' last word stays
    /// inside it.
    #[repr(C, align(64))]
    struct Aligned([u8; 256]);

    impl Aligned {
        /// A buffer of NULs.
        fn new() -> Box<Self> {
            Box::new(Self([0; 256]))
        }

        /// The address of byte `i`.
        fn at(&mut self, i: usize) -> *mut c_char {
            self.0.as_mut_ptr().wrapping_add(i).cast()
        }

        /// Places `text` at `offset`, followed by a NUL and then non-NUL
        /// bytes, and returns its address.
        fn place(&mut self, offset: usize, text: &[u8]) -> *mut c_char {
            self.0.fill(b'#');
            self.0[offset..offset + text.len()].copy_from_slice(text);
            self.0[offset + text.len()] = 0;
            self.at(offset)
        }
    }

    /// The string at `p`.
    fn string<'a>(p: *const c_char) -> &'a [u8] {
        // SAFETY: every pointer tested is to a NUL-terminated string.
        unsafe { CStr::from_ptr(p) }.to_bytes()
    }

    #[test]
    fn strlen_counts_to_the_nul() {
        // SAFETY: C string literals are NUL-terminated.
        assert_eq!(unsafe { strlen(c"".as_ptr()) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { strlen(c"ferrousli".as_ptr()) }, 9);
    }

    #[test]
    fn the_word_functions_find_the_end_at_every_alignment_and_length() {
        let mut buf = Aligned::new();
        for offset in 0..16 {
            for len in 0..100 {
                let text: Vec<u8> = (0..len).map(|i| b'a' + (i % 26) as u8).collect();
                let s = buf.place(offset, &text);
                // SAFETY: `place` wrote a NUL-terminated string.
                assert_eq!(unsafe { strlen(s) }, len, "strlen at {offset}+{len}");
                // SAFETY: as above.
                assert_eq!(unsafe { strnlen(s, len + 5) }, len);
                // SAFETY: as above; `strnlen` reads no more than `len / 2` bytes.
                assert_eq!(unsafe { strnlen(s, len / 2) }, len / 2);
                // SAFETY: as above.
                let nul = unsafe { strchrnul(s, c_int::from(b'!')) };
                assert_eq!(nul, s.wrapping_add(len), "strchrnul at {offset}+{len}");
                // SAFETY: as above.
                assert!(unsafe { strchr(s, c_int::from(b'!')) }.is_null());
                // SAFETY: as above.
                assert_eq!(unsafe { strchr(s, 0) }, s.wrapping_add(len));
                // The NUL is searched as an ordinary byte by `memchr`.
                // SAFETY: the buffer holds `len + 1` bytes from `s`.
                let found = unsafe { memchr(s.cast(), 0, len + 1) };
                assert_eq!(found, s.wrapping_add(len).cast());
                // SAFETY: the buffer holds `len` bytes from `s`.
                assert!(unsafe { memchr(s.cast(), 0, len) }.is_null());
            }
        }
    }

    #[test]
    fn the_word_functions_find_a_match_at_every_position() {
        let mut buf = Aligned::new();
        for offset in 0..16 {
            for at in 0..40 {
                let mut text = vec![b'x'; 48];
                text[at] = 0x80;
                text[at + 3] = 0x80;
                let s = buf.place(offset, &text);
                // SAFETY: `place` wrote a NUL-terminated string.
                let found = unsafe { strchr(s, 0x80) };
                assert_eq!(found, s.wrapping_add(at), "strchr at {offset}+{at}");
                // A `c` beyond a byte uses its low byte.
                // SAFETY: as above.
                assert_eq!(unsafe { strchr(s, 0x180) }, found);
                // SAFETY: as above.
                assert_eq!(unsafe { strchrnul(s, 0x80) }, found);
                // SAFETY: the buffer holds 48 bytes from `s`.
                let found = unsafe { memchr(s.cast(), -128, 48) };
                assert_eq!(found, s.wrapping_add(at).cast(), "memchr at {offset}+{at}");
                // SAFETY: as above.
                assert!(unsafe { memchr(s.cast(), 0x80, at) }.is_null());
                // SAFETY: as above.
                let last = unsafe { memrchr(s.cast(), 0x80, 48) };
                assert_eq!(last, s.wrapping_add(at + 3).cast());
                // SAFETY: as above.
                assert_eq!(unsafe { strrchr(s, 0x80) }, s.wrapping_add(at + 3));
            }
        }
    }

    #[test]
    fn has_zero_byte_sees_each_position_and_nothing_else() {
        assert!(!has_zero_byte(usize::MAX));
        assert!(!has_zero_byte(ONES));
        assert!(!has_zero_byte(HIGHS));
        for byte in 0..WORD {
            assert!(has_zero_byte(usize::MAX & !(0xff << (8 * byte))));
            // A 0x80 or 0x01 next to others must not look like a zero.
            assert!(!has_zero_byte(ONES | (0x80 << (8 * byte))));
        }
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
        // SAFETY: as above.
        assert!(unsafe { strcoll(c"b".as_ptr(), c"a".as_ptr()) } > 0);
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
        // SAFETY: `buf` holds four bytes.
        unsafe { explicit_bzero(p, 3) };
        assert_eq!(&buf, b"\0\0\0z");
    }

    #[test]
    fn mempcpy_and_memccpy_return_the_end() {
        let mut buf = [b'.'; 8];
        let p = buf.as_mut_ptr().cast::<c_void>();
        // SAFETY: both hold three bytes.
        let end = unsafe { mempcpy(p, b"abc".as_ptr().cast(), 3) };
        assert_eq!(end, p.wrapping_byte_add(3));
        // SAFETY: both hold six bytes.
        let end = unsafe { memccpy(p, b"xy:zw!".as_ptr().cast(), c_int::from(b':'), 6) };
        assert_eq!(end, p.wrapping_byte_add(3));
        assert_eq!(&buf, b"xy:.....");
        // SAFETY: as above.
        let end = unsafe { memccpy(p, b"qrstuv".as_ptr().cast(), c_int::from(b':'), 6) };
        assert!(end.is_null());
        assert_eq!(&buf, b"qrstuv..");
    }

    #[test]
    fn copies_terminate_pad_and_return_what_they_should() {
        let mut buf = [b'x'; 16];
        let p = buf.as_mut_ptr().cast::<c_char>();
        // SAFETY: `buf` has room for the string.
        assert_eq!(unsafe { stpcpy(p, c"abc".as_ptr()) }, p.wrapping_add(3));
        assert_eq!(&buf[..5], b"abc\0x");
        // SAFETY: as above.
        assert_eq!(unsafe { strcpy(p, c"".as_ptr()) }, p);
        assert_eq!(buf[0], 0);

        buf = [b'x'; 16];
        // SAFETY: `buf` holds 6 bytes.
        assert_eq!(unsafe { stpncpy(p, c"abc".as_ptr(), 6) }, p.wrapping_add(3));
        assert_eq!(&buf[..7], b"abc\0\0\0x");
        buf = [b'x'; 16];
        assert_eq!(
            // SAFETY: as above.
            unsafe { stpncpy(p, c"abcdef".as_ptr(), 4) },
            p.wrapping_add(4)
        );
        assert_eq!(&buf[..5], b"abcdx");
        // SAFETY: as above.
        assert_eq!(unsafe { strncpy(p, c"q".as_ptr(), 0) }, p);
        assert_eq!(buf[0], b'a');

        let mut buf = [0_u8; 16];
        let p = buf.as_mut_ptr().cast::<c_char>();
        // SAFETY: `buf` has room for both strings.
        let _ = unsafe { strcat(p, c"ab".as_ptr()) };
        // SAFETY: as above.
        let _ = unsafe { strcat(p, c"cd".as_ptr()) };
        // SAFETY: as above.
        assert_eq!(unsafe { strncat(p, c"efgh".as_ptr(), 2) }, p);
        assert_eq!(string(p), b"abcdef");
        // SAFETY: as above.
        let _ = unsafe { strncat(p, c"".as_ptr(), 5) };
        assert_eq!(string(p), b"abcdef");
    }

    #[test]
    fn strlcpy_and_strlcat_report_the_length_they_needed() {
        let mut buf = [b'x'; 8];
        let p = buf.as_mut_ptr().cast::<c_char>();
        // SAFETY: `buf` holds 8 bytes.
        assert_eq!(unsafe { strlcpy(p, c"hello world".as_ptr(), 8) }, 11);
        assert_eq!(string(p), b"hello w");
        // A zero size writes nothing, so null is fine.
        // SAFETY: nothing is written.
        assert_eq!(unsafe { strlcpy(null_mut(), c"abc".as_ptr(), 0) }, 3);
        // SAFETY: `buf` holds 8 bytes.
        assert_eq!(unsafe { strlcpy(p, c"ab".as_ptr(), 8) }, 2);
        // SAFETY: as above.
        assert_eq!(unsafe { strlcat(p, c"cdefghij".as_ptr(), 8) }, 10);
        assert_eq!(string(p), b"abcdefg");
        // No NUL within the size: nothing is written.
        buf = *b"abcdefgh";
        // SAFETY: nothing is read past 4 bytes of `buf`.
        assert_eq!(unsafe { strlcat(p, c"xyz".as_ptr(), 4) }, 7);
        assert_eq!(&buf, b"abcdefgh");
    }

    #[test]
    fn spans_and_breaks() {
        let s = c"aaababccdd0001122223";
        // SAFETY: C string literals are NUL-terminated.
        assert_eq!(unsafe { strspn(s.as_ptr(), c"abcd".as_ptr()) }, 10);
        // SAFETY: as above.
        assert_eq!(unsafe { strspn(s.as_ptr(), c"".as_ptr()) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { strspn(s.as_ptr(), c"a".as_ptr()) }, 3);
        // SAFETY: as above.
        assert_eq!(unsafe { strcspn(s.as_ptr(), c"0123".as_ptr()) }, 10);
        // SAFETY: as above.
        assert_eq!(unsafe { strcspn(s.as_ptr(), c"".as_ptr()) }, 20);
        // SAFETY: as above.
        assert_eq!(unsafe { strcspn(s.as_ptr(), c"3".as_ptr()) }, 19);
        assert_eq!(
            // SAFETY: as above.
            unsafe { strcspn(c"\xff\x80 x".as_ptr(), c" \x80".as_ptr()) },
            1
        );
        // SAFETY: as above.
        let p = unsafe { strpbrk(s.as_ptr(), c"0123".as_ptr()) };
        assert_eq!(p, s.as_ptr().wrapping_add(10).cast_mut());
        // SAFETY: as above.
        assert!(unsafe { strpbrk(s.as_ptr(), c"xyz".as_ptr()) }.is_null());
    }

    #[test]
    fn strtok_r_skips_runs_and_strsep_keeps_empty_fields() {
        let mut buf = *b",,ab,,c,\0";
        let mut save = null_mut();
        let delim = c",".as_ptr();
        // SAFETY: `buf` is a writable NUL-terminated string.
        let first = unsafe { strtok_r(buf.as_mut_ptr().cast(), delim, &raw mut save) };
        assert_eq!(string(first), b"ab");
        // SAFETY: `save` points into `buf`.
        let second = unsafe { strtok_r(null_mut(), delim, &raw mut save) };
        assert_eq!(string(second), b"c");
        // SAFETY: as above.
        assert!(unsafe { strtok_r(null_mut(), delim, &raw mut save) }.is_null());
        assert!(save.is_null());
        // SAFETY: `save` is null, which ends the tokens.
        assert!(unsafe { strtok_r(null_mut(), delim, &raw mut save) }.is_null());

        let mut buf = *b"a,,b\0";
        let mut rest = buf.as_mut_ptr().cast::<c_char>();
        let mut fields = Vec::new();
        loop {
            // SAFETY: `rest` is null or points into `buf`.
            let field = unsafe { strsep(&raw mut rest, delim) };
            if field.is_null() {
                break;
            }
            fields.push(string(field).to_vec());
        }
        assert_eq!(fields, [b"a".to_vec(), b"".to_vec(), b"b".to_vec()]);

        let mut buf = *b"x y\0";
        // SAFETY: `buf` is a writable NUL-terminated string.
        let token = unsafe { strtok(buf.as_mut_ptr().cast(), c" ".as_ptr()) };
        assert_eq!(string(token), b"x");
        // SAFETY: `strtok` saved a position inside `buf`.
        let token = unsafe { strtok(null_mut(), c" ".as_ptr()) };
        assert_eq!(string(token), b"y");
        // SAFETY: as above.
        assert!(unsafe { strtok(null_mut(), c" ".as_ptr()) }.is_null());
    }

    /// Where `needle` first occurs in `haystack`, by the obvious method.
    fn naive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn searches_agree_with_the_obvious_method() {
        // Short alphabets make periodic needles and near misses common.
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut buf = Aligned::new();
        for round in 0..4000 {
            let alphabet = 1 + (round % 3) as u32;
            let haystack: Vec<u8> = (0..next() % 60)
                .map(|_| b'a' + (next() % alphabet) as u8)
                .collect();
            let needle: Vec<u8> = (0..next() % 12)
                .map(|_| b'a' + (next() % alphabet) as u8)
                .collect();
            let want = naive(&haystack, &needle);

            // SAFETY: both slices are valid for their lengths.
            let found = unsafe {
                memmem(
                    haystack.as_ptr().cast(),
                    haystack.len(),
                    needle.as_ptr().cast(),
                    needle.len(),
                )
            };
            let got = (!found.is_null()).then(|| found.addr() - haystack.as_ptr().addr());
            assert_eq!(got, want, "memmem({haystack:?}, {needle:?})");

            let h = buf.place(round % 8, &haystack);
            let mut n = needle.clone();
            n.push(0);
            // SAFETY: `h` and `n` are NUL-terminated strings.
            let found = unsafe { strstr(h, n.as_ptr().cast()) };
            let got = (!found.is_null()).then(|| found.addr() - h.addr());
            assert_eq!(got, want, "strstr({haystack:?}, {needle:?})");
        }
    }

    #[test]
    fn strstr_finds_a_long_needle_in_a_long_string() {
        let mut haystack = vec![b'a'; 1000];
        haystack.extend_from_slice(b"aaaaaaaaaaaab");
        haystack.push(0);
        let mut needle = vec![b'a'; 300];
        needle.push(b'b');
        needle.push(0);
        // SAFETY: both are NUL-terminated.
        let found = unsafe { strstr(haystack.as_ptr().cast(), needle.as_ptr().cast()) };
        assert_eq!(found.addr() - haystack.as_ptr().addr(), 1012 - 300);
        needle.insert(0, b'c');
        // SAFETY: as above.
        assert!(unsafe { strstr(haystack.as_ptr().cast(), needle.as_ptr().cast()) }.is_null());
    }

    #[test]
    fn strcasestr_ignores_case() {
        let h = c"Hello, World";
        // SAFETY: C string literals are NUL-terminated.
        let found = unsafe { strcasestr(h.as_ptr(), c"wORLD".as_ptr()) };
        assert_eq!(found, h.as_ptr().wrapping_add(7).cast_mut());
        // SAFETY: as above.
        assert!(unsafe { strcasestr(h.as_ptr(), c"worlds".as_ptr()) }.is_null());
        assert_eq!(
            // SAFETY: as above.
            unsafe { strcasestr(c"".as_ptr(), c"".as_ptr()) },
            c"".as_ptr().cast_mut()
        );
    }

    #[test]
    fn strverscmp_orders_versions() {
        let ordered = [
            c"000", c"00", c"01", c"010", c"09", c"0", c"1", c"9", c"10", c"a1", c"a2", c"a10",
            c"jan", c"jan9", c"jan10",
        ];
        for (i, a) in ordered.iter().enumerate() {
            for (j, b) in ordered.iter().enumerate() {
                // SAFETY: C string literals are NUL-terminated.
                let order = unsafe { strverscmp(a.as_ptr(), b.as_ptr()) };
                assert_eq!(
                    order.signum(),
                    (i as i32 - j as i32).signum(),
                    "{a:?} {b:?}"
                );
            }
        }
    }

    #[test]
    fn strxfrm_copies_only_when_it_fits() {
        let mut buf = [b'x'; 8];
        let p = buf.as_mut_ptr().cast::<c_char>();
        // SAFETY: `buf` holds 8 bytes.
        assert_eq!(unsafe { strxfrm(p, c"abc".as_ptr(), 3) }, 3);
        assert_eq!(buf[0], b'x');
        // SAFETY: as above.
        assert_eq!(unsafe { strxfrm(p, c"abc".as_ptr(), 4) }, 3);
        assert_eq!(string(p), b"abc");
        // SAFETY: nothing is written with a zero size.
        assert_eq!(unsafe { strxfrm(null_mut(), c"abcd".as_ptr(), 0) }, 4);
    }

    #[test]
    fn swab_exchanges_pairs_and_leaves_an_odd_byte() {
        let mut out = [b'.'; 6];
        // SAFETY: both hold five bytes.
        unsafe { swab(b"abcde".as_ptr().cast(), out.as_mut_ptr().cast(), 5) };
        assert_eq!(&out, b"badc..");
        // SAFETY: a negative length copies nothing.
        unsafe { swab(b"ab".as_ptr().cast(), out.as_mut_ptr().cast(), -2) };
        assert_eq!(&out, b"badc..");
    }
}
