//! The Linux system call dispatch layer.
//!
//! Stage 7 of `docs/ROADMAP.md`. Everything below this point is the kernel
//! talking to itself; this is where a program that was not written for Ferrix
//! asks it for something, using the numbers and the conventions Linux fixed.
//!
//! # The seam
//!
//! One function, [`dispatch`], agreed with the stage 6 owner so that neither
//! side has to know the other's job. Their trap vector saves registers, fills
//! a [`SyscallArgs`] from the frame, and calls it. It returns an [`Outcome`]
//! which their code applies. That puts every register convention on their side
//! of the line and every ABI decision on this one, and it is the reason
//! `SyscallArgs` has public fields, no constructor and nothing fallible in it:
//! a trampoline that has already switched stacks must not meet a `Result`.
//!
//! [`Outcome`] has two variants rather than being a bare `isize` because "put
//! this in the return register" does not describe every call. `execve` and a
//! freshly created `clone` child both resume on a register frame that was
//! *constructed* rather than returned into, so there is nothing to return.
//! Saying that as data — an entry point and a stack pointer — keeps this layer
//! free of any architecture's `TrapFrame`.
//!
//! # Three number tables, one dispatch
//!
//! x86-64, AArch64 and ARMv7-A each number their calls differently, and
//! `libs/linux-abi` folds all three onto one [`Syscall`]. Which table applies
//! is the one architecture-dependent fact here, so it is asked of the facade
//! ([`arch::decode_syscall`]) rather than decided with a `cfg` — generic kernel
//! code naming an architecture is what `scripts/check-crate-layering.sh`
//! exists to stop.
//!
//! # What answers today
//!
//! The calls that need no process state: identity, and yielding. Everything
//! else returns `ENOSYS`, which is a real answer rather than a placeholder —
//! it is what Linux returns for a call it does not implement, and a program
//! that gets it can fall back. The alternative, a handler that pretends to
//! succeed, is how a program ends up wrong much later for reasons nobody can
//! trace back here.

pub(crate) mod attributes;
pub(crate) mod check;
pub(crate) mod credentials;
pub(crate) mod deliver;
pub(crate) mod exec;
pub(crate) mod family;
pub(crate) mod fd;
pub(crate) mod file;
pub(crate) mod flock;
pub(crate) mod fsctl;
pub(crate) mod futex;
pub(crate) mod image;
pub(crate) mod kill;
pub(crate) mod limits;
pub(crate) mod load;
pub(crate) mod memory;
pub(crate) mod namespace;
pub(crate) mod native;
pub(crate) mod path;
pub(crate) mod pipe;
pub(crate) mod poll;
pub(crate) mod process;
pub(crate) mod registry;
pub(crate) mod signal;
pub(crate) mod sockets;
pub(crate) mod stat;
pub(crate) mod system;
pub(crate) mod thread;
pub(crate) mod time;
pub(crate) mod tty;
pub(crate) mod uaccess;

use core::sync::atomic::{AtomicU32, Ordering};

use ferrix_linux_abi::errno::{self, Errno};
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::AT_FDCWD;

use crate::arch;
use crate::console::println;
use crate::sched;
use crate::syscall::memory::{MmapRequest, OffsetUnit};
use crate::syscall::process::Process;

/// A system call as it arrived, before anything has been decided about it.
///
/// Deliberately dumb. The number is raw — this architecture's, not folded onto
/// [`Syscall`] yet — and the arguments are in the order the architecture's
/// calling convention puts them, because the only code that can put them in
/// that order is the code that read the registers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SyscallArgs {
    /// The number the program passed, in this architecture's own table.
    pub(crate) number: usize,
    /// The six argument registers, in order. A call taking fewer leaves the
    /// rest as whatever the program happened to have in them, which is why no
    /// handler may read past its own arity.
    pub(crate) args: [u64; 6],
}

/// What the trap path should do when a call returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Write this into the return register and resume the program.
    ///
    /// Already encoded as Linux encodes it: a value in `-4095..=-1` is
    /// `-errno`, anything else is success.
    Return(isize),
    /// Discard the saved registers and begin executing at `entry` with `stack`.
    ///
    /// `execve`, and the child side of `clone`. Data rather than "the frame has
    /// been replaced", so that this module never names a `TrapFrame`.
    Enter {
        /// Where the program's first instruction is.
        entry: u64,
        /// The stack pointer it starts with, already 16-byte aligned.
        stack: u64,
    },
}

/// Answer one system call.
///
/// `regs` is the caller's saved user registers, which a fork child resumes
/// from; `None` from a kernel caller, which cannot fork.
///
/// Never returns an error and never panics: an unknown number is `ENOSYS`, the
/// same as Linux. There is nothing above this to catch a failure — the caller
/// is a trap vector with a program waiting on it — so every path here has to
/// end in a value.
pub(crate) fn dispatch(args: &SyscallArgs, regs: Option<&arch::UserRegs>) -> Outcome {
    // The native ABI first, by range, before any Linux table is asked: the
    // two ABIs never have to agree about a number, and `arch::decode_syscall`
    // never sees one of Ferrix's own. See `native`.
    if ferrix_native_abi::nr::is_native(args.number) {
        let process = process::current();
        return Outcome::Return(errno::encode(native::dispatch(args, process.as_deref())));
    }
    let Some(call) = arch::decode_syscall(args.number) else {
        unanswered(None, args.number);
        return Outcome::Return(Errno::ENOSYS.as_return_value());
    };
    // Resolved once, here, rather than reached for inside each handler: the
    // handlers take `&Process` so that the boot self-check can call them
    // against a process it built itself, months before a program can.
    let process = process::current();

    // `exit` ends the calling thread and `exit_group` its whole process; both
    // end the task here and never come back, and with one thread they are the
    // same. The reference is dropped first, because nothing after this line
    // runs to drop it. A kernel thread has no process to end, and gets `ESRCH`
    // from the table like every other call that needs one.
    if matches!(call, Syscall::Exit | Syscall::ExitGroup) && process.is_some() {
        drop(process);
        let status = truncate(args.args[0]) as i32 & 0xFF;
        if call == Syscall::Exit {
            process::exit_thread_current(status);
        }
        process::exit_current(status);
    }

    // `clone`, `clone3`, `fork` and `vfork` need the caller's saved registers,
    // which a kernel caller has none of, and a reference to the parent to keep.
    if matches!(
        call,
        Syscall::Clone | Syscall::Clone3 | Syscall::Fork | Syscall::Vfork
    ) {
        let (Some(parent), Some(regs)) = (process.as_ref(), regs) else {
            return Outcome::Return(Errno::ESRCH.as_return_value());
        };
        return Outcome::Return(errno::encode(family::sys_clone(
            parent, call, &args.args, regs,
        )));
    }

    // `execve` resumes on a frame it built rather than returning, and a failure
    // past its point of no return ends the process -- after the reference is
    // dropped, for the reason above.
    if matches!(call, Syscall::Execve | Syscall::Execveat) {
        let Some(caller) = process.as_deref() else {
            return Outcome::Return(Errno::ESRCH.as_return_value());
        };
        let a = args.args;
        let entered = if matches!(call, Syscall::Execveat) {
            exec::sys_execveat(caller, fd::arg(a[0]), a[1], a[2], a[3], truncate(a[4]))
        } else {
            exec::sys_execve(caller, a[0], a[1], a[2])
        };
        return match entered {
            Ok((entry, stack)) => Outcome::Enter { entry, stack },
            Err(exec::ExecveError::Refused(error)) => Outcome::Return(error.as_return_value()),
            Err(exec::ExecveError::Lost) => {
                drop(process);
                process::exit_current(exec::lost_status())
            }
        };
    }

    // The calls that act on the calling thread's own signal state go to their
    // own table, with the caller's stack pointer for `sigaltstack`; a kernel
    // caller, with no registers, passes zero, which is on no stack.
    let thread = thread::current();
    let sp = regs.map_or(0, arch::UserRegs::stack_pointer);
    let answer = match signal::dispatch(call, &args.args, thread.as_deref(), sp) {
        Some(answer) => answer,
        None => handle(call, args, process.as_deref()),
    };
    if answer == Err(Errno::ENOSYS) {
        unanswered(Some(call), args.number);
    }
    // A blocking call interrupted by a signal returns a restart code, never
    // seen by the program: record the call so the way back to user mode can
    // restart it or turn it into `EINTR`. The number and first argument are
    // captured from the entry registers here, because the return register is
    // about to overwrite one of them. See `deliver::return_to_user`.
    if let (Err(error), Some(thread)) = (answer, thread.as_ref())
        && error.is_restart()
    {
        thread.with_own_signals(|signals| signals.mark_restart(args.number as u64, args.args[0]));
    }
    Outcome::Return(errno::encode(answer))
}

/// How many more calls answered `ENOSYS` may be reported. See
/// [`report_unanswered`].
static UNANSWERED_LINES: AtomicU32 = AtomicU32::new(0);

/// Report the next `lines` calls answered `ENOSYS` on the console, a line each.
///
/// What turns a foreign program's failure into the name of the call it was
/// missing: busybox refused a call often carries on and fails later, or prints
/// nothing, and the serial log is all a boot test has. Off unless init turns
/// it on around a program, because the boot self-check answers every number up
/// to 600 with `ENOSYS` on purpose. A bound rather than a switch, because a
/// program that retries a refused call forever needs reporting once, not a
/// log of it.
pub(crate) fn report_unanswered(lines: u32) {
    UNANSWERED_LINES.store(lines, Ordering::Relaxed);
}

/// One `ENOSYS`, reported if [`report_unanswered`] left room for it.
fn unanswered(call: Option<Syscall>, number: usize) {
    let room = UNANSWERED_LINES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
        left.checked_sub(1)
    });
    if room.is_err() {
        return;
    }
    match call {
        Some(call) => println!("  syscall  {call:?} (number {number}) answered ENOSYS"),
        None => println!("  syscall  number {number}, in no table, answered ENOSYS"),
    }
}

/// The dispatch table proper.
///
/// Split in two by what a call needs rather than by what it does: the first
/// group answers from the kernel's own state, the second needs the caller's
/// address space and is `ESRCH` without one. `ESRCH` rather than `EFAULT`
/// because the honest failure is "there is no process here", which is true of
/// every call today and will be true of none once stage 6's transition lands.
fn handle(call: Syscall, args: &SyscallArgs, process: Option<&Process>) -> Result<usize, Errno> {
    // The process's own number when there is a process, and the running
    // task's when there is not -- the boot self-checks call this with none.
    if matches!(call, Syscall::Getpid | Syscall::Gettid) {
        // A thread answers `gettid` with its own number, when the caller is a
        // thread of this process. A process's first thread is numbered by its
        // pid, which is what glibc's `raise` and a fork child's
        // `CLONE_CHILD_SETTID` expect to agree.
        let pid = process.map(Process::pid).filter(|&pid| pid != 0);
        let tid = thread::current()
            .filter(|thread| {
                process.is_some_and(|process| core::ptr::eq(thread.process().as_ref(), process))
            })
            .map(|thread| thread.tid())
            .filter(|&tid| tid != 0);
        let id = match call {
            Syscall::Gettid => tid.or(pid),
            _ => pid,
        };
        return Ok(id.map_or_else(current_id, |id| id as usize));
    }
    if let (Syscall::Getppid, Some(process)) = (call, process) {
        return Ok(process.parent_pid() as usize);
    }
    if let Some(id) = process.and_then(|process| credentials::identity(call, process)) {
        return Ok(id as usize);
    }
    if let Some(answer) = stateless(call, args) {
        return answer;
    }
    let process = process.ok_or(Errno::ESRCH)?;
    with_process(call, args, process)
}

/// The calls that need no process: identity, and yielding.
///
/// `None` means "not one of mine", which is what lets the two tables be read
/// independently rather than as one match with a fallthrough nobody can see
/// the end of.
fn stateless(call: Syscall, args: &SyscallArgs) -> Option<Result<usize, Errno>> {
    let _ = args;
    let answer = match call {
        Syscall::Gettid => Ok(current_id()),
        // Ferrix has one process tree and no init yet, so the boot task's
        // parent is itself. A program that walks up from here terminates.
        Syscall::Getppid => Ok(1),
        // Root's ids, for a caller with no process: the boot checks, calling
        // from a kernel task. A process's own ids are answered from its
        // credentials in `handle`, before this table is asked.
        Syscall::Getuid | Syscall::Geteuid | Syscall::Getgid | Syscall::Getegid => Ok(0),
        Syscall::SchedYield => {
            sched::yield_now();
            Ok(0)
        }
        _ => return None,
    };
    Some(answer)
}

/// The calls that reshape or read the caller's address space.
fn with_process(call: Syscall, args: &SyscallArgs, process: &Process) -> Result<usize, Errno> {
    let a = args.args;
    if let Some(answer) = descriptors(call, &a, process) {
        return answer;
    }
    if let Some(answer) = path::dispatch(call, args, process) {
        return answer;
    }
    if let Some(answer) = fsctl::dispatch(call, &a, process) {
        return answer;
    }
    let answer = attributes::dispatch(call, &a, process)
        .or_else(|| limits::dispatch(call, &a, process))
        .or_else(|| credentials::dispatch(call, &a, process))
        .or_else(|| sockets::dispatch(call, &a, process))
        .or_else(|| system::dispatch(call, &a, process))
        .or_else(|| time::dispatch(call, &a, process))
        .or_else(|| kill::dispatch(call, &a, process));
    if let Some(answer) = answer {
        return answer;
    }
    match call {
        // Still `ENOSYS`, each on purpose. There is no swap to turn on or off.
        Syscall::Swapon | Syscall::Swapoff => Err(Errno::ENOSYS),
        // There are no loadable modules: the kernel is one image.
        Syscall::InitModule | Syscall::FinitModule | Syscall::DeleteModule => Err(Errno::ENOSYS),
        // No System V IPC. `mmap(MAP_SHARED)` stands in for its shared memory,
        // pipes and sockets for its message queues, futexes for its semaphores.
        // Named here so each one is reported by name, not as a number no table
        // has.
        Syscall::Shmget | Syscall::Shmat | Syscall::Shmdt | Syscall::Shmctl => Err(Errno::ENOSYS),
        Syscall::Msgget | Syscall::Msgsnd | Syscall::Msgrcv | Syscall::Msgctl => Err(Errno::ENOSYS),
        Syscall::Semget | Syscall::Semop | Syscall::Semctl => Err(Errno::ENOSYS),
        Syscall::Semtimedop | Syscall::SemtimedopTime64 => Err(Errno::ENOSYS),
        // No process accounting to switch on.
        Syscall::Acct => Err(Errno::ENOSYS),
        // No controlling terminals to hang up until stage 15's tty layer.
        Syscall::Vhangup => Err(Errno::ENOSYS),
        // `rseq` is refused in `attributes::dispatch`, which says why.
        // `mmap` and `mmap2` differ in one argument's unit and nothing else,
        // which is exactly why they are separate calls: the difference is
        // invisible at the call site and catastrophic if guessed.
        Syscall::Mmap => memory::sys_mmap(process, &mmap_request(&a, OffsetUnit::Bytes)),
        Syscall::Mmap2 => memory::sys_mmap(process, &mmap_request(&a, OffsetUnit::Pages)),
        Syscall::Munmap => memory::sys_munmap(process, a[0], a[1]),
        Syscall::Mprotect => memory::sys_mprotect(process, a[0], a[1], truncate(a[2])),
        Syscall::Brk => memory::sys_brk(process, a[0]),
        Syscall::Mremap => memory::sys_mremap(process, a[0], a[1], a[2], truncate(a[3]), a[4]),
        Syscall::Msync => memory::sys_msync(process, a[0], a[1], truncate(a[2])),
        Syscall::Unshare => namespace::sys_unshare(process, a[0]),
        Syscall::Setns => namespace::sys_setns(process, fd::arg(a[0]), truncate(a[1])),
        Syscall::SetTidAddress => Ok(set_tid_address(process, a[0])),
        Syscall::ClockGettime => {
            time::sys_clock_gettime(process, a[0], a[1], time::TimeWidth::Native)
        }
        Syscall::ClockGettime64 => {
            time::sys_clock_gettime(process, a[0], a[1], time::TimeWidth::Wide)
        }
        Syscall::Gettimeofday => time::sys_gettimeofday(process, a[0], a[1]),
        Syscall::Time => time::sys_time(process, a[0]),
        Syscall::Getrandom => time::sys_getrandom(process, a[0], a[1], a[2]),
        Syscall::Uname => system::sys_uname(process, a[0]),
        Syscall::Poll => poll::sys_poll(process, a[0], a[1], a[2] as i32),
        Syscall::Select => poll::sys_select(process, a[0] as i32, [a[1], a[2], a[3]], a[4]),
        Syscall::Wait4 => family::sys_wait4(process, a[0] as i32, a[1], truncate(a[2]), a[3]),
        Syscall::Waitid => family::sys_waitid(
            process,
            truncate(a[0]),
            truncate(a[1]),
            a[2],
            truncate(a[3]),
            a[4],
        ),
        Syscall::Setpgid => family::sys_setpgid(process, a[0] as i32, a[1] as i32),
        Syscall::Getpgid => family::sys_getpgid(process, a[0] as i32),
        Syscall::Getpgrp => family::sys_getpgid(process, 0),
        Syscall::Getsid => family::sys_getsid(process, a[0] as i32),
        Syscall::Setsid => family::sys_setsid(process),
        Syscall::Futex => futex::sys_futex(process, &a, time::TimeWidth::Native),
        Syscall::FutexTime64 => futex::sys_futex(process, &a, time::TimeWidth::Wide),
        Syscall::RtSigaction => signal::sys_rt_sigaction(process, truncate(a[0]), a[1], a[2], a[3]),
        _ => Err(Errno::ENOSYS),
    }
}

/// The calls that take a descriptor, or make one.
///
/// A table of its own, like [`stateless`], so that `None` means "not one of
/// mine" and the two can be read separately. Every descriptor is narrowed to
/// the ABI's 32-bit `int` here, once, by [`fd::arg`].
fn descriptors(call: Syscall, a: &[u64; 6], process: &Process) -> Option<Result<usize, Errno>> {
    let fd = fd::arg(a[0]);
    let answer = match call {
        Syscall::Openat => fd::sys_openat(process, fd, a[1], truncate(a[2]), truncate(a[3])),
        Syscall::Open => fd::sys_openat(process, AT_FDCWD, a[0], truncate(a[1]), truncate(a[2])),
        Syscall::Close => fd::sys_close(process, fd),
        Syscall::Read => file::sys_read(process, fd, a[1], a[2]),
        Syscall::Write => file::sys_write(process, fd, a[1], a[2]),
        Syscall::Readv => file::sys_readv(process, fd, a[1], a[2]),
        Syscall::Writev => file::sys_writev(process, fd, a[1], a[2]),
        Syscall::Pread64 => file::sys_pread64(process, fd, a[1], a[2], wide(a, 3)),
        Syscall::Pwrite64 => file::sys_pwrite64(process, fd, a[1], a[2], wide(a, 3)),
        Syscall::Lseek => fd::sys_lseek(process, fd, native_signed(a[1]), truncate(a[2])),
        Syscall::Llseek => fd::sys_llseek(process, fd, a[1], a[2], a[3], truncate(a[4])),
        Syscall::Dup => fd::sys_dup(process, fd),
        Syscall::Dup2 => fd::sys_dup2(process, fd, fd::arg(a[1])),
        Syscall::Dup3 => fd::sys_dup3(process, fd, fd::arg(a[1]), truncate(a[2])),
        Syscall::Fcntl | Syscall::Fcntl64 if flock::is_record_lock(truncate(a[1]), call) => {
            flock::sys_fcntl_lock(process, fd, truncate(a[1]), a[2], call)
        }
        Syscall::Fcntl | Syscall::Fcntl64 => fd::sys_fcntl(process, fd, truncate(a[1]), a[2]),
        Syscall::Ftruncate => fd::sys_ftruncate(process, fd, native_signed(a[1])),
        Syscall::Ftruncate64 => fd::sys_ftruncate(process, fd, wide(a, 1)),
        Syscall::Ioctl => fd::sys_ioctl(process, fd, truncate(a[1]), a[2]),
        Syscall::Flock => flock::sys_flock(process, fd, truncate(a[1])),
        _ => return None,
    };
    Some(answer)
}

/// A signed argument one native word wide: `off_t` and `long`.
///
/// Narrowed to the word before it is widened, so that a 32-bit caller's
/// `-1`, which arrives as `0xFFFF_FFFF`, is `-1` and not four billion. On a
/// 64-bit build the narrowing is the identity.
fn native_signed(value: u64) -> i64 {
    value as usize as isize as i64
}

/// A 64-bit argument the C prototype puts at position `slot`: `loff_t`.
///
/// One register on a 64-bit architecture. On ARMv7-A two, low word first,
/// and -- because the EABI passes a 64-bit value in an even-numbered register
/// pair -- starting at the next even register, which leaves a hole when
/// `slot` is odd. `pread64(fd, buf, count, pos)` therefore takes `pos` from
/// registers 4 and 5, not 3 and 4, and `ftruncate64(fd, length)` from 2 and 3.
/// QEMU's user-mode emulator applies the same rule (`regpairs_aligned` in
/// `linux-user/user-internals.h`, true for ARM EABI).
///
/// The only 32-bit ABI this kernel has is the EABI, so the word size decides
/// it, as it does for `ferrix_ustack`'s layout.
fn wide(a: &[u64; 6], slot: usize) -> i64 {
    if size_of::<usize>() == 8 {
        return a.get(slot).copied().unwrap_or(0) as i64;
    }
    let pair = slot + slot % 2;
    let low = a.get(pair).copied().unwrap_or(0) & 0xFFFF_FFFF;
    let high = a.get(pair + 1).copied().unwrap_or(0) & 0xFFFF_FFFF;
    (high << 32 | low) as i64
}

/// `mmap`'s six registers as a request.
///
/// `unit` is the caller's, not the register block's: it is the whole
/// difference between `mmap` and `mmap2`, and it is not in the arguments.
fn mmap_request(a: &[u64; 6], unit: OffsetUnit) -> MmapRequest {
    MmapRequest {
        addr: a[0],
        len: a[1],
        prot: truncate(a[2]),
        flags: truncate(a[3]),
        fd: signed(a[4]),
        offset: a[5],
        unit,
    }
}

/// A flag word, which is 32 bits wide in the ABI however wide the register is.
///
/// Truncating rather than refusing: a 64-bit caller's upper half is whatever
/// the compiler left in the register, and Linux ignores it. Refusing would
/// break correct programs.
fn truncate(value: u64) -> u32 {
    value as u32
}

/// A file descriptor, which the ABI passes as a signed 32-bit value.
///
/// `mmap` is given `-1` for an anonymous mapping, and `-1` arrives in a 64-bit
/// register as `0xFFFF_FFFF` from a 32-bit caller and `0xFFFF_FFFF_FFFF_FFFF`
/// from a 64-bit one. Narrowing to `i32` first makes both of them `-1`.
fn signed(value: u64) -> i64 {
    i64::from(value as u32 as i32)
}

/// The running task's identifier, or the boot task's if the scheduler has not
/// started.
///
/// Zero is not a valid Linux pid, so the fallback is one: a program reading
/// `getpid()` as zero would conclude something very strange about where it is.
fn current_id() -> usize {
    match sched::current() {
        Some(task) => usize::try_from(task.id).unwrap_or(1),
        None => 1,
    }
}

/// `set_tid_address`: record the address to clear when the calling thread
/// ends, and answer its id.
///
/// The number `gettid` answers, which a libc keeps as the thread's id and
/// later hands to `tgkill`; the task's own number would disagree. The address
/// is the calling thread's, when the caller is a thread of `process`; the
/// self-checks call with none.
fn set_tid_address(process: &Process, address: u64) -> usize {
    let tid = thread::current_of(process).map_or(0, |thread| thread.set_clear_child_tid(address));
    match (tid, process.pid()) {
        (0, 0) => current_id(),
        (0, pid) => pid as usize,
        (tid, _) => tid as usize,
    }
}
