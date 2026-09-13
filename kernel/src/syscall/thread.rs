//! A thread: the part of a program that runs, as distinct from the program.
//!
//! # What is the thread's and what is the process's
//!
//! A [`Process`] is what a program has: its address space, descriptors,
//! credentials, signal dispositions and children. A [`Thread`] is what one
//! line of execution through it has: its thread id; the address
//! `set_tid_address` or `CLONE_CHILD_CLEARTID` asked to have cleared when that
//! line ends, which is how `pthread_join` learns it has; and its own signal
//! state -- the blocked mask, the alternate stack, the signals sent to it
//! alone and the call it is to restart ([`ThreadSignals`]). Its kernel stack
//! and its saved user registers stay on the scheduler's
//! [`crate::sched::Task`], which holds the thread, as the thread holds its
//! process.
//!
//! # One thread per process, for now
//!
//! Every process runs exactly one thread, its leader, whose thread id is the
//! process id: what `gettid` answers and what a libc hands `tgkill`. A second
//! thread, with a number of its own from the same space, is `clone` with
//! `CLONE_THREAD`, which is still refused.
//!
//! # Locks
//!
//! A thread's signal state has a lock of its own, taken inside its process's
//! signal lock when both are needed ([`Thread::with_signals`]) and never the
//! other way round.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sched;
use crate::sync::SpinLock;
use crate::syscall::process::Process;
use crate::syscall::signal::{self, Signals, ThreadSignals};

/// One line of execution through a process.
#[derive(Debug)]
pub(crate) struct Thread {
    /// Its thread id. A leader's is its process's pid, and zero with it.
    tid: u32,
    /// The program it runs. Holding it is what keeps the process alive while
    /// the thread is: a process does not own its threads, its threads own it.
    process: Arc<Process>,
    /// The address `set_tid_address` or `CLONE_CHILD_CLEARTID` registered, to
    /// be zeroed and woken when the thread ends. Zero means none.
    clear_child_tid: AtomicU64,
    /// Its own signal state.
    signals: SpinLock<ThreadSignals>,
}

impl Thread {
    /// The first thread of `process`, numbered by its pid, blocking nothing.
    pub(crate) fn leader(process: &Arc<Process>) -> Thread {
        Thread::with(process, ThreadSignals::default())
    }

    /// The first thread of a fork child `process`, made by `parent`: it
    /// inherits the parent's blocked mask and alternate stack, and nothing the
    /// parent was sent or was in the middle of.
    pub(crate) fn forked(process: &Arc<Process>, parent: &Thread) -> Thread {
        let inherited = parent.with_own_signals(|signals| signals.inherited());
        Thread::with(process, ThreadSignals::from(inherited))
    }

    /// The first thread of `process`, numbered by its pid, with `signals`.
    fn with(process: &Arc<Process>, signals: ThreadSignals) -> Thread {
        Thread {
            tid: process.pid(),
            process: Arc::clone(process),
            clear_child_tid: AtomicU64::new(0),
            signals: SpinLock::new(signals),
        }
    }

    /// Its thread id; zero if its process was made with every pid in use.
    pub(crate) fn tid(&self) -> u32 {
        self.tid
    }

    /// The process it runs.
    pub(crate) fn process(&self) -> &Arc<Process> {
        &self.process
    }

    /// Record the address to clear when it ends, and report its thread id,
    /// which is what `set_tid_address` returns.
    ///
    /// musl uses the *return value* as its process id during startup, so this
    /// must answer with a real identifier. It is one of the few calls where a
    /// plausible-looking stub is worse than an error: an `ENOSYS` musl
    /// survives, a wrong pid it does not.
    pub(crate) fn set_clear_child_tid(&self, address: u64) -> u32 {
        self.clear_child_tid.store(address, Ordering::Release);
        self.tid
    }

    /// The address registered to be cleared, or zero.
    pub(crate) fn clear_child_tid(&self) -> u64 {
        self.clear_child_tid.load(Ordering::Acquire)
    }

    /// Take the address registered to be cleared, leaving none: once, when
    /// the thread ends or its program is replaced.
    pub(crate) fn take_clear_child_tid(&self) -> u64 {
        self.clear_child_tid.swap(0, Ordering::AcqRel)
    }

    /// Read or change its own signal state, under its lock.
    ///
    /// A closure rather than a guard, as for [`Process::with_signals`]: every
    /// copy to or from the program is outside it.
    pub(crate) fn with_own_signals<R>(&self, change: impl FnOnce(&mut ThreadSignals) -> R) -> R {
        change(&mut self.signals.lock())
    }

    /// Read or change its process's signal state and its own together: the
    /// process's lock first, then its own.
    pub(crate) fn with_signals<R>(
        &self,
        change: impl FnOnce(&mut Signals, &mut ThreadSignals) -> R,
    ) -> R {
        self.process
            .with_signals(|shared| change(shared, &mut self.signals.lock()))
    }

    /// Whether a wait it is in should end: its process has ended, or a signal
    /// it does not block is pending for it or for its process.
    pub(crate) fn signal_pending(&self) -> bool {
        self.process.is_terminated()
            || self.with_signals(|shared, own| signal::deliverable(shared, own) != 0)
    }
}

/// The thread the running task runs, or `None` for a kernel thread.
pub(crate) fn current() -> Option<Arc<Thread>> {
    sched::current().and_then(|task| task.thread().cloned())
}

/// The running task's thread, if it is one of `process`'s.
pub(crate) fn current_of(process: &Process) -> Option<Arc<Thread>> {
    current().filter(|thread| core::ptr::eq(thread.process().as_ref(), process))
}
