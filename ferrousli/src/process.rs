//! `unistd.h`'s process calls: creating and replacing processes, and the
//! process's user, group, session and process group ids.
//!
//! The `set*id` calls change only the calling thread's credentials in the
//! kernel. With one thread that is the process's. When there are threads,
//! they must run on every thread, as musl's `__setxid` does.

use core::ffi::{CStr, c_char, c_int, c_uint};
use core::sync::atomic::Ordering;

use crate::errno;
use crate::stdlib::{environ, getenv};
use crate::syscall::{self, nr};

/// `SIGCHLD`, from `asm/signal.h`: the signal a child sends its parent when
/// it ends.
const SIGCHLD: usize = 17;
/// `PATH_MAX`, from `linux/limits.h`.
const PATH_MAX: usize = 4096;
/// `NAME_MAX`, from `linux/limits.h`.
const NAME_MAX: usize = 255;
/// The search path when `PATH` is not set: musl's.
const DEFAULT_PATH: &[u8] = b"/usr/local/bin:/bin:/usr/bin";
/// An id argument that leaves the id unchanged: `(uid_t)-1`.
const UNCHANGED: usize = c_uint::MAX as usize;

/// Creates a child process, a copy of this one. Returns the child's id in the
/// parent, and 0 in the child.
///
/// It is `clone` with only an exit signal, which is what the kernel's `fork`
/// does and all AArch64 has. With every other argument zero, x86-64's and
/// AArch64's different `clone` argument orders do not matter.
///
/// The child's thread control block is a copy of the parent's, so its thread
/// id is recorded again. Nothing else is done for the child yet: there are no
/// `pthread_atfork` handlers and no other threads' state to discard.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fork() -> c_int {
    // SAFETY: without `CLONE_VM` the child gets a copy of the address space,
    // so nothing either process holds is shared with the other.
    let ret = unsafe { syscall::syscall6(nr::CLONE, SIGCHLD, 0, 0, 0, 0, 0) };
    // In unit tests the thread pointer belongs to the host's C library, and
    // no test forks.
    #[cfg(not(test))]
    if ret == 0 {
        crate::thread::refresh_tid();
    }
    errno::from_syscall(ret) as c_int
}

/// Creates a child process that may only call `_exit` or an `exec` function.
///
/// It is [`fork`]. A real `vfork` shares the parent's memory until the child
/// calls `exec`, which saves copying but cannot be written as a Rust function
/// that returns in the child. POSIX allows `fork` in its place.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn vfork() -> c_int {
    fork()
}

/// Calls the kernel's `execve`, and returns the error number if it returns.
///
/// # Safety
///
/// As [`execve`].
unsafe fn exec(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // SAFETY: the kernel only reads the path and the two arrays, which the
    // caller vouches for.
    let ret = unsafe { syscall::syscall3(nr::EXECVE, path.addr(), argv.addr(), envp.addr()) };
    errno::decode(ret).err().unwrap_or(0)
}

/// Replaces the process with the program at `path`, run with the arguments
/// `argv` and the environment `envp`. Returns only on failure.
///
/// # Safety
///
/// `path` must be a NUL-terminated string, and `argv` and `envp` arrays of
/// NUL-terminated strings ending in a null pointer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn execve(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // SAFETY: the caller's contract is `exec`'s.
    let error = unsafe { exec(path, argv, envp) };
    errno::set(error);
    -1
}

/// [`execve`] with the current environment.
///
/// # Safety
///
/// As [`execve`], and `environ` must be a valid environment.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn execv(path: *const c_char, argv: *const *const c_char) -> c_int {
    let envp = environ
        .load(Ordering::Relaxed)
        .cast_const()
        .cast::<*const c_char>();
    // SAFETY: the caller vouches for the path, the arguments and `environ`.
    unsafe { execve(path, argv, envp) }
}

/// Writes `dir`, a `/` unless `dir` is empty, `file` and a NUL into `buf`.
/// Returns whether it fit.
///
/// An empty entry in `PATH` means the working directory, so it gives `file`
/// alone.
fn join(dir: &[u8], file: &[u8], buf: &mut [u8]) -> bool {
    let slash = usize::from(!dir.is_empty());
    let Some((dir_part, rest)) = buf.split_at_mut_checked(dir.len()) else {
        return false;
    };
    let Some((slash_part, rest)) = rest.split_at_mut_checked(slash) else {
        return false;
    };
    let Some((file_part, rest)) = rest.split_at_mut_checked(file.len()) else {
        return false;
    };
    let Some(nul) = rest.first_mut() else {
        return false;
    };
    dir_part.copy_from_slice(dir);
    slash_part.fill(b'/');
    file_part.copy_from_slice(file);
    *nul = 0;
    true
}

/// Replaces the process with the program `file`, searched for in `PATH`, with
/// the arguments `argv` and the current environment. A `file` containing a
/// `/` is not searched for.
///
/// Adapted from musl (MIT). Each candidate path is built in a buffer on the
/// stack, so nothing is allocated. A directory too long to make a path with
/// is skipped. A candidate that does not exist or is not a file goes on to the
/// next; any other failure stops the search. If a candidate was found but not
/// permitted, the search fails with `EACCES`.
///
/// A file the kernel cannot run (`ENOEXEC`) is not handed to the shell, as
/// POSIX asks: musl does not either.
///
/// # Safety
///
/// `file` must be a NUL-terminated string, `argv` an array of NUL-terminated
/// strings ending in a null pointer, and `environ` a valid environment.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int {
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(file) }.to_bytes();
    let envp = environ
        .load(Ordering::Relaxed)
        .cast_const()
        .cast::<*const c_char>();
    if name.is_empty() {
        errno::set(errno::ENOENT);
        return -1;
    }
    if name.contains(&b'/') {
        // SAFETY: the caller vouches for everything `execve` reads.
        return unsafe { execve(file, argv, envp) };
    }
    if name.len() > NAME_MAX {
        errno::set(errno::ENAMETOOLONG);
        return -1;
    }

    // SAFETY: the name is a C string literal, and the caller vouches for
    // `environ`.
    let path = unsafe { getenv(c"PATH".as_ptr()) };
    let path = if path.is_null() {
        DEFAULT_PATH
    } else {
        // SAFETY: `getenv` returns a NUL-terminated string from the
        // environment.
        unsafe { CStr::from_ptr(path) }.to_bytes()
    };

    let mut buf = [0_u8; PATH_MAX + 1 + NAME_MAX + 1];
    let mut error = errno::ENOENT;
    let mut denied = false;
    for dir in path.split(|&byte| byte == b':') {
        if !join(dir, name, &mut buf) {
            continue;
        }
        // SAFETY: `join` wrote a NUL-terminated path into `buf`, and the
        // caller vouches for the arguments and the environment.
        error = unsafe { exec(buf.as_ptr().cast(), argv, envp) };
        match error {
            errno::EACCES => denied = true,
            errno::ENOENT | errno::ENOTDIR => {}
            _ => break,
        }
    }
    errno::set(
        if denied && matches!(error, errno::EACCES | errno::ENOENT | errno::ENOTDIR) {
            errno::EACCES
        } else {
            error
        },
    );
    -1
}

/// Makes a system call that takes no arguments and cannot fail.
fn id(number: usize) -> c_int {
    // SAFETY: each call this is used for reads no memory.
    let ret = unsafe { syscall::syscall0(number) };
    ret as c_int
}

/// Makes an id-setting system call with up to three id arguments.
fn set_ids(number: usize, a0: usize, a1: usize, a2: usize) -> c_int {
    // SAFETY: each call this is used for reads no memory.
    let ret = unsafe { syscall::syscall3(number, a0, a1, a2) };
    errno::from_syscall(ret) as c_int
}

/// The calling process's id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getpid() -> c_int {
    id(nr::GETPID)
}

/// The parent process's id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getppid() -> c_int {
    id(nr::GETPPID)
}

/// The real user id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getuid() -> c_uint {
    id(nr::GETUID) as c_uint
}

/// The effective user id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn geteuid() -> c_uint {
    id(nr::GETEUID) as c_uint
}

/// The real group id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getgid() -> c_uint {
    id(nr::GETGID) as c_uint
}

/// The effective group id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getegid() -> c_uint {
    id(nr::GETEGID) as c_uint
}

/// Sets the user ids to `uid`, as far as the process's privilege allows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setuid(uid: c_uint) -> c_int {
    set_ids(nr::SETUID, uid as usize, 0, 0)
}

/// Sets the group ids to `gid`, as far as the process's privilege allows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setgid(gid: c_uint) -> c_int {
    set_ids(nr::SETGID, gid as usize, 0, 0)
}

/// Sets the effective user id, leaving the real and saved ones alone.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn seteuid(euid: c_uint) -> c_int {
    set_ids(nr::SETRESUID, UNCHANGED, euid as usize, UNCHANGED)
}

/// Sets the effective group id, leaving the real and saved ones alone.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setegid(egid: c_uint) -> c_int {
    set_ids(nr::SETRESGID, UNCHANGED, egid as usize, UNCHANGED)
}

/// Sets the real and effective user ids; -1 leaves one unchanged.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setreuid(ruid: c_uint, euid: c_uint) -> c_int {
    set_ids(nr::SETREUID, ruid as usize, euid as usize, 0)
}

/// Sets the real and effective group ids; -1 leaves one unchanged.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setregid(rgid: c_uint, egid: c_uint) -> c_int {
    set_ids(nr::SETREGID, rgid as usize, egid as usize, 0)
}

/// Stores up to `size` supplementary group ids at `list`, and returns how
/// many there are. A zero `size` only counts them.
///
/// # Safety
///
/// `list` must be valid for writes of `size` `gid_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getgroups(size: c_int, list: *mut c_uint) -> c_int {
    // SAFETY: the kernel writes at most `size` ids at `list`, as the caller
    // vouches, and none when `size` is zero.
    let ret = unsafe { syscall::syscall2(nr::GETGROUPS, size as usize, list.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Sets the supplementary group ids to the `size` at `list`.
///
/// # Safety
///
/// `list` must be valid for reads of `size` `gid_t`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn setgroups(size: usize, list: *const c_uint) -> c_int {
    // SAFETY: the kernel only reads the list, which the caller vouches for.
    let ret = unsafe { syscall::syscall2(nr::SETGROUPS, size, list.addr()) };
    errno::from_syscall(ret) as c_int
}

/// Makes the calling process the leader of a new session and process group,
/// and returns the session's id.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setsid() -> c_int {
    // SAFETY: `setsid` reads no memory.
    let ret = unsafe { syscall::syscall0(nr::SETSID) };
    errno::from_syscall(ret) as c_int
}

/// The session id of process `pid`, or of the calling process if it is zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getsid(pid: c_int) -> c_int {
    set_ids(nr::GETSID, pid as usize, 0, 0)
}

/// The process group id of process `pid`, or of the calling process if it is
/// zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getpgid(pid: c_int) -> c_int {
    set_ids(nr::GETPGID, pid as usize, 0, 0)
}

/// Moves process `pid`, or the calling process if it is zero, into process
/// group `pgid`, or a new group named after it if `pgid` is zero.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setpgid(pid: c_int, pgid: c_int) -> c_int {
    set_ids(nr::SETPGID, pid as usize, pgid as usize, 0)
}

/// The calling process's process group id. It is `getpgid(0)`, since AArch64
/// has no `getpgrp`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getpgrp() -> c_int {
    getpgid(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(dir: &[u8], file: &[u8], len: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0xff; len];
        join(dir, file, &mut buf).then_some(buf)
    }

    #[test]
    fn a_search_path_entry_and_a_name_join_with_one_slash() {
        assert_eq!(
            joined(b"/bin", b"sh", 8).as_deref(),
            Some(&b"/bin/sh\0"[..])
        );
        // An empty entry is the working directory.
        assert_eq!(joined(b"", b"sh", 3).as_deref(), Some(&b"sh\0"[..]));
    }

    #[test]
    fn a_candidate_that_does_not_fit_is_refused() {
        assert_eq!(joined(b"/bin", b"sh", 7), None);
        assert_eq!(joined(b"", b"sh", 2), None);
    }

    #[test]
    fn the_process_ids_are_the_hosts() {
        assert_eq!(getpid().cast_unsigned(), std::process::id());
        assert_eq!(getpgrp(), getpgid(0));
        assert_eq!(getsid(-1), -1);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::ESRCH);
    }
}
