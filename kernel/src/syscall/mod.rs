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

pub(crate) mod check;
pub(crate) mod exec;
pub(crate) mod file;
pub(crate) mod image;
pub(crate) mod load;
pub(crate) mod memory;
pub(crate) mod process;
pub(crate) mod time;
pub(crate) mod uaccess;

use ferrix_linux_abi::errno::{self, Errno};
use ferrix_linux_abi::nr::Syscall;

use crate::arch;
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
    #[expect(
        dead_code,
        reason = "execve is the next handler; the variant is the agreed seam, \
                  and adding it later would change every handler's signature"
    )]
    Enter {
        /// Where the program's first instruction is.
        entry: u64,
        /// The stack pointer it starts with, already 16-byte aligned.
        stack: u64,
    },
}

/// Answer one system call.
///
/// Never returns an error and never panics: an unknown number is `ENOSYS`, the
/// same as Linux. There is nothing above this to catch a failure — the caller
/// is a trap vector with a program waiting on it — so every path here has to
/// end in a value.
pub(crate) fn dispatch(args: &SyscallArgs) -> Outcome {
    let Some(call) = arch::decode_syscall(args.number) else {
        return Outcome::Return(Errno::ENOSYS.as_return_value());
    };
    // Resolved once, here, rather than reached for inside each handler: the
    // handlers take `&Process` so that the boot self-check can call them
    // against a process it built itself, months before a program can.
    let process = process::current();
    Outcome::Return(errno::encode(handle(call, args, process.as_deref())))
}

/// The dispatch table proper.
///
/// Split in two by what a call needs rather than by what it does: the first
/// group answers from the kernel's own state, the second needs the caller's
/// address space and is `ESRCH` without one. `ESRCH` rather than `EFAULT`
/// because the honest failure is "there is no process here", which is true of
/// every call today and will be true of none once stage 6's transition lands.
fn handle(call: Syscall, args: &SyscallArgs, process: Option<&Process>) -> Result<usize, Errno> {
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
        Syscall::Getpid | Syscall::Gettid => Ok(current_id()),
        // Ferrix has one process tree and no init yet, so the boot task's
        // parent is itself. A program that walks up from here terminates.
        Syscall::Getppid => Ok(1),
        // Everything runs as root because there are no credentials yet. A real
        // answer, not a stub: it is what a single-user system with no `setuid`
        // reports, and stage 12 replaces it with a lookup rather than
        // unpicking it.
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
    match call {
        // `mmap` and `mmap2` differ in one argument's unit and nothing else,
        // which is exactly why they are separate calls: the difference is
        // invisible at the call site and catastrophic if guessed.
        Syscall::Mmap => memory::sys_mmap(process, &mmap_request(&a, OffsetUnit::Bytes)),
        Syscall::Mmap2 => memory::sys_mmap(process, &mmap_request(&a, OffsetUnit::Pages)),
        Syscall::Munmap => memory::sys_munmap(process, a[0], a[1]),
        Syscall::Mprotect => memory::sys_mprotect(process, a[0], a[1], truncate(a[2])),
        Syscall::Brk => memory::sys_brk(process, a[0]),
        Syscall::SetTidAddress => Ok(process.set_clear_child_tid(a[0], current_id())),
        Syscall::Read => file::sys_read(process, a[0], a[1], a[2]),
        Syscall::ClockGettime => {
            time::sys_clock_gettime(process, a[0], a[1], time::TimeWidth::Native)
        }
        Syscall::ClockGettime64 => {
            time::sys_clock_gettime(process, a[0], a[1], time::TimeWidth::Wide)
        }
        Syscall::Gettimeofday => time::sys_gettimeofday(process, a[0]),
        Syscall::Getrandom => time::sys_getrandom(process, a[0], a[1], a[2]),
        Syscall::Write => file::sys_write(process, a[0], a[1], a[2]),
        Syscall::Writev => file::sys_writev(process, a[0], a[1], a[2]),
        _ => Err(Errno::ENOSYS),
    }
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
