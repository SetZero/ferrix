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

    /// Block until `ready` is true, giving the CPU to something else
    /// meanwhile.
    ///
    /// `ready` is called with interrupts unmasked and no lock held, so it may
    /// read anything; it is called once before blocking and once after being
    /// queued, which is what makes the wake-up impossible to lose.
    pub(crate) fn wait_until(&self, mut ready: impl FnMut() -> bool) {
        while !ready() {
            let Some(task) = super::current() else {
                // No scheduler yet: there is nothing to switch to, so the
                // only honest thing is to spin on the condition.
                core::hint::spin_loop();
                continue;
            };

            task.set_state(BLOCKED);
            self.waiters.lock().push(Arc::clone(&task));

            // The last look, now that a waker could find us.
            if ready() {
                task.set_state(RUNNABLE);
                return;
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
