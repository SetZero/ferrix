//! Processes making processes, and waiting for them: `clone`, `fork`, `vfork`,
//! `wait4`, `waitid`, and the process groups and sessions a shell's job control
//! is built on.
//!
//! # One thread per process
//!
//! A child made here is a new process with a copy of its parent's address
//! space and a task of its own. `clone` asking for a new *thread* -- sharing
//! the address space without `CLONE_VFORK`, or `CLONE_THREAD` -- is refused
//! with `ENOSYS`, honestly: busybox never asks, and a thread is a task sharing
//! a process, which is a change to what a process is rather than a flag here.
//!
//! # `vfork` copies
//!
//! Linux's `vfork` child borrows its parent's memory until it calls `execve`
//! or exits, and the parent sleeps meanwhile. Here the child gets a
//! copy-on-write copy instead, and the parent still sleeps until the child
//! calls `execve` or ends. Everything a correct `vfork` child may do -- call
//! `execve` or `_exit` -- behaves the same. What differs is what POSIX already
//! calls undefined: a child writing to memory its parent then reads, which
//! glibc's `posix_spawn` does to report an `execve` failure. There the parent
//! sees success and the child exits with 127, as a shell would report anyway.

use alloc::sync::Arc;

use ferrix_bootinfo::Arch;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::SIGCHLD;

use crate::arch;
use crate::syscall::process::{self, Process};
use crate::syscall::{registry, uaccess};

/// The low byte of `clone`'s flags: the signal the parent is told with.
const CSIGNAL: u64 = 0xFF;
/// Share the address space.
const CLONE_VM: u64 = 0x0000_0100;
/// Share the working directory and root.
const CLONE_FS: u64 = 0x0000_0200;
/// Share the file descriptor table.
const CLONE_FILES: u64 = 0x0000_0400;
/// Share signal handlers.
const CLONE_SIGHAND: u64 = 0x0000_0800;
/// The parent sleeps until the child calls `execve` or ends.
const CLONE_VFORK: u64 = 0x0000_4000;
/// Make a thread of the same process.
const CLONE_THREAD: u64 = 0x0001_0000;
/// Give the child this thread pointer.
const CLONE_SETTLS: u64 = 0x0008_0000;
/// Write the child's id into the parent's memory.
const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
/// Have the child's id cleared in its memory when it ends.
const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
/// Write the child's id into the child's memory.
const CLONE_CHILD_SETTID: u64 = 0x0100_0000;

/// `wait4`: return at once if nothing has ended.
const WNOHANG: u32 = 1;
/// `wait4`: report stopped children too. Nothing stops yet, so accepted.
const WUNTRACED: u32 = 2;
/// `waitid`: report children that exited.
const WEXITED: u32 = 4;
/// `wait4`/`waitid`: report continued children too. Accepted, never matched.
const WCONTINUED: u32 = 8;
/// `waitid`: leave the child waitable.
const WNOWAIT: u32 = 0x0100_0000;
/// Linux's thread-selection bits, accepted on `wait4` and meaningless with one
/// thread per process.
const WAIT_THREAD_BITS: u32 = 0xE000_0000;

/// `waitid`'s `idtype`: any child.
const P_ALL: u32 = 0;
/// `waitid`'s `idtype`: the child with this pid.
const P_PID: u32 = 1;
/// `waitid`'s `idtype`: any child in this process group.
const P_PGID: u32 = 2;

/// `si_code` for a child that exited.
const CLD_EXITED: i32 = 1;
/// `si_code` for a child a signal ended.
const CLD_KILLED: i32 = 2;

/// Bytes in `siginfo_t` on every architecture.
const SIGINFO_BYTES: usize = 128;

/// `clone`, `fork` and `vfork`. Answers the child's pid to the parent; the
/// child answers zero, from a copy of the parent's registers.
///
/// # Errors
///
/// `ENOSYS` for a thread; `ENOMEM` if the address space cannot be copied;
/// `EAGAIN` with every pid in use or no task for the child; `EFAULT` for a bad
/// id pointer.
pub(crate) fn sys_clone(
    parent: &Arc<Process>,
    call: Syscall,
    a: &[u64; 6],
    regs: &arch::UserRegs,
) -> Result<usize, Errno> {
    let (flags, stack, parent_tid, child_tid, tls) = match call {
        Syscall::Fork => (u64::from(SIGCHLD), 0, 0, 0, 0),
        Syscall::Vfork => (CLONE_VM | CLONE_VFORK | u64::from(SIGCHLD), 0, 0, 0, 0),
        // `CONFIG_CLONE_BACKWARDS` on both Arm architectures puts the thread
        // pointer before the child's id pointer; x86-64 has them the other way.
        _ => match arch::ARCH {
            Arch::X86_64 => (a[0], a[1], a[2], a[3], a[4]),
            Arch::AArch64 | Arch::Armv7a => (a[0], a[1], a[2], a[4], a[3]),
        },
    };
    if flags & (CLONE_THREAD | CLONE_SIGHAND) != 0 {
        return Err(Errno::ENOSYS);
    }
    if flags & CLONE_VM != 0 && flags & CLONE_VFORK == 0 {
        return Err(Errno::ENOSYS);
    }

    let space = parent.space().fork().map_err(|_| Errno::ENOMEM)?;
    let child = registry::register(Process::forked(
        parent,
        space,
        flags & CLONE_FILES != 0,
        flags & CLONE_FS != 0,
    ));
    let pid = child.pid();
    if pid == 0 {
        return Err(Errno::EAGAIN);
    }
    child.set_exit_signal((flags & CSIGNAL) as u32);

    let id = pid.to_le_bytes();
    if flags & CLONE_PARENT_SETTID != 0 {
        uaccess::copy_to_user(parent.space(), parent_tid, &id).map_err(|_| Errno::EFAULT)?;
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        uaccess::copy_to_user(child.space(), child_tid, &id).map_err(|_| Errno::EFAULT)?;
    }
    if flags & CLONE_CHILD_CLEARTID != 0 {
        let _ = child.set_clear_child_tid(child_tid, pid as usize);
    }

    // The child starts with its parent's registers as they are right now, in
    // this system call: its thread pointer and floating-point state, and the
    // saved frame with the return value made zero.
    // SAFETY: inside the parent's own system call, so the live user registers
    // are the parent's.
    let mut state = unsafe { arch::UserState::capture() };
    if flags & CLONE_SETTLS != 0 {
        state.set_thread_pointer(tls);
    }
    let mut child_regs = regs.for_child();
    if stack != 0 {
        child_regs.set_stack(&mut state, stack);
    }
    child.set_resume(child_regs);

    parent.adopt(Arc::clone(&child));
    if process::start_forked(&child, state).is_err() {
        parent.disown(&child);
        return Err(Errno::EAGAIN);
    }
    if flags & CLONE_VFORK != 0 {
        child.wait_vfork_release(parent);
    }
    Ok(pid as usize)
}

/// Which children a wait is for, decoded from `wait4`'s `pid`.
fn wait4_selector(process: &Process, pid: i32) -> impl Fn(&Process) -> bool {
    let own_group = process.pgid();
    move |child: &Process| match pid {
        -1 => true,
        0 => child.pgid() == own_group,
        pid if pid > 0 => child.pid() == pid.unsigned_abs(),
        pid => child.pgid() == pid.unsigned_abs(),
    }
}

/// Wait until a child `select` accepts has ended, then take it (or leave it,
/// for `WNOWAIT`). `None` with `WNOHANG` when none has yet.
fn wait_for_child(
    process: &Process,
    select: &dyn Fn(&Process) -> bool,
    options: u32,
    remove: bool,
) -> Result<Option<Arc<Process>>, Errno> {
    loop {
        if let Some(child) = process.reap_child(select, remove)? {
            return Ok(Some(child));
        }
        if options & WNOHANG != 0 {
            return Ok(None);
        }
        let _ = process.child_exited().wait_until_deadline(
            || process.is_terminated() || process.has_ended_child(select),
            u64::MAX,
        );
        if process.is_terminated() {
            return Err(Errno::EINTR);
        }
    }
}

/// `wait4`.
///
/// # Errors
///
/// `EINVAL` for an unknown option; `ECHILD` with no child to wait for; `EINTR`
/// if the caller itself is ended while it waits; `EFAULT` for a bad pointer.
pub(crate) fn sys_wait4(
    process: &Process,
    pid: i32,
    wstatus: u64,
    options: u32,
    rusage: u64,
) -> Result<usize, Errno> {
    if options & !(WNOHANG | WUNTRACED | WCONTINUED | WAIT_THREAD_BITS) != 0 {
        return Err(Errno::EINVAL);
    }
    let select = wait4_selector(process, pid);
    let Some(child) = wait_for_child(process, &select, options, true)? else {
        return Ok(0);
    };
    if wstatus != 0 {
        let status = child.wait_status().unwrap_or(0);
        uaccess::copy_to_user(process.space(), wstatus, &status.to_le_bytes())
            .map_err(|_| Errno::EFAULT)?;
    }
    if rusage != 0 {
        zero_rusage(process, rusage)?;
    }
    Ok(child.pid() as usize)
}

/// `waitid`.
///
/// # Errors
///
/// As [`sys_wait4`], and `EINVAL` without `WEXITED` or for an unknown `idtype`.
pub(crate) fn sys_waitid(
    process: &Process,
    idtype: u32,
    id: u32,
    infop: u64,
    options: u32,
    rusage: u64,
) -> Result<usize, Errno> {
    if options & WEXITED == 0
        || options & !(WNOHANG | WEXITED | WUNTRACED | WCONTINUED | WNOWAIT | WAIT_THREAD_BITS) != 0
    {
        return Err(Errno::EINVAL);
    }
    let select = move |child: &Process| match idtype {
        P_ALL => true,
        P_PID => child.pid() == id,
        P_PGID => child.pgid() == id,
        _ => false,
    };
    if !matches!(idtype, P_ALL | P_PID | P_PGID) {
        return Err(Errno::EINVAL);
    }
    let found = wait_for_child(process, &select, options, options & WNOWAIT == 0)?;
    if infop != 0 {
        let mut info = [0_u8; SIGINFO_BYTES];
        if let Some(child) = &found {
            let (code, status) = match child.ended_by_signal() {
                Some(signal) => (CLD_KILLED, signal as i32),
                None => (CLD_EXITED, child.exit_status().unwrap_or(0) & 0xFF),
            };
            // `si_signo`, `si_errno`, `si_code`, then the union, which starts
            // at the first pointer-aligned offset: 16 on 64-bit, 12 on 32-bit.
            let union = if size_of::<usize>() == 8 { 16 } else { 12 };
            put_i32(&mut info, 0, SIGCHLD as i32)?;
            put_i32(&mut info, 8, code)?;
            put_i32(&mut info, union, child.pid() as i32)?;
            put_i32(&mut info, union + 8, status)?;
        }
        uaccess::copy_to_user(process.space(), infop, &info).map_err(|_| Errno::EFAULT)?;
    }
    if rusage != 0 {
        zero_rusage(process, rusage)?;
    }
    Ok(0)
}

/// Write `value` into `buffer` at `at`.
fn put_i32(buffer: &mut [u8], at: usize, value: i32) -> Result<(), Errno> {
    buffer
        .get_mut(at..at + 4)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Write an empty `struct rusage`: eighteen native words. Nothing accounts a
/// process's resource use yet, and zero is what a process that used none would
/// be told.
fn zero_rusage(process: &Process, at: u64) -> Result<(), Errno> {
    let bytes = [0_u8; 18 * 8];
    let size = 18 * size_of::<usize>();
    uaccess::copy_to_user(process.space(), at, bytes.get(..size).ok_or(Errno::EINVAL)?)
        .map_err(|_| Errno::EFAULT)
}

/// `setpgid`.
///
/// # Errors
///
/// `ESRCH` if `pid` is neither the caller nor one of its children; `EINVAL`
/// for a negative group; `EPERM` for a session leader, or a group that does not
/// exist in the caller's session.
pub(crate) fn sys_setpgid(process: &Process, pid: i32, pgid: i32) -> Result<usize, Errno> {
    if pgid < 0 {
        return Err(Errno::EINVAL);
    }
    let target = if pid == 0 || pid.unsigned_abs() == process.pid() {
        None
    } else if pid > 0 && process.has_child(pid.unsigned_abs()) {
        Some(registry::find(pid.unsigned_abs()).ok_or(Errno::ESRCH)?)
    } else {
        return Err(Errno::ESRCH);
    };
    let target_process: &Process = target.as_deref().unwrap_or(process);
    let group = if pgid == 0 {
        target_process.pid()
    } else {
        pgid.unsigned_abs()
    };
    if target_process.sid() == target_process.pid() {
        return Err(Errno::EPERM);
    }
    if group != target_process.pid() {
        let exists = registry::live()
            .iter()
            .any(|other| other.pgid() == group && other.sid() == process.sid());
        if !exists {
            return Err(Errno::EPERM);
        }
    }
    target_process.set_pgid(group);
    Ok(0)
}

/// `getpgid`: the process group of `pid`, or of the caller for zero.
///
/// # Errors
///
/// `ESRCH` for a pid no process has.
pub(crate) fn sys_getpgid(process: &Process, pid: i32) -> Result<usize, Errno> {
    if pid == 0 {
        return Ok(process.pgid() as usize);
    }
    let other = registry::find(pid.unsigned_abs()).ok_or(Errno::ESRCH)?;
    Ok(other.pgid() as usize)
}

/// `getsid`: the session of `pid`, or of the caller for zero.
///
/// # Errors
///
/// `ESRCH` for a pid no process has.
pub(crate) fn sys_getsid(process: &Process, pid: i32) -> Result<usize, Errno> {
    if pid == 0 {
        return Ok(process.sid() as usize);
    }
    let other = registry::find(pid.unsigned_abs()).ok_or(Errno::ESRCH)?;
    Ok(other.sid() as usize)
}

/// `setsid`: lead a new session and process group.
///
/// # Errors
///
/// `EPERM` if the caller already leads a process group, which is what stops a
/// group leader from leaving its members in a session it no longer belongs to.
pub(crate) fn sys_setsid(process: &Process) -> Result<usize, Errno> {
    let leads_a_group = registry::live()
        .iter()
        .any(|other| other.pgid() == process.pid());
    if leads_a_group {
        return Err(Errno::EPERM);
    }
    process.lead_new_session();
    Ok(process.pid() as usize)
}
