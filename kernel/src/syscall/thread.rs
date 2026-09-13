//! A thread: the part of a program that runs, as distinct from the program.
//!
//! # What is the thread's and what is the process's
//!
//! A [`Process`] is what a program has: its address space, descriptors,
//! credentials, signal dispositions and children. A [`Thread`] is what one
//! line of execution through it has: its thread id, and the address
//! `set_tid_address` or `CLONE_CHILD_CLEARTID` asked to have cleared when that
//! line ends, which is how `pthread_join` learns it has. Its kernel stack and
//! its saved user registers stay on the scheduler's [`crate::sched::Task`],
//! which holds the thread, as the thread holds its process.
//!
//! # One thread per process, for now
//!
//! Every process runs exactly one thread, its leader, whose thread id is the
//! process id: what `gettid` answers and what a libc hands `tgkill`. A second
//! thread, with a number of its own from the same space, is `clone` with
//! `CLONE_THREAD`, which is still refused.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sched;
use crate::syscall::process::Process;

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
}

impl Thread {
    /// The first thread of `process`, numbered by its pid.
    pub(crate) fn leader(process: &Arc<Process>) -> Thread {
        Thread {
            tid: process.pid(),
            process: Arc::clone(process),
            clear_child_tid: AtomicU64::new(0),
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
}

/// The thread the running task runs, or `None` for a kernel thread.
pub(crate) fn current() -> Option<Arc<Thread>> {
    sched::current().and_then(|task| task.thread().cloned())
}
