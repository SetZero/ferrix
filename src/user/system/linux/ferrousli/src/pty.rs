//! `stdlib.h`'s pseudo-terminal calls: `posix_openpt`, `grantpt`, `unlockpt`,
//! `ptsname` and `ptsname_r`; and the BSD ones glibc and musl keep in
//! `pty.h` and `utmp.h`, `openpty`, `forkpty` and `login_tty`, as musl's
//! `misc/openpty.c`, `misc/forkpty.c` and `misc/login_tty.c`.
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
use core::ffi::{c_char, c_int, c_void};
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

/// `O_RDWR | O_NOCTTY`, from `asm-generic/fcntl.h`.
const O_RDWR_NOCTTY: c_int = 0o2 | 0o400;
/// `O_CLOEXEC`, from `asm-generic/fcntl.h`.
const O_CLOEXEC: c_int = 0o2_000_000;
/// `TIOCSCTTY`, from `include/bits/ioctl.h`: make a terminal the caller's
/// controlling terminal.
const TIOCSCTTY: usize = 0x540e;
/// `TCSANOW`, from `include/termios.h`.
const TCSANOW: c_int = 0;
/// Bytes of the slave's name `openpty` writes: `/dev/pts/`, the number and
/// the NUL, as musl's buffer holds.
const OPENPTY_NAME: usize = 20;

/// Runs `f` with cancellation disabled, as musl's `openpty` and `forkpty`
/// do: neither may leave a descriptor open because it was cancelled.
fn uncancellable<T>(f: impl FnOnce() -> T) -> T {
    let mut old: c_int = 0;
    // SAFETY: `old` is a live local.
    let _ = unsafe {
        crate::cancel::pthread_setcancelstate(c_int::from(crate::cancel::DISABLE), &raw mut old)
    };
    let result = f();
    // SAFETY: restoring the state just read; no old state is asked for.
    let _ = unsafe { crate::cancel::pthread_setcancelstate(old, null_mut()) };
    result
}

/// Opens a new pseudo-terminal: its master into `*master` and its slave into
/// `*slave`, both without making either the controlling terminal; the
/// slave's name into `name` when it is not null; the slave's attributes from
/// `*tio` and window size from `*ws` when they are not null. Returns 0, or -1
/// with `errno` set, having closed what it opened.
///
/// As musl 1.2.5's `misc/openpty.c`.
///
/// # Safety
///
/// `master` and `slave` must be valid for writes of an `int`; `name` null or
/// valid for writes of 20 bytes, as glibc's and musl's callers give; `tio`
/// null or a `struct termios`; `ws` null or a `struct winsize`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn openpty(
    master: *mut c_int,
    slave: *mut c_int,
    name: *mut c_char,
    tio: *const crate::termios::Termios,
    ws: *const c_void,
) -> c_int {
    // SAFETY: the path is a NUL-terminated literal.
    let m = unsafe { open(c"/dev/ptmx".as_ptr(), O_RDWR_NOCTTY, 0) };
    if m < 0 {
        return -1;
    }
    uncancellable(|| {
        let mut path = [0u8; OPENPTY_NAME];
        let opened = if unlockpt(m) != 0 {
            None
        } else {
            match slave_number(m) {
                Ok(number) if write_name(number, &mut path) == 0 => {
                    // SAFETY: `write_name` left a NUL-terminated path.
                    let s = unsafe { open(path.as_ptr().cast(), O_RDWR_NOCTTY, 0) };
                    (s >= 0).then_some(s)
                }
                Ok(_) => None,
                Err(error) => {
                    errno::set(error);
                    None
                }
            }
        };
        let Some(s) = opened else {
            let _ = crate::unistd::close(m);
            return -1;
        };
        if !name.is_null() {
            let len = path
                .iter()
                .position(|&byte| byte == 0)
                .map_or(0, |end| end + 1);
            // SAFETY: the caller vouches for 20 bytes at `name`, and the path
            // with its NUL is no longer.
            unsafe { core::ptr::copy_nonoverlapping(path.as_ptr(), name.cast(), len) };
        }
        if !tio.is_null() {
            // SAFETY: the caller vouches for `*tio`.
            let _ = unsafe { crate::termios::tcsetattr(s, TCSANOW, tio) };
        }
        if !ws.is_null() {
            // SAFETY: the caller vouches for `*ws`.
            let _ = unsafe { crate::termios::tcsetwinsize(s, ws) };
        }
        // SAFETY: the caller vouches for `*master`.
        unsafe { master.write(m) };
        // SAFETY: and for `*slave`.
        unsafe { slave.write(s) };
        0
    })
}

/// Makes terminal `fd` the calling process's controlling terminal in a new
/// session, and its standard input, output and error. Returns 0, or -1 with
/// `errno` set.
///
/// As musl 1.2.5's `misc/login_tty.c`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn login_tty(fd: c_int) -> c_int {
    let _ = crate::process::setsid();
    // SAFETY: `TIOCSCTTY` takes its argument by value.
    let ret = unsafe { syscall::syscall3(nr::IOCTL, fd as usize, TIOCSCTTY, 0) };
    if errno::from_syscall(ret) != 0 {
        return -1;
    }
    for standard in 0..3 {
        let _ = crate::unistd::dup2(fd, standard);
    }
    if fd > 2 {
        let _ = crate::unistd::close(fd);
    }
    0
}

/// Forks with a new pseudo-terminal, as [`openpty`] opens one, as the
/// child's controlling terminal and standard streams. The parent gets the
/// child's id and the master in `*master`; the child gets 0. Returns -1 with
/// `errno` set, and no child left, when the terminal cannot be opened, the
/// fork fails, or the child cannot take the terminal.
///
/// As musl 1.2.5's `misc/forkpty.c`: the child reports a failure of
/// [`login_tty`] through a pipe, and the parent reaps it.
///
/// # Safety
///
/// As [`openpty`], with `master` in place of both descriptors.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn forkpty(
    master: *mut c_int,
    name: *mut c_char,
    tio: *const crate::termios::Termios,
    ws: *const c_void,
) -> c_int {
    let (mut m, mut s) = (-1, -1);
    // SAFETY: both descriptors are live locals; the caller vouches for the
    // rest.
    if unsafe { openpty(&raw mut m, &raw mut s, name, tio, ws) } < 0 {
        return -1;
    }
    let mask = crate::pthread::block_app_signals();
    let pid = uncancellable(|| {
        let mut pipe: [c_int; 2] = [-1; 2];
        // SAFETY: `pipe` is two live `int`s.
        if unsafe { crate::unistd::pipe2(pipe.as_mut_ptr(), O_CLOEXEC) } != 0 {
            let _ = crate::unistd::close(s);
            return -1;
        }
        let [from_child, to_parent] = pipe;
        let pid = crate::process::fork();
        if pid == 0 {
            let _ = crate::unistd::close(m);
            let _ = crate::unistd::close(from_child);
            if login_tty(s) != 0 {
                let error = errno::get();
                // SAFETY: the pipe's write end, and one live `int`.
                let _ = unsafe {
                    crate::unistd::write(to_parent, (&raw const error).cast(), size_of::<c_int>())
                };
                crate::unistd::_exit(127);
            }
            let _ = crate::unistd::close(to_parent);
            return 0;
        }
        let _ = crate::unistd::close(s);
        let _ = crate::unistd::close(to_parent);
        let mut error: c_int = 0;
        let mut pid = pid;
        // SAFETY: the pipe's read end, and one live `int`.
        let got =
            unsafe { crate::unistd::read(from_child, (&raw mut error).cast(), size_of::<c_int>()) };
        if pid > 0 && got > 0 {
            let mut status: c_int = 0;
            // SAFETY: `status` is a live local.
            let _ = unsafe { crate::wait::waitpid(pid, &raw mut status, 0) };
            pid = -1;
            errno::set(error);
        }
        let _ = crate::unistd::close(from_child);
        pid
    });
    if pid > 0 {
        // SAFETY: the caller vouches for `*master`.
        unsafe { master.write(m) };
    } else if pid < 0 {
        let _ = crate::unistd::close(m);
    }
    crate::pthread::restore_signals(mask);
    pid
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
