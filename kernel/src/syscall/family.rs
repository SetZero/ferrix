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

use alloc::vec;

use ferrix_bootinfo::{Arch, PAGE_SIZE, is_user_address};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::SIGCHLD;

use crate::arch;
use crate::syscall::process::{self, Process};
use crate::syscall::thread::Thread;
use crate::syscall::{registry, uaccess};

/// The low byte of `clone`'s flags: the signal the parent is told with.
const CSIGNAL: u64 = 0xFF;
/// Every flag the legacy `clone` has room for; `clone3` alone has more.
const CLONE_LEGACY_FLAGS: u64 = 0xFFFF_FFFF;
/// Put a pidfd for the child in the parent.
const CLONE_PIDFD: u64 = 0x0000_1000;
/// Make the child its parent's sibling.
const CLONE_PARENT: u64 = 0x0000_8000;
/// Ignored by `clone` since Linux 2.6.2, and refused by `clone3`.
const CLONE_DETACHED: u64 = 0x0040_0000;
/// `clone3` only: reset the child's signal handlers to the default.
const CLONE_CLEAR_SIGHAND: u64 = 0x1_0000_0000;
/// `clone3` only: start the child in the cgroup `cgroup` names.
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;
/// The size of the first published `struct clone_args`, and the least
/// `clone3` accepts.
const CLONE_ARGS_SIZE_VER0: u64 = 64;
/// The size of the latest `struct clone_args` this kernel knows the fields of
/// (`CLONE_ARGS_SIZE_VER2`).
const CLONE_ARGS_KNOWN: usize = 88;
/// The most levels of pid namespace `set_tid` may name (`MAX_PID_NS_LEVEL`).
const MAX_PID_NS_LEVEL: u64 = 32;
/// The highest signal number.
const NSIG: u64 = 64;
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
/// `wait4`: report stopped children too; `WSTOPPED` to `waitid`.
const WUNTRACED: u32 = 2;
/// `waitid`: report children that exited.
const WEXITED: u32 = 4;
/// `wait4`/`waitid`: report continued children too.
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

use crate::syscall::kill::{CLD_CONTINUED, CLD_EXITED, CLD_KILLED, CLD_STOPPED};

/// Bytes in `siginfo_t` on every architecture.
const SIGINFO_BYTES: usize = 128;

/// What a new process is asked for, however the call spelled it.
#[derive(Debug, Clone, Copy, Default)]
struct CloneRequest {
    /// The `CLONE_*` flags, with the exit signal in the low byte.
    flags: u64,
    /// The child's stack pointer, or zero to keep the parent's.
    stack: u64,
    /// Where `CLONE_PARENT_SETTID` writes the child's id.
    parent_tid: u64,
    /// Where `CLONE_CHILD_SETTID` writes it and `CLONE_CHILD_CLEARTID` clears it.
    child_tid: u64,
    /// The thread pointer `CLONE_SETTLS` gives the child.
    tls: u64,
}

/// `clone`, `clone3`, `fork` and `vfork`. Answers the child's pid to the
/// parent; the child answers zero, from a copy of the parent's registers.
///
/// # Errors
///
/// `ENOSYS` for a thread or a pidfd; `ENOMEM` if the address space cannot be
/// copied; `EAGAIN` with every pid in use or no task for the child; `EFAULT`
/// for a bad id pointer; and what [`clone3_request`] refuses.
pub(crate) fn sys_clone(
    parent: &Arc<Process>,
    call: Syscall,
    a: &[u64; 6],
    regs: &arch::UserRegs,
) -> Result<usize, Errno> {
    let request = match call {
        Syscall::Fork => CloneRequest {
            flags: u64::from(SIGCHLD),
            ..CloneRequest::default()
        },
        Syscall::Vfork => CloneRequest {
            flags: CLONE_VM | CLONE_VFORK | u64::from(SIGCHLD),
            ..CloneRequest::default()
        },
        Syscall::Clone3 => clone3_request(parent, a[0], a[1])?,
        // `CONFIG_CLONE_BACKWARDS` on both Arm architectures puts the thread
        // pointer before the child's id pointer; x86-64 has them the other way.
        // The flags are an `unsigned long` Linux narrows to 32 bits, so a
        // 64-bit caller cannot reach `clone3`'s flags through here.
        _ => match arch::ARCH {
            Arch::X86_64 => CloneRequest {
                flags: a[0] & CLONE_LEGACY_FLAGS,
                stack: a[1],
                parent_tid: a[2],
                child_tid: a[3],
                tls: a[4],
            },
            Arch::AArch64 | Arch::Armv7a => CloneRequest {
                flags: a[0] & CLONE_LEGACY_FLAGS,
                stack: a[1],
                parent_tid: a[2],
                tls: a[3],
                child_tid: a[4],
            },
        },
    };
    clone_with(parent, &request, regs)
}

/// Read and check `clone3`'s `struct clone_args`, `size` bytes of it at `at`.
///
/// # The size is a version
///
/// The structure grows at its end, and `size` says which version the caller
/// was built against. Less than the first version is `EINVAL`. More than this
/// kernel knows is fine as long as every byte it does not know is zero -- a
/// newer program asking for nothing new -- and `E2BIG` otherwise, which is
/// how a program learns the kernel is older than the feature it wanted. More
/// than a page is `E2BIG` without looking.
///
/// # Errors
///
/// Those above; `EFAULT` for a structure that cannot be read; `EINVAL` for
/// what Linux's `clone3_args_valid` refuses -- unknown flags, an exit signal
/// both in the flags and in its field, `CLONE_SIGHAND` with
/// `CLONE_CLEAR_SIGHAND`, a stack without a size or a size without a stack;
/// `ENOSYS` for `set_tid` and `CLONE_INTO_CGROUP`, which need pid namespaces
/// and cgroups this kernel does not have.
fn clone3_request(parent: &Process, at: u64, size: u64) -> Result<CloneRequest, Errno> {
    if size > PAGE_SIZE {
        return Err(Errno::E2BIG);
    }
    if size < CLONE_ARGS_SIZE_VER0 {
        return Err(Errno::EINVAL);
    }
    let size = usize::try_from(size).map_err(|_| Errno::EINVAL)?;
    let mut bytes = [0_u8; CLONE_ARGS_KNOWN];
    let known = bytes
        .get_mut(..size.min(CLONE_ARGS_KNOWN))
        .ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(parent.space(), at, known).map_err(|_| Errno::EFAULT)?;
    if let Some(extra) = size
        .checked_sub(CLONE_ARGS_KNOWN)
        .filter(|&extra| extra > 0)
    {
        let mut rest = vec![0_u8; extra];
        let from = at
            .checked_add(CLONE_ARGS_KNOWN as u64)
            .ok_or(Errno::EFAULT)?;
        uaccess::copy_from_user(parent.space(), from, &mut rest).map_err(|_| Errno::EFAULT)?;
        if rest.iter().any(|&byte| byte != 0) {
            return Err(Errno::E2BIG);
        }
    }
    let [
        flags,
        _pidfd,
        child_tid,
        parent_tid,
        exit_signal,
        stack,
        stack_size,
        tls,
        set_tid,
        set_tid_size,
        _cgroup,
    ]: [u64; 11] = core::array::from_fn(|index| {
        bytes
            .get(index * 8..index * 8 + 8)
            .and_then(|field| field.try_into().ok())
            .map_or(0, u64::from_le_bytes)
    });

    if set_tid_size > MAX_PID_NS_LEVEL
        || (set_tid == 0 && set_tid_size > 0)
        || (set_tid != 0 && set_tid_size == 0)
    {
        return Err(Errno::EINVAL);
    }
    if exit_signal > NSIG {
        return Err(Errno::EINVAL);
    }
    if flags & !(CLONE_LEGACY_FLAGS | CLONE_CLEAR_SIGHAND | CLONE_INTO_CGROUP) != 0
        || flags & (CLONE_DETACHED | CSIGNAL) != 0
        || flags & (CLONE_SIGHAND | CLONE_CLEAR_SIGHAND) == CLONE_SIGHAND | CLONE_CLEAR_SIGHAND
        || (flags & (CLONE_THREAD | CLONE_PARENT) != 0 && exit_signal != 0)
    {
        return Err(Errno::EINVAL);
    }
    // A stack is its lowest address and a size, where `clone` took its top.
    let stack_top = match (stack, stack_size) {
        (0, 0) => 0,
        (0, _) | (_, 0) => return Err(Errno::EINVAL),
        (base, size) => {
            let top = base.checked_add(size).ok_or(Errno::EINVAL)?;
            if !is_user_address(base) || !is_user_address(top - 1) {
                return Err(Errno::EINVAL);
            }
            top
        }
    };
    if set_tid != 0 || flags & CLONE_INTO_CGROUP != 0 {
        return Err(Errno::ENOSYS);
    }
    Ok(CloneRequest {
        // The exit signal has a field of its own here and the low byte of the
        // flags in `clone`; checked above to be a signal and the byte to be
        // clear.
        flags: flags | exit_signal,
        stack: stack_top,
        parent_tid,
        child_tid,
        tls,
    })
}

/// Make the process `request` asks for. See [`sys_clone`].
fn clone_with(
    parent: &Arc<Process>,
    request: &CloneRequest,
    regs: &arch::UserRegs,
) -> Result<usize, Errno> {
    let CloneRequest {
        flags,
        stack,
        parent_tid,
        child_tid,
        tls,
    } = *request;
    if flags & (CLONE_THREAD | CLONE_SIGHAND) != 0 {
        return Err(Errno::ENOSYS);
    }
    if flags & CLONE_VM != 0 && flags & CLONE_VFORK == 0 {
        return Err(Errno::ENOSYS);
    }
    // No pidfds yet. Refused rather than ignored: the caller would read a
    // descriptor number out of memory nothing wrote.
    if flags & CLONE_PIDFD != 0 {
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
    // What glibc's `posix_spawn` asks for, so that its child need not reset
    // every handler itself before `execve`. Linux leaves the alternate stack
    // alone here and the exec reset takes it; the child is about to `execve`,
    // where it goes anyway.
    if flags & CLONE_CLEAR_SIGHAND != 0 {
        child.with_signals(crate::syscall::signal::Signals::reset_for_exec);
    }

    let id = pid.to_le_bytes();
    if flags & CLONE_PARENT_SETTID != 0 {
        uaccess::copy_to_user(parent.space(), parent_tid, &id).map_err(|_| Errno::EFAULT)?;
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        uaccess::copy_to_user(child.space(), child_tid, &id).map_err(|_| Errno::EFAULT)?;
    }
    // The child's one thread, made here so that the address it is to clear
    // when it ends is recorded before it can run.
    let thread = Arc::new(Thread::leader(&child));
    if flags & CLONE_CHILD_CLEARTID != 0 {
        let _ = thread.set_clear_child_tid(child_tid);
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
    if process::start_forked(thread, state).is_err() {
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

/// What a wait found a child to have done.
#[derive(Debug)]
enum Change {
    /// Ended.
    Ended(Arc<Process>),
    /// Stopped, for this signal.
    Stopped(Arc<Process>, u32),
    /// Continued.
    Continued(Arc<Process>),
}

impl Change {
    /// The child.
    fn child(&self) -> &Arc<Process> {
        match self {
            Change::Ended(child) | Change::Stopped(child, _) | Change::Continued(child) => child,
        }
    }
}

/// Wait until a child `select` accepts has ended (with `WEXITED`), stopped
/// (with `WUNTRACED`) or continued (with `WCONTINUED`), then take what it did
/// (or leave it, for `WNOWAIT`). `None` with `WNOHANG` when none has yet.
///
/// # Errors
///
/// `ECHILD` with no child `select` accepts; `EINTR` when a signal the caller
/// does not block arrives first, or the caller is ended.
fn wait_for_child(
    process: &Process,
    select: &dyn Fn(&Process) -> bool,
    options: u32,
    remove: bool,
) -> Result<Option<Change>, Errno> {
    let exits = options & WEXITED != 0;
    let stops = options & WUNTRACED != 0;
    let continues = options & WCONTINUED != 0;
    loop {
        let ended = process.reap_child(select, remove && exits)?;
        if exits && let Some(child) = ended {
            return Ok(Some(Change::Ended(child)));
        }
        if let Some((child, signal)) = process.changed_child(select, stops, continues, remove) {
            return Ok(Some(match signal {
                0 => Change::Continued(child),
                signal => Change::Stopped(child, signal),
            }));
        }
        if options & WNOHANG != 0 {
            return Ok(None);
        }
        if process.signal_pending() {
            // A restart code, not `EINTR`: `wait4` restarts under `SA_RESTART`.
            // The way back turns it into `EINTR` when the handler lacks it.
            return Err(Errno::ERESTARTSYS);
        }
        let _ = process.child_exited().wait_until_deadline(
            || {
                process.signal_pending()
                    || (exits && process.has_ended_child(select))
                    || process
                        .changed_child(select, stops, continues, false)
                        .is_some()
            },
            u64::MAX,
        );
    }
}

/// `wait4`.
///
/// # Errors
///
/// `EINVAL` for an unknown option; `ECHILD` with no child to wait for; `EINTR`
/// if a signal arrives or the caller is ended while it waits; `EFAULT` for a
/// bad pointer.
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
    let Some(change) = wait_for_child(process, &select, options | WEXITED, true)? else {
        return Ok(0);
    };
    let child = change.child();
    if wstatus != 0 {
        // The status word: an exit or a killing signal as `wait_status` has
        // it; a stop as the signal in the second byte over 0x7f; a continue
        // as 0xffff.
        let status = match &change {
            Change::Ended(_) => child.wait_status().unwrap_or(0),
            Change::Stopped(_, signal) => ((*signal as i32) << 8) | 0x7f,
            Change::Continued(_) => 0xffff,
        };
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
/// As [`sys_wait4`], and `EINVAL` without one of `WEXITED`, `WSTOPPED` and
/// `WCONTINUED`, or for an unknown `idtype`.
pub(crate) fn sys_waitid(
    process: &Process,
    idtype: u32,
    id: u32,
    infop: u64,
    options: u32,
    rusage: u64,
) -> Result<usize, Errno> {
    if options & (WEXITED | WUNTRACED | WCONTINUED) == 0
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
        if let Some(change) = &found {
            let child = change.child();
            let (code, status) = match (change, child.ended_by_signal()) {
                (Change::Stopped(_, signal), _) => (CLD_STOPPED, *signal as i32),
                (Change::Continued(_), _) => {
                    (CLD_CONTINUED, ferrix_linux_abi::types::SIGCONT as i32)
                }
                (Change::Ended(_), Some(signal)) => (CLD_KILLED, signal as i32),
                (Change::Ended(_), None) => (CLD_EXITED, child.exit_status().unwrap_or(0) & 0xFF),
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
