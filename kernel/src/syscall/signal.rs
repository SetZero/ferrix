//! Signal state: what a program asked to happen on each signal, what it has
//! blocked, and what is waiting to reach it.
//!
//! # Three modules, one table
//!
//! This one keeps the table and answers the calls that only read or change it
//! -- `rt_sigaction`, `rt_sigprocmask`, `sigaltstack`. Sending a signal is
//! `super::kill`'s, and a signal reaching a program -- a frame on its stack, a
//! handler run, `rt_sigreturn` unwinding it -- is `super::deliver`'s. Both work
//! through the methods here, under the process lock, so that "is it blocked,
//! is it ignored, is it already pending" is one decision rather than three
//! reads with a sender racing in between.
//!
//! # Layouts
//!
//! Both structures are built from native words, so they are narrower on
//! ARMv7-A, and they are read and written as native words here for the reason
//! `writev` gives: `libs/linux-abi`'s `Sigaction` and `Stack` are the 64-bit
//! layouts, and using them for a 32-bit program would read its fields at twice
//! the stride.
//!
//! * `struct sigaction`, the kernel's and not the C library's: handler, flags
//!   and restorer as three words, then the 8-byte mask. All three
//!   architectures define `SA_RESTORER`, so the field is present on each.
//! * `stack_t`: the stack pointer as a word, the flags as an `int`, the size
//!   as a word. On a 64-bit machine the `int` is padded to the word, which is
//!   why the size sits at the second word on both widths.

use alloc::boxed::Box;
use alloc::vec;

use ferrix_bootinfo::Arch;
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    NSIG, SA_NOCLDSTOP, SA_NOCLDWAIT, SA_NODEFER, SA_ONSTACK, SA_RESETHAND, SIG_BLOCK, SIG_DFL,
    SIG_IGN, SIG_SETMASK, SIG_UNBLOCK, SIGABRT, SIGBUS, SIGCHLD, SIGCONT, SIGFPE, SIGILL, SIGKILL,
    SIGQUIT, SIGSEGV, SIGSTOP, SIGSYS, SIGTRAP, SIGTSTP, SIGTTIN, SIGTTOU, SIGURG, SIGWINCH,
    SIGXCPU, SIGXFSZ, SS_DISABLE, SS_ONSTACK,
};

use crate::arch;
use crate::syscall::deliver::StackRecord;
use crate::syscall::process::Process;
use crate::syscall::uaccess;

/// The only `sigsetsize` the kernel accepts: one 64-bit word.
///
/// The C library's own `sigset_t` is far larger, and the system call takes a
/// size precisely so the two can differ. Linux refuses anything else with
/// `EINVAL`, and so does this.
pub(crate) const SIGSET_SIZE: u64 = 8;

/// `SS_AUTODISARM`: clear the alternate stack when a handler is entered on
/// it. Not in `libs/linux-abi`; it is a flag *bit* on top of the mode, and the
/// mode check has to take it off before comparing.
const SS_AUTODISARM: i32 = i32::MIN;

/// Bytes in a native word.
const WORD: usize = size_of::<usize>();

/// Bytes in the kernel's `struct sigaction` on this architecture.
const SIGACTION_BYTES: usize = WORD * 3 + 8;

/// Bytes in `stack_t` on this architecture.
const STACK_BYTES: usize = WORD * 3;

/// The two signals nothing may catch, block or ignore.
pub(crate) const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// The signals whose default action is to stop.
const STOP_SIGNALS: u64 = bit(SIGSTOP) | bit(SIGTSTP) | bit(SIGTTIN) | bit(SIGTTOU);

/// The signals a fault raises, which Linux hands to a program before any
/// other pending signal: the instruction that faulted is the one it is about.
const SYNCHRONOUS: u64 =
    bit(SIGSEGV) | bit(SIGBUS) | bit(SIGILL) | bit(SIGTRAP) | bit(SIGFPE) | bit(SIGSYS);

/// What a program asked to happen on one signal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Disposition {
    /// The handler address, or `SIG_DFL` or `SIG_IGN`.
    pub(crate) handler: u64,
    /// The `SA_*` flags.
    pub(crate) flags: u64,
    /// The trampoline the handler returns into, which issues `rt_sigreturn`.
    pub(crate) restorer: u64,
    /// Signals blocked while the handler runs, never including the two that
    /// cannot be blocked.
    pub(crate) mask: u64,
}

/// What happens to a signal nobody installed a handler for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefaultAction {
    /// The process ends, reported as killed by the signal.
    Terminate,
    /// The same, for the signals Linux would also dump core for. Nothing
    /// dumps core here, so a waiting parent sees no core flag.
    Core,
    /// Nothing happens.
    Ignore,
    /// The process stops until `SIGCONT`.
    Stop,
    /// A stopped process continues; nothing else happens.
    Continue,
}

/// The default action of `signal`, from `signal(7)`'s table.
pub(crate) const fn default_action(signal: u32) -> DefaultAction {
    match signal {
        SIGCHLD | SIGURG | SIGWINCH => DefaultAction::Ignore,
        SIGCONT => DefaultAction::Continue,
        SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => DefaultAction::Stop,
        SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE | SIGSEGV | SIGXCPU | SIGXFSZ
        | SIGSYS => DefaultAction::Core,
        _ => DefaultAction::Terminate,
    }
}

/// Who or what raised a pending signal: the `siginfo` a handler is given.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The kernel, for its own reasons: an interval timer, a broken pipe.
    #[default]
    Kernel,
    /// `kill` from the process with this pid.
    User {
        /// The sender.
        pid: u32,
    },
    /// `tkill` or `tgkill` from the process with this pid.
    Thread {
        /// The sender.
        pid: u32,
    },
    /// A child changed state.
    Child {
        /// `CLD_EXITED`, `CLD_KILLED`, `CLD_STOPPED` or `CLD_CONTINUED`.
        code: i32,
        /// The child.
        pid: u32,
        /// Its exit code, or the signal that ended, stopped or continued it.
        status: i32,
    },
    /// A fault in the program's own instruction.
    Fault {
        /// `SEGV_MAPERR`, `SEGV_ACCERR`, `ILL_ILLOPC` and their like.
        code: i32,
        /// The address the fault was about.
        address: u64,
    },
}

/// `si_code` for a signal the kernel raised.
const SI_KERNEL: i32 = 0x80;
/// `si_code` for `kill`.
const SI_USER: i32 = 0;
/// `si_code` for `tkill` and `tgkill`.
const SI_TKILL: i32 = -6;

/// Bytes in `siginfo_t` on every architecture.
pub(crate) const SIGINFO_BYTES: usize = 128;

impl Origin {
    /// The `siginfo_t` a handler sees for `signal` raised this way.
    ///
    /// `si_signo`, `si_errno` and `si_code` are three `int`s; the union starts
    /// at the first pointer-aligned offset after them, 16 on a 64-bit machine
    /// and 12 on a 32-bit one. In it, `kill` and a child put the pid and uid
    /// first, a child its status after them, and a fault the address.
    pub(crate) fn encode(self, signal: u32) -> [u8; SIGINFO_BYTES] {
        let mut info = [0_u8; SIGINFO_BYTES];
        let union = if WORD == 8 { 16 } else { 12 };
        put_int(&mut info, 0, signal as i32);
        let code = match self {
            Origin::Kernel => SI_KERNEL,
            Origin::User { pid } => {
                put_int(&mut info, union, pid as i32);
                SI_USER
            }
            Origin::Thread { pid } => {
                put_int(&mut info, union, pid as i32);
                SI_TKILL
            }
            Origin::Child { code, pid, status } => {
                put_int(&mut info, union, pid as i32);
                put_int(&mut info, union + 8, status);
                code
            }
            Origin::Fault { code, address } => {
                let _ = put_word(&mut info, union, address);
                code
            }
        };
        put_int(&mut info, 8, code);
        info
    }
}

/// Put an `int` into a `siginfo` buffer. Every offset used is a constant well
/// inside the 128 bytes.
fn put_int(buffer: &mut [u8], at: usize, value: i32) {
    if let Some(slot) = buffer.get_mut(at..at + 4) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
}

/// An installed alternate signal stack. A size of zero means none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AltStack {
    /// Its lowest address.
    sp: u64,
    /// Its size in bytes.
    size: u64,
    /// Whether `SS_AUTODISARM` was asked for.
    autodisarm: bool,
}

/// `ITIMER_REAL`: when `SIGALRM` is next due, and how often after.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Alarm {
    /// When it is next due, in nanoseconds on the counter; zero when disarmed.
    pub(crate) deadline: u64,
    /// The period it re-arms with once due; zero for a one-shot.
    pub(crate) interval: u64,
}

/// What [`Signals::post`] decided about a signal sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Posted {
    /// Ignored, and gone.
    Discarded,
    /// Its default action ends the process, and nothing blocks it: the sender
    /// ends the process at once rather than leaving it to find out.
    Fatal,
    /// Waiting for the process to reach user mode, or to unblock it.
    Pending,
}

/// A system call to restart because a signal interrupted it, captured when the
/// call returned a restart code.
///
/// The number and first argument are kept because the return register
/// overwrites one or the other of them: the number on x86-64, whose `RAX` is
/// both, and the first argument on the two Arm architectures, whose `x0`/`r0`
/// is both. The way back to user mode puts back whichever its architecture
/// clobbered before rewinding the program counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Restart {
    /// The call's number, in the architecture's own table.
    pub(crate) nr: u64,
    /// The value in its first argument register when it was made.
    pub(crate) arg0: u64,
}

/// A sleep to resume through `restart_syscall`: what `nanosleep` and
/// `clock_nanosleep` leave behind when a signal interrupts them with time
/// still to run, so the resume waits out the time left rather than starting
/// the whole sleep again. Linux keeps this in `current->restart_block`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestartBlock {
    /// The absolute deadline on the counter the sleep runs to.
    pub(crate) deadline: u64,
    /// Where the time left is written if the resume is interrupted again, or
    /// zero for an absolute sleep, which never reports a remainder.
    pub(crate) rem: u64,
    /// The width of the `timespec` at `rem`.
    pub(crate) width: crate::syscall::time::TimeWidth,
}

/// A signal taken off the pending set, with everything delivery needs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Taken {
    /// Its number.
    pub(crate) signal: u32,
    /// Who raised it.
    pub(crate) origin: Origin,
    /// What the program asked to happen.
    pub(crate) action: Disposition,
}

/// Everything about signals a process has told the kernel, and what is
/// waiting to reach it.
///
/// The two per-signal tables are on the heap rather than inline. Inline they
/// are three kibibytes, and a `Process` carries this by value through
/// `Process::new`, `registry::register` and `Arc::new` -- each a copy on a
/// sixteen-kibibyte kernel stack. That was the x86-64 boot's double fault in
/// `Process::new`, and the AArch64 boot's hang at the same check.
#[derive(Debug, Clone)]
pub(crate) struct Signals {
    /// Signal `n` is at index `n - 1`. Always 64 entries.
    actions: Box<[Disposition]>,
    /// The blocked mask, bit `n - 1` for signal `n`.
    blocked: u64,
    /// The alternate stack, if one is installed.
    alt: AltStack,
    /// Signals raised and not yet delivered. One bit each: a second `SIGUSR1`
    /// sent before the first is delivered is the same pending signal, as on
    /// Linux for the classic signals.
    pending: u64,
    /// Who raised each pending signal, first sender kept. Always 64 entries.
    origins: Box<[Origin]>,
    /// The mask `rt_sigsuspend` replaced, to be put back on the way to user
    /// mode -- after a handler's frame has saved it, if one runs.
    saved_mask: Option<u64>,
    /// `ITIMER_REAL`.
    alarm: Alarm,
    /// The call to restart, set when a blocking call returned a restart code
    /// and consumed on the way back to user mode.
    restart: Option<Restart>,
    /// A sleep to resume through `restart_syscall`.
    restart_block: Option<RestartBlock>,
}

impl Signals {
    /// Whether a child that ends is released without being waited for:
    /// `SIGCHLD` ignored, or its handler installed with `SA_NOCLDWAIT`. What a
    /// daemon that never calls `wait` relies on to leave no zombies.
    pub(crate) fn reaps_children_automatically(&self) -> bool {
        self.actions
            .get(SIGCHLD as usize - 1)
            .is_some_and(|action| action.handler == SIG_IGN || action.flags & SA_NOCLDWAIT != 0)
    }

    /// Whether a child stopping or continuing should go untold: `SIGCHLD`
    /// installed with `SA_NOCLDSTOP`.
    pub(crate) fn ignores_child_stops(&self) -> bool {
        self.actions
            .get(SIGCHLD as usize - 1)
            .is_some_and(|action| action.flags & SA_NOCLDSTOP != 0)
    }

    /// Replace the blocked mask with `mask`, returning the mask it replaced:
    /// what `ppoll` and `pselect6` wait under, and put back afterwards.
    /// `SIGKILL` and `SIGSTOP` are never blocked, whatever `mask` says.
    pub(crate) fn replace_blocked(&mut self, mask: u64) -> u64 {
        core::mem::replace(&mut self.blocked, mask & !UNBLOCKABLE)
    }

    /// What `execve` does to them: every handler goes back to the default,
    /// because the new program has none of the old one's code to run; a signal
    /// that was ignored stays ignored, which is how `nohup` works; the blocked
    /// mask, the pending signals and the interval timer are kept; and the
    /// alternate stack goes, since it was the old program's memory.
    pub(crate) fn reset_for_exec(&mut self) {
        for action in self.actions.iter_mut() {
            let ignored = action.handler == SIG_IGN;
            *action = Disposition::default();
            if ignored {
                action.handler = SIG_IGN;
            }
        }
        self.alt = AltStack::default();
    }

    /// What a `fork` child starts without: its parent's pending signals and
    /// interval timer, which were its parent's.
    pub(crate) fn reset_for_fork(&mut self) {
        self.pending = 0;
        self.origins.fill(Origin::Kernel);
        self.saved_mask = None;
        self.alarm = Alarm::default();
        self.restart = None;
        self.restart_block = None;
    }

    /// Record that the running call is to be restarted if a signal it is about
    /// to meet allows it: its number and first argument, kept because the
    /// return register overwrites one of them. Set by dispatch when a blocking
    /// call returns a restart code.
    pub(crate) const fn mark_restart(&mut self, nr: u64, arg0: u64) {
        self.restart = Some(Restart { nr, arg0 });
    }

    /// Take the call to restart, if one was marked. Consumed once, on the way
    /// back to user mode, so a later trap cannot act on a stale one.
    pub(crate) const fn take_restart(&mut self) -> Option<Restart> {
        self.restart.take()
    }

    /// Leave a sleep to resume through `restart_syscall`.
    pub(crate) const fn set_restart_block(&mut self, block: RestartBlock) {
        self.restart_block = Some(block);
    }

    /// Take the sleep left for `restart_syscall` to resume.
    pub(crate) const fn take_restart_block(&mut self) -> Option<RestartBlock> {
        self.restart_block.take()
    }

    /// Install a disposition directly, for the boot self-checks: they drive the
    /// delivery decision without a user-space `rt_sigaction`, which would need a
    /// `struct sigaction` written into a program's own memory first.
    pub(crate) fn install_action(&mut self, signal: u32, handler: u64, flags: u64) {
        if let Ok(index) = index_of(signal)
            && let Some(slot) = self.actions.get_mut(index)
        {
            *slot = Disposition {
                handler,
                flags,
                restorer: 0,
                mask: 0,
            };
        }
    }

    /// Arm an alternate stack directly, for the boot self-check that a handler
    /// with `SA_ONSTACK` is placed on it.
    pub(crate) const fn arm_alt_stack_for_check(&mut self, sp: u64, size: u64) {
        self.alt = AltStack {
            sp,
            size,
            autodisarm: false,
        };
    }

    /// Pending signals nothing blocks: what delivery has to act on.
    pub(crate) const fn deliverable(&self) -> u64 {
        self.pending & !self.blocked
    }

    /// Pending signals, blocked ones included.
    pub(crate) const fn pending(&self) -> u64 {
        self.pending
    }

    /// The blocked mask.
    pub(crate) const fn blocked(&self) -> u64 {
        self.blocked
    }

    /// Whether the way back to user mode has anything to do here: a signal to
    /// deliver, or a mask `rt_sigsuspend` left to put back.
    pub(crate) const fn needs_attention(&self) -> bool {
        self.deliverable() != 0 || self.saved_mask.is_some()
    }

    /// Record `signal` sent to this process, or decide it needs no recording.
    ///
    /// Linux's order. A signal the program ignores, explicitly or by default,
    /// is discarded -- unless it is blocked, because the program may install a
    /// handler before it unblocks it. One whose default action is fatal and
    /// which nothing blocks is fatal now. Sending a stop signal cancels a
    /// pending `SIGCONT`, and `SIGCONT` cancels pending stops.
    pub(crate) fn post(&mut self, signal: u32, origin: Origin) -> Posted {
        let Ok(index) = index_of(signal) else {
            return Posted::Discarded;
        };
        if signal == SIGKILL {
            return Posted::Fatal;
        }
        if bit(signal) & STOP_SIGNALS != 0 {
            self.pending &= !bit(SIGCONT);
        }
        if signal == SIGCONT {
            self.pending &= !STOP_SIGNALS;
        }
        let action = self.actions.get(index).copied().unwrap_or_default();
        let blocked = self.blocked & bit(signal) != 0;
        let default = default_action(signal);
        let ignored = action.handler == SIG_IGN
            || action.handler == SIG_DFL
                && matches!(default, DefaultAction::Ignore | DefaultAction::Continue);
        if ignored && !blocked {
            return Posted::Discarded;
        }
        if action.handler == SIG_DFL
            && !blocked
            && matches!(default, DefaultAction::Terminate | DefaultAction::Core)
        {
            return Posted::Fatal;
        }
        if self.pending & bit(signal) == 0
            && let Some(slot) = self.origins.get_mut(index)
        {
            *slot = origin;
        }
        self.pending |= bit(signal);
        Posted::Pending
    }

    /// Raise `signal` for a fault the program cannot be allowed to ignore: a
    /// blocked or ignored one is unblocked and reset to its default first, so
    /// the program either handles it or dies of it, and never retries the
    /// instruction for ever. Linux's `force_sig_info`.
    pub(crate) fn force(&mut self, signal: u32, origin: Origin) -> Posted {
        let Ok(index) = index_of(signal) else {
            return Posted::Discarded;
        };
        let blocked = self.blocked & bit(signal) != 0;
        if let Some(action) = self.actions.get_mut(index)
            && (blocked || action.handler == SIG_IGN)
        {
            action.handler = SIG_DFL;
        }
        self.blocked &= !bit(signal);
        self.post(signal, origin)
    }

    /// Take the next deliverable signal off the pending set: a fault's first,
    /// then the lowest numbered, as Linux chooses.
    pub(crate) fn take_next(&mut self) -> Option<Taken> {
        let ready = self.deliverable();
        if ready == 0 {
            return None;
        }
        let chosen = if ready & SYNCHRONOUS != 0 {
            ready & SYNCHRONOUS
        } else {
            ready
        };
        self.take(chosen.trailing_zeros() + 1)
    }

    /// Take the lowest pending signal in `set`, blocked or not: what
    /// `rt_sigtimedwait` accepts.
    pub(crate) fn take_from(&mut self, set: u64) -> Option<Taken> {
        let ready = self.pending & set;
        if ready == 0 {
            return None;
        }
        self.take(ready.trailing_zeros() + 1)
    }

    /// Take `signal` off the pending set.
    fn take(&mut self, signal: u32) -> Option<Taken> {
        let index = index_of(signal).ok()?;
        self.pending &= !bit(signal);
        let origin = self.origins.get(index).copied().unwrap_or_default();
        let action = self.actions.get(index).copied().unwrap_or_default();
        Some(Taken {
            signal,
            origin,
            action,
        })
    }

    /// Where a handler's frame goes below, for a program whose stack pointer
    /// is `sp`: the alternate stack's top when the handler asked for it and the
    /// program is not already on it, and otherwise the program's own stack
    /// past the red zone the ABI lets a leaf function use without moving `sp`.
    pub(crate) fn frame_base(&self, flags: u64, sp: u64) -> u64 {
        let base = sp.wrapping_sub(arch::SIGNAL_RED_ZONE);
        if flags & SA_ONSTACK != 0 && self.alt.size != 0 && !self.on_alt_stack(base) {
            self.alt.sp.wrapping_add(self.alt.size)
        } else {
            base
        }
    }

    /// Whether `sp` is on the alternate stack. Never, with `SS_AUTODISARM`:
    /// the stack is disarmed while a handler runs on it, so nothing is on it.
    fn on_alt_stack(&self, sp: u64) -> bool {
        !self.alt.autodisarm && sp > self.alt.sp && sp - self.alt.sp <= self.alt.size
    }

    /// Enter a handler for `taken`: answer the mask its frame saves and the
    /// alternate stack as its frame records it, then block what the handler
    /// asked to have blocked, forget the handler if it was one-shot, and
    /// disarm the alternate stack if it asked for that.
    pub(crate) fn enter_handler(&mut self, taken: &Taken) -> (u64, StackRecord) {
        let saved = self.saved_mask.take().unwrap_or(self.blocked);
        let mut adding = taken.action.mask;
        if taken.action.flags & SA_NODEFER == 0 {
            adding |= bit(taken.signal);
        }
        self.blocked = (self.blocked | adding) & !UNBLOCKABLE;
        if taken.action.flags & SA_RESETHAND != 0
            && let Ok(index) = index_of(taken.signal)
            && let Some(action) = self.actions.get_mut(index)
        {
            action.handler = SIG_DFL;
        }
        let record = StackRecord {
            sp: self.alt.sp,
            flags: self.alt_flags(),
            size: self.alt.size,
        };
        if self.alt.autodisarm {
            self.alt = AltStack::default();
        }
        (saved, record)
    }

    /// The alternate stack's flags as `sigaltstack` and a frame report them.
    fn alt_flags(&self) -> i32 {
        let mut flags = if self.alt.size == 0 { SS_DISABLE } else { 0 };
        if self.alt.autodisarm {
            flags |= SS_AUTODISARM;
        }
        flags
    }

    /// Leave a handler, as `rt_sigreturn` does: the mask its frame saved comes
    /// back, and so does the alternate stack, if that is still a stack
    /// `sigaltstack` would accept from a program whose stack pointer is `sp`.
    pub(crate) fn leave_handler(&mut self, mask: u64, stack: StackRecord, sp: u64) {
        self.blocked = mask & !UNBLOCKABLE;
        let _ = self.install_alt_stack((stack.sp, stack.flags, stack.size), sp);
    }

    /// Put back the mask `rt_sigsuspend` replaced, if no handler's frame took
    /// it first.
    pub(crate) fn restore_saved_mask(&mut self) {
        if let Some(mask) = self.saved_mask.take() {
            self.blocked = mask;
        }
    }

    /// Block `mask` instead until the way back to user mode, as
    /// `rt_sigsuspend` does.
    pub(crate) fn suspend_with(&mut self, mask: u64) {
        if self.saved_mask.is_none() {
            self.saved_mask = Some(self.blocked);
        }
        self.blocked = mask & !UNBLOCKABLE;
    }

    /// `ITIMER_REAL` as it stands.
    pub(crate) const fn alarm(&self) -> Alarm {
        self.alarm
    }

    /// Replace `ITIMER_REAL`, and answer what it was.
    pub(crate) const fn set_alarm(&mut self, alarm: Alarm) -> Alarm {
        core::mem::replace(&mut self.alarm, alarm)
    }

    /// Whether `ITIMER_REAL` is due at `now`, re-arming or disarming it if so.
    /// A periodic timer that fell more than a period behind is re-armed from
    /// `now`, so a stalled machine owes one `SIGALRM`, not a backlog.
    pub(crate) fn tick_alarm(&mut self, now: u64) -> bool {
        let Alarm { deadline, interval } = self.alarm;
        if deadline == 0 || deadline > now {
            return false;
        }
        self.alarm.deadline = match deadline.checked_add(interval) {
            _ if interval == 0 => 0,
            Some(next) if next > now => next,
            _ => now.saturating_add(interval),
        };
        true
    }

    /// Install an alternate stack from `sigaltstack`'s `(sp, flags, size)`,
    /// for a program whose stack pointer is `sp_now`.
    fn install_alt_stack(&mut self, stack: (u64, i32, u64), sp_now: u64) -> Result<(), Errno> {
        let (sp, flags, size) = stack;
        if self.on_alt_stack(sp_now) {
            return Err(Errno::EPERM);
        }
        let autodisarm = flags & SS_AUTODISARM != 0;
        self.alt = match flags & !SS_AUTODISARM {
            SS_DISABLE => AltStack::default(),
            0 | SS_ONSTACK if size < minimum_stack() => return Err(Errno::ENOMEM),
            0 | SS_ONSTACK => AltStack {
                sp,
                size,
                autodisarm,
            },
            _ => return Err(Errno::EINVAL),
        };
        Ok(())
    }
}

impl Default for Signals {
    fn default() -> Self {
        Signals {
            actions: vec![Disposition::default(); NSIG as usize].into_boxed_slice(),
            blocked: 0,
            alt: AltStack::default(),
            pending: 0,
            origins: vec![Origin::Kernel; NSIG as usize].into_boxed_slice(),
            saved_mask: None,
            alarm: Alarm::default(),
            restart: None,
            restart_block: None,
        }
    }
}

/// The mask bit for signal `number`, which must be `1..=64`.
pub(crate) const fn bit(number: u32) -> u64 {
    1 << (number - 1)
}

/// `rt_sigaction`.
///
/// Linux's order, which a program can observe: the new action is read before
/// anything changes, so a bad `act` pointer changes nothing; the old action
/// is written after, so a bad `oldact` pointer reports `EFAULT` with the new
/// action already installed. A pending signal the new action ignores is
/// discarded, as it would have been had it arrived now.
pub(crate) fn sys_rt_sigaction(
    process: &Process,
    signal: u32,
    act: u64,
    old: u64,
    sigsetsize: u64,
) -> Result<usize, Errno> {
    if sigsetsize != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let index = index_of(signal)?;
    let new = if act == 0 {
        None
    } else if signal == SIGKILL || signal == SIGSTOP {
        return Err(Errno::EINVAL);
    } else {
        Some(read_sigaction(process, act)?)
    };

    let previous = process.with_signals(|signals| {
        let slot = signals.actions.get_mut(index).ok_or(Errno::EINVAL)?;
        let previous = *slot;
        if let Some(mut new) = new {
            new.mask &= !UNBLOCKABLE;
            *slot = new;
            let ignored = new.handler == SIG_IGN
                || new.handler == SIG_DFL
                    && matches!(
                        default_action(signal),
                        DefaultAction::Ignore | DefaultAction::Continue
                    );
            if ignored {
                signals.pending &= !bit(signal);
            }
        }
        Ok(previous)
    })?;

    if old != 0 {
        write_sigaction(process, old, previous)?;
    }
    Ok(0)
}

/// `rt_sigprocmask`.
///
/// `how` is only looked at when there is a set to apply, as on Linux: a query
/// with a nonsense `how` succeeds. When `how` is refused the old mask is not
/// written either. A signal this unblocks is delivered on the way back to user
/// mode, before the call appears to return.
pub(crate) fn sys_rt_sigprocmask(
    process: &Process,
    how: u32,
    set: u64,
    old: u64,
    sigsetsize: u64,
) -> Result<usize, Errno> {
    if sigsetsize != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let request = if set == 0 {
        None
    } else {
        let mut bytes = [0_u8; 8];
        uaccess::copy_from_user(process.space(), set, &mut bytes).map_err(|_| Errno::EFAULT)?;
        Some(u64::from_le_bytes(bytes) & !UNBLOCKABLE)
    };

    let previous = process.with_signals(|signals| {
        let previous = signals.blocked;
        if let Some(request) = request {
            signals.blocked = match how {
                SIG_BLOCK => previous | request,
                SIG_UNBLOCK => previous & !request,
                SIG_SETMASK => request,
                _ => return Err(Errno::EINVAL),
            };
        }
        Ok(previous)
    })?;

    if old != 0 {
        uaccess::copy_to_user(process.space(), old, &previous.to_le_bytes())
            .map_err(|_| Errno::EFAULT)?;
    }
    Ok(0)
}

/// `sigaltstack`, for a program whose stack pointer is `sp`.
///
/// The old stack is reported only if the new one was accepted, which is
/// Linux's order. A program running on its alternate stack sees `SS_ONSTACK`
/// in the old flags and may not change the stack: `EPERM`. The boot
/// self-check, which has no stack pointer, passes zero, which is on no stack.
pub(crate) fn sys_sigaltstack(
    process: &Process,
    ss: u64,
    old: u64,
    sp: u64,
) -> Result<usize, Errno> {
    let request = if ss == 0 {
        None
    } else {
        Some(read_stack(process, ss)?)
    };

    let (previous, on_stack) = process.with_signals(|signals| {
        let previous = signals.alt;
        let on_stack = signals.on_alt_stack(sp);
        let flags = signals.alt_flags();
        if let Some(request) = request {
            signals.install_alt_stack(request, sp)?;
        }
        Ok::<_, Errno>(((previous, flags), on_stack))
    })?;

    if old != 0 {
        let (previous, mut flags) = previous;
        if on_stack {
            flags = SS_ONSTACK;
        }
        write_stack(process, old, previous.sp, flags, previous.size)?;
    }
    Ok(0)
}

/// `MINSIGSTKSZ` for this architecture: the smallest alternate stack the
/// kernel accepts. AArch64's is larger because its signal frame carries the
/// full SIMD state.
const fn minimum_stack() -> u64 {
    match arch::ARCH {
        Arch::AArch64 => 5120,
        Arch::X86_64 | Arch::Armv7a => 2048,
    }
}

/// The table index for `signal`, or `EINVAL` outside `1..=64`.
fn index_of(signal: u32) -> Result<usize, Errno> {
    if signal == 0 || signal > NSIG {
        return Err(Errno::EINVAL);
    }
    usize::try_from(signal - 1).map_err(|_| Errno::EINVAL)
}

/// Read a `struct sigaction` from the program.
fn read_sigaction(process: &Process, at: u64) -> Result<Disposition, Errno> {
    let mut buffer = [0_u8; 32];
    let bytes = buffer.get_mut(..SIGACTION_BYTES).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)?;
    let mut mask = [0_u8; 8];
    mask.copy_from_slice(bytes.get(WORD * 3..).ok_or(Errno::EINVAL)?);
    Ok(Disposition {
        handler: word_at(bytes, 0)?,
        flags: word_at(bytes, WORD)?,
        restorer: word_at(bytes, WORD * 2)?,
        mask: u64::from_le_bytes(mask),
    })
}

/// Write a `struct sigaction` to the program.
fn write_sigaction(process: &Process, at: u64, action: Disposition) -> Result<(), Errno> {
    let mut buffer = [0_u8; 32];
    let bytes = buffer.get_mut(..SIGACTION_BYTES).ok_or(Errno::EINVAL)?;
    put_word(bytes, 0, action.handler)?;
    put_word(bytes, WORD, action.flags)?;
    put_word(bytes, WORD * 2, action.restorer)?;
    bytes
        .get_mut(WORD * 3..)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(&action.mask.to_le_bytes());
    uaccess::copy_to_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)
}

/// Read a `stack_t` from the program: pointer, flags, size.
fn read_stack(process: &Process, at: u64) -> Result<(u64, i32, u64), Errno> {
    let mut buffer = [0_u8; 24];
    let bytes = buffer.get_mut(..STACK_BYTES).ok_or(Errno::EINVAL)?;
    uaccess::copy_from_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)?;
    let mut flags = [0_u8; 4];
    flags.copy_from_slice(bytes.get(WORD..WORD + 4).ok_or(Errno::EINVAL)?);
    Ok((
        word_at(bytes, 0)?,
        i32::from_le_bytes(flags),
        word_at(bytes, WORD * 2)?,
    ))
}

/// Write a `stack_t` to the program. Padding goes out as zero.
fn write_stack(process: &Process, at: u64, sp: u64, flags: i32, size: u64) -> Result<(), Errno> {
    let mut buffer = [0_u8; 24];
    let bytes = buffer.get_mut(..STACK_BYTES).ok_or(Errno::EINVAL)?;
    put_word(bytes, 0, sp)?;
    bytes
        .get_mut(WORD..WORD + 4)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(&flags.to_le_bytes());
    put_word(bytes, WORD * 2, size)?;
    uaccess::copy_to_user(process.space(), at, bytes).map_err(|_| Errno::EFAULT)
}

/// The native word at `offset`, zero-extended.
fn word_at(bytes: &[u8], offset: usize) -> Result<u64, Errno> {
    let end = offset.checked_add(WORD).ok_or(Errno::EINVAL)?;
    let mut word = [0_u8; 8];
    word.get_mut(..WORD)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(bytes.get(offset..end).ok_or(Errno::EINVAL)?);
    Ok(u64::from_le_bytes(word))
}

/// Put `value` at `offset` as a native word. On a 32-bit machine every value
/// written here came from a 32-bit read, so the truncation drops nothing.
fn put_word(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), Errno> {
    let end = offset.checked_add(WORD).ok_or(Errno::EINVAL)?;
    bytes
        .get_mut(offset..end)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(value.to_le_bytes().get(..WORD).ok_or(Errno::EINVAL)?);
    Ok(())
}
