//! `fcntl.h`: opening files, and controlling open descriptors.
//!
//! # Variadic functions
//!
//! C declares `open`, `openat`, `fcntl`, `ioctl`, `mremap` and `syscall` with
//! a trailing `...`, and stable Rust cannot define a variadic function. They
//! are defined here and in [`crate::ioctl`], [`crate::mman`] and
//! [`crate::unistd`] with fixed parameters in the variadic slots instead.
//!
//! That is sound on x86-64, and on AArch64 Linux, because an integer or
//! pointer variadic argument travels in the register or stack slot a fixed
//! one in the same position would. A caller that passes fewer arguments than
//! the definition names leaves unrelated values in the remaining registers,
//! so each of these functions reads a variadic parameter only when the fixed
//! ones say the caller passed it, as `open` does with `mode`.
//!
//! Functions whose argument list has no bound, such as `execl`, cannot be
//! written this way, and are not here yet.
//!
//! The `*64` names are glibc's large-file interface. With a 64-bit `off_t`
//! each is the plain function, and they are exported for programs built
//! against glibc.

use core::ffi::{c_char, c_int, c_short, c_uint, c_ulong};
use core::mem::{offset_of, size_of};

use crate::errno;
use crate::syscall::{self, nr};

/// `O_WRONLY`, from `asm-generic/fcntl.h`.
pub const O_WRONLY: c_int = 0o1;
/// `O_CREAT`, from `asm-generic/fcntl.h`.
pub const O_CREAT: c_int = 0o100;
/// `O_TRUNC`, from `asm-generic/fcntl.h`.
pub const O_TRUNC: c_int = 0o1000;
/// `O_DIRECTORY`, from `asm-generic/fcntl.h`.
pub const O_DIRECTORY: c_int = 0o200_000;
/// `O_TMPFILE`, from `asm-generic/fcntl.h`: `__O_TMPFILE | O_DIRECTORY`.
pub const O_TMPFILE: c_int = 0o20_000_000 | O_DIRECTORY;

/// `AT_FDCWD`, from `linux/fcntl.h`: resolve relative paths from the working
/// directory.
pub const AT_FDCWD: c_int = -100;
/// `AT_SYMLINK_NOFOLLOW`, from `linux/fcntl.h`.
pub const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
/// `AT_REMOVEDIR`, from `linux/fcntl.h`.
pub const AT_REMOVEDIR: c_int = 0x200;

/// `F_GETFD`, from `asm-generic/fcntl.h`.
pub const F_GETFD: c_int = 1;
/// `F_GETOWN`, from `asm-generic/fcntl.h`.
const F_GETOWN: c_int = 9;
/// `F_GETOWN_EX`, from `asm-generic/fcntl.h`.
const F_GETOWN_EX: c_int = 16;
/// `F_OWNER_PGRP`, from `asm-generic/fcntl.h`.
const F_OWNER_PGRP: c_int = 2;

/// C's `struct flock`, which `F_GETLK`, `F_SETLK` and `F_SETLKW` take through
/// `fcntl`'s third argument. The kernel's `struct flock` in
/// `asm-generic/fcntl.h` has the same fields and, on x86-64, no
/// `__ARCH_FLOCK_EXTRA_SYSID`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Flock {
    /// `F_RDLCK`, `F_WRLCK` or `F_UNLCK`.
    pub l_type: c_short,
    /// Where `l_start` counts from: `SEEK_SET`, `SEEK_CUR` or `SEEK_END`.
    pub l_whence: c_short,
    /// The first byte of the range.
    pub l_start: i64,
    /// The length of the range; zero means to the end of the file.
    pub l_len: i64,
    /// The process holding a conflicting lock, from `F_GETLK`.
    pub l_pid: c_int,
}

const _: () = assert!(size_of::<Flock>() == 32);
const _: () = assert!(offset_of!(Flock, l_start) == 8);
const _: () = assert!(offset_of!(Flock, l_len) == 16);
const _: () = assert!(offset_of!(Flock, l_pid) == 24);

/// The kernel's `struct f_owner_ex`, from `asm-generic/fcntl.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FOwnerEx {
    kind: c_int,
    pid: c_int,
}

const _: () = assert!(size_of::<FOwnerEx>() == 8);

/// Opens `path`, relative to the directory `dirfd` refers to. `mode` is read
/// only when `flags` creates a file.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn openat(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: c_uint,
) -> c_int {
    // A caller that creates nothing need not pass a mode, and the register
    // then holds whatever it last held.
    let mode = if flags & O_CREAT != 0 || flags & O_TMPFILE == O_TMPFILE {
        mode
    } else {
        0
    };
    // SAFETY: the kernel only reads `path`, which the caller vouches for.
    // Casting `dirfd` and `flags` sign-extends, and the kernel reads the low
    // 32 bits back.
    let ret = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            dirfd as usize,
            path.addr(),
            flags as usize,
            mode as usize,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Opens `path`. `mode` is read only when `flags` creates a file.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `openat`'s.
    unsafe { openat(AT_FDCWD, path, flags, mode) }
}

/// Creates `path`, or truncates it, and opens it for writing.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn creat(path: *const c_char, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `open`'s.
    unsafe { open(path, O_CREAT | O_WRONLY | O_TRUNC, mode) }
}

/// Performs `cmd` on `fd`. The third argument is passed to the kernel as it
/// came, for every command.
///
/// `F_GETOWN` is the exception in how the result is read: a process group
/// comes back from the kernel as a negative number, which a small group id
/// makes look like an error. It is asked as `F_GETOWN_EX` instead, as musl
/// does.
///
/// # Safety
///
/// For a command that takes a pointer, `arg` must be valid for what the
/// command reads and writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fcntl(fd: c_int, cmd: c_int, arg: c_ulong) -> c_int {
    if cmd == F_GETOWN {
        let mut owner = FOwnerEx::default();
        // SAFETY: the kernel writes `owner`, a live local.
        let ret = unsafe {
            syscall::syscall3(
                nr::FCNTL,
                fd as usize,
                F_GETOWN_EX as usize,
                (&raw mut owner).addr(),
            )
        };
        match errno::decode(ret) {
            Ok(_) if owner.kind == F_OWNER_PGRP => return owner.pid.wrapping_neg(),
            Ok(_) => return owner.pid,
            Err(errno::EINVAL) => {}
            Err(error) => {
                errno::set(error);
                return -1;
            }
        }
    }
    // SAFETY: the caller vouches for `arg` for this command.
    let ret = unsafe { syscall::syscall3(nr::FCNTL, fd as usize, cmd as usize, arg as usize) };
    errno::from_syscall(ret) as c_int
}

/// Advises the kernel how `len` bytes of `fd` from `offset` will be used.
/// Returns the error number rather than setting `errno`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn posix_fadvise(fd: c_int, offset: i64, len: i64, advice: c_int) -> c_int {
    // SAFETY: `fadvise64` reads no memory. On x86-64 its arguments are
    // (fd, offset, len, advice); AArch64's `fadvise64_64` takes the same.
    let ret = unsafe {
        syscall::syscall4(
            nr::FADVISE64,
            fd as usize,
            offset as usize,
            len as usize,
            advice as usize,
        )
    };
    match errno::decode(ret) {
        Ok(_) => 0,
        Err(error) => error,
    }
}

/// Makes sure the `len` bytes of `fd` from `offset` have storage. Returns the
/// error number rather than setting `errno`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn posix_fallocate(fd: c_int, offset: i64, len: i64) -> c_int {
    // SAFETY: `fallocate` reads no memory. Mode zero allocates.
    let ret =
        unsafe { syscall::syscall4(nr::FALLOCATE, fd as usize, 0, offset as usize, len as usize) };
    match errno::decode(ret) {
        Ok(_) => 0,
        Err(error) => error,
    }
}

/// glibc's large-file name for [`open`].
///
/// # Safety
///
/// As [`open`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn open64(path: *const c_char, flags: c_int, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `open`'s.
    unsafe { open(path, flags, mode) }
}

/// glibc's large-file name for [`openat`].
///
/// # Safety
///
/// As [`openat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn openat64(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: c_uint,
) -> c_int {
    // SAFETY: the caller's contract is `openat`'s.
    unsafe { openat(dirfd, path, flags, mode) }
}

/// glibc's large-file name for [`creat`].
///
/// # Safety
///
/// As [`creat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn creat64(path: *const c_char, mode: c_uint) -> c_int {
    // SAFETY: the caller's contract is `creat`'s.
    unsafe { creat(path, mode) }
}
