//! The system calls streams make, each returning the value or the error
//! number. The constants and the terminal structure come from the kernel's
//! UAPI headers, as each says.

use core::ffi::{c_char, c_int};
use core::mem::size_of;

use crate::errno;
use crate::fcntl::O_DIRECTORY;
use crate::syscall::{self, nr};

/// `O_RDONLY`, from `asm-generic/fcntl.h`.
pub(crate) const O_RDONLY: c_int = 0o0;
/// `O_WRONLY`, from `asm-generic/fcntl.h`.
pub(crate) const O_WRONLY: c_int = 0o1;
/// `O_RDWR`, from `asm-generic/fcntl.h`.
pub(crate) const O_RDWR: c_int = 0o2;
/// `O_ACCMODE`, from `asm-generic/fcntl.h`.
pub(crate) const O_ACCMODE: c_int = 0o3;
/// `O_CREAT`, from `asm-generic/fcntl.h`.
pub(crate) const O_CREAT: c_int = 0o100;
/// `O_EXCL`, from `asm-generic/fcntl.h`.
pub(crate) const O_EXCL: c_int = 0o200;
/// `O_TRUNC`, from `asm-generic/fcntl.h`.
pub(crate) const O_TRUNC: c_int = 0o1000;
/// `O_APPEND`, from `asm-generic/fcntl.h`.
pub(crate) const O_APPEND: c_int = 0o2000;
/// `O_CLOEXEC`, from `asm-generic/fcntl.h`.
pub(crate) const O_CLOEXEC: c_int = 0o2000000;
/// `O_TMPFILE`: `__O_TMPFILE | O_DIRECTORY`, from `asm-generic/fcntl.h`.
pub(crate) const O_TMPFILE: c_int = 0o20000000 | O_DIRECTORY;

/// `F_SETFD`, from `asm-generic/fcntl.h`.
pub(crate) const F_SETFD: c_int = 2;
/// `F_GETFL`, from `asm-generic/fcntl.h`.
pub(crate) const F_GETFL: c_int = 3;
/// `F_SETFL`, from `asm-generic/fcntl.h`.
pub(crate) const F_SETFL: c_int = 4;
/// `FD_CLOEXEC`, from `asm-generic/fcntl.h`.
pub(crate) const FD_CLOEXEC: c_int = 1;

/// `AT_FDCWD`, from `linux/fcntl.h`.
pub(crate) const AT_FDCWD: c_int = -100;
/// `AT_REMOVEDIR`, from `linux/fcntl.h`.
pub(crate) const AT_REMOVEDIR: c_int = 0x200;

/// `TCGETS`, from `asm-generic/ioctls.h`.
const TCGETS: usize = 0x5401;

/// The kernel's `struct termios`, from `asm-generic/termbits.h`, which
/// `TCGETS` fills. Only whether the call succeeds is used.
#[repr(C)]
#[derive(Debug, Default)]
struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    /// `NCCS` is 19.
    c_cc: [u8; 19],
}

const _: () = assert!(size_of::<Termios>() == 36);

/// A system call's result as the value or the error number.
fn result(ret: isize) -> Result<usize, c_int> {
    errno::decode(ret)
}

/// Opens `path` relative to the working directory.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
pub(crate) unsafe fn open(path: *const c_char, flags: c_int, mode: c_int) -> Result<c_int, c_int> {
    // SAFETY: the caller passes a NUL-terminated path, which the kernel only
    // reads. The descriptor and flags sign-extend, and the kernel reads their
    // low 32 bits.
    let ret = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD as usize,
            path.addr(),
            flags as usize,
            mode as usize,
        )
    };
    result(ret).map(|fd| fd as c_int)
}

/// Closes `fd`.
pub(crate) fn close(fd: c_int) -> Result<(), c_int> {
    // SAFETY: `close` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::CLOSE, fd as usize, 0) };
    result(ret).map(|_| ())
}

/// Reads up to `len` bytes from `fd` into `buf`.
///
/// # Safety
///
/// `buf` must be valid for writes of `len` bytes.
pub(crate) unsafe fn read(fd: c_int, buf: *mut u8, len: usize) -> Result<usize, c_int> {
    // SAFETY: the caller vouches for the buffer.
    let ret = unsafe { syscall::syscall3(nr::READ, fd as usize, buf.addr(), len) };
    result(ret)
}

/// Writes up to `len` bytes from `buf` to `fd`.
///
/// # Safety
///
/// `buf` must be valid for reads of `len` bytes.
pub(crate) unsafe fn write(fd: c_int, buf: *const u8, len: usize) -> Result<usize, c_int> {
    // SAFETY: the caller vouches for the buffer, which the kernel only reads.
    let ret = unsafe { syscall::syscall3(nr::WRITE, fd as usize, buf.addr(), len) };
    result(ret)
}

/// Moves `fd`'s offset, and returns the new one.
pub(crate) fn lseek(fd: c_int, offset: i64, whence: c_int) -> Result<i64, c_int> {
    match syscall::lseek(fd as usize, offset, whence as usize) {
        // The range is an error number, which fits a `c_int`.
        error @ -4095..0 => Err(-error as c_int),
        offset => Ok(offset),
    }
}

/// `fcntl` with an integer argument.
pub(crate) fn fcntl(fd: c_int, command: c_int, arg: c_int) -> Result<c_int, c_int> {
    // SAFETY: the commands used here take an integer and read no memory.
    let ret = unsafe { syscall::syscall3(nr::FCNTL, fd as usize, command as usize, arg as usize) };
    result(ret).map(|value| value as c_int)
}

/// Whether `fd` is a terminal.
pub(crate) fn is_terminal(fd: c_int) -> bool {
    let mut termios = Termios::default();
    // SAFETY: `TCGETS` writes a `struct termios`, and `termios` is one.
    let ret =
        unsafe { syscall::syscall3(nr::IOCTL, fd as usize, TCGETS, (&raw mut termios).addr()) };
    result(ret).is_ok()
}

/// Makes `new` a copy of `old`.
pub(crate) fn dup3(old: c_int, new: c_int, flags: c_int) -> Result<(), c_int> {
    // SAFETY: `dup3` reads no memory.
    let ret = unsafe { syscall::syscall3(nr::DUP3, old as usize, new as usize, flags as usize) };
    result(ret).map(|_| ())
}

/// Removes the name `path`, a directory if `flags` has `AT_REMOVEDIR`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
pub(crate) unsafe fn unlink(path: *const c_char, flags: c_int) -> Result<(), c_int> {
    // SAFETY: the caller passes a NUL-terminated path, which the kernel only
    // reads.
    let ret =
        unsafe { syscall::syscall3(nr::UNLINKAT, AT_FDCWD as usize, path.addr(), flags as usize) };
    result(ret).map(|_| ())
}

/// Fills `buf` with random bytes, or fails.
pub(crate) fn random(buf: &mut [u8]) -> Result<(), c_int> {
    // SAFETY: `buf` is a live slice for the kernel to write.
    let ret = unsafe { syscall::syscall3(nr::GETRANDOM, buf.as_mut_ptr().addr(), buf.len(), 0) };
    match result(ret) {
        Ok(n) if n == buf.len() => Ok(()),
        Ok(_) => Err(errno::EAGAIN),
        Err(error) => Err(error),
    }
}
