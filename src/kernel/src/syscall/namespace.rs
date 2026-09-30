//! `unshare` and `setns`: mount namespaces, and nothing else yet.
//!
//! A process is in one mount namespace, named in its fs context beside its
//! root and working directory, and `unshare(CLONE_NEWNS)` or
//! `clone(CLONE_NEWNS)` gives it a copy of the one it was in
//! (`docs/NAMESPACES.md` §2.1): the same mounts, new ones, so that what it
//! mounts or unmounts after is its own. Every other namespace a program can
//! ask for is answered as a Linux kernel built without it answers, because
//! that is what Ferrix is for those: one pid space, one of everything else. A
//! program that asks for one is told no, and `EINVAL` is the no it already
//! handles -- `CONFIG_*_NS` off is an ordinary configuration, and
//! `unshare(1)` and container runtimes check for it.
//!
//! The two `unshare` flags that are not namespaces are different. Giving up a
//! descriptor table or a working directory shared through `clone(CLONE_FILES)`
//! or `clone(CLONE_FS)` needs no namespace at all, and a process that shares
//! neither has already done it: see [`sys_unshare`].

use alloc::sync::Arc;

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{CLONE_FILES, CLONE_FS};
use ferrix_vfs::Context;

use crate::fallible;
use crate::fs;
use crate::sync::SpinLock;
use crate::syscall::credentials;
use crate::syscall::fd;
use crate::syscall::process::Process;

/// Give the process a mount namespace of its own.
pub(crate) const CLONE_NEWNS: u64 = 0x0002_0000;

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
/// `CLONE_NEWNS` implies `CLONE_FS`, as on Linux, so it is refused the same
/// way in a process whose context is shared -- one of several threads, which
/// share theirs -- and otherwise needs privilege (`EPERM`), and then copies
/// the namespace into the context ([`copy_namespace`]).
///
/// Every other flag names a namespace, or asks to leave a thread group or an
/// address space, and is `EINVAL`.
pub(crate) fn sys_unshare(process: &Process, flags: u64) -> Result<usize, Errno> {
    if flags & !(CLONE_FILES | CLONE_FS | CLONE_NEWNS) != 0 {
        return Err(Errno::EINVAL);
    }
    if flags & CLONE_FILES != 0 && Arc::strong_count(process.files()) > 1 {
        return Err(Errno::EINVAL);
    }
    if flags & (CLONE_FS | CLONE_NEWNS) != 0 && Arc::strong_count(process.fs_context()) > 1 {
        return Err(Errno::EINVAL);
    }
    if flags & CLONE_NEWNS != 0 {
        credentials::require_privilege(process)?;
        copy_namespace(process.fs_context())?;
    }
    Ok(0)
}

/// Put a copy of `context`'s mount namespace in it, with its root and
/// working directory moved to their mounts' copies: what `unshare` and
/// `clone` with `CLONE_NEWNS` do. The copy is charged to the running task's
/// job, the one asking.
///
/// The context is read, the copy made with no lock held, and the context
/// written: nothing else changes it between, since it is either a new
/// child's, not yet running, or one [`sys_unshare`] found unshared. What it
/// held is dropped after its lock is released, since the namespace it named
/// may end there.
///
/// # Errors
///
/// `ENOMEM` for memory or past the job's memory limit; the context is
/// unchanged then.
pub(crate) fn copy_namespace(context: &SpinLock<Context>) -> Result<(), Errno> {
    let (mut root, mut cwd, from) = {
        let context = context.lock();
        (
            context.root.clone(),
            context.cwd.clone(),
            fs::namespace_of(&context),
        )
    };
    let copy = from.copy(&mut [&mut root, &mut cwd])?;
    let copy = fallible::try_arc(copy).map_err(|_| Errno::ENOMEM)?;
    let displaced = {
        let mut context = context.lock();
        (
            core::mem::replace(&mut context.root, root),
            core::mem::replace(&mut context.cwd, cwd),
            context.ns.replace(copy),
        )
    };
    drop((displaced, from));
    Ok(())
}

/// `setns`.
///
/// No descriptor names a namespace yet -- `/proc/<pid>/ns/mnt` is a link to
/// read, not one to open -- so every open descriptor is the wrong kind:
/// `EINVAL`, as Linux answers for a descriptor that is not a namespace. A
/// closed one is `EBADF` first, which is the order Linux checks them in.
pub(crate) fn sys_setns(process: &Process, fd: i32, nstype: u32) -> Result<usize, Errno> {
    let _ = nstype;
    let _open = fd::file(process, fd)?;
    Err(Errno::EINVAL)
}
