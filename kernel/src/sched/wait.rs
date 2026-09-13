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
//! The order here is the one that closes it. A waiter joins the queue and
//! marks itself blocked *before* it looks at the condition the last time. A
//! waker takes the same lock, so it either sees the waiter — and wakes it —
//! or it made the condition true before the waiter's final look, which then
//! sees it. Marking itself runnable again is enough to cancel the block,
//! because the scheduler decides whether a task leaves the run queue by
//! reading that state, under the run queue's lock, at the moment it switches.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use ferrix_sync::IrqSpinLock;

use crate::arch;

use super::task::{BLOCKED, RUNNABLE, Task};

/// How long a bounded wait sleeps before looking again of its own accord.
///
/// Belt and braces against a missing notify: see `wait_until_deadline`.
const RECHECK_NANOS: u64 = 5_000_000;

/// Tasks waiting for one thing.
#[derive(Debug)]
pub(crate) struct WaitQueue {
    /// Who is waiting.
    ///
    /// **Masking interrupts, so that a handler may wake the queue and a
    /// holder cannot be switched out.** A bound interrupt's handler queues
    /// its packet on a port and calls `wake_all` for the task waiting there,
    /// and a plain lock taken by a handler that interrupted its own holder
    /// would spin forever. The wait's correctness still rests on the order
    /// in `wait_until_deadline`, not on the lock. The other thing the masking
    /// buys is the reason it was added: a holder cannot be switched out with
    /// it held. The lock is a ticket lock, and a ticket
    /// lock hands itself to whoever is next in line whether or not that
    /// context is running. A holder preempted while holding it — a worker in
    /// `wake_all`, cut by the timer inside the few instructions the lock is
    /// held — stalls every waiter until it is scheduled again, and on a
    /// queue of five hundred tasks at the shortest slice that is nearly two
    /// hundred milliseconds. Worse, the waiters spin their slices away, are
    /// preempted holding *tickets*, and the lock then passes to each of them
    /// in turn, each hand-off costing another round of the queue.
    ///
    /// That was the thousand-task check taking twenty to fifty seconds one
    /// boot in three on two processors: the checker spinning here with
    /// interrupts on, on a processor that was tickless and never interrupted,
    /// while the workers finishing on the other processor queued for the same
    /// lock — two hundred thousand switches to run a thousand tasks that
    /// needed three thousand, and at the end a quarter of them finished but
    /// unable to leave. A holder with interrupts masked cannot be preempted,
    /// so the lock is held for exactly the instructions it covers and no
    /// convoy can form. Any plain `SpinLock` taken from a task with
    /// interrupts on is exposed to the same thing once it is contended; this
    /// is the one the scheduler's own check contends.
    waiters: IrqSpinLock<Vec<Arc<Task>>, arch::Irq>,
    /// Waits on this queue that a wake ended: `wake_all` took the task off the
    /// list, and the wait found what it was waiting for when it ran again.
    ///
    /// For the checks, which need to tell a wake from the recheck without
    /// timing either. A task the recheck timer wakes is still on the list when
    /// it runs; one a waker woke is not, however long the processor took to
    /// run it -- so the count is a fact about the waker, not about the host.
    woken: AtomicU32,
}

impl WaitQueue {
    /// A queue with nobody on it.
    pub(crate) const fn new() -> WaitQueue {
        WaitQueue {
            waiters: IrqSpinLock::new(Vec::new()),
            woken: AtomicU32::new(0),
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
        // Whether the last sleep ended because a waker took this task off the
        // list, for the count `waits_ended_by_a_wake` reports.
        let mut drained = false;
        loop {
            if ready() {
                if drained {
                    let _ = self.woken.fetch_add(1, Ordering::Relaxed);
                }
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

            // **Sleep in slices, not in one span to the deadline.** A wait
            // queue only ends a wait early if somebody notifies it, and a
            // caller that forgets is not detectable from here — the wait
            // simply costs its whole budget and then succeeds. Waking
            // periodically turns that mistake from a twenty-second silent tax
            // into a few milliseconds, which is the difference between a bug
            // that hides for a day and one that never matters.
            //
            // Polling a condition is the wrong shape for a kernel and the
            // right one for a checking harness, which is all this serves.
            let slice = crate::timer::now_nanos().saturating_add(RECHECK_NANOS);
            let wake_at = if slice < deadline { slice } else { deadline };

            // **Findable at every instant, which fixes the order.** Interrupts
            // are on here, and any interrupt exit may switch this task out. A
            // switch that finds it `BLOCKED` takes it off the run queue and
            // files it as a sleeper only if it has a deadline. So `BLOCKED` is
            // set last, once the deadline and the waiter entry both exist.
            // Set first, a switch between the lines lost the task: blocked,
            // no deadline to wake it, on no list a waker reads.
            task.set_sleep_deadline(wake_at);
            self.waiters.lock().push(Arc::clone(&task));
            task.set_state(BLOCKED);

            // The last look, now that both a waker and the timer could find
            // us. Cancelling the sleep as well as the block, so a deadline
            // this task never used cannot wake it out of some later wait, and
            // in the reverse order for the same reason: runnable first, so a
            // switch in between leaves the task where it is.
            if ready() {
                task.set_state(RUNNABLE);
                let _ = task.take_sleep_deadline();
                let _ = self.unqueue(task.id);
                return true;
            }
            super::block();
            // Running again, so not filed anywhere: whatever deadline is left
            // belongs to no sleep and must not reach the next switch.
            let _ = task.take_sleep_deadline();

            // **Off the queue on the way out, however we left.** A waiter that
            // returns while still listed is woken by the *next* `wake_all`,
            // out of whatever it happens to be doing then — which showed up as
            // the sleep check failing with "a sleep came back before its
            // deadline", a task cut short of a sleep it had nothing to do with
            // this queue for. `wake_all` drains the list, so this only matters
            // for the paths that leave without being drained: the recheck
            // timer, and the condition coming true.
            drained = !self.unqueue(task.id);
        }
    }

    /// Take `id` off the waiter list, and say whether it was on it.
    fn unqueue(&self, id: super::TaskId) -> bool {
        let mut waiters = self.waiters.lock();
        let listed = waiters.len();
        waiters.retain(|waiter| waiter.id != id);
        waiters.len() != listed
    }

    /// How many waits on this queue a wake has ended: see the field.
    pub(crate) fn waits_ended_by_a_wake(&self) -> u32 {
        self.woken.load(Ordering::Relaxed)
    }

    /// Wake everything waiting.
    pub(crate) fn wake_all(&self) {
        let waiters = core::mem::take(&mut *self.waiters.lock());
        for task in &waiters {
            super::wake(task);
        }
    }
}
