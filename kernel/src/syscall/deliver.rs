//! Signals reaching a program: the way back to user mode, handler frames,
//! `rt_sigreturn`, faults, and the calls that wait for a signal.
//!
//! # Where delivery happens
//!
//! On the way back to user mode, and nowhere else, because that is the only
//! place the program's registers are to hand and about to be used. Every way
//! back calls [`needs_attention`] and, when it answers yes, [`return_to_user`]
//! with the registers as an [`arch::UserContext`]: the end of a system call on
//! all three architectures, and the end of every trap -- a tick, an IPI, a
//! fault -- taken from user mode. A process ended from outside leaves there;
//! a stopped one waits there; and a signal with a handler is delivered by
//! rewriting the registers to enter the handler, on a frame written to the
//! program's stack that records the registers it had.
//!
//! # What is the architecture's
//!
//! The frame. Linux fixed a different `rt_sigframe` for each architecture, and
//! a C library's handler trampolines and `ucontext_t` readers depend on every
//! offset, so each architecture writes and reads its own
//! ([`arch::setup_signal_frame`], [`arch::restore_signal_frame`]). What is
//! decided here is everything else: which signal, whether it has a handler,
//! the mask while it runs, the stack it runs on, and what happens when the
//! frame cannot be written.
//!
//! # Interrupted calls, and `SA_RESTART`
//!
//! A call that waits -- `wait4`, `poll`, a pipe, `pause`, `rt_sigsuspend` --
//! also stops waiting when a signal is deliverable. What it returns is not
//! `EINTR` but one of the kernel-internal restart codes ([`Errno::is_restart`]),
//! which never reaches the program: this module turns each into a restart of
//! the interrupted call or into `EINTR`, exactly as Linux's
//! `arch_do_signal_or_restart` does, before the program is resumed.
//!
//! The rule, per code:
//!
//! * `ERESTARTSYS` (a read, write, `wait4`, pipe or futex wait): the call
//!   restarts if a handler with `SA_RESTART` runs, or if no handler runs at
//!   all; otherwise `EINTR`.
//! * `ERESTARTNOHAND` (`poll`, `select`, `pselect6`): `EINTR` if a handler
//!   runs, restart if none does. So a handler -- the usual interrupter --
//!   always sees `EINTR` from these.
//! * `ERESTARTNOINTR`: always restarts.
//! * `ERESTART_RESTARTBLOCK` (`nanosleep`, `clock_nanosleep`): `EINTR` if a
//!   handler runs; otherwise the call is re-entered as `restart_syscall`,
//!   which waits out the time left rather than the whole sleep again.
//!
//! Restarting means rewinding the saved program counter to the call's own
//! instruction and putting back the argument register the return value
//! clobbered, so that resuming re-executes the call. The rewind differs per
//! architecture and lives in each `arch::UserContext`; the decision is here.
//! It is made before any handler frame is built, so the frame saves the
//! resolved resume point and `rt_sigreturn` returns straight into it.

use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::is_user_address;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{SA_RESTART, SIG_DFL, SIG_IGN, SIGSEGV};

use crate::arch;
use crate::console::println;
use crate::syscall::process::{self, Process};
use crate::syscall::signal::{
    self, DefaultAction, Origin, Posted, Restart, SIGSET_SIZE, Taken, UNBLOCKABLE,
};
use crate::syscall::thread::{self, Thread};
use crate::syscall::time::TimeWidth;
use crate::syscall::uaccess;
use crate::user::space::AddressSpace;

pub(crate) use crate::syscall::signal::SIGINFO_BYTES;

/// How many signals one way back to user mode acts on. One more than there
/// are signals, so every pending one can be taken; a bound, so that a stop
/// and continue sent in a tight loop cannot keep a task in the kernel.
const DELIVERY_ROUNDS: usize = 65;

/// A frame the architecture could not write, or would not accept back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BadFrame;

/// `stack_t` as a frame records it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StackRecord {
    /// `ss_sp`.
    pub(crate) sp: u64,
    /// `ss_flags`.
    pub(crate) flags: i32,
    /// `ss_size`.
    pub(crate) size: u64,
}

/// Everything an architecture needs to put a handler's frame on the stack.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameRequest {
    /// The signal.
    pub(crate) signal: u32,
    /// Its `siginfo_t`, already encoded.
    pub(crate) info: [u8; SIGINFO_BYTES],
    /// Where the handler is.
    pub(crate) handler: u64,
    /// The handler's `SA_*` flags.
    pub(crate) flags: u64,
    /// Where the handler returns to.
    pub(crate) restorer: u64,
    /// The mask the frame saves, which `rt_sigreturn` puts back.
    pub(crate) mask: u64,
    /// The address the frame is built below, alternate stack already chosen.
    pub(crate) stack: u64,
    /// The alternate stack, as `uc_stack` records it.
    pub(crate) altstack: StackRecord,
}

/// What an architecture read back out of a frame, besides the registers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Restored {
    /// The mask the frame saved.
    pub(crate) mask: u64,
    /// The alternate stack the frame recorded.
    pub(crate) altstack: StackRecord,
}

/// A frame's bytes while they are built or read: offsets checked, so an
/// architecture's layout is written as a table of constants and a mistake in
/// one is a refused frame rather than a panic.
#[derive(Debug)]
pub(crate) struct FrameBytes(Vec<u8>);

impl FrameBytes {
    /// `len` zero bytes.
    pub(crate) fn zeroed(len: usize) -> FrameBytes {
        FrameBytes(vec![0; len])
    }

    /// `len` bytes of the program's memory at `at`.
    pub(crate) fn read(space: &AddressSpace, at: u64, len: usize) -> Result<FrameBytes, BadFrame> {
        let mut bytes = vec![0; len];
        uaccess::copy_from_user(space, at, &mut bytes).map_err(|_| BadFrame)?;
        Ok(FrameBytes(bytes))
    }

    /// Write them into the program's memory at `at`.
    pub(crate) fn write(&self, space: &AddressSpace, at: u64) -> Result<(), BadFrame> {
        uaccess::copy_to_user(space, at, &self.0).map_err(|_| BadFrame)
    }

    /// Put `bytes` at `at`.
    pub(crate) fn put(&mut self, at: usize, bytes: &[u8]) -> Result<(), BadFrame> {
        let end = at.checked_add(bytes.len()).ok_or(BadFrame)?;
        self.0
            .get_mut(at..end)
            .ok_or(BadFrame)?
            .copy_from_slice(bytes);
        Ok(())
    }

    /// Put a 32-bit value at `at`.
    pub(crate) fn put_u32(&mut self, at: usize, value: u32) -> Result<(), BadFrame> {
        self.put(at, &value.to_le_bytes())
    }

    /// Put a 64-bit value at `at`.
    pub(crate) fn put_u64(&mut self, at: usize, value: u64) -> Result<(), BadFrame> {
        self.put(at, &value.to_le_bytes())
    }

    /// Put a native word at `at`.
    fn put_word(&mut self, at: usize, value: u64) -> Result<(), BadFrame> {
        let word = value.to_le_bytes();
        self.put(at, word.get(..size_of::<usize>()).ok_or(BadFrame)?)
    }

    /// Put a `stack_t` at `at`: pointer, `int` flags, size, in native words.
    pub(crate) fn put_stack(&mut self, at: usize, stack: StackRecord) -> Result<(), BadFrame> {
        let word = size_of::<usize>();
        self.put_word(at, stack.sp)?;
        self.put(at + word, &stack.flags.to_le_bytes())?;
        self.put_word(at + 2 * word, stack.size)
    }

    /// The `len` bytes at `at`.
    pub(crate) fn get(&self, at: usize, len: usize) -> Result<&[u8], BadFrame> {
        let end = at.checked_add(len).ok_or(BadFrame)?;
        self.0.get(at..end).ok_or(BadFrame)
    }

    /// The 32-bit value at `at`.
    pub(crate) fn u32_at(&self, at: usize) -> Result<u32, BadFrame> {
        let mut value = [0_u8; 4];
        value.copy_from_slice(self.get(at, 4)?);
        Ok(u32::from_le_bytes(value))
    }

    /// The 64-bit value at `at`.
    pub(crate) fn u64_at(&self, at: usize) -> Result<u64, BadFrame> {
        let mut value = [0_u8; 8];
        value.copy_from_slice(self.get(at, 8)?);
        Ok(u64::from_le_bytes(value))
    }

    /// The native word at `at`, zero-extended.
    fn word_at(&self, at: usize) -> Result<u64, BadFrame> {
        let word = size_of::<usize>();
        let mut value = [0_u8; 8];
        value
            .get_mut(..word)
            .ok_or(BadFrame)?
            .copy_from_slice(self.get(at, word)?);
        Ok(u64::from_le_bytes(value))
    }

    /// The `stack_t` at `at`.
    pub(crate) fn stack_at(&self, at: usize) -> Result<StackRecord, BadFrame> {
        let word = size_of::<usize>();
        Ok(StackRecord {
            sp: self.word_at(at)?,
            flags: self.u32_at(at + word)? as i32,
            size: self.word_at(at + 2 * word)?,
        })
    }
}

/// Whether the way back to user mode has anything to do for the running task:
/// its process ended or stopped, a signal to deliver to its thread, or a mask
/// to put back.
///
/// Cheap, and asked on every way back, so that the registers are only copied
/// out into an [`arch::UserContext`] when something will use them.
pub(crate) fn needs_attention() -> bool {
    thread::current().is_some_and(|thread| {
        let process = thread.process();
        process.is_terminated()
            || process.is_stopped()
            || thread.with_signals(|shared, own| signal::needs_attention(shared, own))
    })
}

/// Act on everything [`needs_attention`] found, with `context` the registers
/// the program is about to resume with.
///
/// Called with interrupts masked, and returns with them masked. They are open
/// in between: writing a frame can fault in a page of the program's stack,
/// and a stopped process waits here for as long as it stays stopped.
///
/// Does not return when the process has ended.
pub(crate) fn return_to_user(context: &mut arch::UserContext) {
    let Some(thread) = thread::current() else {
        return;
    };
    let process = thread.process();
    arch::enable_interrupts();

    // A blocking call interrupted by a signal returns a restart code in the
    // return register. Resolve it against the signal about to be delivered,
    // the way Linux does on the syscall exit path. `take_restart` answers
    // `Some` only just after such a call, and takes it once so a later trap
    // cannot act on a stale one; the register is checked too, so a way back
    // that is not a syscall return -- a tick, a fault -- is never rewound.
    let mut restart = thread
        .with_own_signals(signal::ThreadSignals::take_restart)
        .zip(RestartKind::of(context.syscall_result()));

    for _ in 0..DELIVERY_ROUNDS {
        if process.is_terminated() {
            break;
        }
        if process.is_stopped() {
            let _ = process.resumed().wait_until_deadline(
                || !process.is_stopped() || process.is_terminated(),
                u64::MAX,
            );
            continue;
        }
        let Some(taken) = thread.with_signals(signal::take_next) else {
            break;
        };
        // The first signal that runs a handler settles the restart: only a
        // handler can turn one into `EINTR`. A default action -- a stop, an
        // ignore -- leaves it pending, so a stop then continue restarts
        // transparently and a later handler still gets to decide.
        if let Some((ctx, kind)) = restart
            && runs_a_handler(&taken)
        {
            resolve_restart(context, &ctx, kind, taken.action.flags);
            restart = None;
        }
        act(&thread, context, &taken);
    }
    // No handler ran -- a stop, an ignore, or nothing was left to deliver --
    // so the call restarts transparently.
    if let Some((ctx, kind)) = restart {
        restart_call(context, &ctx, kind);
    }
    thread.with_own_signals(signal::ThreadSignals::restore_saved_mask);
    if thread.process().is_terminated() {
        // Left with interrupts still open, as the release its exit may run
        // needs. Nothing after the thread's exit runs to drop it.
        drop(thread);
        process::leave_current();
    }
    arch::disable_interrupts();
}

/// Which restart code a system call left in the return register, if any. The
/// kernel-internal codes are the only errors above what a program can see, so
/// a value outside them -- an ordinary result, or the live register of a way
/// back that is not a syscall return -- is `None` and starts no restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartKind {
    /// `ERESTARTSYS`: restart under `SA_RESTART`, or with no handler.
    Sys,
    /// `ERESTARTNOINTR`: always restart.
    NoIntr,
    /// `ERESTARTNOHAND`: restart only with no handler.
    NoHand,
    /// `ERESTART_RESTARTBLOCK`: resume through `restart_syscall`.
    RestartBlock,
}

impl RestartKind {
    /// The code a return value of `value` carries, if it is a restart code.
    fn of(value: isize) -> Option<RestartKind> {
        if value == Errno::ERESTARTSYS.as_return_value() {
            Some(RestartKind::Sys)
        } else if value == Errno::ERESTARTNOINTR.as_return_value() {
            Some(RestartKind::NoIntr)
        } else if value == Errno::ERESTARTNOHAND.as_return_value() {
            Some(RestartKind::NoHand)
        } else if value == Errno::ERESTART_RESTARTBLOCK.as_return_value() {
            Some(RestartKind::RestartBlock)
        } else {
            None
        }
    }
}

/// Whether `taken` runs a handler of the program's own, as opposed to a
/// default action or being ignored -- the only case that can turn a restart
/// into `EINTR`.
fn runs_a_handler(taken: &Taken) -> bool {
    !matches!(taken.action.handler, SIG_DFL | SIG_IGN)
}

/// Settle a restart against a handler that is about to run: restart the call
/// if the code and the handler's `SA_RESTART` flag allow it, and otherwise
/// leave `EINTR` in the return register, at the call's own resume point.
fn resolve_restart(context: &mut arch::UserContext, ctx: &Restart, kind: RestartKind, flags: u64) {
    if restarts(kind, true, flags) {
        restart_call(context, ctx, kind);
    } else {
        context.set_syscall_result(Errno::EINTR.as_return_value());
    }
}

/// Whether a call with restart code `kind` restarts, given whether a handler
/// runs and its `SA_*` flags. Linux's rule, in one place so the boot self-check
/// can assert the whole truth table:
///
/// * with no handler, every code restarts (the call had nothing to report to);
/// * `ERESTARTNOINTR` always restarts;
/// * `ERESTARTSYS` restarts only under `SA_RESTART`;
/// * `ERESTARTNOHAND` and `ERESTART_RESTARTBLOCK` never restart through a
///   handler -- they become `EINTR`.
fn restarts(kind: RestartKind, runs_handler: bool, flags: u64) -> bool {
    if !runs_handler {
        return true;
    }
    match kind {
        RestartKind::NoIntr => true,
        RestartKind::Sys => flags & SA_RESTART != 0,
        RestartKind::NoHand | RestartKind::RestartBlock => false,
    }
}

/// Assert the restart decision matches Linux's for every code, with and without
/// a handler and its `SA_RESTART` flag -- the boot self-check for `SA_RESTART`,
/// each row its own negative control. It proves the decision this module makes;
/// the per-architecture rewind that carries it out is in each `UserContext` and
/// cross-checked against `arch/*/kernel/signal.c` and QEMU's `cpu_loop`.
pub(crate) fn check_restart_decisions() -> Result<(), &'static str> {
    // Only the kernel-internal codes classify; an ordinary result or `EINTR`
    // must not, or a live register at a tick could be mistaken for one.
    if RestartKind::of(0).is_some()
        || RestartKind::of(Errno::EINTR.as_return_value()).is_some()
        || RestartKind::of(Errno::EFAULT.as_return_value()).is_some()
    {
        return Err("an ordinary return value was read as a restart code");
    }
    for (value, kind) in [
        (Errno::ERESTARTSYS.as_return_value(), RestartKind::Sys),
        (Errno::ERESTARTNOINTR.as_return_value(), RestartKind::NoIntr),
        (Errno::ERESTARTNOHAND.as_return_value(), RestartKind::NoHand),
        (
            Errno::ERESTART_RESTARTBLOCK.as_return_value(),
            RestartKind::RestartBlock,
        ),
    ] {
        if RestartKind::of(value) != Some(kind) {
            return Err("a restart code did not classify as itself");
        }
    }
    // (kind, a handler runs, its flags, whether the call should restart).
    let matrix = [
        (RestartKind::Sys, true, SA_RESTART, true),
        (RestartKind::Sys, true, 0, false),
        (RestartKind::NoIntr, true, 0, true),
        (RestartKind::NoHand, true, SA_RESTART, false),
        (RestartKind::RestartBlock, true, SA_RESTART, false),
        (RestartKind::Sys, false, 0, true),
        (RestartKind::NoHand, false, 0, true),
        (RestartKind::RestartBlock, false, 0, true),
        (RestartKind::NoIntr, false, 0, true),
    ];
    for (kind, handler, flags, expect) in matrix {
        if restarts(kind, handler, flags) != expect {
            return Err("the restart decision does not match Linux's for some case");
        }
    }
    Ok(())
}

/// Rewind the saved registers so the interrupted call re-executes: back to its
/// own instruction, with the clobbered argument register restored. A
/// `restart_block` call is re-entered as `restart_syscall` instead of itself,
/// so it waits out only the time left.
fn restart_call(context: &mut arch::UserContext, ctx: &Restart, kind: RestartKind) {
    let restart_block = matches!(kind, RestartKind::RestartBlock);
    context.rewind_syscall(ctx.nr, ctx.arg0, restart_block);
}

/// Do what `taken` asks of `thread`: nothing, the default action, or its
/// handler.
fn act(thread: &Thread, context: &mut arch::UserContext, taken: &Taken) {
    let process = thread.process();
    match taken.action.handler {
        SIG_IGN => {}
        SIG_DFL => match signal::default_action(taken.signal) {
            DefaultAction::Terminate | DefaultAction::Core => {
                process::kill(process, 128 + taken.signal as i32);
            }
            DefaultAction::Stop => process.enter_stop(taken.signal),
            DefaultAction::Ignore | DefaultAction::Continue => {}
        },
        _ => {
            if run_handler(thread, context, taken).is_err() {
                // Linux's `force_sigsegv`: a handler that cannot be entered
                // is a program that cannot go on.
                println!(
                    "  signal   pid {} has no room for signal {}'s frame; ending it with SIGSEGV",
                    process.pid(),
                    taken.signal
                );
                process::kill(process, 128 + SIGSEGV as i32);
            }
        }
    }
}

/// Enter `taken`'s handler: choose the stack, change the mask, and have the
/// architecture write the frame and point the registers at the handler.
fn run_handler(
    thread: &Thread,
    context: &mut arch::UserContext,
    taken: &Taken,
) -> Result<(), BadFrame> {
    if !is_user_address(taken.action.handler) {
        return Err(BadFrame);
    }
    let sp = context.stack_pointer();
    let (mask, altstack, stack) = thread.with_signals(|shared, own| {
        let stack = own.frame_base(taken.action.flags, sp);
        let (mask, altstack) = signal::enter_handler(shared, own, taken);
        (mask, altstack, stack)
    });
    let request = FrameRequest {
        signal: taken.signal,
        info: taken.origin.encode(taken.signal),
        handler: taken.action.handler,
        flags: taken.action.flags,
        restorer: taken.action.restorer,
        mask,
        stack,
        altstack,
    };
    arch::setup_signal_frame(thread.process().space(), context, &request)
}

/// `rt_sigreturn`, and ARMv7-A's `sigreturn` when `rt` is false: put back the
/// registers, mask and alternate stack a handler's frame saved. The frame is
/// found from the stack pointer the handler returned with, which is all a
/// restorer leaves.
///
/// A frame that cannot be read, or does not hold together, ends the process
/// with `SIGSEGV`, as on Linux: there is no context left to return an error to.
pub(crate) fn sigreturn(context: &mut arch::UserContext, rt: bool) {
    let Some(thread) = thread::current() else {
        return;
    };
    let process = thread.process();
    match arch::restore_signal_frame(process.space(), context, rt) {
        Ok(restored) => {
            let sp = context.stack_pointer();
            thread.with_own_signals(|signals| {
                signals.leave_handler(restored.mask, restored.altstack, sp);
            });
        }
        Err(BadFrame) => {
            println!(
                "  signal   pid {} returned from a handler through a bad frame; ending it with SIGSEGV",
                process.pid()
            );
            process::kill(process, 128 + SIGSEGV as i32);
        }
    }
}

/// Raise `signal` against the running task's thread for a fault its own
/// instruction took, so that it can neither block nor ignore it. Answers what
/// became of it -- [`Posted::Fatal`] when the process has already been ended --
/// or `None` when the running task has no thread, which a fault from user
/// mode never lacks unless the kernel entered user mode without one.
pub(crate) fn force(signal: u32, origin: Origin) -> Option<Posted> {
    let thread = thread::current()?;
    let posted = thread.with_signals(|shared, own| signal::force(shared, own, signal, origin));
    if posted == Posted::Fatal {
        process::kill(thread.process(), 128 + signal as i32);
    }
    Some(posted)
}

/// Wait until a signal is deliverable, the process ends, `also` holds, or
/// `deadline` passes.
fn wait_for_signal(process: &Process, deadline: u64, mut also: impl FnMut() -> bool) {
    let _ = process
        .signalled()
        .wait_until_deadline(|| process.signal_pending() || also(), deadline);
}

/// `rt_sigsuspend`: block `mask` instead, wait for a signal, and return
/// `EINTR`. The old mask comes back on the way to user mode -- after a
/// handler's frame has saved it, so the handler runs under `mask` and the
/// program resumes under its own.
///
/// # Errors
///
/// `EINVAL` for a set size other than eight; `EFAULT` for a bad set; `EINTR`,
/// always, once it has waited.
pub(crate) fn sys_rt_sigsuspend(
    thread: &Thread,
    mask: u64,
    sigsetsize: u64,
) -> Result<usize, Errno> {
    if sigsetsize != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let mask = read_sigset(thread.process(), mask)?;
    thread.with_own_signals(|signals| signals.suspend_with(mask));
    wait_for_signal(thread.process(), u64::MAX, || false);
    Err(Errno::EINTR)
}

/// `pause`: wait for a signal, and return `EINTR`.
///
/// # Errors
///
/// `EINTR`, always.
pub(crate) fn sys_pause(process: &Process) -> Result<usize, Errno> {
    wait_for_signal(process, u64::MAX, || false);
    Err(Errno::EINTR)
}

/// `rt_sigpending`: the pending signals the calling thread blocks, its own and
/// its process's. Linux copies `sigsetsize` bytes of the set and refuses only a
/// size larger than its own.
///
/// # Errors
///
/// `EINVAL` for a size above eight; `EFAULT` for a bad pointer.
pub(crate) fn sys_rt_sigpending(thread: &Thread, at: u64, sigsetsize: u64) -> Result<usize, Errno> {
    if sigsetsize > SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let process = thread.process();
    let set = thread.with_signals(|shared, own| (shared.pending() | own.pending()) & own.blocked());
    let bytes = set.to_le_bytes();
    let len = usize::try_from(sigsetsize).map_err(|_| Errno::EINVAL)?;
    if len > 0 {
        uaccess::copy_to_user(process.space(), at, bytes.get(..len).ok_or(Errno::EINVAL)?)
            .map_err(|_| Errno::EFAULT)?;
    }
    Ok(0)
}

/// `rt_sigtimedwait` and `rt_sigtimedwait_time64`: take a pending signal in
/// `set` -- blocked, as a program waiting this way has made it -- without
/// delivering it, and answer its number, with its `siginfo` written to `info`.
///
/// # Errors
///
/// `EINVAL` for a set size other than eight or a bad timeout; `EFAULT` for a
/// bad pointer; `EAGAIN` when the timeout passes first; `EINTR` when another
/// signal, one the caller does not block, arrives first.
pub(crate) fn sys_rt_sigtimedwait(
    thread: &Thread,
    set: u64,
    info: u64,
    timeout: u64,
    sigsetsize: u64,
    width: TimeWidth,
) -> Result<usize, Errno> {
    if sigsetsize != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let process = thread.process();
    let set = read_sigset(process, set)? & !UNBLOCKABLE;
    let deadline = if timeout == 0 {
        u64::MAX
    } else {
        crate::timer::now_nanos().saturating_add(read_timespec(process, timeout, width)?)
    };
    wait_for_signal(process, deadline, || {
        thread.with_signals(|shared, own| (shared.pending() | own.pending()) & set != 0)
    });
    match thread.with_signals(|shared, own| signal::take_from(shared, own, set)) {
        Some(taken) => {
            if info != 0 {
                uaccess::copy_to_user(process.space(), info, &taken.origin.encode(taken.signal))
                    .map_err(|_| Errno::EFAULT)?;
            }
            Ok(taken.signal as usize)
        }
        None if thread.signal_pending() => Err(Errno::EINTR),
        None => Err(Errno::EAGAIN),
    }
}

/// Read an 8-byte signal set from the program.
fn read_sigset(process: &Process, at: u64) -> Result<u64, Errno> {
    let mut bytes = [0_u8; 8];
    uaccess::copy_from_user(process.space(), at, &mut bytes).map_err(|_| Errno::EFAULT)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Read a `struct timespec` of `width` as nanoseconds.
fn read_timespec(process: &Process, at: u64, width: TimeWidth) -> Result<u64, Errno> {
    let field = if width == TimeWidth::Wide {
        8
    } else {
        size_of::<usize>()
    };
    let mut bytes = [0_u8; 16];
    let wanted = bytes.get_mut(..field * 2).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, wanted).map_err(|_| Errno::EFAULT)?;
    let value = |index: usize| -> Result<i64, Errno> {
        let raw = wanted
            .get(index * field..(index + 1) * field)
            .ok_or(Errno::EINVAL)?;
        let mut word = [0_u8; 8];
        word.get_mut(..field)
            .ok_or(Errno::EINVAL)?
            .copy_from_slice(raw);
        Ok(if field == 8 {
            i64::from_le_bytes(word)
        } else {
            i64::from(u64::from_le_bytes(word) as u32 as i32)
        })
    };
    let seconds = u64::try_from(value(0)?).map_err(|_| Errno::EINVAL)?;
    let nanos = u64::try_from(value(1)?).map_err(|_| Errno::EINVAL)?;
    if nanos >= 1_000_000_000 {
        return Err(Errno::EINVAL);
    }
    Ok(seconds.saturating_mul(1_000_000_000).saturating_add(nanos))
}
