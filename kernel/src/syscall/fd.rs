//! Descriptors: `openat`, `close`, `dup` and its kin, `fcntl`, `lseek`,
//! `ftruncate` and `ioctl`.
//!
//! The rules about numbers live in `ferrix_vfs::fd::FdTable` and the rules
//! about offsets in `ferrix_vfs::OpenFile`, both host-tested. What is here is
//! the part neither can do: reading a program's arguments, choosing this
//! architecture's flag bits, and holding the process's table lock for exactly
//! as long as a table operation takes.
//!
//! # The lock is never held across I/O
//!
//! Every call takes the table lock, clones or removes what it needs, and lets
//! go before touching a file. A `read` of the console waits minutes for a
//! person to type; a table lock held across it would stop every other thread
//! of the program from opening anything. And a description that `close` or
//! `dup2` displaces is dropped after the lock is released, because dropping
//! the last reference to an open file releases its dentry, and that chain is
//! unbounded.
//!
//! # `ioctl`
//!
//! Resolved here like every other call on a descriptor, and handed to
//! `crate::syscall::tty` when the descriptor is the console. Every other file
//! is `ENOTTY`.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    AT_FDCWD, F_DUPFD, F_DUPFD_CLOEXEC, F_GETFD, F_GETFL, F_SETFD, F_SETFL, FD_CLOEXEC, O_ACCMODE,
    O_APPEND, O_CLOEXEC, O_CREAT, O_EXCL, O_NONBLOCK, O_PATH, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY,
    SEEK_CUR, SEEK_DATA, SEEK_END, SEEK_HOLE, SEEK_SET,
};
use ferrix_vfs::fd::FdTable;
use ferrix_vfs::{FileType, Location, OpenFile, OpenFlags, Whence};

use crate::arch;
use crate::fs;
use crate::fs::console;
use crate::panic::{catalog, fatal};
use crate::syscall::process::Process;
use crate::syscall::tty;
use crate::syscall::uaccess::{self, UserError};

/// Linux's `PATH_MAX`, counting the terminator.
const PATH_MAX: usize = 4096;

/// A descriptor argument, which the ABI passes as a 32-bit `int`.
///
/// Narrowed first so that `-1` from a 32-bit caller and from a 64-bit one are
/// both `-1`, and so that a 64-bit caller's stale upper half is ignored, as
/// Linux ignores it.
pub(crate) fn arg(value: u64) -> i32 {
    value as u32 as i32
}

/// The description `fd` names, with the table lock already released.
pub(crate) fn file(process: &Process, fd: i32) -> Result<Arc<OpenFile>, Errno> {
    process.files().lock().get(fd).map(Arc::clone)
}

/// Where a `*at` call's relative path starts: `None` for the working
/// directory, the directory `dirfd` names otherwise.
///
/// # Errors
///
/// `EBADF` for a descriptor that names nothing, `ENOTDIR` for one that names
/// something other than a directory. An `O_PATH` descriptor on a directory is
/// accepted, which is what `O_PATH` exists for.
pub(crate) fn start_location(process: &Process, dirfd: i32) -> Result<Option<Location>, Errno> {
    if dirfd == AT_FDCWD {
        return Ok(None);
    }
    let file = file(process, dirfd)?;
    if file.kind() != FileType::Directory {
        return Err(Errno::ENOTDIR);
    }
    Ok(Some(file.location().clone()))
}

/// [`start_location`] for a particular path.
///
/// An absolute path never looks at `dirfd`, which Linux documents and programs
/// rely on: `openat(-1, "/etc/passwd", ...)` succeeds. Refusing the bad
/// descriptor first would break them.
pub(crate) fn start_for(
    process: &Process,
    dirfd: i32,
    path: &[u8],
) -> Result<Option<Location>, Errno> {
    if path.first() == Some(&b'/') {
        return Ok(None);
    }
    start_location(process, dirfd)
}

/// A path out of the program's memory.
///
/// # Errors
///
/// `ENAMETOOLONG` for a path with no terminator within `PATH_MAX`, `EFAULT`
/// for one the program cannot read.
pub(crate) fn user_path(process: &Process, at: u64) -> Result<Vec<u8>, Errno> {
    let mut path = Vec::new();
    match uaccess::copy_cstr_from_user(process.space(), at, PATH_MAX, &mut path) {
        Ok(()) => Ok(path),
        // The copy stops at the limit with every byte read, and at a fault
        // with fewer: the length is what tells the two apart.
        Err(UserError::Fault) if path.len() >= PATH_MAX => Err(Errno::ENAMETOOLONG),
        Err(_) => Err(Errno::EFAULT),
    }
}

/// Descriptors 0, 1 and 2 for a new process, all naming one open description
/// of the console.
///
/// One description rather than three opens, which is how Linux's first
/// process gets them and what a program can observe: `fcntl(0, F_SETFL,
/// O_NONBLOCK)` changes descriptor 1 too.
pub(crate) fn standard_streams() -> FdTable<Arc<OpenFile>> {
    match console_table() {
        Ok(table) => table,
        Err(errno) => fatal!(
            catalog::CONSOLE_DESCRIPTORS,
            "a new process could not be given the console: errno {}",
            errno.0
        ),
    }
}

/// See [`standard_streams`].
fn console_table() -> Result<FdTable<Arc<OpenFile>>, Errno> {
    let console = console::open_console()?;
    let mut table = FdTable::new();
    for _ in 0..3 {
        let _ = table.insert(Arc::clone(&console), false)?;
    }
    Ok(table)
}

/// A descriptor number as a return value.
fn number(fd: i32) -> Result<usize, Errno> {
    usize::try_from(fd).map_err(|_| Errno::EBADF)
}

/// Decode `open`'s flag word with this architecture's bits, into what the VFS
/// is asked for and whether the descriptor is close-on-exec.
///
/// `O_DIRECT` and `O_LARGEFILE` are recognised and ignored: nothing here has a
/// page cache to bypass, and every offset is 64 bits. They are in the table so
/// that their bits are never mistaken for the flags that share them on another
/// architecture. Unknown bits are ignored, as `open` has always ignored them.
pub(crate) fn decode_open_flags(raw: u32) -> (OpenFlags, bool) {
    let bits = arch::OPEN_FLAGS;
    let set = |flag: u32| raw & flag != 0;
    let cloexec = set(O_CLOEXEC);
    if set(O_PATH) {
        // `O_PATH` keeps only these three; every other flag is ignored.
        let flags = OpenFlags {
            path: true,
            directory: set(bits.directory),
            nofollow: set(bits.nofollow),
            ..OpenFlags::default()
        };
        return (flags, cloexec);
    }
    let (read, write) = match raw & O_ACCMODE {
        O_RDONLY => (true, false),
        O_WRONLY => (false, true),
        O_RDWR => (true, true),
        // Mode 3: neither, which Linux allows for a descriptor only `ioctl`
        // will use.
        _ => (false, false),
    };
    let flags = OpenFlags {
        read,
        write,
        create: set(O_CREAT),
        exclusive: set(O_EXCL),
        truncate: set(O_TRUNC),
        append: set(O_APPEND),
        directory: set(bits.directory),
        nofollow: set(bits.nofollow),
        path: false,
        nonblock: set(O_NONBLOCK),
    };
    (flags, cloexec)
}

/// `openat`, and `open` with `AT_FDCWD`.
pub(crate) fn sys_openat(
    process: &Process,
    dirfd: i32,
    path: u64,
    raw_flags: u32,
    mode: u32,
) -> Result<usize, Errno> {
    let path = user_path(process, path)?;
    let (flags, cloexec) = decode_open_flags(raw_flags);
    let start = start_for(process, dirfd, &path)?;
    // A copy of the context rather than the lock: the walk calls into
    // filesystems, and `chdir` on another thread must not wait for it.
    let context = process.fs_context().lock().clone();
    let file = fs::namespace().open(
        &context,
        start.as_ref(),
        &path,
        &flags,
        mode & 0o7777 & !process.umask(),
    )?;
    number(process.files().lock().insert(file, cloexec)?)
}

/// `close`.
pub(crate) fn sys_close(process: &Process, fd: i32) -> Result<usize, Errno> {
    // The guard is a temporary of this statement, so the description is
    // dropped below with the lock already released.
    let file = process.files().lock().remove(fd)?;
    drop(file);
    Ok(0)
}

/// `dup`.
pub(crate) fn sys_dup(process: &Process, fd: i32) -> Result<usize, Errno> {
    let mut files = process.files().lock();
    let file = Arc::clone(files.get(fd)?);
    number(files.insert(file, false)?)
}

/// `dup2`: as `dup3` with no flags, except that duplicating a descriptor onto
/// itself is a check that it is open rather than an error.
pub(crate) fn sys_dup2(process: &Process, old: i32, new: i32) -> Result<usize, Errno> {
    if old == new {
        let _ = file(process, old)?;
        return number(new);
    }
    replace(process, old, new, false)
}

/// `dup3`.
pub(crate) fn sys_dup3(process: &Process, old: i32, new: i32, flags: u32) -> Result<usize, Errno> {
    if flags & !O_CLOEXEC != 0 || old == new {
        return Err(Errno::EINVAL);
    }
    replace(process, old, new, flags & O_CLOEXEC != 0)
}

/// Make `new` name what `old` does, in one hold of the table lock, so that a
/// `close(old)` on another thread cannot land between the lookup and the
/// install.
fn replace(process: &Process, old: i32, new: i32, cloexec: bool) -> Result<usize, Errno> {
    let displaced = {
        let mut files = process.files().lock();
        let file = Arc::clone(files.get(old)?);
        files.install(new, file, cloexec)?
    };
    // Released outside the lock: this may be the last reference.
    drop(displaced);
    number(new)
}

/// `fcntl` and `fcntl64`, which differ only in the record-lock commands, and
/// those are refused by both.
///
/// The descriptor is looked up before the command is: an unknown command on a
/// closed descriptor is `EBADF`, as on Linux.
pub(crate) fn sys_fcntl(process: &Process, fd: i32, cmd: u32, arg: u64) -> Result<usize, Errno> {
    let file = file(process, fd)?;
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            // At or above the table's limit is `EINVAL`, which the table says.
            let min = i32::try_from(arg).map_err(|_| Errno::EINVAL)?;
            let new = process
                .files()
                .lock()
                .insert_from(min, file, cmd == F_DUPFD_CLOEXEC)?;
            number(new)
        }
        F_GETFD => {
            let cloexec = process.files().lock().cloexec(fd)?;
            Ok(if cloexec { FD_CLOEXEC as usize } else { 0 })
        }
        F_SETFD => {
            let cloexec = arg & u64::from(FD_CLOEXEC) != 0;
            process.files().lock().set_cloexec(fd, cloexec)?;
            Ok(0)
        }
        F_GETFL => Ok(status_word(&file) as usize),
        _ if file.is_path() => Err(Errno::EBADF),
        F_SETFL => {
            // Only these two may change after the open; Linux ignores the
            // access mode and the creation flags here rather than refusing.
            let mut status = file.status();
            status.append = arg & u64::from(O_APPEND) != 0;
            status.nonblock = arg & u64::from(O_NONBLOCK) != 0;
            file.set_status(status);
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}

/// What `F_GETFL` reports: the access mode and the status flags.
///
/// busybox's `printf` builtin asks this of descriptor 1 before it writes and
/// prints nothing if the call fails, so the console's descriptors must answer
/// it -- with `O_RDWR`, which is how they were opened.
fn status_word(file: &OpenFile) -> u32 {
    if file.is_path() {
        return O_PATH;
    }
    let mode = match (file.readable(), file.writable()) {
        (true, true) => O_RDWR,
        (false, true) => O_WRONLY,
        (true, false) => O_RDONLY,
        (false, false) => O_ACCMODE,
    };
    let status = file.status();
    let append = if status.append { O_APPEND } else { 0 };
    let nonblock = if status.nonblock { O_NONBLOCK } else { 0 };
    mode | append | nonblock
}

/// `SEEK_*` as the VFS names it.
fn whence(raw: u32) -> Result<Whence, Errno> {
    match raw {
        SEEK_SET => Ok(Whence::Set),
        SEEK_CUR => Ok(Whence::Current),
        SEEK_END => Ok(Whence::End),
        SEEK_DATA => Ok(Whence::Data),
        SEEK_HOLE => Ok(Whence::Hole),
        _ => Err(Errno::EINVAL),
    }
}

/// `lseek`.
///
/// The offset is a native word: 64 bits on the 64-bit architectures, a signed
/// 32-bit `off_t` on ARMv7-A, whose wide seeks go through `_llseek`. A result
/// the return register cannot carry as a non-negative value is `EOVERFLOW`,
/// after the position has moved -- which is Linux's order too.
pub(crate) fn sys_lseek(process: &Process, fd: i32, offset: i64, raw: u32) -> Result<usize, Errno> {
    let file = file(process, fd)?;
    let at = file.seek(offset, whence(raw)?)?;
    isize::try_from(at)
        .ok()
        .and_then(|at| usize::try_from(at).ok())
        .ok_or(Errno::EOVERFLOW)
}

/// `_llseek`, ARMv7-A's 64-bit seek: the offset in two words, the result
/// through a pointer, because a 32-bit return register cannot hold it.
pub(crate) fn sys_llseek(
    process: &Process,
    fd: i32,
    high: u64,
    low: u64,
    result: u64,
    raw: u32,
) -> Result<usize, Errno> {
    let file = file(process, fd)?;
    let offset = (u64::from(high as u32) << 32 | u64::from(low as u32)) as i64;
    let at = file.seek(offset, whence(raw)?)?;
    uaccess::copy_to_user(process.space(), result, &at.to_le_bytes()).map_err(|_| Errno::EFAULT)?;
    Ok(0)
}

/// `ftruncate` and `ftruncate64`.
///
/// A negative length is refused before the descriptor is looked at, as on
/// Linux.
pub(crate) fn sys_ftruncate(process: &Process, fd: i32, length: i64) -> Result<usize, Errno> {
    let length = u64::try_from(length).map_err(|_| Errno::EINVAL)?;
    file(process, fd)?.set_len(length)?;
    Ok(0)
}

/// `ioctl`: `EBADF` for a closed descriptor, the terminal requests for the
/// console, and `ENOTTY` for every other file, which is what Linux answers for
/// a descriptor that is not a terminal. See `crate::syscall::tty`.
pub(crate) fn sys_ioctl(
    process: &Process,
    fd: i32,
    request: u32,
    arg: u64,
) -> Result<usize, Errno> {
    let file = file(process, fd)?;
    if Arc::ptr_eq(file.inode(), &console::console_inode()) {
        return tty::ioctl(process, &file, request, arg);
    }
    Err(Errno::ENOTTY)
}
