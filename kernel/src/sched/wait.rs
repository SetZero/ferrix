//! Waiting for something, and being woken for it.
//!
//! # The lost wake-up
//!
//! The race every wait queue is built around: a task checks a condition,
//! finds it false, and is about to block — and between the two, another CPU
//! makes the condition true and wakes everyone waiting. If the sleeper is not
//! on the queue yet, that wake-up finds nobody, and the sleeper waits for an
//! event that has already happened.
//!
//! The order here is the one that closes it. A waiter marks itself blocked
//! and joins the queue *before* it looks at the condition the last time. A
//! waker takes the same lock, so it either sees the waiter — and wakes it —
//! or it made the condition true before the waiter's final look, which then
//! sees it. Marking itself runnable again is enough to cancel the block,
//! because the scheduler decides whether a task leaves the run queue by
//! reading that state, under the run queue's lock, at the moment it switches.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_sync::SpinLock;

use super::task::{BLOCKED, RUNNABLE, Task};

/// Tasks waiting for one thing.
#[derive(Debug)]
pub(crate) struct WaitQueue {
    /// Who is waiting. A plain lock: nothing takes it from an interrupt
    /// handler, and every holder masks interrupts for the few instructions it
    /// is held.
    waiters: SpinLock<Vec<Arc<Task>>>,
}

impl WaitQueue {
    /// A queue with nobody on it.
    pub(crate) const fn new() -> WaitQueue {
        WaitQueue {
            waiters: SpinLock::new(Vec::new()),
        }
    }

    /// Block until `ready` is true or `deadline` passes, whichever comes
    /// first. Answers whether `ready` was the reason.
    ///
    /// # Why a wait ever needs a deadline
    ///
    /// A wait woken only by a waker is the right shape for a kernel that
    /// works and the wrong one for a check trying to find out whether it
    /// does. A scheduler that has lost a task wakes nobody, and a wait with
    /// no deadline turns that into a boot test which says nothing for two
    /// minutes and then times out. With one, the same failure is a sentence
    /// naming what never happened — so there is only this form, and callers
    /// that genuinely never want to give up pass one far enough out to say
    /// so.
    ///
    /// The deadline is the sleep the scheduler already knows how to serve:
    /// the task goes on the run queue's sleeper set as well as on this queue,
    /// so the timer wakes it even when nothing else does. Being woken by the
    /// waker first leaves a stale entry in one of the two, and both tolerate
    /// it — waking a task that is already runnable is a no-op, and a sleeper
    /// whose deadline was cleared is dropped when it is next looked at.
    pub(crate) fn wait_until_deadline(
        &self,
        mut ready: impl FnMut() -> bool,
        deadline: u64,
    ) -> bool {
        loop {
            if ready() {
                return true;
            }
            if crate::timer::now_nanos() >= deadline {
                return false;
            }

            let Some(task) = super::current() else {
                // No scheduler yet, so there is nothing to switch to and
                // nothing to be woken by: spin, and let the deadline above
                // end it.
                core::hint::spin_loop();
                continue;
            };

            task.set_state(BLOCKED);
            task.set_sleep_deadline(deadline);
            self.waiters.lock().push(Arc::clone(&task));

            // The last look, now that both a waker and the timer could find
            // us. Cancelling the sleep as well as the block, so a deadline
            // this task never used cannot wake it out of some later wait.
            if ready() {
                let _ = task.take_sleep_deadline();
                task.set_state(RUNNABLE);
                return true;
            }
            super::block();
        }
    }

    /// Wake everything waiting.
    pub(crate) fn wake_all(&self) {
        let waiters = core::mem::take(&mut *self.waiters.lock());
        for task in &waiters {
            super::wake(task);
        }
    }
}
