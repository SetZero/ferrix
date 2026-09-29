//! `stdlib.h`'s pseudo-terminal calls: `posix_openpt`, `grantpt`, `unlockpt`,
//! `ptsname` and `ptsname_r`.
//!
//! As musl 1.2.5's `misc/pty.c` and `misc/ptsname.c` (MIT; see
//! [`crate::math`] for the notice), with one difference: `grantpt` asks the
//! kernel whether `fd` is a master, as glibc's does and POSIX's `EINVAL`
//! asks, where musl's answers 0 for any descriptor. There is nothing else for
//! it to do on Linux, where devpts gives each slave its owner and mode as it
//! makes it.
//!
//! `/dev/ptmx` hands out a master; `TIOCGPTN` says which slave is its, which
//! is `/dev/pts/<number>`, and `TIOCSPTLCK` with a zero lets that slave be
//! opened.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};
use core::ptr::null_mut;

use crate::errno;
use crate::fcntl::open;
use crate::syscall::{self, nr};

/// `TIOCGPTN`, from `include/bits/ioctl.h`: the master's slave number.
const TIOCGPTN: usize = 0x8004_5430;
/// `TIOCSPTLCK`, from `include/bits/ioctl.h`: lock or unlock the slave.
const TIOCSPTLCK: usize = 0x4004_5431;

/// Where the slaves are, before the number.
const PTS: &[u8] = b"/dev/pts/";

/// Bytes `ptsname` keeps: `/dev/pts/`, the longest `int` and the NUL, as
/// musl's buffer holds.
const NAME_BYTES: usize = PTS.len() + 3 * size_of::<c_int>() + 1;

/// Opens a new master with `flags`, `O_RDWR` and `O_NOCTTY` among them as
/// POSIX asks callers to pass, and returns its descriptor. A kernel out of
/// slaves answers `ENOSPC`, which is `EAGAIN` here, as POSIX names it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn posix_openpt(flags: c_int) -> c_int {
    // SAFETY: the path is a NUL-terminated literal.
    let fd = unsafe { open(c"/dev/ptmx".as_ptr(), flags, 0) };
    if fd < 0 && errno::get() == errno::ENOSPC {
        errno::set(errno::EAGAIN);
    }
    fd
}

/// Asks the kernel for master `fd`'s slave number.
fn slave_number(fd: c_int) -> Result<c_int, c_int> {
    let mut number: c_int = 0;
    // SAFETY: `TIOCGPTN` writes one `int`, the live local.
    let ret =
        unsafe { syscall::syscall3(nr::IOCTL, fd as usize, TIOCGPTN, (&raw mut number).addr()) };
    errno::decode(ret).map(|_| number)
}

/// Gives the slave of master `fd` to the caller. On Linux it already is:
/// this only checks that `fd` is a master, `EINVAL` when it is some other
/// file.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn grantpt(fd: c_int) -> c_int {
    match slave_number(fd) {
        Ok(_) => 0,
        Err(error) => {
            errno::set(if error == errno::ENOTTY {
                errno::EINVAL
            } else {
                error
            });
            -1
        }
    }
}

/// Lets the slave of master `fd` be opened.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn unlockpt(fd: c_int) -> c_int {
    let unlock: c_int = 0;
    // SAFETY: `TIOCSPTLCK` reads one `int`, the live local.
    let ret = unsafe {
        syscall::syscall3(
            nr::IOCTL,
            fd as usize,
            TIOCSPTLCK,
            (&raw const unlock).addr(),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Writes `/dev/pts/<number>` and a NUL into `buf`, returning `ERANGE` if it
/// does not fit.
fn write_name(number: c_int, buf: &mut [u8]) -> c_int {
    // The digits from the right, as many as the number has.
    let mut digits = [0u8; 10];
    let mut left = number.unsigned_abs();
    let mut used = 0;
    for slot in digits.iter_mut().rev() {
        *slot = b'0' + (left % 10) as u8;
        used += 1;
        left /= 10;
        if left == 0 {
            break;
        }
    }
    let digits = digits
        .get(digits.len().saturating_sub(used)..)
        .unwrap_or_default();
    let name = PTS.iter().chain(digits).chain(&[0]);
    if buf.len() < PTS.len() + digits.len() + 1 {
        return errno::ERANGE;
    }
    for (out, byte) in buf.iter_mut().zip(name) {
        *out = *byte;
    }
    0
}

/// Writes the name of master `fd`'s slave into the `len` bytes at `buf`.
/// Returns 0, or the error number: `ERANGE` when the name does not fit,
/// `ENOTTY` when `fd` is not a master.
///
/// # Safety
///
/// `buf` must be valid for writes of `len` bytes, or null.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ptsname_r(fd: c_int, buf: *mut c_char, len: usize) -> c_int {
    let number = match slave_number(fd) {
        Ok(number) => number,
        Err(error) => return error,
    };
    if buf.is_null() {
        return errno::ERANGE;
    }
    // SAFETY: the caller vouches for `len` writable bytes at `buf`.
    let buf = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
    write_name(number, buf)
}

/// The buffer `ptsname` returns.
#[derive(Debug)]
struct NameBuffer(UnsafeCell<[u8; NAME_BYTES]>);

// SAFETY: POSIX documents `ptsname`'s result as static storage the next call
// overwrites, and the function as not thread-safe, as musl's is. Only
// `ptsname` writes it.
unsafe impl Sync for NameBuffer {}

/// What `ptsname` returns.
static NAME: NameBuffer = NameBuffer(UnsafeCell::new([0; NAME_BYTES]));

/// The name of master `fd`'s slave, in static storage the next call
/// overwrites, or null with `errno` set.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ptsname(fd: c_int) -> *mut c_char {
    let buffer = NAME.0.get().cast::<c_char>();
    // SAFETY: see `NameBuffer`; the buffer holds `NAME_BYTES` bytes.
    let result = unsafe { ptsname_r(fd, buffer, NAME_BYTES) };
    if result != 0 {
        errno::set(result);
        return null_mut();
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slave_is_named_under_dev_pts_and_a_short_buffer_is_erange() {
        let mut buf = [0xffu8; NAME_BYTES];
        assert_eq!(write_name(7, &mut buf), 0);
        assert_eq!(&buf[..12], b"/dev/pts/7\0\xff");
        assert_eq!(write_name(c_int::MAX, &mut buf), 0);
        assert_eq!(&buf[..20], b"/dev/pts/2147483647\0");
        // `/dev/pts/12` and its NUL are twelve bytes.
        let mut short = [0u8; 11];
        assert_eq!(write_name(12, &mut short), errno::ERANGE);
        let mut exact = [0u8; 12];
        assert_eq!(write_name(12, &mut exact), 0);
        assert_eq!(&exact, b"/dev/pts/12\0");
    }
}
