//! `unistd.h`: descriptors, the file system calls, and `syscall`.
//!
//! The rest of `unistd.h` is elsewhere: creating processes and the process's
//! ids in [`crate::process`], `sysconf` in [`crate::sysconf`], `sleep`,
//! `usleep`, `pause` and `alarm` in [`crate::time`], `gethostname` in
//! [`crate::utsname`] and `getentropy` in [`crate::random`].
//!
//! A call that names a path is made as its `*at` system call relative to
//! `AT_FDCWD`, because that is all AArch64 has, and the result is the same.
//! `syscall` is variadic in C; [`crate::fcntl`] says why it is defined with
//! fixed parameters. A system call with fewer arguments than the helper it
//! goes through gets zeros in the rest, which the kernel does not read.

use core::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_void};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::fcntl::{AT_EACCESS, AT_FDCWD, AT_REMOVEDIR, AT_SYMLINK_NOFOLLOW, F_GETFD};
use crate::syscall::{self, nr};
use crate::{errno, exit};

/// `TCGETS`, from `asm-generic/ioctls.h`.
const TCGETS: usize = 0x5401;

/// The kernel's `struct termios`, from `asm-generic/termbits.h`, which
/// `TCGETS` writes. It is not C's `struct termios`, which is larger.
#[repr(C)]
#[derive(Debug, Default)]
struct KernelTermios {
    iflag: c_uint,
    oflag: c_uint,
    cflag: c_uint,
    lflag: c_uint,
    line: c_uchar,
    /// `NCCS` is 19.
    cc: [c_uchar; 19],
}

const _: () = assert!(size_of::<KernelTermios>() == 36);

/// Reads up to `count` bytes from `fd` into `buf`.
///
/// # Safety
///
/// `buf` must be valid for writes of `count` bytes that nothing else refers
/// to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize {
    // SAFETY: the caller vouches for the buffer. Casting `fd` sign-extends,
    // and the kernel reads the low 32 bits back.
    let ret =
        unsafe { crate::cancel::syscall_cp(nr::READ, fd as usize, buf.addr(), count, 0, 0, 0) };
    errno::from_syscall(ret)
}

/// Writes up to `count` bytes from `buf` to `fd`.
///
/// # Safety
///
/// `buf` must be valid for reads of `count` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    // SAFETY: the caller vouches for the buffer, and the kernel only reads it.
    let ret =
        unsafe { crate::cancel::syscall_cp(nr::WRITE, fd as usize, buf.addr(), count, 0, 0, 0) };
    errno::from_syscall(ret)
}

/// Reads up to `count` bytes from `fd` at `offset` into `buf`, leaving the
/// file offset alone.
///
/// # Safety
///
/// As [`read`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pread(fd: c_int, buf: *mut c_void, count: usize, offset: i64) -> isize {
    // SAFETY: as in `read`.
    let ret = unsafe {
        #[cfg(not(target_arch = "arm"))]
        let offset = (offset as usize, 0, 0);
        // The offset's pair starts at r4, leaving r3 unused.
        #[cfg(target_arch = "arm")]
        let offset = (0, syscall::low(offset), syscall::high(offset));
        crate::cancel::syscall_cp(
            nr::PREAD64,
            fd as usize,
            buf.addr(),
            count,
            offset.0,
            offset.1,
            offset.2,
        )
    };
    errno::from_syscall(ret)
}

/// Writes up to `count` bytes from `buf` to `fd` at `offset`, leaving the
/// file offset alone.
///
/// # Safety
///
/// As [`write`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pwrite(fd: c_int, buf: *const c_void, count: usize, offset: i64) -> isize {
    // SAFETY: as in `write`.
    let ret = unsafe {
        #[cfg(not(target_arch = "arm"))]
        let offset = (offset as usize, 0, 0);
        // The offset's pair starts at r4, leaving r3 unused.
        #[cfg(target_arch = "arm")]
        let offset = (0, syscall::low(offset), syscall::high(offset));
        crate::cancel::syscall_cp(
            nr::PWRITE64,
            fd as usize,
            buf.addr(),
            count,
            offset.0,
            offset.1,
            offset.2,
        )
    };
    errno::from_syscall(ret)
}

/// Ends the process with `status` at once. POSIX's name for `_Exit`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn _exit(status: c_int) -> ! {
    exit::_Exit(status)
}

/// Closes `fd`.
///
/// Linux releases the descriptor even when `close` is interrupted, so
/// `EINTR` is reported as success, as musl does. Retrying would close a
/// descriptor another thread may have opened since.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn close(fd: c_int) -> c_int {
    // SAFETY: `close` reads no memory.
    let ret = unsafe { crate::cancel::syscall_cp(nr::CLOSE, fd as usize, 0, 0, 0, 0, 0) };
    match errno::decode(ret) {
        Ok(_) | Err(errno::EINTR) => 0,
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// Moves `fd`'s offset to `offset` from `whence`, and returns the new one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64 {
    match syscall::lseek(fd as usize, offset, whence as usize) {
        error @ -4095..0 => {
            // The range is an error number, which fits a `c_int`.
            errno::set(-error as c_int);
            -1
        }
        offset => offset,
    }
}

/// Copies `fd` to the lowest free descriptor.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dup(fd: c_int) -> c_int {
    // SAFETY: `dup` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::DUP, fd as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Makes `new` a copy of `old`, closing `new` first, with `O_CLOEXEC` the
/// only flag. `EBUSY`, which Linux reports when `new` is being opened by
/// another thread at that moment, is retried, as musl does.
fn duplicate(old: c_int, new: c_int, flags: c_int) -> c_int {
    loop {
        // SAFETY: `dup3` reads no memory.
        let ret =
            unsafe { syscall::syscall3(nr::DUP3, old as usize, new as usize, flags as usize) };
        if ret != -(errno::EBUSY as isize) {
            return errno::from_syscall(ret) as c_int;
        }
    }
}

/// Makes `new` a copy of `old`, closing `new` first. If they are the same,
/// only checks that `old` is open.
///
/// It is `dup3`, which every architecture has, with `dup2`'s case of equal
/// descriptors handled first, as musl does where there is no `dup2`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dup2(old: c_int, new: c_int) -> c_int {
    if old == new {
        // SAFETY: `F_GETFD` reads no memory.
        let ret = unsafe { syscall::syscall2(nr::FCNTL, old as usize, F_GETFD as usize) };
        return match errno::decode(ret) {
            Ok(_) => old,
            Err(error) => {
                errno::set(error);
                -1
            }
        };
    }
    duplicate(old, new, 0)
}

/// Makes `new` a copy of `old`, closing `new` first, with `flags`. Equal
/// descriptors fail with `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn dup3(old: c_int, new: c_int, flags: c_int) -> c_int {
    duplicate(old, new, flags)
}

/// Creates a pipe with `flags`, and stores its read and write ends in
/// `fds[0]` and `fds[1]`.
///
/// # Safety
///
/// `fds` must be valid for writes of two `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pipe2(fds: *mut c_int, flags: c_int) -> c_int {
    // SAFETY: the kernel writes two ints at `fds`, as the caller vouches.
    let ret = unsafe { syscall::syscall2(nr::PIPE2, fds.addr(), flags as usize) };
    errno::from_syscall(ret) as c_int
}

/// Creates a pipe, and stores its read and write ends in `fds[0]` and
/// `fds[1]`.
///
/// # Safety
///
/// As [`pipe2`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pipe(fds: *mut c_int) -> c_int {
    // SAFETY: the caller's contract is `pipe2`'s.
    unsafe { pipe2(fds, 0) }
}

/// Copies up to `len` bytes from `fd_in` to `fd_out` in the kernel, each from
/// its offset if one is given, updating it, and from the file offset
/// otherwise; returns how many were copied.
///
/// # Safety
///
/// `off_in` and `off_out` must each be null or valid for a read and a write of
/// an `off_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn copy_file_range(
    fd_in: c_int,
    off_in: *mut i64,
    fd_out: c_int,
    off_out: *mut i64,
    len: usize,
    flags: c_uint,
) -> isize {
    // SAFETY: the kernel reads and writes the offsets the caller vouches for.
    let ret = unsafe {
        syscall::syscall6(
            nr::COPY_FILE_RANGE,
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

/// The limits and options `fpathconf` answers, indexed by `_PC_` name: musl's
/// table, the same for every file, as musl gives them.
const PATHCONF: [c_long; 21] = [
    8,    // _PC_LINK_MAX: _POSIX_LINK_MAX
    255,  // _PC_MAX_CANON: _POSIX_MAX_CANON
    255,  // _PC_MAX_INPUT: _POSIX_MAX_INPUT
    255,  // _PC_NAME_MAX: NAME_MAX
    4096, // _PC_PATH_MAX: PATH_MAX
    4096, // _PC_PIPE_BUF: PIPE_BUF
    1,    // _PC_CHOWN_RESTRICTED
    1,    // _PC_NO_TRUNC
    0,    // _PC_VDISABLE
    1,    // _PC_SYNC_IO
    -1,   // _PC_ASYNC_IO
    -1,   // _PC_PRIO_IO
    -1,   // _PC_SOCK_MAXBUF
    64,   // _PC_FILESIZEBITS
    4096, // _PC_REC_INCR_XFER_SIZE
    4096, // _PC_REC_MAX_XFER_SIZE
    4096, // _PC_REC_MIN_XFER_SIZE
    4096, // _PC_REC_XFER_ALIGN
    4096, // _PC_ALLOC_SIZE_MIN
    -1,   // _PC_SYMLINK_MAX
    1,    // _PC_2_SYMLINKS
];

/// The value of limit or option `name` for the file `fd` refers to, from
/// musl's fixed table, whatever the file; -1 without changing `errno` for a
/// limit that has none, and -1 with `EINVAL` for an unknown name.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fpathconf(fd: c_int, name: c_int) -> c_long {
    let _ = fd;
    match usize::try_from(name).ok().and_then(|at| PATHCONF.get(at)) {
        Some(&value) => value,
        None => {
            errno::set(errno::EINVAL);
            -1
        }
    }
}

/// [`fpathconf`] for the file at `path`, which, as in musl, is not looked at.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pathconf(path: *const c_char, name: c_int) -> c_long {
    let _ = path;
    fpathconf(-1, name)
}

/// Writes `fd`'s data and metadata to storage.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fsync(fd: c_int) -> c_int {
    // SAFETY: `fsync` reads no memory.
    let ret = unsafe { crate::cancel::syscall_cp(nr::FSYNC, fd as usize, 0, 0, 0, 0, 0) };
    errno::from_syscall(ret) as c_int
}

/// Writes `fd`'s data, and the metadata needed to read it, to storage.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fdatasync(fd: c_int) -> c_int {
    // SAFETY: `fdatasync` reads no memory.
    let ret = unsafe { crate::cancel::syscall_cp(nr::FDATASYNC, fd as usize, 0, 0, 0, 0, 0) };
    errno::from_syscall(ret) as c_int
}

/// Sets the size of the file `fd` refers to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ftruncate(fd: c_int, len: i64) -> c_int {
    // SAFETY: `ftruncate` reads no memory.
    #[cfg(not(target_arch = "arm"))]
    let ret = unsafe { syscall::syscall2(nr::FTRUNCATE, fd as usize, len as usize) };
    // SAFETY: as above; the length's pair starts at r2, leaving r1 unused.
    #[cfg(target_arch = "arm")]
    let ret = unsafe {
        syscall::syscall4(
            nr::FTRUNCATE64,
            fd as usize,
            0,
            syscall::low(len),
            syscall::high(len),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Sets the size of the file at `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn truncate(path: *const c_char, len: i64) -> c_int {
    // SAFETY: the kernel only reads `path`.
    #[cfg(not(target_arch = "arm"))]
    let ret = unsafe { syscall::syscall2(nr::TRUNCATE, path.addr(), len as usize) };
    // SAFETY: as above; the length's pair starts at r2, leaving r1 unused.
    #[cfg(target_arch = "arm")]
    let ret = unsafe {
        syscall::syscall4(
            nr::TRUNCATE64,
            path.addr(),
            0,
            syscall::low(len),
            syscall::high(len),
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Checks whether the real ids may access `path`, relative to `dirfd`, as
/// `mode` asks. `flags` may hold `AT_EACCESS`, to check with the effective
/// ids, and `AT_SYMLINK_NOFOLLOW`.
///
/// With flags this needs the `faccessat2` system call, from Linux 5.8.
/// musl's fallback for an older kernel, which checks in a child process with
/// its ids swapped, is not here.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn faccessat(
    dirfd: c_int,
    path: *const c_char,
    mode: c_int,
    flags: c_int,
) -> c_int {
    let ret = if flags == 0 {
        // SAFETY: the kernel only reads `path`.
        unsafe { syscall::syscall3(nr::FACCESSAT, dirfd as usize, path.addr(), mode as usize) }
    } else {
        // SAFETY: as above.
        unsafe {
            syscall::syscall4(
                nr::FACCESSAT2,
                dirfd as usize,
                path.addr(),
                mode as usize,
                flags as usize,
            )
        }
    };
    errno::from_syscall(ret) as c_int
}

/// Checks whether the real ids may access `path` as `mode` asks.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn access(path: *const c_char, mode: c_int) -> c_int {
    // SAFETY: the caller's contract is `faccessat`'s.
    unsafe { faccessat(AT_FDCWD, path, mode, 0) }
}

/// Checks whether the effective ids may access `path` as `mode` asks: a GNU
/// extension musl has too, which libxkbcommon checks its include paths with.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn euidaccess(path: *const c_char, mode: c_int) -> c_int {
    // SAFETY: the caller's contract is `faccessat`'s.
    unsafe { faccessat(AT_FDCWD, path, mode, AT_EACCESS) }
}

/// [`euidaccess`] under its other name.
///
/// # Safety
///
/// As [`euidaccess`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn eaccess(path: *const c_char, mode: c_int) -> c_int {
    // SAFETY: the caller's contract is `euidaccess`'s.
    unsafe { euidaccess(path, mode) }
}

/// Changes the working directory to `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn chdir(path: *const c_char) -> c_int {
    // SAFETY: the kernel only reads `path`.
    let ret = unsafe { syscall::syscall2(nr::CHDIR, path.addr(), 0) };
    errno::from_syscall(ret) as c_int
}

/// Changes the working directory to the one `fd` refers to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fchdir(fd: c_int) -> c_int {
    // SAFETY: `fchdir` reads no memory.
    let ret = unsafe { syscall::syscall2(nr::FCHDIR, fd as usize, 0) };
    errno::from_syscall(ret) as c_int
}

/// Copies the working directory's absolute path into the `size` bytes at
/// `buf`, and returns `buf`, or null with `errno` set. `ERANGE` means the
/// buffer is too small.
///
/// POSIX leaves a null `buf` unspecified, and glibc and musl allocate one.
/// That needs `malloc`, which there is not yet, so it fails with `EINVAL`, as
/// a zero `size` does.
///
/// # Safety
///
/// `buf` must be null or valid for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: usize) -> *mut c_char {
    if buf.is_null() || size == 0 {
        errno::set(errno::EINVAL);
        return null_mut();
    }
    // SAFETY: the kernel writes at most `size` bytes at `buf`, as the caller
    // vouches.
    let ret = unsafe { syscall::syscall2(nr::GETCWD, buf.addr(), size) };
    match errno::decode(ret) {
        Err(error) => {
            errno::set(error);
            null_mut()
        }
        // A directory outside the process's root comes back as a path that
        // does not start with `/`. musl reports it as `ENOENT`, as glibc does.
        // SAFETY: the kernel wrote at least one byte, so `buf[0]` is set.
        Ok(written) if written == 0 || unsafe { buf.read() } != b'/' as c_char => {
            errno::set(errno::ENOENT);
            null_mut()
        }
        Ok(_) => buf,
    }
}

/// The working directory in memory from `malloc`, which the caller frees:
/// GNU's `get_current_dir_name`. As in glibc, `$PWD` is the answer when it
/// names the working directory, which keeps the symbolic links the shell
/// followed to get there; otherwise it is `getcwd`'s.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn get_current_dir_name() -> *mut c_char {
    /// `PATH_MAX`.
    const PATH_MAX: usize = 4096;
    // SAFETY: the name is a C string literal.
    let pwd = unsafe { crate::stdlib::getenv(c"PWD".as_ptr()) };
    if !pwd.is_null() {
        let mut named = crate::stat::Stat::default();
        let mut here = crate::stat::Stat::default();
        // SAFETY: `pwd` is the environment's string, and `named` a local.
        let both = unsafe { crate::stat::stat(pwd, &raw mut named) } == 0
            // SAFETY: a C string literal, and `here` a local.
            && unsafe { crate::stat::stat(c".".as_ptr(), &raw mut here) } == 0;
        if both && named.st_dev == here.st_dev && named.st_ino == here.st_ino {
            // SAFETY: `pwd` is a NUL-terminated string.
            return unsafe { crate::string::strdup(pwd) };
        }
    }
    let mut buf = [0 as c_char; PATH_MAX];
    // SAFETY: `buf` is a live local of `PATH_MAX` bytes.
    if unsafe { getcwd(buf.as_mut_ptr(), PATH_MAX) }.is_null() {
        return null_mut();
    }
    // SAFETY: `getcwd` wrote a NUL-terminated string.
    unsafe { crate::string::strdup(buf.as_ptr()) }
}

/// How many descriptors the process may have open: the soft
/// `RLIMIT_NOFILE`, or `OPEN_MAX`'s 256 without one, as in musl.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getdtablesize() -> c_int {
    let mut limit = crate::resource::Rlimit::default();
    // SAFETY: `limit` is a live local.
    if unsafe { crate::resource::getrlimit(crate::resource::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return 256;
    }
    c_int::try_from(limit.rlim_cur).unwrap_or(c_int::MAX)
}

/// Removes the link `path`, relative to `dirfd`, or the empty directory with
/// `AT_REMOVEDIR` in `flags`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn unlinkat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int {
    // SAFETY: the kernel only reads `path`.
    let ret =
        unsafe { syscall::syscall3(nr::UNLINKAT, dirfd as usize, path.addr(), flags as usize) };
    errno::from_syscall(ret) as c_int
}

/// Removes the link `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `unlinkat`'s.
    unsafe { unlinkat(AT_FDCWD, path, 0) }
}

/// Removes the empty directory `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn rmdir(path: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `unlinkat`'s.
    unsafe { unlinkat(AT_FDCWD, path, AT_REMOVEDIR) }
}

/// Creates the hard link `new`, relative to `newdirfd`, to `old`, relative
/// to `olddirfd`.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn linkat(
    olddirfd: c_int,
    old: *const c_char,
    newdirfd: c_int,
    new: *const c_char,
    flags: c_int,
) -> c_int {
    // SAFETY: the kernel only reads the two paths.
    let ret = unsafe {
        syscall::syscall6(
            nr::LINKAT,
            olddirfd as usize,
            old.addr(),
            newdirfd as usize,
            new.addr(),
            flags as usize,
            0,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// Creates the hard link `new` to `old`.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn link(old: *const c_char, new: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `linkat`'s.
    unsafe { linkat(AT_FDCWD, old, AT_FDCWD, new, 0) }
}

/// Creates the symbolic link `path`, relative to `dirfd`, holding `target`.
///
/// # Safety
///
/// Both strings must be NUL-terminated.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn symlinkat(
    target: *const c_char,
    dirfd: c_int,
    path: *const c_char,
) -> c_int {
    // SAFETY: the kernel only reads the two strings.
    let ret =
        unsafe { syscall::syscall3(nr::SYMLINKAT, target.addr(), dirfd as usize, path.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Creates the symbolic link `path` holding `target`.
///
/// # Safety
///
/// Both strings must be NUL-terminated.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn symlink(target: *const c_char, path: *const c_char) -> c_int {
    // SAFETY: the caller's contract is `symlinkat`'s.
    unsafe { symlinkat(target, AT_FDCWD, path) }
}

/// Copies what the symbolic link `path`, relative to `dirfd`, holds into the
/// `size` bytes at `buf`, without a NUL, and returns how many bytes it
/// copied, at most `size`.
///
/// The kernel refuses a zero size with `EINVAL`, where POSIX expects the
/// link to be checked. So a zero size reads into a one-byte buffer and
/// reports zero bytes, as musl does.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `buf` valid for writes of
/// `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readlinkat(
    dirfd: c_int,
    path: *const c_char,
    buf: *mut c_char,
    size: usize,
) -> isize {
    let mut dummy: c_char = 0;
    let (target, len) = if size == 0 {
        (&raw mut dummy, 1)
    } else {
        (buf, size)
    };
    // SAFETY: the kernel reads `path` and writes at most `len` bytes at
    // `target`, which is the caller's buffer or a live local byte.
    let ret = unsafe {
        syscall::syscall4(
            nr::READLINKAT,
            dirfd as usize,
            path.addr(),
            target.addr(),
            len,
        )
    };
    match errno::decode(ret) {
        Ok(_) if size == 0 => 0,
        _ => errno::from_syscall(ret),
    }
}

/// Copies what the symbolic link `path` holds into `buf`, as [`readlinkat`]
/// does.
///
/// # Safety
///
/// As [`readlinkat`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readlink(path: *const c_char, buf: *mut c_char, size: usize) -> isize {
    // SAFETY: the caller's contract is `readlinkat`'s.
    unsafe { readlinkat(AT_FDCWD, path, buf, size) }
}

/// Whether `fd` refers to a terminal: 1, or 0 with `errno` set to `ENOTTY`,
/// or to `EBADF` if `fd` is not open.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn isatty(fd: c_int) -> c_int {
    let mut termios = KernelTermios::default();
    // SAFETY: `TCGETS` writes a kernel `struct termios` into the live local.
    let ret =
        unsafe { syscall::syscall3(nr::IOCTL, fd as usize, TCGETS, (&raw mut termios).addr()) };
    match errno::decode(ret) {
        Ok(_) => 1,
        Err(errno::EBADF) => {
            errno::set(errno::EBADF);
            0
        }
        Err(_) => {
            errno::set(errno::ENOTTY);
            0
        }
    }
}

/// Makes system call `number` with six arguments, and returns its result, or
/// -1 with `errno` set.
///
/// A caller passes as many arguments as the call takes, and the registers
/// for the others hold whatever they held, which the kernel does not read.
///
/// # Safety
///
/// The call must be sound with these arguments.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn syscall(
    number: c_long,
    a0: c_long,
    a1: c_long,
    a2: c_long,
    a3: c_long,
    a4: c_long,
    a5: c_long,
) -> c_long {
    // SAFETY: the caller vouches for the call.
    let ret = unsafe {
        syscall::syscall6(
            number as usize,
            a0 as usize,
            a1 as usize,
            a2 as usize,
            a3 as usize,
            a4 as usize,
            a5 as usize,
        )
    };
    errno::from_syscall(ret) as c_long
}

/// glibc's large-file name for [`lseek`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lseek64(fd: c_int, offset: i64, whence: c_int) -> i64 {
    lseek(fd, offset, whence)
}

/// glibc's large-file name for [`pread`].
///
/// # Safety
///
/// As [`pread`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pread64(fd: c_int, buf: *mut c_void, count: usize, offset: i64) -> isize {
    // SAFETY: the caller's contract is `pread`'s.
    unsafe { pread(fd, buf, count, offset) }
}

/// glibc's large-file name for [`pwrite`].
///
/// # Safety
///
/// As [`pwrite`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pwrite64(
    fd: c_int,
    buf: *const c_void,
    count: usize,
    offset: i64,
) -> isize {
    // SAFETY: the caller's contract is `pwrite`'s.
    unsafe { pwrite(fd, buf, count, offset) }
}

/// glibc's large-file name for [`ftruncate`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn ftruncate64(fd: c_int, len: i64) -> c_int {
    ftruncate(fd, len)
}

/// glibc's large-file name for [`truncate`].
///
/// # Safety
///
/// As [`truncate`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn truncate64(path: *const c_char, len: i64) -> c_int {
    // SAFETY: the caller's contract is `truncate`'s.
    unsafe { truncate(path, len) }
}

/// Makes `path`'s owner `uid` and its group `gid`, leaving either as it is if
/// it is -1, and following a final symbolic link.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn chown(path: *const c_char, uid: c_uint, gid: c_uint) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { fchownat(AT_FDCWD, path, uid, gid, 0) }
}

/// `chown`, for a final symbolic link itself.
///
/// # Safety
///
/// As `chown`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn lchown(path: *const c_char, uid: c_uint, gid: c_uint) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { fchownat(AT_FDCWD, path, uid, gid, AT_SYMLINK_NOFOLLOW) }
}

/// `chown`, for the file `fd` refers to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fchown(fd: c_int, uid: c_uint, gid: c_uint) -> c_int {
    // SAFETY: `fchown` reads no memory.
    let ret = unsafe { syscall::syscall3(nr::FCHOWN, fd as usize, uid as usize, gid as usize) };
    errno::from_syscall(ret) as c_int
}

/// `chown`, for `path` relative to the directory `dirfd`, with
/// `AT_SYMLINK_NOFOLLOW` and `AT_EMPTY_PATH` in `flags`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fchownat(
    dirfd: c_int,
    path: *const c_char,
    uid: c_uint,
    gid: c_uint,
    flags: c_int,
) -> c_int {
    // SAFETY: the kernel only reads `path`.
    let ret = unsafe {
        syscall::syscall6(
            nr::FCHOWNAT,
            dirfd as usize,
            path.addr(),
            uid as usize,
            gid as usize,
            flags as usize,
            0,
        )
    };
    errno::from_syscall(ret) as c_int
}

/// The host's id. Like musl's, it is 0: there is no `/etc/hostid` to read.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn gethostid() -> c_long {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last_errno() -> c_int {
        // SAFETY: the pointer is this thread's errno.
        unsafe { errno::__errno_location().read() }
    }

    #[test]
    fn a_pipe_is_not_a_terminal_and_a_closed_descriptor_is_not_open() {
        let mut fds = [-1; 2];
        // SAFETY: `fds` has room for two ints.
        assert_eq!(unsafe { pipe2(fds.as_mut_ptr(), 0) }, 0);
        let [read_end, write_end] = fds;
        assert_eq!(isatty(read_end), 0);
        assert_eq!(last_errno(), errno::ENOTTY);
        assert_eq!(dup2(read_end, read_end), read_end);
        assert_eq!(dup3(read_end, read_end, 0), -1);
        assert_eq!(last_errno(), errno::EINVAL);
        assert_eq!(close(read_end), 0);
        assert_eq!(close(write_end), 0);
        assert_eq!(close(write_end), -1);
        assert_eq!(last_errno(), errno::EBADF);
        assert_eq!(dup2(write_end, write_end), -1);
        assert_eq!(last_errno(), errno::EBADF);
        assert_eq!(isatty(write_end), 0);
        assert_eq!(last_errno(), errno::EBADF);
    }

    #[test]
    fn getcwd_refuses_a_buffer_it_would_have_to_allocate() {
        // SAFETY: the call fails before writing.
        assert!(unsafe { getcwd(null_mut(), 0) }.is_null());
        assert_eq!(last_errno(), errno::EINVAL);
        let mut one: c_char = 0;
        // SAFETY: the buffer is one live byte.
        assert!(unsafe { getcwd(&raw mut one, 1) }.is_null());
        assert_eq!(last_errno(), errno::ERANGE);
    }
}
