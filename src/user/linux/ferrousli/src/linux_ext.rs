//! Linux calls glibc wraps and musl 1.2.5 does not, or wraps differently:
//! `statx`, the process descriptors (`pidfd_open`, `pidfd_send_signal`),
//! `close_range` and `closefrom`, the new mount API (`open_tree`,
//! `move_mount`, `mount_setattr`, `fsopen`, `fsconfig`, `fsmount` and
//! `fspick`), file handles (`name_to_handle_at`, `open_by_handle_at`),
//! `mincore`, `ptrace`, `clone`, and the message
//! queue attributes. systemd, GLib, libmount and Chrome call them.
//!
//! Each is its system call, the number read from the kernel's headers by
//! `tools/gen-abi.py`, and a kernel without one answers `ENOSYS`, which
//! each of those callers takes as "not here" and works around. Where glibc
//! does more than the call, it says so at the function.

use core::ffi::{c_char, c_int, c_long, c_uint, c_void};

use crate::errno;
use crate::syscall::{self, nr};

/// Makes system call `number` with up to five arguments, as a C function
/// returns it: the result, or -1 with `errno` set.
fn call(number: usize, args: [usize; 5]) -> c_long {
    let [a0, a1, a2, a3, a4] = args;
    // SAFETY: each caller vouches for the memory its arguments point to.
    let ret = unsafe { syscall::syscall6(number, a0, a1, a2, a3, a4, 0) };
    errno::from_syscall(ret) as c_long
}

/// Fills `*buf`, a `struct statx`, with what `mask` asks about `path`
/// relative to `dirfd`, or about `dirfd` itself with an empty path and
/// `AT_EMPTY_PATH`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `buf` valid for a write of a
/// `struct statx`, 256 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn statx(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mask: c_uint,
    buf: *mut c_void,
) -> c_int {
    call(
        nr::STATX,
        [
            dirfd as usize,
            path.addr(),
            flags as usize,
            mask as usize,
            buf.addr(),
        ],
    ) as c_int
}

/// A descriptor for process `pid`, which becomes readable when it exits.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pidfd_open(pid: c_int, flags: c_uint) -> c_int {
    call(nr::PIDFD_OPEN, [pid as usize, flags as usize, 0, 0, 0]) as c_int
}

/// Sends `sig` to the process `pidfd` names, with `*info` if not null.
///
/// # Safety
///
/// `info` must be null or valid for a read of a `siginfo_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn pidfd_send_signal(
    pidfd: c_int,
    sig: c_int,
    info: *mut c_void,
    flags: c_uint,
) -> c_int {
    call(
        nr::PIDFD_SEND_SIGNAL,
        [pidfd as usize, sig as usize, info.addr(), flags as usize, 0],
    ) as c_int
}

/// Closes, or with `CLOSE_RANGE_CLOEXEC` marks close-on-exec, every open
/// descriptor from `first` to `last`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn close_range(first: c_uint, last: c_uint, flags: c_int) -> c_int {
    call(
        nr::CLOSE_RANGE,
        [first as usize, last as usize, flags as usize, 0, 0],
    ) as c_int
}

/// Closes every descriptor from `lowfd` up.
///
/// glibc asks `close_range` and, on a kernel without it, closes the
/// descriptors `/proc/self/fd` lists. Here the second way is to close every
/// number below the descriptor limit, which is the same set without needing
/// `/proc`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn closefrom(lowfd: c_int) {
    let first = lowfd.max(0);
    if close_range(first as c_uint, c_uint::MAX, 0) == 0 {
        return;
    }
    let limit = crate::unistd::getdtablesize();
    for fd in first..limit {
        // SAFETY: `close` reads no memory; a number that is not open fails.
        let _ = unsafe { syscall::syscall2(nr::CLOSE, fd as usize, 0) };
    }
}

/// Reports which pages of `len` bytes at `addr` are resident, one byte a
/// page into `vec`, whose low bit is set for a resident page.
///
/// # Safety
///
/// `vec` must be valid for writes of one byte for each page in the range.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mincore(addr: *mut c_void, len: usize, vec: *mut u8) -> c_int {
    call(nr::MINCORE, [addr.addr(), len, vec.addr(), 0, 0]) as c_int
}

/// A detached copy, or with no `OPEN_TREE_CLONE` a reference, of the mount
/// at `path` relative to `dirfd`, as a descriptor for `move_mount`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn open_tree(dirfd: c_int, path: *const c_char, flags: c_uint) -> c_int {
    call(
        nr::OPEN_TREE,
        [dirfd as usize, path.addr(), flags as usize, 0, 0],
    ) as c_int
}

/// Moves or attaches the mount at `from_path` to `to_path`, each relative
/// to its directory descriptor.
///
/// # Safety
///
/// Both paths must be NUL-terminated strings.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn move_mount(
    from_dirfd: c_int,
    from_path: *const c_char,
    to_dirfd: c_int,
    to_path: *const c_char,
    flags: c_uint,
) -> c_int {
    call(
        nr::MOVE_MOUNT,
        [
            from_dirfd as usize,
            from_path.addr(),
            to_dirfd as usize,
            to_path.addr(),
            flags as usize,
        ],
    ) as c_int
}

/// A file system context for the file system type `fsname`, as a descriptor
/// that [`fsconfig`] configures and [`fsmount`] mounts.
///
/// # Safety
///
/// `fsname` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fsopen(fsname: *const c_char, flags: c_uint) -> c_int {
    call(nr::FSOPEN, [fsname.addr(), flags as usize, 0, 0, 0]) as c_int
}

/// Sets, with `cmd` one of the `FSCONFIG_*` commands, the parameter `key` of
/// the file system context `fd` to `value` and `aux`, whose meaning the
/// command gives; or creates or reconfigures the file system it describes.
///
/// # Safety
///
/// `key` must be null or a NUL-terminated string, and `value` null or what
/// `cmd` says it points to.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fsconfig(
    fd: c_int,
    cmd: c_uint,
    key: *const c_char,
    value: *const c_void,
    aux: c_int,
) -> c_int {
    call(
        nr::FSCONFIG,
        [
            fd as usize,
            cmd as usize,
            key.addr(),
            value.addr(),
            aux as usize,
        ],
    ) as c_int
}

/// A detached mount of the file system the context `fd` created, with the
/// `MOUNT_ATTR_*` bits in `attr_flags`, as a descriptor for `move_mount`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fsmount(fd: c_int, flags: c_uint, attr_flags: c_uint) -> c_int {
    call(
        nr::FSMOUNT,
        [fd as usize, flags as usize, attr_flags as usize, 0, 0],
    ) as c_int
}

/// A file system context for reconfiguring the file system mounted at `path`
/// relative to `dirfd`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fspick(dirfd: c_int, path: *const c_char, flags: c_uint) -> c_int {
    call(
        nr::FSPICK,
        [dirfd as usize, path.addr(), flags as usize, 0, 0],
    ) as c_int
}

/// Changes the attributes of the mount at `path`, and with `AT_RECURSIVE`
/// of those below it, to `*attr`, a `struct mount_attr` of `size` bytes.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `attr` valid for a read of
/// `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mount_setattr(
    dirfd: c_int,
    path: *const c_char,
    flags: c_uint,
    attr: *mut c_void,
    size: usize,
) -> c_int {
    call(
        nr::MOUNT_SETATTR,
        [
            dirfd as usize,
            path.addr(),
            flags as usize,
            attr.addr(),
            size,
        ],
    ) as c_int
}

/// A handle for the file at `path`, into `*handle`, a `struct file_handle`
/// whose `handle_bytes` says how much room it has, and the id of its mount
/// into `*mount_id`.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, `handle` valid for a
/// `struct file_handle` with `handle_bytes` bytes after its header, and
/// `mount_id` for a write of an `int`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn name_to_handle_at(
    dirfd: c_int,
    path: *const c_char,
    handle: *mut c_void,
    mount_id: *mut c_int,
    flags: c_int,
) -> c_int {
    call(
        nr::NAME_TO_HANDLE_AT,
        [
            dirfd as usize,
            path.addr(),
            handle.addr(),
            mount_id.addr(),
            flags as usize,
        ],
    ) as c_int
}

/// Opens the file `*handle` names on the mount `mount_fd` is on.
///
/// # Safety
///
/// `handle` must be a valid `struct file_handle`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn open_by_handle_at(
    mount_fd: c_int,
    handle: *mut c_void,
    flags: c_int,
) -> c_int {
    call(
        nr::OPEN_BY_HANDLE_AT,
        [mount_fd as usize, handle.addr(), flags as usize, 0, 0],
    ) as c_int
}

/// `PTRACE_PEEKTEXT`, `PTRACE_PEEKDATA` and `PTRACE_PEEKUSER`: the requests
/// whose word the C function returns rather than stores.
const PTRACE_PEEK: [c_int; 3] = [1, 2, 3];

/// Traces process `pid`. C declares it variadic; the four arguments are
/// passed whether the caller gave them or not, as `prctl`'s are
/// ([`crate::linux::prctl`]).
///
/// As in glibc and musl, a peek request returns the word it read, and
/// clears `errno` on success, since the word may be -1: the kernel stores
/// it at `data`, which the C function supplies itself.
///
/// # Safety
///
/// `addr` and `data` must be what `request` reads or writes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ptrace(
    request: c_int,
    pid: c_int,
    addr: *mut c_void,
    data: *mut c_void,
) -> c_long {
    let mut word: c_long = 0;
    let peek = PTRACE_PEEK.contains(&request);
    let data = if peek { (&raw mut word).cast() } else { data };
    let ret = call(
        nr::PTRACE,
        [request as usize, pid as usize, addr.addr(), data.addr(), 0],
    );
    if peek && ret >= 0 {
        errno::set(0);
        return word;
    }
    ret
}

/// Starts a child that shares what `flags` says with the caller, running
/// `entry(arg)` on `stack` and exiting with what it returns: glibc's and
/// musl's `clone`. C declares the last three arguments variadic, read only
/// when `flags` asks for them; they are passed to the kernel either way, and
/// it reads only those.
///
/// # Safety
///
/// `stack` must be the top of memory the child may use, `entry` sound to
/// run there with what `flags` shares, and `parent_tid`, `tls` and
/// `child_tid` what `flags` asks the kernel to use.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn clone(
    entry: Option<crate::arch::Entry>,
    stack: *mut c_void,
    flags: c_int,
    arg: *mut c_void,
    parent_tid: *mut c_int,
    tls: *mut c_void,
    child_tid: *mut c_int,
) -> c_int {
    let Some(entry) = entry else {
        errno::set(errno::EINVAL);
        return -1;
    };
    if stack.is_null() {
        errno::set(errno::EINVAL);
        return -1;
    }
    // SAFETY: the caller vouches for every argument.
    let ret = unsafe {
        crate::arch::clone(
            entry,
            stack.addr(),
            flags as c_uint as usize,
            arg,
            parent_tid,
            tls.addr(),
            child_tid,
        )
    };
    errno::from_syscall(ret as isize) as c_int
}

/// C's `struct mq_attr`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MqAttr {
    /// `O_NONBLOCK`, or 0.
    pub mq_flags: c_long,
    /// The most messages the queue holds.
    pub mq_maxmsg: c_long,
    /// The largest message.
    pub mq_msgsize: c_long,
    /// The messages in the queue now.
    pub mq_curmsgs: c_long,
    /// Reserved.
    pub pad: [c_long; 4],
}

/// Stores the attributes of message queue `mqd` in `*attr`. systemd asks it
/// of a descriptor to learn whether it is a queue at all.
///
/// # Safety
///
/// `attr` must be valid for a write of a `struct mq_attr`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mq_getattr(mqd: c_int, attr: *mut MqAttr) -> c_int {
    // SAFETY: the caller's contract is `mq_setattr`'s, with nothing to set.
    unsafe { mq_setattr(mqd, core::ptr::null(), attr) }
}

/// Sets the `O_NONBLOCK` flag of message queue `mqd` from `*new`, and stores
/// what the attributes were in `*old` unless it is null.
///
/// # Safety
///
/// `new` must be null or valid for a read of a `struct mq_attr`, and `old`
/// null or valid for a write of one.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn mq_setattr(mqd: c_int, new: *const MqAttr, old: *mut MqAttr) -> c_int {
    call(
        nr::MQ_GETSETATTR,
        [mqd as usize, new.addr(), old.addr(), 0, 0],
    ) as c_int
}
