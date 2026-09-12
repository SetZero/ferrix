//! One CPU's run queue: what is running there, what is waiting, and when the
//! timer should next interrupt it.
//!
//! # The lock
//!
//! One plain [`SpinLock`] per CPU, always taken with interrupts already
//! masked — never an `IrqSpinLock`, because a context switch hands the lock
//! from the outgoing context to the incoming one and a guard cannot cross
//! that. The rule it replaces the guard with is written at each call site: the
//! lock is taken by `lock_manually` and released by exactly one
//! `force_unlock`, either by the same context when no switch happened or by
//! the context switched to.
//!
//! Order against the rest of the kernel's locks: a run queue's lock is taken
//! *outside* the heap's and the frame allocator's — placing an entity in the
//! tree allocates — and nothing is ever taken outside it. Nothing here maps
//! memory, so `mm`'s table lock never appears under it.
//!
//! # Tickless
//!
//! The timer is armed for the moment this CPU next has a decision to make:
//! the end of the running task's slice, or the first sleeper's wake-up,
//! whichever comes first. A CPU running one task with nothing queued behind it
//! arms nothing at all, because there is no decision to make until something
//! else happens — which is what "tickless" means and why the timer facade's
//! one-shot is the primitive stage 3 left behind.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use ferrix_sched::{Config, CpuLoad, EntityState, Load, RunQueue, slice_for};

use super::task::{RUNNABLE, Task, TaskId};

/// How much CPU a task asks for at a time.
///
/// Three milliseconds: long enough that a switch costs a fraction of a per
/// cent of it under an emulator, short enough that a task waiting behind
/// three others still runs within a human's idea of immediately. It is also
/// the unit of the fairness bound — no task strays further than one of these
/// from its share — so it is the number stage 5's exit criterion is stated
/// in.
pub(crate) const TARGET_LATENCY_NS: u64 = 3_000_000;

/// The shortest slice the scheduler will hand out, however many tasks are
/// runnable.
///
/// Below this a processor spends more time switching than running. Linux calls
/// the same number `sched_min_granularity`, and the ratio between it and the
/// target latency is what decides how many runnable tasks it takes before
/// latency has to give: eight, here.
pub(crate) const MIN_SLICE_NS: u64 = TARGET_LATENCY_NS / 8;

/// The largest slice, which is what one runnable task gets and what the
/// fairness bound is stated in.
pub(crate) const SLICE_NS: u64 = TARGET_LATENCY_NS;

/// The shortest interval worth arming the timer for: below this the interrupt
/// costs more than the time it measures.
const MIN_ARM_NS: u64 = 20_000;

/// What one CPU's scheduling has done, for the boot report.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    /// Context switches.
    pub(crate) switches: u64,
    /// Tasks taken from another CPU's queue.
    pub(crate) stolen_in: u64,
    /// Tasks another CPU took from this one.
    pub(crate) stolen_out: u64,
    /// The most any task ran past its deadline before being switched out.
    pub(crate) worst_overrun: u64,
    /// The most any task's service strayed from its share while a measurement
    /// window was open.
    pub(crate) worst_lag: u64,
    /// Whether that window is open.
    pub(crate) measuring: bool,
}

/// One CPU's queue.
#[derive(Debug)]
pub(crate) struct CpuQueue {
    /// The fair class. The idle task is not in it: it is what runs when this
    /// is empty, which is what makes it the bottom of the class stack rather
    /// than a task with a very small weight.
    pub(crate) fair: RunQueue<Arc<Task>>,
    /// What is running, which is the idle task when the fair class is empty.
    pub(crate) current: Option<Arc<Task>>,
    /// This CPU's idle task.
    pub(crate) idle: Option<Arc<Task>>,
    /// What was running before the last switch, for the incoming context to
    /// finish with.
    pub(crate) previous: Option<Arc<Task>>,
    /// Tasks asleep on this CPU, by the instant they wake.
    pub(crate) sleepers: BTreeMap<(u64, TaskId), Arc<Task>>,
    /// How busy this processor has been, decaying.
    ///
    /// "Busy" means running something that is not the idle task. It is
    /// accumulated wherever time is already being accounted, so it costs a
    /// pair of adds rather than a pass of its own.
    load: Load,
    /// When [`Self::account_load`] last folded time into `load`.
    load_updated: u64,
    /// When the running task was last charged.
    pub(crate) exec_start: u64,
    /// What this CPU's scheduling has done.
    pub(crate) stats: Stats,
}

impl CpuQueue {
    /// An empty queue.
    pub(crate) fn new() -> Result<CpuQueue, &'static str> {
        let fair = RunQueue::new(Config {
            slice_ns: TARGET_LATENCY_NS,
        })
        .map_err(|_| "the scheduler's slice is not a slice")?;
        Ok(CpuQueue {
            fair,
            current: None,
            idle: None,
            previous: None,
            sleepers: BTreeMap::new(),
            load: Load::new(),
            load_updated: 0,
            exec_start: 0,
            stats: Stats::default(),
        })
    }

    /// Charge the running task for the time since it was last charged.
    ///
    /// Also where the overrun is measured: how far past its deadline a task
    /// ran before the timer cut it. That is the difference between the slice
    /// the scheduler asked for and the request it actually served, and it is
    /// what widens the fairness bound on a real machine.
    pub(crate) fn account(&mut self, now: u64) {
        self.account_load(now);
        let delta = now.saturating_sub(self.exec_start);
        self.exec_start = now;
        if delta == 0 {
            return;
        }
        let Some(remaining) = self.fair.remaining_ns() else {
            return;
        };
        if let Some(task) = self.fair.current() {
            task.add_runtime(delta);
        }
        if self.fair.update_curr(delta) {
            let overrun = delta.saturating_sub(remaining);
            self.stats.worst_overrun = self.stats.worst_overrun.max(overrun);
        }
        if self.stats.measuring {
            self.measure();
        }
    }

    /// Compare every task's service with its weighted share, and remember the
    /// worst difference.
    ///
    /// This is EEVDF's promise, measured in real nanoseconds on a running
    /// machine rather than in the scheduler's own virtual time: a task's lag
    /// is what it was owed less what it had, and the theorem says that stays
    /// inside one request. Measured from each task's baseline so that a task
    /// which joined the queue later is not asked to account for time before
    /// it arrived.
    fn measure(&mut self) {
        let mut total = 0u128;
        let mut weights = 0u128;
        self.fair.for_each(|view| {
            if view.payload.is_measured() {
                total += u128::from(view.payload.since_baseline(view.sum_exec));
                weights += u128::from(view.weight);
            }
        });
        if weights == 0 {
            return;
        }

        let mut worst = 0u64;
        self.fair.for_each(|view| {
            if !view.payload.is_measured() {
                return;
            }
            let share = total * u128::from(view.weight) / weights;
            let had = u128::from(view.payload.since_baseline(view.sum_exec));
            worst = worst.max(share.abs_diff(had) as u64);
        });
        self.stats.worst_lag = self.stats.worst_lag.max(worst);
    }

    /// Wake every task whose sleep has ended.
    pub(crate) fn wake_sleepers(&mut self, now: u64) {
        while let Some((key, task)) = self.sleepers.pop_first() {
            if key.0 > now {
                let _ = self.sleepers.insert(key, task);
                return;
            }
            task.set_state(RUNNABLE);
            self.insert(&task);
        }
    }

    /// Put a runnable task into the fair class, carrying its lag.
    pub(crate) fn insert(&mut self, task: &Arc<Task>) {
        if let Err(refused) = self
            .fair
            .enqueue(task.id, Arc::clone(task), task.entity_state())
        {
            // The only refusals are a duplicate identifier and a zero weight,
            // neither of which this kernel can produce; dropping the clone is
            // what keeps the task alive in the table regardless.
            drop(refused.payload);
            return;
        }
        task.set_queued(true);
        self.rescale_slice();
    }

    /// Take the running task out of the fair class, keeping what it needs to
    /// come back with.
    pub(crate) fn detach_current(&mut self) {
        if let Some((_, task, state)) = self.fair.remove_curr() {
            task.store_entity_state(state);
            task.set_queued(false);
        }
    }

    /// What should run now: the fair class's choice, or the idle task.
    pub(crate) fn pick_next(&mut self) -> Option<Arc<Task>> {
        if let Some(task) = self.fair.pick_next() {
            return Some(Arc::clone(task));
        }
        self.idle.clone()
    }

    /// Whether this CPU has anything to run besides its idle task.
    pub(crate) fn has_work(&self) -> bool {
        !self.fair.is_empty()
    }

    /// Fold the time since the last call into the load average.
    ///
    /// Busy is "the fair queue had something runnable", not "a task was
    /// current": the idle task is current when there is nothing to do, and
    /// counting it would make every processor read as permanently full.
    pub(crate) fn account_load(&mut self, now: u64) {
        let elapsed = now.saturating_sub(self.load_updated);
        if elapsed == 0 {
            return;
        }
        self.load_updated = now;
        // The level is the queue's total weight, so a processor with four
        // runnable tasks reads four times one with a single task rather than
        // the same "busy". The idle task is not in the fair queue, so an idle
        // processor contributes nothing without a special case.
        self.load.accumulate(elapsed, self.fair.load_weight());
    }

    /// This processor as the balancer sees it.
    pub(crate) fn snapshot(&self) -> CpuLoad {
        CpuLoad {
            queued: self.len(),
            average: self.load.average(),
            // **Nothing to run, not "running the idle task".** A processor
            // that has just been given a task still has the idle task current
            // until it next schedules, so the second of a burst of placements
            // would see it as idle and pile on behind the first. What the
            // placer is asking is whether this processor has work, and an
            // empty fair queue is that question answered.
            idle: !self.has_work(),
        }
    }

    /// The load average, for the boot log.
    pub(crate) fn load_average(&self) -> u64 {
        self.load.average()
    }

    /// Whether this processor is running its idle task, or nothing at all.
    ///
    /// The idle task is deliberately not in the fair queue — it is what runs
    /// when that queue is empty, not the lowest-weight thing in it — so
    /// `should_preempt` has nothing to compare a new arrival against and
    /// answers false. That makes "should the target switch?" the wrong
    /// question to ask on its own when placing a task on another processor:
    /// an idle processor is asleep, and a sleeping processor that is never
    /// told has no way to find out.
    pub(crate) fn is_running_idle(&self) -> bool {
        match (self.current.as_ref(), self.idle.as_ref()) {
            (Some(current), Some(idle)) => Arc::ptr_eq(current, idle),
            (None, _) => true,
            (Some(_), None) => false,
        }
    }

    /// Arm the timer for the next decision this CPU has to make.
    pub(crate) fn arm_timer(&self, now: u64) {
        let sleeper = self.sleepers.keys().next().map(|(at, _)| *at);
        // A slice only ends in a decision if something is waiting for it. One
        // task alone on a CPU is left to run: interrupting it would change
        // nothing, and this is where tickless comes from.
        let slice = if self.fair.queued() > 0 {
            self.fair
                .remaining_ns()
                .map(|left| now.saturating_add(left))
        } else {
            None
        };

        match [sleeper, slice].into_iter().flatten().min() {
            Some(at) => crate::timer::after(at.saturating_sub(now).max(MIN_ARM_NS)),
            None => crate::timer::stop(),
        }
    }

    /// The task another CPU should take from this one, if any may be moved.
    pub(crate) fn steal_candidate(&self, to: usize) -> Option<TaskId> {
        // `may_run_on` is the one that matters now that affinity is a set:
        // a task allowed on processors 0 and 1 must not be moved to 2 however
        // idle 2 is. `is_pinned` stays because it says something different —
        // a task with exactly one home should not be moved even *to* that
        // home, since it is already there.
        self.fair.latest_where(|task| {
            !task.is_pinned() && task.may_run_on(to) && task.state() == RUNNABLE
        })
    }

    /// Take a queued task off this queue, for another one to run.
    pub(crate) fn release(&mut self, id: TaskId) -> Option<(Arc<Task>, EntityState)> {
        let (task, state) = self.fair.remove(id)?;
        task.set_queued(false);
        self.rescale_slice();
        Some((task, state))
    }

    /// Share the target latency out among however many are runnable now.
    ///
    /// A fixed slice is also a fixed latency bound *per task*, so the last of
    /// `n` runnable tasks waits `n` slices — which at stage 5's thousand
    /// threads and three milliseconds each is three seconds before the last
    /// one is looked at. Scaling the slice makes the wait the target latency
    /// instead, until the floor stops it.
    fn rescale_slice(&mut self) {
        let slice = slice_for(TARGET_LATENCY_NS, MIN_SLICE_NS, self.fair.len());
        // The only refusal is a slice of zero, and `slice_for` floors at
        // `MIN_SLICE_NS`, which is not zero.
        let _ = self.fair.set_slice_ns(slice);
    }

    /// The slice this queue is currently handing out.
    pub(crate) fn slice_ns(&self) -> u64 {
        self.fair.config().slice_ns
    }

    /// Whether the running task should give way to something queued.
    pub(crate) fn should_preempt(&self) -> bool {
        self.fair.should_preempt()
    }

    /// Take `id` out of this processor's sleeper set, if it is in it.
    ///
    /// **A task woken early is still filed under the deadline it no longer
    /// intends to keep.** Leave the entry behind and `wake_sleepers` finds it
    /// later, sees a time already past, and makes the task runnable — out of
    /// whatever it is doing by then. What that looked like was the sleep check
    /// failing with "a sleep came back before its deadline": a twenty
    /// millisecond sleep cut short by a five millisecond deadline the task had
    /// abandoned two checks earlier.
    ///
    /// Scanned rather than keyed, because the map is keyed by wake-up time and
    /// the task's own copy of that was consumed when it was filed. Sleeper sets
    /// are short.
    pub(crate) fn remove_sleeper(&mut self, id: TaskId) -> bool {
        let before = self.sleepers.len();
        self.sleepers.retain(|(_, sleeping), _| *sleeping != id);
        self.sleepers.len() != before
    }

    /// Tasks waiting to run: everything on the queue but the running one.
    ///
    /// The number `arm_timer` decides on, so it is also the number that says
    /// whether this processor's timer needs to exist at all.
    pub(crate) fn waiting(&self) -> usize {
        self.fair.queued()
    }

    /// Tasks on this queue, the running one included.
    pub(crate) fn len(&self) -> usize {
        self.fair.len()
    }

    /// Check the fair class's own bookkeeping.
    pub(crate) fn check_invariants(&self) -> Result<(), &'static str> {
        self.fair.check_invariants()?;
        let running_is_current = match (self.fair.current(), self.current.as_ref()) {
            (Some(fair), Some(current)) => Arc::ptr_eq(fair, current),
            // Nothing in the fair class: the idle task is what runs.
            (None, Some(current)) => self
                .idle
                .as_ref()
                .is_some_and(|idle| Arc::ptr_eq(idle, current)),
            _ => false,
        };
        if !running_is_current {
            return Err("the running task is not the fair class's running entity");
        }
        // Whether a dead task is still queued is not asked here. It was, of
        // `previous`, which `finish_switch` empties before it releases the
        // lock this runs under, so it could never fail. `finish_switch` asks
        // it instead, at the moment it has an answer.
        Ok(())
    }
}
