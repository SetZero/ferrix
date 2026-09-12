//! Jobs: containers of processes, and where kill authority lives.
//!
//! `docs/ARCHITECTURE.md` §3 and §7. A userspace driver that wedges has to be
//! killable as a unit, together with anything it started, and a job is that
//! unit: a tree of jobs, each holding processes, where killing one ends every
//! process in it and in every job beneath it.
//!
//! # Who keeps whom alive
//!
//! A job holds its processes and its children weakly, and a child holds its
//! parent strongly. So a job lives as long as a handle names it or a job
//! beneath it exists, and a process that has exited is not kept alive by the
//! job it was in — the job's list is where it is *found*, not what keeps it.
//!
//! # A killed job stays killed
//!
//! Killing marks the job under the same lock that adding a process or a child
//! takes. Anything added before the mark is found by the kill; anything
//! arriving after it is refused. There is no window in which a process can
//! join a job that is being killed and survive it.

use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;

use ferrix_sync::SpinLock;

use crate::sched::WaitQueue;
use crate::syscall::process::{self, Process};

/// The status a process ended by a job kill reports.
///
/// 128 plus `SIGKILL`, which is what a shell prints for a killed child and
/// what `process::kill`'s own boot check uses.
pub(crate) const KILLED_STATUS: i32 = 137;

/// Why a job refused something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobError {
    /// The job has been killed, and takes no new processes or children.
    Killed,
}

/// A job.
#[derive(Debug)]
pub(crate) struct Job {
    /// The job this one is inside, kept alive by it. `None` for a root.
    ///
    /// Never read: holding it is its whole purpose, so a parent outlives every
    /// job beneath it and a kill of the parent can still reach them.
    #[expect(dead_code, reason = "held to keep the parent alive, not to be read")]
    parent: Option<Arc<Job>>,
    /// Its members, and whether it has been killed.
    state: SpinLock<Members>,
    /// Woken when it is killed, for anything waiting on `TERMINATED`.
    waiters: WaitQueue,
}

/// What a job holds.
#[derive(Debug, Default)]
struct Members {
    /// Set once, by the first kill reaching this job, and never cleared.
    killed: bool,
    /// The jobs directly inside this one.
    children: Vec<Weak<Job>>,
    /// The processes directly inside this one.
    processes: Vec<Weak<Process>>,
}

impl Job {
    /// A job with no parent.
    pub(crate) fn new_root() -> Arc<Job> {
        Arc::new(Job {
            parent: None,
            state: SpinLock::new(Members::default()),
            waiters: WaitQueue::new(),
        })
    }

    /// A new job inside this one.
    ///
    /// # Errors
    ///
    /// [`JobError::Killed`].
    pub(crate) fn new_child(self: &Arc<Job>) -> Result<Arc<Job>, JobError> {
        let mut members = self.state.lock();
        if members.killed {
            return Err(JobError::Killed);
        }
        let child = Arc::new(Job {
            parent: Some(Arc::clone(self)),
            state: SpinLock::new(Members::default()),
            waiters: WaitQueue::new(),
        });
        // Pruned as it grows, so a job that makes and drops children in a
        // loop does not keep a list of every one it ever had.
        members.children.retain(|child| child.strong_count() > 0);
        members.children.push(Arc::downgrade(&child));
        Ok(child)
    }

    /// Put `process` in this job.
    ///
    /// # Errors
    ///
    /// [`JobError::Killed`], in which case the caller should not start it.
    pub(crate) fn adopt(&self, process: &Arc<Process>) -> Result<(), JobError> {
        let mut members = self.state.lock();
        if members.killed {
            return Err(JobError::Killed);
        }
        members
            .processes
            .retain(|process| process.strong_count() > 0);
        members.processes.push(Arc::downgrade(process));
        Ok(())
    }

    /// Whether it has been killed.
    pub(crate) fn is_killed(&self) -> bool {
        self.state.lock().killed
    }

    /// The queue woken when it is killed.
    pub(crate) fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }

    /// End every process in this job and in every job beneath it with
    /// `status`, and refuse anything added to any of them afterwards.
    ///
    /// Walked with a list rather than recursion, for the reason
    /// `object::dispose` drops that way: a job tree is as deep as a program
    /// made it. Each job is marked and emptied under its own lock, and its
    /// processes are killed once that lock is released, because
    /// `process::kill` wakes tasks and takes the scheduler's locks.
    ///
    /// Returns how many of the processes it found had not already ended.
    pub(crate) fn kill(self: &Arc<Job>, status: i32) -> usize {
        let mut pending = vec![Arc::clone(self)];
        let mut ended = 0;
        while let Some(job) = pending.pop() {
            let (children, processes) = {
                let mut members = job.state.lock();
                members.killed = true;
                (
                    core::mem::take(&mut members.children),
                    core::mem::take(&mut members.processes),
                )
            };
            for member in processes.iter().filter_map(Weak::upgrade) {
                if !member.is_terminated() {
                    ended += 1;
                }
                process::kill(&member, status);
            }
            pending.extend(children.iter().filter_map(Weak::upgrade));
            job.waiters.wake_all();
        }
        ended
    }
}
