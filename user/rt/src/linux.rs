//! The Linux calls a native program may make, for the few that need them.
//!
//! A native program is not a POSIX one and has no libc, no descriptors of its
//! own and no `std`. But the kernel gives *every* process a descriptor table
//! and a namespace (`Process::with_pid` in `kernel/src/syscall/process.rs`),
//! and `dispatch` picks the ABI by the number's range and by nothing else --
//! so a native program that wants a socket may simply ask for one. This
//! module is that: the handful of calls `user/vport` needs to put a virtio
//! port on a Unix socket, and no more.
//!
//! `docs/CLIPBOARD.md` §5 is why this exists. It is deliberately not a libc:
//! a call is added here when a program in this tree needs it, with the
//! numbers taken from `ferrix_linux_abi::nr`, which pins them against the
//! kernel's own tables rather than remembering them.
//!
//! # Errors
//!
//! Every call returns `Result<usize, Errno>`: the kernel leaves `-errno` in
//! `-4095..=-1`, exactly as Linux does, and [`decode`] separates the two.

use ferrix_linux_abi::errno::Errno;

use crate::arch::{self, nr};

/// `AF_UNIX`, the only family this module has a use for.
pub const AF_UNIX: usize = 1;
/// `SOCK_STREAM`.
pub const SOCK_STREAM: usize = 1;
/// `SOCK_NONBLOCK`, as a flag on `socket` and `accept4`.
pub const SOCK_NONBLOCK: usize = 0o4000;
/// `F_SETFL`.
pub const F_SETFL: usize = 4;
/// `O_NONBLOCK`.
pub const O_NONBLOCK: usize = 0o4000;

/// `CLOCK_MONOTONIC`, the clock a native [`Deadline`] is measured against.
///
/// [`Deadline`]: ferrix_native::handle::Deadline
pub const CLOCK_MONOTONIC: usize = 1;

/// Bytes of a `sockaddr_un`: the family, then the path.
pub const SOCKADDR_UN_BYTES: usize = 110;

/// The longest path a Unix socket address carries, with room for its NUL.
pub const PATH_MAX: usize = SOCKADDR_UN_BYTES - 2 - 1;

/// A `sockaddr_un` for `path`, and its length, or `None` if the path is too
/// long to be one.
///
/// The address is `AF_UNIX` as a little-endian `u16`, then the path, then a
/// NUL -- the layout every Linux architecture shares.
#[must_use]
pub fn sockaddr_un(path: &[u8]) -> Option<([u8; SOCKADDR_UN_BYTES], usize)> {
    if path.is_empty() || path.len() > PATH_MAX {
        return None;
    }
    let mut address = [0_u8; SOCKADDR_UN_BYTES];
    let family = (AF_UNIX as u16).to_le_bytes();
    *address.first_mut()? = family[0];
    *address.get_mut(1)? = family[1];
    address.get_mut(2..2 + path.len())?.copy_from_slice(path);
    // The length Linux wants is the family, the path and its NUL.
    Some((address, 2 + path.len() + 1))
}

/// A result register as the kernel left it.
///
/// # Errors
///
/// The `Errno` a value in `-4095..=-1` names.
pub fn decode(value: usize) -> Result<usize, Errno> {
    // The same window `libs/native`'s decode uses, and Linux's own.
    let signed = value as isize;
    if (-4095..0).contains(&signed) {
        Err(Errno(u16::try_from(-signed).unwrap_or(0)))
    } else {
        Ok(value)
    }
}

/// `socket(family, kind, protocol)`.
///
/// # Errors
///
/// Whatever the kernel answers.
pub fn socket(family: usize, kind: usize, protocol: usize) -> Result<usize, Errno> {
    // SAFETY: no pointer arguments.
    decode(unsafe { arch::linux(nr::SOCKET, [family, kind, protocol, 0, 0, 0]) })
}

/// `bind(fd, address, len)`.
///
/// # Errors
///
/// Whatever the kernel answers.
pub fn bind(fd: usize, address: &[u8], len: usize) -> Result<usize, Errno> {
    let at = address.as_ptr().addr();
    // SAFETY: `address` is borrowed for the call and is at least `len` bytes,
    // which the caller and `sockaddr_un` together guarantee; the kernel only
    // reads it.
    decode(unsafe { arch::linux(nr::BIND, [fd, at, len.min(address.len()), 0, 0, 0]) })
}

/// `listen(fd, backlog)`.
///
/// # Errors
///
/// Whatever the kernel answers.
pub fn listen(fd: usize, backlog: usize) -> Result<usize, Errno> {
    // SAFETY: no pointer arguments.
    decode(unsafe { arch::linux(nr::LISTEN, [fd, backlog, 0, 0, 0, 0]) })
}

/// `accept4(fd, NULL, NULL, flags)`: the peer's address is not asked for,
/// since a Unix socket's is empty.
///
/// # Errors
///
/// Whatever the kernel answers; `EAGAIN` on a non-blocking socket with
/// nobody waiting.
pub fn accept4(fd: usize, flags: usize) -> Result<usize, Errno> {
    // SAFETY: the two address arguments are null, which `accept4` defines as
    // "do not report the peer".
    decode(unsafe { arch::linux(nr::ACCEPT4, [fd, 0, 0, flags, 0, 0]) })
}

/// `read(fd, bytes)`.
///
/// # Errors
///
/// Whatever the kernel answers; `EAGAIN` when nothing is waiting.
pub fn read(fd: usize, bytes: &mut [u8]) -> Result<usize, Errno> {
    let at = bytes.as_mut_ptr().addr();
    let len = bytes.len();
    // SAFETY: `bytes` is borrowed exclusively for the call and is `len`
    // bytes; the kernel writes at most that many.
    decode(unsafe { arch::linux(nr::READ, [fd, at, len, 0, 0, 0]) })
}

/// `write(fd, bytes)`.
///
/// # Errors
///
/// Whatever the kernel answers; `EAGAIN` when the pipe is full.
pub fn write(fd: usize, bytes: &[u8]) -> Result<usize, Errno> {
    let at = bytes.as_ptr().addr();
    // SAFETY: `bytes` is borrowed for the call and is at least its own
    // length; the kernel only reads it.
    decode(unsafe { arch::linux(nr::WRITE, [fd, at, bytes.len(), 0, 0, 0]) })
}

/// `close(fd)`.
///
/// # Errors
///
/// Whatever the kernel answers.
pub fn close(fd: usize) -> Result<usize, Errno> {
    // SAFETY: no pointer arguments.
    decode(unsafe { arch::linux(nr::CLOSE, [fd, 0, 0, 0, 0, 0]) })
}

/// `fcntl(fd, command, argument)`.
///
/// # Errors
///
/// Whatever the kernel answers.
pub fn fcntl(fd: usize, command: usize, argument: usize) -> Result<usize, Errno> {
    // SAFETY: no pointer arguments for the commands this module uses.
    decode(unsafe { arch::linux(nr::FCNTL, [fd, command, argument, 0, 0, 0]) })
}

/// Take `path` out of the filesystem, so that binding it again succeeds.
///
/// Which of `unlink` and `unlinkat` spells this is the architecture's, and is
/// settled behind `crate::arch` so that nothing here has to ask.
///
/// # Errors
///
/// Whatever the kernel answers, `ENOENT` included, which a caller clearing
/// the way for a `bind` should ignore.
pub fn unlink(path: &[u8]) -> Result<usize, Errno> {
    // SAFETY: `path` is borrowed for the call and NUL-terminated by its
    // caller; the kernel only reads it.
    let result = unsafe { arch::unlink(path.as_ptr().addr()) };
    decode(result)
}

/// Nanoseconds on `CLOCK_MONOTONIC`, which is the clock a native
/// `Deadline::At` names.
///
/// This is here so that a driver waiting on a native port can say "for the
/// next ten milliseconds" -- the kernel takes only absolute deadlines, so a
/// relative wait is this plus the interval.
///
/// # Errors
///
/// Whatever the kernel answers. A `timespec` is two words of the
/// architecture's width, so the reading itself is `crate::arch`'s.
pub fn monotonic_nanos() -> Result<u64, Errno> {
    let mut nanos = 0_u64;
    let _read = decode(arch::monotonic_nanos(&mut nanos))?;
    Ok(nanos)
}
