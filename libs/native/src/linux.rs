//! The few Linux calls a native program makes, typed the way the native ones
//! are.
//!
//! # Why a native program may do this at all
//!
//! A Ferrix process is not "a native process" or "a Linux process". Every
//! process gets a native handle table, a POSIX descriptor table *and* a VFS
//! namespace -- `Process::with_pid` builds all three for every process there
//! is -- and the dispatcher takes the native ABI **by number range**, before
//! any Linux table is asked. The two ABIs are two ranges of number, not two
//! kinds of process, and one program may use both.
//!
//! `ferrix-rt` has in fact been relying on this since it was written: a
//! native program's `exit` is Linux's `exit_group`, called with a Linux
//! number through the same instruction as every native call. This module is
//! the same trick made typed and deliberate, and `docs/CLIPBOARD.md` §5 is
//! the argument for why a driver wants it: `user/vport` holds a device
//! through native handles and offers the port it opens as an ordinary
//! `AF_UNIX` socket, so that the agent on the other end is an ordinary `std`
//! program and the port needs no kernel representation.
//!
//! # What is here, and what is not
//!
//! Exactly what that driver needs and nothing else: a stream socket, bound,
//! listening and accepted; reads and writes; a clock to time a wait by. This
//! is not a libc and is not trying to become one. A program wanting more of
//! Linux than this should be a `std` program, which every program under
//! `compositor/` already is.
//!
//! # Errors are `errno`, undecorated
//!
//! [`crate::Error`] names the native failures because `libs/native-abi` fixes
//! which `errno` each one travels as. Linux's have no such mapping -- an
//! `EAGAIN` from `read` means what Linux says it means -- so these return
//! [`Errno`] and the caller matches on that.

use ferrix_linux_abi::errno::{Errno, MAX_ERRNO};
use ferrix_linux_abi::socket::{AF_UNIX, SOCKADDR_UN_SIZE, SUN_PATH_OFFSET, UNIX_PATH_MAX};
use ferrix_linux_abi::types::{AT_FDCWD, CLOCK_MONOTONIC};

use crate::call::{Call, Syscall};

/// The syscall numbers, which are the architecture's and not the kernel's.
mod nr {
    #[cfg(target_arch = "aarch64")]
    pub(super) use ferrix_linux_abi::nr::aarch64::*;
    #[cfg(target_arch = "arm")]
    pub(super) use ferrix_linux_abi::nr::arm::*;
    // The host, where the tests run against a fake `Syscall` that traps
    // nothing, shares x86-64's table.
    #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
    pub(super) use ferrix_linux_abi::nr::x86_64::*;
}

/// Turn a result register into a value or an `errno`.
fn decode(value: usize) -> Result<usize, Errno> {
    let signed = value.cast_signed();
    if signed < 0 && signed >= -(MAX_ERRNO as isize) {
        // Above `-MAX_ERRNO`, so the magnitude fits `u16` and cannot truncate.
        Err(Errno(signed.unsigned_abs() as u16))
    } else {
        Ok(value)
    }
}

/// Turn a result register into a descriptor.
fn decode_fd(value: usize) -> Result<i32, Errno> {
    let raw = decode(value)?;
    i32::try_from(raw).map_err(|_| Errno::EINVAL)
}

/// A `struct sockaddr_un` for `path`, and how many bytes of it matter.
///
/// # Errors
///
/// `ENAMETOOLONG` for a path that does not fit, counting the NUL Linux
/// requires of a filesystem-named socket.
pub fn sockaddr_un(path: &[u8]) -> Result<([u8; SOCKADDR_UN_SIZE], usize), Errno> {
    let mut address = [0_u8; SOCKADDR_UN_SIZE];
    // The NUL is not optional here: a path filling `sun_path` exactly is
    // legal on Linux and is *not* what this driver wants, since the agent
    // opens the same name through the filesystem.
    if path.len() >= UNIX_PATH_MAX {
        return Err(Errno::ENAMETOOLONG);
    }
    let family = AF_UNIX.to_ne_bytes();
    for (slot, byte) in address.iter_mut().zip(family) {
        *slot = byte;
    }
    for (slot, byte) in address
        .iter_mut()
        .skip(SUN_PATH_OFFSET)
        .zip(path.iter().copied())
    {
        *slot = byte;
    }
    Ok((address, SUN_PATH_OFFSET + path.len() + 1))
}

/// An open descriptor, closed when it is dropped.
#[derive(Debug)]
pub struct Fd<S: Syscall> {
    /// The descriptor itself, or a negative number once it has been taken.
    fd: i32,
    /// How to reach the kernel.
    sys: S,
}

impl<S: Syscall> Fd<S> {
    /// Adopt `fd`, which this will close.
    #[must_use]
    pub const fn from_raw(sys: S, fd: i32) -> Self {
        Fd { fd, sys }
    }

    /// The descriptor, still owned here.
    #[must_use]
    pub const fn raw(&self) -> i32 {
        self.fd
    }

    /// `read(2)` into `bytes`: how many arrived.
    ///
    /// # Errors
    ///
    /// Whatever `read` returns, `EAGAIN` included for a non-blocking
    /// descriptor with nothing ready.
    pub fn read(&self, bytes: &mut [u8]) -> Result<usize, Errno> {
        let len = bytes.len();
        decode(
            Call::new(nr::READ)
                .value(self.fd.cast_unsigned() as usize)
                .output(bytes)
                .value(len)
                .make(self.sys),
        )
    }

    /// `write(2)`: how many bytes went, which may be fewer than offered.
    ///
    /// # Errors
    ///
    /// Whatever `write` returns, `EAGAIN` and `EPIPE` included.
    pub fn write(&self, bytes: &[u8]) -> Result<usize, Errno> {
        let len = bytes.len();
        decode(
            Call::new(nr::WRITE)
                .value(self.fd.cast_unsigned() as usize)
                .input(bytes)
                .value(len)
                .make(self.sys),
        )
    }

    /// `listen(2)`.
    ///
    /// # Errors
    ///
    /// Whatever `listen` returns.
    pub fn listen(&self, backlog: i32) -> Result<(), Errno> {
        decode(
            Call::new(nr::LISTEN)
                .value(self.fd.cast_unsigned() as usize)
                .value(backlog.cast_unsigned() as usize)
                .make(self.sys),
        )
        .map(|_| ())
    }

    /// `bind(2)` to the `AF_UNIX` path `path`.
    ///
    /// # Errors
    ///
    /// `ENAMETOOLONG` for a path that does not fit, and whatever `bind`
    /// returns -- `EADDRINUSE` for a name already there.
    pub fn bind_unix(&self, path: &[u8]) -> Result<(), Errno> {
        let (address, len) = sockaddr_un(path)?;
        let bytes = address.get(..len).unwrap_or(&address);
        decode(
            Call::new(nr::BIND)
                .value(self.fd.cast_unsigned() as usize)
                .input(bytes)
                .value(len)
                .make(self.sys),
        )
        .map(|_| ())
    }

    /// `accept4(2)` with no address wanted.
    ///
    /// # Errors
    ///
    /// Whatever `accept4` returns, `EAGAIN` included when nothing is waiting.
    pub fn accept(&self, flags: u32) -> Result<Fd<S>, Errno> {
        let fd = decode_fd(
            Call::new(nr::ACCEPT4)
                .value(self.fd.cast_unsigned() as usize)
                .value(0)
                .value(0)
                .value(flags as usize)
                .make(self.sys),
        )?;
        Ok(Fd::from_raw(self.sys, fd))
    }
}

impl<S: Syscall> Drop for Fd<S> {
    fn drop(&mut self) {
        if self.fd >= 0 {
            let _ = Call::new(nr::CLOSE)
                .value(self.fd.cast_unsigned() as usize)
                .make(self.sys);
        }
    }
}

/// `socket(2)`.
///
/// # Errors
///
/// Whatever `socket` returns: `EAFNOSUPPORT` for a family this kernel does
/// not have, which for anything but `AF_UNIX` it does not.
pub fn socket<S: Syscall>(sys: S, domain: u16, kind: u32, protocol: u32) -> Result<Fd<S>, Errno> {
    let fd = decode_fd(
        Call::new(nr::SOCKET)
            .value(usize::from(domain))
            .value(kind as usize)
            .value(protocol as usize)
            .make(sys),
    )?;
    Ok(Fd::from_raw(sys, fd))
}

/// `unlinkat(AT_FDCWD, path, 0)`, for clearing a socket name left by a run
/// that did not get to remove its own.
///
/// # Errors
///
/// Whatever `unlinkat` returns; `ENOENT` is the ordinary case and not a
/// failure a caller need mind.
pub fn unlink<S: Syscall>(sys: S, path: &[u8]) -> Result<(), Errno> {
    let mut terminated = [0_u8; UNIX_PATH_MAX];
    if path.len() >= UNIX_PATH_MAX {
        return Err(Errno::ENAMETOOLONG);
    }
    for (slot, byte) in terminated.iter_mut().zip(path.iter().copied()) {
        *slot = byte;
    }
    let bytes = terminated.get(..path.len() + 1).unwrap_or(&terminated);
    decode(
        Call::new(nr::UNLINKAT)
            .value(AT_FDCWD.cast_unsigned() as usize)
            .input(bytes)
            .value(0)
            .make(sys),
    )
    .map(|_| ())
}

/// `clock_gettime(CLOCK_MONOTONIC)`, in nanoseconds.
///
/// This is how a native program turns the relative wait it wants into the
/// absolute [`crate::Deadline`] a port wait takes.
///
/// # Errors
///
/// Whatever `clock_gettime` returns.
pub fn monotonic_nanos<S: Syscall>(sys: S) -> Result<u64, Errno> {
    // `struct timespec`: two native-width words, seconds then nanoseconds.
    let mut timespec = [0_u8; 2 * size_of::<usize>()];
    let _ = decode(
        Call::new(nr::CLOCK_GETTIME)
            .value(CLOCK_MONOTONIC as usize)
            .output(&mut timespec)
            .make(sys),
    )?;
    let width = size_of::<usize>();
    let mut word = [0_u8; 8];
    for (slot, byte) in word.iter_mut().zip(timespec.iter().copied().take(width)) {
        *slot = byte;
    }
    let seconds = u64::from_ne_bytes(word);
    let mut word = [0_u8; 8];
    for (slot, byte) in word
        .iter_mut()
        .zip(timespec.iter().copied().skip(width).take(width))
    {
        *slot = byte;
    }
    let nanos = u64::from_ne_bytes(word);
    Ok(seconds.saturating_mul(1_000_000_000).saturating_add(nanos))
}
