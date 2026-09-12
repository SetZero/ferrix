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

use ferrix_linux_abi::errno::{self, Errno};
use ferrix_linux_abi::nr::Syscall;

use crate::arch;
use crate::sched;

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
    #[expect(
        dead_code,
        reason = "no handler reads an argument yet; the field is the agreed \
                  shape of the seam, and the trap path fills it from the frame"
    )]
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
    Outcome::Return(errno::encode(handle(call, args)))
}

/// The dispatch table proper.
///
/// One match on an architecture-neutral call. The arms are grouped the way the
/// roadmap groups the work, so that a stage which fills one group in touches
/// one part of this function.
fn handle(call: Syscall, args: &SyscallArgs) -> Result<usize, Errno> {
    // No handler takes an argument yet. The parameter is part of the shape
    // agreed with the trap path, and the first handler that reads user memory
    // will need it; dropping it now would mean changing the signature then.
    let _ = args;
    match call {
        // Identity. These need no process state beyond the running task, which
        // is why they are the first calls this kernel can honestly answer.
        Syscall::Getpid | Syscall::Gettid => Ok(current_id()),
        // Ferrix has one process tree and no init yet, so the boot task's
        // parent is itself. A program that walks up from here terminates.
        Syscall::Getppid => Ok(1),
        // Everything runs as root because there are no credentials yet. This
        // is a real answer, not a stub: it is what a single-user system with
        // no `setuid` reports, and stage 12 replaces it with a lookup rather
        // than unpicking it.
        Syscall::Getuid | Syscall::Geteuid | Syscall::Getgid | Syscall::Getegid => Ok(0),

        // Scheduling.
        Syscall::SchedYield => {
            sched::yield_now();
            Ok(0)
        }

        // Everything else. `ENOSYS` is Linux's own answer for a call it does
        // not implement, so a program that gets it can fall back; a handler
        // that pretended to succeed would go wrong somewhere else entirely.
        _ => Err(Errno::ENOSYS),
    }
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
