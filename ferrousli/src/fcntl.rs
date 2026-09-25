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
#[cfg(target_arch = "x86_64")]
pub const O_DIRECTORY: c_int = 0o200_000;
/// `O_DIRECTORY`, from the architecture's `asm/fcntl.h`: AArch64 and ARM move
/// it, `O_NOFOLLOW`, `O_DIRECT` and `O_LARGEFILE` from the generic values.
#[cfg(not(target_arch = "x86_64"))]
pub const O_DIRECTORY: c_int = 0o40_000;
/// `O_NOFOLLOW`, from `asm-generic/fcntl.h`.
#[cfg(target_arch = "x86_64")]
pub const O_NOFOLLOW: c_int = 0o400_000;
/// `O_NOFOLLOW`, from the architecture's `asm/fcntl.h`.
#[cfg(not(target_arch = "x86_64"))]
pub const O_NOFOLLOW: c_int = 0o100_000;
/// `O_TMPFILE`, from `asm-generic/fcntl.h`: `__O_TMPFILE | O_DIRECTORY`.
pub const O_TMPFILE: c_int = 0o20_000_000 | O_DIRECTORY;

/// `AT_FDCWD`, from `linux/fcntl.h`: resolve relative paths from the working
/// directory.
pub const AT_FDCWD: c_int = -100;
/// `AT_SYMLINK_NOFOLLOW`, from `linux/fcntl.h`.
pub const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
/// `AT_REMOVEDIR`, from `linux/fcntl.h`.
pub const AT_REMOVEDIR: c_int = 0x200;
/// `AT_EACCESS`, from `linux/fcntl.h`: `faccessat` checks with the effective
/// ids. The same bit as `AT_REMOVEDIR`, which only `unlinkat` reads.
pub const AT_EACCESS: c_int = 0x200;

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
        crate::cancel::syscall_cp(
            nr::OPENAT,
            dirfd as usize,
            path.addr(),
            flags as usize,
            mode as usize,
            0,
            0,
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
    let ret = if cmd == F_SETLKW {
        // Waiting for a lock is a cancellation point; musl makes only this
        // command one, and not `F_OFD_SETLKW`, and so does this.
        // SAFETY: the caller vouches for `arg` for this command.
        unsafe {
            crate::cancel::syscall_cp(nr::FCNTL, fd as usize, cmd as usize, arg as usize, 0, 0, 0)
        }
    } else {
        // SAFETY: as above.
        unsafe { syscall::syscall3(nr::FCNTL, fd as usize, cmd as usize, arg as usize) }
    };
    errno::from_syscall(ret) as c_int
}

/// `F_SETLKW`: set a record lock, waiting for it.
const F_SETLKW: c_int = 7;

/// The record-lock commands on a `struct flock` whose offsets are 64-bit,
/// which [`Flock`] always is: `F_GETLK`, `F_SETLK` and `F_SETLKW` on a 64-bit
/// architecture, and their `*64` forms on a 32-bit one, as musl's
/// `bits/fcntl.h` defines them.
#[cfg(target_pointer_width = "64")]
const LOCK_COMMANDS: [c_int; 3] = [5, 6, F_SETLKW];
/// See the 64-bit definition.
#[cfg(target_pointer_width = "32")]
const LOCK_COMMANDS: [c_int; 3] = [12, 13, 14];

/// `lockf`'s operations, from `unistd.h`.
const F_ULOCK: c_int = 0;
/// See [`F_ULOCK`].
const F_LOCK: c_int = 1;
/// See [`F_ULOCK`].
const F_TLOCK: c_int = 2;
/// See [`F_ULOCK`].
const F_TEST: c_int = 3;

/// `struct flock`'s lock types, from `asm-generic/fcntl.h`.
const F_RDLCK: c_short = 0;
/// See [`F_RDLCK`].
const F_WRLCK: c_short = 1;
/// See [`F_RDLCK`].
const F_UNLCK: c_short = 2;

/// Locks, unlocks or tests the `len` bytes of `fd` from its offset -- to the
/// end of the file for zero, and before the offset for a negative length --
/// as a POSIX record lock, as musl does. `F_LOCK` waits; `F_TLOCK` fails
/// instead; `F_TEST` fails with `EACCES` when another process holds a lock
/// on the range.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lockf(fd: c_int, op: c_int, len: i64) -> c_int {
    let [get, set, set_wait] = LOCK_COMMANDS;
    let (command, l_type) = match op {
        F_TEST => (get, F_RDLCK),
        F_ULOCK => (set, F_UNLCK),
        F_TLOCK => (set, F_WRLCK),
        F_LOCK => (set_wait, F_WRLCK),
        _ => {
            errno::set(errno::EINVAL);
            return -1;
        }
    };
    let mut lock = Flock {
        l_type,
        l_whence: crate::stdio::file::SEEK_CUR as c_short,
        l_len: len,
        ..Flock::default()
    };
    // SAFETY: the command reads, or for `F_TEST` writes, `lock`, a live local.
    let ret = unsafe { fcntl(fd, command, (&raw mut lock).addr() as c_ulong) };
    if op != F_TEST || ret < 0 {
        return ret;
    }
    if lock.l_type == F_UNLCK || lock.l_pid == crate::process::getpid() {
        return 0;
    }
    errno::set(errno::EACCES);
    -1
}

/// glibc's large-file name for [`lockf`]: the length is already 64-bit.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lockf64(fd: c_int, op: c_int, len: i64) -> c_int {
    lockf(fd, op, len)
}

/// Starts, waits for or both, as `flags` says, writing the `len` bytes of
/// `fd` from `offset` to storage. Linux's own call; git uses it to flush a
/// pack's pages before it renames the pack into place.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sync_file_range(fd: c_int, offset: i64, len: i64, flags: c_uint) -> c_int {
    // SAFETY: `sync_file_range` reads no memory.
    #[cfg(not(target_arch = "arm"))]
    let ret = unsafe {
        syscall::syscall4(
            nr::SYNC_FILE_RANGE,
            fd as usize,
            offset as usize,
            len as usize,
            flags as usize,
        )
    };
    // SAFETY: as above. ARMv7-A's form takes the flags second, so that the
    // two 64-bit arguments start at even registers.
    #[cfg(target_arch = "arm")]
    let ret = unsafe {
        syscall::syscall6(
            nr::ARM_SYNC_FILE_RANGE,
            fd as usize,
            flags as usize,
            syscall::low(offset),
            syscall::high(offset),
            syscall::low(len),
            syscall::high(len),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Advises the kernel how `len` bytes of `fd` from `offset` will be used.
/// Returns the error number rather than setting `errno`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn posix_fadvise(fd: c_int, offset: i64, len: i64, advice: c_int) -> c_int {
    // SAFETY: `fadvise64` reads no memory. On x86-64 its arguments are
    // (fd, offset, len, advice); AArch64's `fadvise64_64` takes the same.
    #[cfg(not(target_arch = "arm"))]
    let ret = unsafe {
        syscall::syscall4(
            nr::FADVISE64,
            fd as usize,
            offset as usize,
            len as usize,
            advice as usize,
        )
    };
    // SAFETY: as above. ARMv7-A's form takes the advice second, so that the
    // two 64-bit arguments start at even registers.
    #[cfg(target_arch = "arm")]
    let ret = unsafe {
        syscall::syscall6(
            nr::ARM_FADVISE64_64,
            fd as usize,
            advice as usize,
            syscall::low(offset),
            syscall::high(offset),
            syscall::low(len),
            syscall::high(len),
        )
    };
    match errno::decode(ret) {
        Ok(_) => 0,
        Err(error) => error,
    }
}

/// The `fallocate` system call: `mode` done to the `len` bytes of `fd` from
/// `offset`, answered as the kernel answers it.
#[cfg(not(target_arch = "arm"))]
fn fallocate_call(fd: c_int, mode: c_int, offset: i64, len: i64) -> isize {
    // SAFETY: `fallocate` reads no memory.
    unsafe {
        syscall::syscall4(
            nr::FALLOCATE,
            fd as usize,
            mode as usize,
            offset as usize,
            len as usize,
        )
    }
}

/// The `fallocate` system call, with the offset and length in pairs of
/// registers.
#[cfg(target_arch = "arm")]
fn fallocate_call(fd: c_int, mode: c_int, offset: i64, len: i64) -> isize {
    // SAFETY: `fallocate` reads no memory.
    unsafe {
        syscall::syscall6(
            nr::FALLOCATE,
            fd as usize,
            mode as usize,
            syscall::low(offset),
            syscall::high(offset),
            syscall::low(len),
            syscall::high(len),
        )
    }
}

/// Makes sure the `len` bytes of `fd` from `offset` have storage. Returns the
/// error number rather than setting `errno`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn posix_fallocate(fd: c_int, offset: i64, len: i64) -> c_int {
    // Mode zero allocates.
    match errno::decode(fallocate_call(fd, 0, offset, len)) {
        Ok(_) => 0,
        Err(error) => error,
    }
}

/// Linux's `fallocate`: allocates the `len` bytes of `fd` from `offset` for
/// mode zero, or does what `mode`'s `FALLOC_FL_*` bits ask -- keep the size,
/// punch a hole, zero a range. Returns 0, or -1 with `errno` set, where
/// [`posix_fallocate`] returns the error.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fallocate(fd: c_int, mode: c_int, offset: i64, len: i64) -> c_int {
    errno::from_syscall(fallocate_call(fd, mode, offset, len)) as c_int
}

/// glibc's large-file name for [`fallocate`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fallocate64(fd: c_int, mode: c_int, offset: i64, len: i64) -> c_int {
    fallocate(fd, mode, offset, len)
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

/// `open` with no mode, as `_FORTIFY_SOURCE` rewrites a two-argument call.
///
/// A call that creates a file must say its mode, and with no third argument
/// the mode would be whatever the register held. The compiler rewrites the
/// two-argument calls it cannot prove safe to this, and glibc stops the
/// program when the flags create a file, as this does.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __open_2(path: *const c_char, flags: c_int) -> c_int {
    if flags & O_CREAT != 0 || flags & O_TMPFILE == O_TMPFILE {
        crate::fortify::fortify_fail(
            b"*** invalid open call: O_CREAT or O_TMPFILE without mode ***: terminated\n",
        );
    }
    // SAFETY: the caller's contract is `open`'s; no mode is read.
    unsafe { open(path, flags, 0) }
}

/// glibc's large-file name for [`__open_2`].
///
/// # Safety
///
/// As [`__open_2`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __open64_2(path: *const c_char, flags: c_int) -> c_int {
    // SAFETY: the caller's contract is `__open_2`'s.
    unsafe { __open_2(path, flags) }
}

/// [`__open_2`] for `openat`: the fortified two-argument `openat`, which
/// stops the program when the flags would create a file with no mode.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __openat_2(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int {
    if flags & O_CREAT != 0 || flags & O_TMPFILE == O_TMPFILE {
        crate::fortify::fortify_fail(
            b"*** invalid openat call: O_CREAT or O_TMPFILE without mode ***: terminated\n",
        );
    }
    // SAFETY: the caller's contract is `openat`'s; no mode is read.
    unsafe { openat(dirfd, path, flags, 0) }
}

/// glibc's large-file name for [`__openat_2`].
///
/// # Safety
///
/// As [`__openat_2`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __openat64_2(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int {
    // SAFETY: the caller's contract is `__openat_2`'s.
    unsafe { __openat_2(dirfd, path, flags) }
}

/// glibc's large-file name for [`fcntl`]. `struct flock` is already the
/// large-file one on a 64-bit architecture.
///
/// # Safety
///
/// As [`fcntl`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fcntl64(fd: c_int, cmd: c_int, arg: c_ulong) -> c_int {
    // SAFETY: the caller's contract is `fcntl`'s.
    unsafe { fcntl(fd, cmd, arg) }
}

/// glibc's large-file name for [`posix_fallocate`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn posix_fallocate64(fd: c_int, offset: i64, len: i64) -> c_int {
    posix_fallocate(fd, offset, len)
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

/// Moves up to `len` bytes between `fd_in` and `fd_out` without copying them
/// through the caller, where one of the two is a pipe.
///
/// `off_in` and `off_out` are the offsets to move from and to, each null for a
/// descriptor's own file position, and each updated when it is not. The kernel
/// refuses an offset for a pipe.
///
/// A cancellation point, as it is in glibc: it blocks until the pipe has room
/// or data unless `SPLICE_F_NONBLOCK` is set.
///
/// # Safety
///
/// `off_in` and `off_out` must each be null or valid for a read and a write of
/// an `off_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn splice(
    fd_in: c_int,
    off_in: *mut i64,
    fd_out: c_int,
    off_out: *mut i64,
    len: usize,
    flags: c_uint,
) -> isize {
    // SAFETY: the caller vouches for both offsets.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::SPLICE,
            fd_in as usize,
            off_in.addr(),
            fd_out as usize,
            off_out.addr(),
            len,
            flags as usize,
        )
    };
    errno::from_syscall(ret)
}

/// Moves up to the `count` buffers at `iov` into or out of the pipe `fd`
/// without copying them, and returns how many bytes moved.
///
/// A cancellation point, for the reason [`splice`] is one.
///
/// # Safety
///
/// `iov` must be valid for reads of `count` `struct iovec`, each naming a
/// buffer the kernel may read or write for as long as the call runs.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vmsplice(
    fd: c_int,
    iov: *const crate::uio::Iovec,
    count: usize,
    flags: c_uint,
) -> isize {
    // SAFETY: the caller vouches for the vector and its buffers.
    let ret = unsafe {
        crate::cancel::syscall_cp(
            nr::VMSPLICE,
            fd as usize,
            iov.addr(),
            count,
            flags as usize,
            0,
            0,
        )
    };
    errno::from_syscall(ret)
}
