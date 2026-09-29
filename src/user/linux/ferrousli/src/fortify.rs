//! glibc's fortified memory functions: `__memcpy_chk` and its relatives.
//!
//! A program compiled against glibc's headers with `_FORTIFY_SOURCE` set — as
//! every Debian and Ubuntu package is, and as `cc -O2` is on those hosts by
//! default — has its calls to `memcpy`, `memmove` and `memset` rewritten to
//! these, with one extra argument: how large the compiler proved the
//! destination to be, or `(size_t)-1` when it could not tell. Each checks the
//! length against it and stops the program when the copy would overrun.
//!
//! musl has none of them, because musl's own headers never rewrite anything.
//! They are here for the same reason `crt1.o` calls `__libc_start_main` with
//! glibc's arguments: an object file compiled against glibc must link. The
//! spike that linked uutils/coreutils against this library found
//! `__memcpy_chk` coming out of a C dependency built with the host's headers.
//!
//! The memory and string functions, and the system calls Chrome's libraries
//! are built to check (`__read_chk`, `__poll_chk`, `__getcwd_chk` and the
//! rest), are here; `printf`'s and `fgets`'s are in `stdio/fortify.rs`, and
//! `__open_2` and `__openat_2` in `fcntl.rs`. Each is the check glibc makes
//! and then the plain function.
//!
//! # What a failed check does
//!
//! glibc calls `__chk_fail`, which writes `*** buffer overflow detected ***:
//! terminated` and raises `SIGABRT`. So does this, through
//! [`crate::signal::abort`]. The check has found a buffer overrun that has
//! not happened yet; there is nothing to return.

use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use core::mem::size_of;

use crate::poll::{Pollfd, poll};
use crate::process::getgroups;
use crate::realpath::realpath;
use crate::signal::abort;
use crate::string::{
    explicit_bzero, memcpy, memmove, mempcpy, memset, stpcpy, strcat, strcpy, strlcat, strlcpy,
    strlen, strncat, strncpy, strnlen,
};
use crate::syscall::{self, nr};
use crate::unistd::{getcwd, read, readlinkat};

/// `PATH_MAX`, the size `realpath` writes up to.
const PATH_MAX: usize = 4096;
/// `FD_SETSIZE`, the descriptors an `fd_set` holds.
const FD_SETSIZE: c_long = 1024;
/// `NFDBITS`, the descriptors one word of an `fd_set` holds.
const NFDBITS: c_long = 8 * size_of::<c_long>() as c_long;

/// Stops the program: a fortified call was asked to write more than its
/// destination holds.
///
/// glibc exports this, and a program compiled against its headers can call it
/// directly, so it keeps the name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __chk_fail() -> ! {
    fortify_fail(b"*** buffer overflow detected ***: terminated\n")
}

/// Writes `message` to standard error and aborts: glibc's `__fortify_fail`,
/// whose message names the check that failed.
pub(crate) fn fortify_fail(message: &[u8]) -> ! {
    // SAFETY: `message` is live, and the kernel only reads it.
    let _ = unsafe { syscall::syscall3(nr::WRITE, 2, message.as_ptr().addr(), message.len()) };
    abort()
}

/// `memcpy`, refusing a copy larger than the destination.
///
/// # Safety
///
/// As `memcpy`: `dest` valid for writes of `n` bytes, `src` for reads of `n`,
/// and the two not overlapping.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __memcpy_chk(
    dest: *mut c_void,
    src: *const c_void,
    n: usize,
    destlen: usize,
) -> *mut c_void {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `memcpy`'s, and the copy fits.
    unsafe { memcpy(dest, src, n) }
}

/// `memmove`, refusing a copy larger than the destination.
///
/// # Safety
///
/// As `memmove`: both pointers valid for `n` bytes, overlapping allowed.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __memmove_chk(
    dest: *mut c_void,
    src: *const c_void,
    n: usize,
    destlen: usize,
) -> *mut c_void {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `memmove`'s, and the copy fits.
    unsafe { memmove(dest, src, n) }
}

/// `memset`, refusing a fill larger than the destination.
///
/// # Safety
///
/// As `memset`: `s` valid for writes of `n` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __memset_chk(
    s: *mut c_void,
    c: c_int,
    n: usize,
    destlen: usize,
) -> *mut c_void {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `memset`'s, and the fill fits.
    unsafe { memset(s, c, n) }
}

/// `mempcpy`, refusing a copy larger than the destination.
///
/// # Safety
///
/// As `mempcpy`: `dest` valid for writes of `n` bytes, `src` for reads of
/// `n`, and the two not overlapping.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __mempcpy_chk(
    dest: *mut c_void,
    src: *const c_void,
    n: usize,
    destlen: usize,
) -> *mut c_void {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `mempcpy`'s, and the copy fits.
    unsafe { mempcpy(dest, src, n) }
}

/// `strcpy`, refusing a string longer than the destination.
///
/// # Safety
///
/// As `strcpy`: `src` a NUL-terminated string and `dest` valid for writes of
/// its length and the NUL.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __strcpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    destlen: usize,
) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated `src`.
    if unsafe { strlen(src) } >= destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `strcpy`'s, and the copy fits.
    unsafe { strcpy(dest, src) }
}

/// `strcat`, refusing a result longer than the destination.
///
/// # Safety
///
/// As `strcat`: both NUL-terminated, and `dest` valid for writes of the
/// joined string and its NUL.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __strcat_chk(
    dest: *mut c_char,
    src: *const c_char,
    destlen: usize,
) -> *mut c_char {
    // SAFETY: the caller passes two NUL-terminated strings.
    let needed = unsafe { strlen(dest) }.saturating_add(unsafe { strlen(src) });
    if needed >= destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `strcat`'s, and the result fits.
    unsafe { strcat(dest, src) }
}

/// `stpcpy`, refusing a string longer than the destination.
///
/// # Safety
///
/// As `stpcpy`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __stpcpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    destlen: usize,
) -> *mut c_char {
    // SAFETY: the caller passes a NUL-terminated `src`.
    if unsafe { strlen(src) } >= destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `stpcpy`'s, and the copy fits.
    unsafe { stpcpy(dest, src) }
}

/// `strncpy`, refusing a count larger than the destination: `strncpy` writes
/// all `n` bytes, padding with NULs.
///
/// # Safety
///
/// As `strncpy`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __strncpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    n: usize,
    destlen: usize,
) -> *mut c_char {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `strncpy`'s, and the copy fits.
    unsafe { strncpy(dest, src, n) }
}

/// `strncat`, refusing a result longer than the destination: the string
/// already there, at most `n` bytes of `src`, and the NUL.
///
/// # Safety
///
/// As `strncat`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __strncat_chk(
    dest: *mut c_char,
    src: *const c_char,
    n: usize,
    destlen: usize,
) -> *mut c_char {
    // SAFETY: `dest` is NUL-terminated, and `src` is read no further than
    // its NUL or `n` bytes.
    let needed = unsafe { strlen(dest) }.saturating_add(unsafe { strnlen(src, n) });
    if needed >= destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `strncat`'s, and the result fits.
    unsafe { strncat(dest, src, n) }
}

/// `strlcpy`, refusing a size larger than the destination: `strlcpy` writes
/// at most `n` bytes, so `n` is what must fit.
///
/// # Safety
///
/// As `strlcpy`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __strlcpy_chk(
    dest: *mut c_char,
    src: *const c_char,
    n: usize,
    destlen: usize,
) -> usize {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `strlcpy`'s, and the copy fits.
    unsafe { strlcpy(dest, src, n) }
}

/// `strlcat`, refusing a size larger than the destination, as
/// [`__strlcpy_chk`] does.
///
/// # Safety
///
/// As `strlcat`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __strlcat_chk(
    dest: *mut c_char,
    src: *const c_char,
    n: usize,
    destlen: usize,
) -> usize {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `strlcat`'s, and the result fits.
    unsafe { strlcat(dest, src, n) }
}

/// `explicit_bzero`, refusing a length larger than the object.
///
/// # Safety
///
/// As `explicit_bzero`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __explicit_bzero_chk(s: *mut c_void, n: usize, destlen: usize) {
    if n > destlen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `explicit_bzero`'s, and it fits.
    unsafe { explicit_bzero(s, n) }
}

/// The word of an `fd_set` that holds descriptor `fd`, for glibc's `FD_SET`,
/// `FD_CLR` and `FD_ISSET`, stopping the program for a descriptor the set
/// cannot hold.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __fdelt_chk(fd: c_long) -> c_long {
    if !(0..FD_SETSIZE).contains(&fd) {
        fortify_fail(b"*** bit out of range 0 - FD_SETSIZE on fd_set ***: terminated\n");
    }
    fd / NFDBITS
}

/// `getcwd` into a buffer of `buflen` bytes.
///
/// # Safety
///
/// As `getcwd`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __getcwd_chk(buf: *mut c_char, size: usize, buflen: usize) -> *mut c_char {
    if size > buflen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `getcwd`'s, and the size fits.
    unsafe { getcwd(buf, size) }
}

/// `getgroups` into an array of `listlen` bytes.
///
/// # Safety
///
/// As `getgroups`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __getgroups_chk(size: c_int, list: *mut c_uint, listlen: usize) -> c_int {
    // A negative size is `getgroups`'s own EINVAL.
    if let Ok(count) = usize::try_from(size)
        && count.saturating_mul(size_of::<c_uint>()) > listlen
    {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `getgroups`'s, and the array fits.
    unsafe { getgroups(size, list) }
}

/// `poll` on an array of `fdslen` bytes.
///
/// # Safety
///
/// As `poll`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __poll_chk(
    fds: *mut Pollfd,
    count: c_ulong,
    timeout: c_int,
    fdslen: usize,
) -> c_int {
    if fdslen / size_of::<Pollfd>() < count as usize {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `poll`'s, and the array holds `count`.
    unsafe { poll(fds, count, timeout) }
}

/// `read` into a buffer of `buflen` bytes.
///
/// # Safety
///
/// As `read`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __read_chk(
    fd: c_int,
    buf: *mut c_void,
    count: usize,
    buflen: usize,
) -> isize {
    if count > buflen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `read`'s, and the read fits.
    unsafe { read(fd, buf, count) }
}

/// `readlinkat` into a buffer of `buflen` bytes.
///
/// # Safety
///
/// As `readlinkat`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __readlinkat_chk(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut c_char,
    size: usize,
    buflen: usize,
) -> isize {
    if size > buflen {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `readlinkat`'s, and the size fits.
    unsafe { readlinkat(dirfd, path, buf, size) }
}

/// `realpath` into a buffer of `resolvedlen` bytes, which must hold
/// `PATH_MAX`, the most `realpath` may write. A null `resolved` comes with
/// `(size_t)-1`.
///
/// # Safety
///
/// As `realpath`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __realpath_chk(
    path: *const c_char,
    resolved: *mut c_char,
    resolvedlen: usize,
) -> *mut c_char {
    if resolvedlen < PATH_MAX {
        __chk_fail();
    }
    // SAFETY: the caller's contract is `realpath`'s, and the buffer holds
    // `PATH_MAX`.
    unsafe { realpath(path, resolved) }
}
