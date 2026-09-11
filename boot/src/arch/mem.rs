//! `memcpy` and its relatives, for the ARMv7-A loader.
//!
//! The compiler lowers copies, fills and comparisons to calls to these
//! functions by name. On the 64-bit pair they come from `compiler_builtins`,
//! which provides them for targets with no C library. The target the ARMv7-A
//! loader is built for expects libc to provide them, and the loader links none
//! — `.cargo/config.toml` says why — so these are the loader's own. Without
//! them the link fails, naming `memcpy`, `memmove` and `memset`.
//!
//! Every access is volatile, which is the one thing about them that is not
//! obvious. A plain byte loop is exactly the pattern LLVM recognises as a copy
//! and replaces with a call to `memcpy` — from inside `memcpy`, forever. A
//! volatile access can be neither merged nor replaced. The cost is speed, and
//! the loader copies and zeroes a few megabytes, once.

/// Copy `n` bytes from `source` to `destination`.
///
/// # Safety
///
/// `source` must be readable and `destination` writable for `n` bytes, and the
/// two ranges must not overlap — the contract C gives `memcpy`, which is the
/// one the compiler relies on when it emits a call to it.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcpy(destination: *mut u8, source: *const u8, n: usize) -> *mut u8 {
    for index in 0..n {
        // SAFETY: inside `source`'s `n` readable bytes.
        let byte = unsafe { source.wrapping_add(index).read_volatile() };
        // SAFETY: inside `destination`'s `n` writable bytes.
        unsafe { destination.wrapping_add(index).write_volatile(byte) };
    }
    destination
}

/// Copy `n` bytes from `source` to `destination`, which may overlap.
///
/// # Safety
///
/// `source` must be readable and `destination` writable for `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memmove(destination: *mut u8, source: *const u8, n: usize) -> *mut u8 {
    // Forwards when the destination is below the source and backwards when it
    // is above, so no byte is overwritten before it has been read.
    if destination.cast_const() <= source {
        for index in 0..n {
            // SAFETY: inside `source`'s `n` readable bytes.
            let byte = unsafe { source.wrapping_add(index).read_volatile() };
            // SAFETY: inside `destination`'s `n` writable bytes.
            unsafe { destination.wrapping_add(index).write_volatile(byte) };
        }
    } else {
        for index in (0..n).rev() {
            // SAFETY: as above.
            let byte = unsafe { source.wrapping_add(index).read_volatile() };
            // SAFETY: as above.
            unsafe { destination.wrapping_add(index).write_volatile(byte) };
        }
    }
    destination
}

/// Fill `n` bytes at `destination` with the low byte of `value`.
///
/// # Safety
///
/// `destination` must be writable for `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memset(destination: *mut u8, value: i32, n: usize) -> *mut u8 {
    // The low byte, as C specifies: the argument is an `int` for historical
    // reasons and only its conversion to `unsigned char` is written.
    let byte = value as u8;
    for index in 0..n {
        // SAFETY: inside `destination`'s `n` writable bytes.
        unsafe { destination.wrapping_add(index).write_volatile(byte) };
    }
    destination
}

/// Compare `n` bytes, returning the difference at the first that differs.
///
/// # Safety
///
/// Both `left` and `right` must be readable for `n` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(left: *const u8, right: *const u8, n: usize) -> i32 {
    for index in 0..n {
        // SAFETY: inside `left`'s `n` readable bytes.
        let a = unsafe { left.wrapping_add(index).read_volatile() };
        // SAFETY: inside `right`'s `n` readable bytes.
        let b = unsafe { right.wrapping_add(index).read_volatile() };
        if a != b {
            return i32::from(a) - i32::from(b);
        }
    }
    0
}

/// Compare `n` bytes for equality only: zero if they are the same.
///
/// # Safety
///
/// As [`memcmp`], which it is: LLVM emits `bcmp` where only equality matters.
#[unsafe(no_mangle)]
unsafe extern "C" fn bcmp(left: *const u8, right: *const u8, n: usize) -> i32 {
    // SAFETY: the caller's contract is `memcmp`'s.
    unsafe { memcmp(left, right, n) }
}
