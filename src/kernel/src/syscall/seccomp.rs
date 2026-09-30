//! `seccomp(2)`: a process may give up the system calls it will ever make
//! again, or ask that a filter judge each one.
//!
//! # What a process holds
//!
//! A [`State`] on the [`Process`]: a mode, and the filters installed, newest
//! first. A fork child takes its parent's, sharing the filters themselves, and
//! `execve` keeps them: that is what makes a filter a sandbox and not a
//! request. A filter is charged to the job that installed it (F-37), by its
//! instructions and its record, and stays charged until the last process
//! holding it ends.
//!
//! Linux holds a filter per thread, and `SECCOMP_FILTER_FLAG_TSYNC` copies it
//! to the others. Ferrix holds one per process, which is `TSYNC` always: a
//! filter installed by one thread judges every thread of its process. A
//! process cannot be left with a thread that escapes its filter, which is the
//! failure `TSYNC` exists to prevent; a program that wants a filter on one
//! thread alone has no way to ask for it.
//!
//! # What it is judged by
//!
//! Every Linux call made by a process with a filter is judged before it is
//! decoded further, against the number, the architecture's audit value, the
//! instruction pointer and the six raw argument registers, as Linux's
//! `seccomp_data` has them: for an i386 program, the i386 number and the 32-bit
//! registers. The answer is the most restrictive of all the filters' (see
//! [`ferrix_seccomp::run_all`]). `ALLOW` and `LOG` let the call through;
//! `ERRNO` fails it with the data as its errno (a data of zero succeeds without
//! running it); `TRAP` sends a `SIGSYS` that cannot be blocked or ignored, with
//! `si_code` `SYS_SECCOMP`; `KILL_THREAD` and `KILL_PROCESS` end the process
//! with `SIGSYS` (one thread is all a process has here); `TRACE` and
//! `USER_NOTIF` answer `ENOSYS`, since there is no tracer and no supervisor,
//! as Linux does without them. An action nobody defined is `KILL_PROCESS`.
//!
//! Strict mode (`SECCOMP_MODE_STRICT`) allows `read`, `write`, `exit` and
//! `sigreturn`, and ends the process with `SIGKILL` at anything else.
//!
//! # Who may install one
//!
//! A process that has set `PR_SET_NO_NEW_PRIVS`, or holds `CAP_SYS_ADMIN` in
//! its own user namespace: otherwise a set-id program could be run under a
//! filter that makes it misbehave as a privileged process.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_kmem::{Charge, arc_footprint, buffer_footprint};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::nr::Syscall;
use ferrix_seccomp::{ACTION_FULL, DATA, Insn, MAX_INSNS, MAX_INSNS_PER_PATH};

use crate::arch;
use crate::syscall::attributes;
use crate::syscall::deliver;
use crate::syscall::process::{self, Process};
use crate::syscall::signal::Origin;
use crate::syscall::uaccess;
use crate::syscall::userns::CAP_SYS_ADMIN;
use crate::trap::{Abi, Outcome, SyscallArgs};

/// `SECCOMP_MODE_DISABLED`.
pub(crate) const MODE_DISABLED: u8 = 0;
/// `SECCOMP_MODE_STRICT`.
pub(crate) const MODE_STRICT: u8 = 1;
/// `SECCOMP_MODE_FILTER`.
pub(crate) const MODE_FILTER: u8 = 2;

/// `seccomp`'s operations.
const SET_MODE_STRICT: u32 = 0;
/// See [`SET_MODE_STRICT`].
const SET_MODE_FILTER: u32 = 1;
/// See [`SET_MODE_STRICT`].
const GET_ACTION_AVAIL: u32 = 2;

/// `seccomp`'s filter flags: the ones that are accepted.
const FLAG_TSYNC: u64 = 1 << 0;
/// Log what the filter does; accepted and not acted on.
const FLAG_LOG: u64 = 1 << 1;
/// Do not turn on the speculation mitigation; accepted, Ferrix has none to
/// leave on.
const FLAG_SPEC_ALLOW: u64 = 1 << 2;
/// A notification descriptor, which Ferrix cannot give.
const FLAG_NEW_LISTENER: u64 = 1 << 3;
/// `TSYNC`, answering `ESRCH` rather than the thread's id on a failure.
const FLAG_TSYNC_ESRCH: u64 = 1 << 4;
/// Every flag Linux defines but the one added last, which is refused with them.
const FLAGS_KNOWN: u64 =
    FLAG_TSYNC | FLAG_LOG | FLAG_SPEC_ALLOW | FLAG_NEW_LISTENER | FLAG_TSYNC_ESRCH;

/// `SYS_SECCOMP`: `si_code` of the `SIGSYS` a `TRAP` raises.
pub(crate) const SYS_SECCOMP: i32 = 1;

/// `SIGSYS`.
const SIGSYS: u32 = 31;
/// `SIGKILL`.
const SIGKILL: u32 = 9;

/// The largest errno a filter's data may carry.
const MAX_ERRNO: u32 = 4095;

/// One installed filter, charged to the job that installed it.
#[derive(Debug)]
pub(crate) struct Filter {
    /// The program, validated.
    insns: Vec<Insn>,
    /// Its heap, charged to the job that installed it (F-37).
    _charge: Charge,
}

/// What a process holds of seccomp.
#[derive(Debug, Clone)]
pub(crate) struct State {
    /// `SECCOMP_MODE_*`.
    mode: u8,
    /// The filters, newest first.
    filters: Vec<Arc<Filter>>,
    /// Instructions across them.
    insns: usize,
}

impl State {
    /// No seccomp.
    pub(crate) const fn new() -> State {
        State {
            mode: MODE_DISABLED,
            filters: Vec::new(),
            insns: 0,
        }
    }

    /// `SECCOMP_MODE_*`.
    pub(crate) const fn mode(&self) -> u8 {
        self.mode
    }
}

/// The `AUDIT_ARCH_*` a call made through `abi` is reported as.
const fn audit_arch(abi: Abi) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        match abi {
            Abi::Native => 0xc000_003e,
            Abi::Compat => 0x4000_0003,
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let _ = abi;
        0xc000_00b7
    }
    #[cfg(target_arch = "arm")]
    {
        let _ = abi;
        0x4000_0028
    }
}

/// Judge the call `args` names for `process` before it runs: `None` lets it
/// through, `Some` is the outcome to answer instead (or a process ended).
///
/// `ip` is the instruction pointer of the program's system call, for the
/// filter's `instruction_pointer` and a `TRAP`'s `si_call_addr`.
pub(crate) fn enforce(
    process: &Process,
    call: Syscall,
    args: &SyscallArgs,
    ip: u64,
) -> Option<Outcome> {
    let mode = process.seccomp_mode();
    if mode == MODE_DISABLED {
        return None;
    }
    if mode == MODE_STRICT {
        let allowed = matches!(
            call,
            Syscall::Read
                | Syscall::Write
                | Syscall::Exit
                | Syscall::ExitGroup
                | Syscall::RtSigreturn
                | Syscall::Sigreturn
        );
        if allowed {
            return None;
        }
        process::kill(process, 128 + SIGKILL as i32);
        return Some(Outcome::Return(Errno::EPERM.as_return_value()));
    }
    let filters: Vec<Arc<Filter>> = process.with_seccomp(|state| state.filters.clone());
    let arch = audit_arch(args.abi);
    let data = ferrix_seccomp::data(args.number as i32, arch, ip, args.args);
    let result = ferrix_seccomp::run_all(filters.iter().map(|filter| &filter.insns[..]), &data);
    let data_half = result & DATA;
    match result & ACTION_FULL {
        ferrix_seccomp::ALLOW | ferrix_seccomp::LOG => None,
        ferrix_seccomp::ERRNO => {
            let errno = data_half.min(MAX_ERRNO) as u16;
            Some(Outcome::Return(if errno == 0 {
                0
            } else {
                Errno(errno).as_return_value()
            }))
        }
        ferrix_seccomp::TRAP => {
            let origin = Origin::Seccomp {
                errno: data_half as i32,
                call_addr: ip,
                syscall: args.number as i32,
                arch,
            };
            let _ = deliver::force(SIGSYS, origin);
            Some(Outcome::Return(Errno::ENOSYS.as_return_value()))
        }
        // No tracer and no supervisor: what Linux answers without them.
        ferrix_seccomp::TRACE | ferrix_seccomp::USER_NOTIF => {
            Some(Outcome::Return(Errno::ENOSYS.as_return_value()))
        }
        // KILL_THREAD, KILL_PROCESS, and every action nobody defined.
        _ => {
            process::kill(process, 128 + SIGSYS as i32);
            Some(Outcome::Return(Errno::ENOSYS.as_return_value()))
        }
    }
}

/// `seccomp(operation, flags, uargs)`.
///
/// # Errors
///
/// As Linux's: `EINVAL` for an operation, a flag or a filter it refuses,
/// `EACCES` without `no_new_privs` or `CAP_SYS_ADMIN`, `EFAULT`, `ENOMEM`,
/// `EOPNOTSUPP` for an action that is not available.
pub(crate) fn sys_seccomp(
    process: &Process,
    operation: u32,
    flags: u64,
    uargs: u64,
    abi: Abi,
) -> Result<usize, Errno> {
    match operation {
        SET_MODE_STRICT => {
            if flags != 0 || uargs != 0 {
                return Err(Errno::EINVAL);
            }
            set_strict(process)
        }
        SET_MODE_FILTER => set_filter(process, flags, uargs, abi),
        GET_ACTION_AVAIL => {
            if flags != 0 {
                return Err(Errno::EINVAL);
            }
            let action = uaccess::get_u32(process.space(), uargs)?;
            match action {
                ferrix_seccomp::KILL_PROCESS
                | ferrix_seccomp::KILL_THREAD
                | ferrix_seccomp::TRAP
                | ferrix_seccomp::ERRNO
                | ferrix_seccomp::LOG
                | ferrix_seccomp::ALLOW => Ok(0),
                _ => Err(Errno::EOPNOTSUPP),
            }
        }
        _ => Err(Errno::EINVAL),
    }
}

/// `prctl(PR_SET_SECCOMP, mode, filter)`.
pub(crate) fn prctl_set(
    process: &Process,
    mode: u64,
    filter: u64,
    abi: Abi,
) -> Result<usize, Errno> {
    match mode {
        1 => set_strict(process),
        2 => set_filter(process, 0, filter, abi),
        _ => Err(Errno::EINVAL),
    }
}

/// `SECCOMP_SET_MODE_STRICT`: refused once filters are in.
fn set_strict(process: &Process) -> Result<usize, Errno> {
    process.with_seccomp(|state| match state.mode {
        MODE_FILTER => Err(Errno::EINVAL),
        _ => {
            state.mode = MODE_STRICT;
            Ok(())
        }
    })?;
    process.publish_seccomp();
    Ok(0)
}

/// `SECCOMP_SET_MODE_FILTER`.
fn set_filter(process: &Process, flags: u64, uargs: u64, abi: Abi) -> Result<usize, Errno> {
    if flags & !FLAGS_KNOWN != 0 || flags & FLAG_NEW_LISTENER != 0 {
        return Err(Errno::EINVAL);
    }
    let permitted = attributes::get(process).no_new_privs
        || process.with_credentials(|held| held.holds(CAP_SYS_ADMIN));
    if !permitted {
        return Err(Errno::EACCES);
    }
    let program = read_program(process, uargs, abi)?;
    ferrix_seccomp::validate(&program).map_err(|_| Errno::EINVAL)?;
    let filter = charged(program)?;
    process.with_seccomp(|state| {
        if state.mode == MODE_STRICT {
            return Err(Errno::EINVAL);
        }
        // Linux counts each filter's instructions and four more for its
        // overhead against the path's budget.
        let total = state
            .insns
            .saturating_add(filter.insns.len())
            .saturating_add(4);
        if total > MAX_INSNS_PER_PATH {
            return Err(Errno::ENOMEM);
        }
        state.filters.try_reserve(1).map_err(|_| Errno::ENOMEM)?;
        state.filters.insert(0, filter.clone());
        state.insns = total;
        state.mode = MODE_FILTER;
        Ok(())
    })?;
    process.publish_seccomp();
    Ok(0)
}

/// `program` as a filter, its heap charged to the running task's job (F-37).
///
/// # Errors
///
/// `ENOMEM`, past the job's memory limit or the machine's.
pub(crate) fn charged(program: Vec<Insn>) -> Result<Arc<Filter>, Errno> {
    let charge = Charge::bytes(
        arc_footprint::<Filter>().saturating_add(buffer_footprint::<Insn>(program.len())),
    )
    .map_err(|_| Errno::ENOMEM)?;
    crate::fallible::try_arc(Filter {
        insns: program,
        _charge: charge,
    })
    .map_err(|_| Errno::ENOMEM)
}

/// The filter a `struct sock_fprog` at `uargs` names: a 16-bit length, and a
/// pointer to that many eight-byte instructions. The structure is 16 bytes
/// with the pointer at 8 for a 64-bit program, and 8 with it at 4 for a 32-bit
/// one.
fn read_program(process: &Process, uargs: u64, abi: Abi) -> Result<Vec<Insn>, Errno> {
    let wide = size_of::<usize>() == 8 && abi == Abi::Native;
    let mut head = [0_u8; 16];
    let used = head
        .get_mut(..if wide { 16 } else { 8 })
        .ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), uargs, used).map_err(|_| Errno::EFAULT)?;
    let length = usize::from(u16::from_le_bytes([head[0], head[1]]));
    let pointer = if wide {
        u64::from_le_bytes(head[8..16].try_into().map_err(|_| Errno::EINVAL)?)
    } else {
        u64::from(u32::from_le_bytes(
            head[4..8].try_into().map_err(|_| Errno::EINVAL)?,
        ))
    };
    if length == 0 || length > MAX_INSNS {
        return Err(Errno::EINVAL);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length * ferrix_seccomp::INSN_BYTES)
        .map_err(|_| Errno::ENOMEM)?;
    bytes.resize(length * ferrix_seccomp::INSN_BYTES, 0);
    uaccess::copy_from_user(process.space(), pointer, &mut bytes).map_err(|_| Errno::EFAULT)?;
    let mut program = Vec::new();
    program
        .try_reserve_exact(length)
        .map_err(|_| Errno::ENOMEM)?;
    program.extend(
        bytes
            .chunks_exact(ferrix_seccomp::INSN_BYTES)
            .filter_map(Insn::from_bytes),
    );
    Ok(program)
}

/// The instruction pointer of the system call `regs` was taken at, or zero for
/// a caller that has no registers.
pub(crate) fn instruction_pointer(regs: Option<&arch::UserRegs>) -> u64 {
    regs.map_or(0, arch::UserRegs::instruction_pointer)
}
