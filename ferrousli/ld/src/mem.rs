//! `memcpy` and its neighbours, which the compiler calls whether or not the
//! source does.
//!
//! rustc and LLVM turn ordinary code — moving a large value, comparing two
//! arrays, zeroing a buffer — into calls to these. A freestanding program has
//! to supply them, and this one cannot take the C library's: the loader runs
//! before the C library is mapped, and linking a second copy of it into every
//! process is the thing a dynamic loader exists to avoid.
//!
//! They are written as `ferrousli/CONVENTIONS.md` requires of this kind of
//! code: plain `while` loops over raw pointers, with no slice operation, no
//! iterator and no move of anything larger than a machine word, because every
//! one of those is a call back into the function being defined.

use core::ffi::{c_int, c_void};

/// `memcpy`.
///
/// # Safety
///
/// `dest` and `src` must each be valid for `n` bytes, and must not overlap.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let to = dest.cast::<u8>();
    let from = src.cast::<u8>();
    let mut at = 0;
    while at < n {
        // SAFETY: the caller promises both are valid for `n` bytes.
        unsafe { to.add(at).write(from.add(at).read()) };
        at += 1;
    }
    dest
}

/// `memmove`, which is `memcpy` except that the two may overlap.
///
/// # Safety
///
/// `dest` and `src` must each be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let to = dest.cast::<u8>();
    let from = src.cast::<u8>();
    // Copying backwards when the destination is above the source is what
    // makes an overlap safe: each byte is read before anything writes over
    // it.
    if (to as usize) < (from as usize) {
        let mut at = 0;
        while at < n {
            // SAFETY: the caller promises both are valid for `n` bytes.
            unsafe { to.add(at).write(from.add(at).read()) };
            at += 1;
        }
    } else {
        let mut at = n;
        while at > 0 {
            at -= 1;
            // SAFETY: as above.
            unsafe { to.add(at).write(from.add(at).read()) };
        }
    }
    dest
}

/// `memset`.
///
/// # Safety
///
/// `dest` must be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memset(dest: *mut c_void, byte: c_int, n: usize) -> *mut c_void {
    let to = dest.cast::<u8>();
    let value = byte as u8;
    let mut at = 0;
    while at < n {
        // SAFETY: the caller promises `dest` is valid for `n` bytes.
        unsafe { to.add(at).write(value) };
        at += 1;
    }
    dest
}

/// `memcmp`.
///
/// # Safety
///
/// `a` and `b` must each be valid for `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    let left = a.cast::<u8>();
    let right = b.cast::<u8>();
    let mut at = 0;
    while at < n {
        // SAFETY: the caller promises both are valid for `n` bytes.
        let (x, y) = unsafe { (left.add(at).read(), right.add(at).read()) };
        if x != y {
            return c_int::from(x) - c_int::from(y);
        }
        at += 1;
    }
    0
}

/// `bcmp`, which LLVM emits where only equality is asked for.
///
/// # Safety
///
/// As [`memcmp`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    // SAFETY: the caller's promise is `memcmp`'s.
    unsafe { memcmp(a, b, n) }
}

/// The personality routine the unwinder would call.
///
/// Nothing here unwinds — the profile builds with `panic = "abort"` and the
/// panic handler traps — but `core` names the symbol, so it has to exist. It
/// is never called, and if it somehow were, returning is the only thing it
/// could do that is not worse.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn rust_eh_personality() {}
