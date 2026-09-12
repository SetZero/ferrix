//! `unshare` and `setns`: namespaces, which Ferrix does not have.
//!
//! Both answer as a Linux kernel built without namespace support answers,
//! because that is what Ferrix is: one mount table, one pid space, one of
//! everything. A program that asks for a namespace of its own is told no, and
//! `EINVAL` is the no it already handles -- `CONFIG_*_NS` off is an ordinary
//! configuration, and `unshare(1)` and container runtimes check for it.
//!
//! The two `unshare` flags that are not namespaces are different. Giving up a
//! descriptor table or a working directory shared through `clone(CLONE_FILES)`
//! or `clone(CLONE_FS)` needs no namespace at all, and a process that shares
//! neither has already done it: see [`sys_unshare`].

use alloc::sync::Arc;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{CLONE_FILES, CLONE_FS};

use crate::syscall::fd;
use crate::syscall::process::Process;

/// `unshare`.
///
/// Zero flags is nothing to do, and succeeds. `CLONE_FILES` and `CLONE_FS`
/// succeed when the process's table, or its root and working directory, are
/// its own already -- which is the case unless `clone` was asked to share
/// them -- because the call asks for a result that is then already true, and
/// Linux does nothing in that case either.
///
/// When one of them *is* shared the answer is `EINVAL`, and that is a
/// departure from Linux, which would make the private copy. A [`Process`]
/// holds its table and its context for its whole life and cannot swap in a
/// copy; until it can, refusing is honest and succeeding would not be, since
/// the caller would go on to change descriptors it believes are its own.
///
/// Every other flag names a namespace, or asks to leave a thread group or an
/// address space, and is `EINVAL`.
pub(crate) fn sys_unshare(process: &Process, flags: u64) -> Result<usize, Errno> {
    if flags & !(CLONE_FILES | CLONE_FS) != 0 {
        return Err(Errno::EINVAL);
    }
    if flags & CLONE_FILES != 0 && Arc::strong_count(process.files()) > 1 {
        return Err(Errno::EINVAL);
    }
    if flags & CLONE_FS != 0 && Arc::strong_count(process.fs_context()) > 1 {
        return Err(Errno::EINVAL);
    }
    Ok(0)
}

/// `setns`.
///
/// There is no namespace a descriptor could name, so every open descriptor is
/// the wrong kind: `EINVAL`, as Linux answers for a descriptor that is not a
/// namespace. A closed one is `EBADF` first, which is the order Linux checks
/// them in.
pub(crate) fn sys_setns(process: &Process, fd: i32, nstype: u32) -> Result<usize, Errno> {
    let _ = nstype;
    let _open = fd::file(process, fd)?;
    Err(Errno::EINVAL)
}
