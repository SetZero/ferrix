//! Scheduling logic: the EEVDF fair class, and the shape of the class stack.
//!
//! Stage 5 of `docs/ROADMAP.md`, and the half of the scheduler that is
//! arithmetic. The kernel's half — tasks, stacks, the context switch, the
//! per-CPU locks and the timer — lives in `kernel/src/sched/`. Everything
//! here is a pure function of the numbers it is handed, so `cargo test` can
//! drive a run queue through hundreds of thousands of decisions and check
//! every one, which a kernel that reboots on a mistake cannot.
//!
//! # EEVDF
//!
//! Earliest Eligible Virtual Deadline First (Stoica and Abdel-Wahab, 1995),
//! which Linux adopted in 6.6 in place of CFS. Each entity has a weight and a
//! *virtual runtime*, which advances as it runs at a rate inversely
//! proportional to its weight. The queue's *virtual time* is the
//! weight-average of its entities' virtual runtimes: where every one of them
//! would be had the CPU been shared out exactly in proportion to weight.
//!
//! * An entity whose virtual runtime is at or behind the queue's virtual time
//!   has had no more than its share. It is **eligible**.
//! * Each entity asks for a *slice* at a time, and its **virtual deadline** is
//!   its virtual runtime plus that slice scaled down by its weight.
//! * The scheduler runs the eligible entity with the earliest virtual
//!   deadline.
//!
//! What that buys over CFS is a bound. The difference between the service an
//! entity has been owed and the service it has had — its *lag* — stays within
//! the largest slice anybody asked for, where on real hardware "asked for" is
//! the slice plus however late the timer cut it. CFS promises fairness in the
//! limit; EEVDF promises it at every instant, to within one request. That is
//! the promise stage 5's exit test measures on a running machine, and the one
//! this crate's tests prove in simulation first.
//!
//! # Virtual time wraps, and is kept relative
//!
//! Virtual runtimes are `u64` nanoseconds and are allowed to wrap: two are
//! compared by the sign of their difference, which is right for any pair
//! closer than half the counter's range. The queue's virtual time is never
//! stored as an absolute number either. What is kept is a base, and the
//! weighted sum of every entity's offset from it — so the average is one
//! division, and eligibility is a comparison that needs none: an entity is
//! eligible exactly when the weighted sum of everybody's offset from it is not
//! negative. The base is moved to the average after every change, which keeps
//! the sum as small as the entities' spread rather than as large as the time
//! the queue has existed.
//!
//! # The running entity
//!
//! As in Linux, the entity that is running is held beside the tree rather
//! than in it — its virtual runtime changes on every update, and a key that
//! changes under a tree is a corrupt tree — but it is counted in the virtual
//! time throughout, because it is competing for the CPU as much as anything
//! queued.
//!
//! ```
//! use ferrix_sched::{Config, EntityState, NICE_0_WEIGHT, RunQueue};
//!
//! let mut queue: RunQueue<&str> = RunQueue::new(Config { slice_ns: 3_000_000 }).unwrap();
//! queue.enqueue(1, "one", EntityState::new(NICE_0_WEIGHT)).unwrap();
//! queue.enqueue(2, "two", EntityState::new(NICE_0_WEIGHT)).unwrap();
//!
//! // Equal weights, equal deadlines: the lower identifier runs first...
//! assert_eq!(queue.pick_next(), Some(&"one"));
//! // ...until its slice is spent, and then the other has the earlier deadline.
//! assert!(queue.update_curr(3_000_000));
//! assert_eq!(queue.pick_next(), Some(&"two"));
//! ```

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod domain;
mod tree;

#[cfg(test)]
mod tests;

use alloc::collections::BTreeMap;
use core::fmt;

pub use domain::{Class, CpuSet, Domain, MAX_CPUS, Mode, check_partition};
use tree::{Key, Tree, before};

/// The weight of a task at nice 0, and the unit every other weight is
/// measured in: an entity of this weight advances its virtual runtime at
/// exactly the rate it runs.
pub const NICE_0_WEIGHT: u32 = 1024;

/// Weights for nice -20 through 19: Linux's `sched_prio_to_weight`.
///
/// Each step is about 1.25 times the next, which is what makes "one nice level
/// is about ten per cent of the CPU" true between any two neighbours, whatever
/// else is running. The same table as Linux, so that a program that renices
/// itself gets the share it would get there.
const WEIGHTS: [u32; 40] = [
    88761, 71755, 56483, 46273, 36291, // -20
    29154, 23254, 18705, 14949, 11916, // -15
    9548, 7620, 6100, 4904, 3906, // -10
    3121, 2501, 1991, 1586, 1277, // -5
    1024, 820, 655, 526, 423, // 0
    335, 272, 215, 172, 137, // 5
    110, 87, 70, 56, 45, // 10
    36, 29, 23, 18, 15, // 15
];

/// The weight for a nice value, or `None` outside -20..=19.
#[must_use]
pub fn weight_of_nice(nice: i32) -> Option<u32> {
    let index = usize::try_from(nice.checked_add(20)?).ok()?;
    WEIGHTS.get(index).copied()
}

/// Why something was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SchedError {
    /// A run queue was configured with a slice of zero, which would give every
    /// entity a deadline equal to its virtual runtime and make the pick a
    /// coin toss.
    ZeroSlice,
    /// An entity has a weight of zero: it could never be owed anything, and
    /// its virtual runtime would advance infinitely fast.
    ZeroWeight,
    /// An entity with this identifier is already on the queue.
    Duplicate(u64),
    /// A scheduling mode that is named, and not written yet.
    ModeUnavailable(Mode),
    /// A domain with no CPUs in it.
    EmptyDomain,
    /// A CPU claimed by two domains.
    Overlap(usize),
    /// A CPU claimed by no domain.
    Uncovered(usize),
    /// A CPU number past the end of a [`CpuSet`], or a domain naming a CPU
    /// the machine does not have.
    NoSuchCpu(usize),
}

impl fmt::Display for SchedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchedError::ZeroSlice => f.write_str("a run queue needs a slice longer than zero"),
            SchedError::ZeroWeight => f.write_str("an entity needs a weight above zero"),
            SchedError::Duplicate(id) => write!(f, "entity {id} is already queued"),
            SchedError::ModeUnavailable(mode) => {
                write!(f, "the {} mode is not written yet", mode.name())
            }
            SchedError::EmptyDomain => f.write_str("a scheduling domain has no CPUs"),
            SchedError::Overlap(cpu) => write!(f, "CPU {cpu} is in two scheduling domains"),
            SchedError::Uncovered(cpu) => write!(f, "CPU {cpu} is in no scheduling domain"),
            SchedError::NoSuchCpu(cpu) => write!(f, "there is no CPU {cpu}"),
        }
    }
}

/// How a run queue shares out its CPU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// How much CPU time an entity asks for at a time, in real nanoseconds.
    ///
    /// The unit of the fairness bound: no entity's lag strays further than
    /// one of these, plus the lateness of the timer that ends it.
    pub slice_ns: u64,
}

/// What an entity carries while it is off every run queue — asleep, or
/// between one CPU's queue and another's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EntityState {
    /// Its share of the CPU, relative to [`NICE_0_WEIGHT`].
    pub weight: u32,
    /// Its lag when it left, in virtual nanoseconds: positive if it was owed
    /// service, negative if it had had more than its share.
    ///
    /// Kept so that sleeping cannot be used to cheat. An entity that leaves
    /// ahead of its share comes back still ahead, rather than with a clean
    /// slate it could collect by blocking for a moment every slice.
    pub vlag: i64,
    /// Real nanoseconds it has run for, ever. Carried rather than reset, so it
    /// is a lifetime total wherever the entity has been.
    pub sum_exec: u64,
}

impl EntityState {
    /// A fresh entity: owed nothing, and never run.
    #[must_use]
    pub const fn new(weight: u32) -> EntityState {
        EntityState {
            weight,
            vlag: 0,
            sum_exec: 0,
        }
    }
}

/// An entity the queue would not take, handed back with the reason.
///
/// The payload comes back rather than being dropped: in the kernel it is the
/// last reference to a task, and losing it would lose the task.
#[derive(Debug)]
pub struct Refused<T> {
    /// What the caller tried to queue.
    pub payload: T,
    /// Why it was refused.
    pub reason: SchedError,
}

/// One entity, while it is on a queue.
#[derive(Debug)]
pub(crate) struct Entity<T> {
    /// The caller's name for it.
    pub(crate) id: u64,
    /// Its share of the CPU.
    pub(crate) weight: u32,
    /// Virtual nanoseconds of service, advancing at `NICE_0_WEIGHT / weight`
    /// times the rate it runs.
    pub(crate) vruntime: u64,
    /// Where its current request ends, in virtual time.
    pub(crate) deadline: u64,
    /// Real nanoseconds it has run for.
    pub(crate) sum_exec: u64,
    /// The caller's data.
    pub(crate) payload: T,
}

impl<T> Entity<T> {
    /// Where it sorts in the tree.
    pub(crate) const fn key(&self) -> Key {
        Key {
            deadline: self.deadline,
            id: self.id,
        }
    }
}

/// An entity as the caller sees it.
#[derive(Debug)]
pub struct EntityView<'a, T> {
    /// The caller's name for it.
    pub id: u64,
    /// Its share of the CPU.
    pub weight: u32,
    /// Its virtual runtime.
    pub vruntime: u64,
    /// Its virtual deadline.
    pub deadline: u64,
    /// Real nanoseconds it has run for.
    pub sum_exec: u64,
    /// The queue's virtual time less its virtual runtime: what it is owed, in
    /// virtual nanoseconds. Eligible exactly when this is not negative.
    pub lag: i64,
    /// Whether it is the running entity rather than a queued one.
    pub running: bool,
    /// The caller's data.
    pub payload: &'a T,
}

/// One CPU's fair class: the entities competing for it, and the one running.
#[derive(Debug)]
pub struct RunQueue<T> {
    /// The slice every entity asks for.
    config: Config,
    /// Queued entities, in deadline order.
    tree: Tree<T>,
    /// Each queued entity's deadline, by identifier: how an entity is found in
    /// the tree when all the caller has is its name.
    deadlines: BTreeMap<u64, u64>,
    /// The running entity, if any. Counted in `sum` and `load`; not in `tree`.
    curr: Option<Entity<T>>,
    /// The base virtual runtimes are measured from.
    zero: u64,
    /// The weighted sum of every entity's virtual runtime less `zero`.
    sum: i128,
    /// The sum of every entity's weight.
    load: u64,
}

/// A real duration as the virtual time an entity of `weight` accrues in it.
fn to_virtual(real_ns: u64, weight: u32) -> u64 {
    let scaled = u128::from(real_ns) * u128::from(NICE_0_WEIGHT) / u128::from(weight.max(1));
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// A virtual duration as the real time it takes an entity of `weight` to
/// accrue it.
fn to_real(virtual_ns: u64, weight: u32) -> u64 {
    let scaled = u128::from(virtual_ns) * u128::from(weight) / u128::from(NICE_0_WEIGHT);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

impl<T> RunQueue<T> {
    /// An empty queue.
    ///
    /// # Errors
    ///
    /// [`SchedError::ZeroSlice`] for a slice of zero.
    pub fn new(config: Config) -> Result<RunQueue<T>, SchedError> {
        if config.slice_ns == 0 {
            return Err(SchedError::ZeroSlice);
        }
        Ok(RunQueue {
            config,
            tree: Tree::new(),
            deadlines: BTreeMap::new(),
            curr: None,
            zero: 0,
            sum: 0,
            load: 0,
        })
    }

    /// How this queue shares out its CPU.
    #[must_use]
    pub const fn config(&self) -> Config {
        self.config
    }

    /// Entities on the queue, the running one included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tree.len() + usize::from(self.curr.is_some())
    }

    /// Whether there is nothing on the queue at all, running or waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Entities waiting, not counting the running one.
    #[must_use]
    pub const fn queued(&self) -> usize {
        self.tree.len()
    }

    /// The sum of every entity's weight.
    #[must_use]
    pub const fn load(&self) -> u64 {
        self.load
    }

    /// Whether `id` is on the queue, running or waiting.
    #[must_use]
    pub fn contains(&self, id: u64) -> bool {
        self.deadlines.contains_key(&id) || self.curr.as_ref().is_some_and(|curr| curr.id == id)
    }

    /// The queue's virtual time: the weight-average of every entity's virtual
    /// runtime, rounded down. The base itself when the queue is empty, which
    /// is where the queue's time stood when it last had anything on it.
    #[must_use]
    pub fn avg_vruntime(&self) -> u64 {
        if self.load == 0 {
            return self.zero;
        }
        let offset = self.sum.div_euclid(i128::from(self.load));
        self.zero.wrapping_add(offset as i64 as u64)
    }

    /// `vruntime` measured from the base, as a signed offset.
    const fn relative(&self, vruntime: u64) -> i128 {
        vruntime.wrapping_sub(self.zero) as i64 as i128
    }

    /// Whether an entity at `vruntime` is eligible: at or behind the queue's
    /// virtual time.
    ///
    /// Asked without dividing, because the division rounds and this must not:
    /// the weighted sum of everybody's offset from `vruntime` is not negative
    /// exactly when the average is not below it.
    fn is_eligible(&self, vruntime: u64) -> bool {
        self.sum >= self.relative(vruntime) * i128::from(self.load)
    }

    /// The lag of an entity at `vruntime`, rounded down: virtual time less
    /// virtual runtime.
    fn lag_at(&self, vruntime: u64) -> i64 {
        if self.load == 0 {
            return 0;
        }
        let owed = self.sum - self.relative(vruntime) * i128::from(self.load);
        owed.div_euclid(i128::from(self.load)) as i64
    }

    /// The virtual length of one slice, for an entity of `weight`.
    fn vslice(&self, weight: u32) -> u64 {
        to_virtual(self.config.slice_ns, weight)
    }

    /// The most lag an entity of `weight` may carry away or bring back: two
    /// slices, as Linux allows. Enough that an entity preempted a moment
    /// before its turn is still owed that turn when it comes back; not so much
    /// that one which slept through a minute collects the minute.
    fn lag_limit(&self, weight: u32) -> i64 {
        i64::try_from(self.vslice(weight).saturating_mul(2)).unwrap_or(i64::MAX)
    }

    /// Count an entity at `vruntime` of `weight` into the virtual time.
    fn add_load(&mut self, vruntime: u64, weight: u32) {
        self.sum += i128::from(weight) * self.relative(vruntime);
        self.load += u64::from(weight);
    }

    /// Take one out again.
    fn sub_load(&mut self, vruntime: u64, weight: u32) {
        self.sum -= i128::from(weight) * self.relative(vruntime);
        self.load -= u64::from(weight);
    }

    /// Move the base to the virtual time, so the sum stays as small as the
    /// entities' spread around it.
    fn normalize(&mut self) {
        if self.load == 0 {
            self.sum = 0;
            return;
        }
        let target = self.avg_vruntime();
        let shift = self.relative(target);
        self.sum -= shift * i128::from(self.load);
        self.zero = target;
    }

    /// Put an entity on the queue, carrying the lag it left its last queue
    /// with.
    ///
    /// The lag is scaled before it is applied, as Linux scales it: adding an
    /// entity behind the virtual time pulls the virtual time back towards it,
    /// by its share of the new total weight, and placing it naively would
    /// leave it owed less than it arrived with. Scaling by the new total over
    /// the old makes the lag it has once it is counted exactly the lag it
    /// brought.
    ///
    /// # Errors
    ///
    /// [`SchedError::ZeroWeight`] or [`SchedError::Duplicate`], with the
    /// payload handed back.
    pub fn enqueue(&mut self, id: u64, payload: T, state: EntityState) -> Result<(), Refused<T>> {
        if state.weight == 0 {
            return Err(Refused {
                payload,
                reason: SchedError::ZeroWeight,
            });
        }
        if self.contains(id) {
            return Err(Refused {
                payload,
                reason: SchedError::Duplicate(id),
            });
        }

        let lag = self.placement_lag(state);
        let vruntime = self.avg_vruntime().wrapping_sub(lag as u64);
        let deadline = vruntime.wrapping_add(self.vslice(state.weight));
        self.add_load(vruntime, state.weight);

        let _ = self.deadlines.insert(id, deadline);
        self.tree.insert(Entity {
            id,
            weight: state.weight,
            vruntime,
            deadline,
            sum_exec: state.sum_exec,
            payload,
        });
        self.normalize();
        Ok(())
    }

    /// The lag to place an arriving entity with, clamped and scaled.
    fn placement_lag(&self, state: EntityState) -> i64 {
        let limit = self.lag_limit(state.weight);
        let lag = state.vlag.clamp(-limit, limit);
        if self.load == 0 {
            return lag;
        }
        let total = i128::from(self.load) + i128::from(state.weight);
        (i128::from(lag) * total / i128::from(self.load)) as i64
    }

    /// Choose what runs next, and make it the running entity.
    ///
    /// The running entity, if there is one, goes back into the tree first and
    /// competes like anything else: this is the whole EEVDF decision, made
    /// afresh. `None` only when the queue is empty.
    pub fn pick_next(&mut self) -> Option<&T> {
        self.put_curr_back();
        let key = self.choose()?;
        let entity = self.tree.remove(key)?;
        let _ = self.deadlines.remove(&entity.id);
        let chosen = self.curr.insert(entity);
        Some(&chosen.payload)
    }

    /// The eligible entity with the earliest deadline.
    ///
    /// The fallback to the earliest deadline outright cannot be taken while
    /// the sums are right — the entity furthest behind is at or behind any
    /// average that includes it — and is there so that a queue whose
    /// bookkeeping had gone wrong would still run something rather than idle
    /// a CPU with work on it. `check_invariants` is what would notice.
    fn choose(&self) -> Option<Key> {
        self.tree
            .pick(|vruntime| self.is_eligible(vruntime))
            .or_else(|| self.tree.first())
            .map(Entity::key)
    }

    /// Return the running entity to the tree, still counted.
    fn put_curr_back(&mut self) {
        if let Some(entity) = self.curr.take() {
            let _ = self.deadlines.insert(entity.id, entity.deadline);
            self.tree.insert(entity);
        }
    }

    /// Charge the running entity for `delta_ns` real nanoseconds of CPU.
    ///
    /// Returns whether that finished its slice, in which case it has already
    /// been given its next deadline and the caller should pick again. The
    /// next deadline runs from where the entity *is*, not from where the last
    /// one was: time a late timer let it overrun is not charged to its next
    /// request. That is the overrun the fairness bound has to allow for.
    pub fn update_curr(&mut self, delta_ns: u64) -> bool {
        let slice_ns = self.config.slice_ns;
        let Some(curr) = self.curr.as_mut() else {
            return false;
        };
        let delta = to_virtual(delta_ns, curr.weight);
        curr.sum_exec = curr.sum_exec.saturating_add(delta_ns);
        curr.vruntime = curr.vruntime.wrapping_add(delta);
        let exhausted = !before(curr.vruntime, curr.deadline);
        if exhausted {
            curr.deadline = curr
                .vruntime
                .wrapping_add(to_virtual(slice_ns, curr.weight));
        }
        let weight = curr.weight;

        self.sum += i128::from(weight) * i128::from(delta);
        self.normalize();
        exhausted
    }

    /// The running entity gives up the rest of its request: its deadline moves
    /// a slice further out, as Linux's `sched_yield` moves it, so anything
    /// eligible with a nearer deadline goes first.
    pub fn yield_curr(&mut self) {
        let slice_ns = self.config.slice_ns;
        if let Some(curr) = self.curr.as_mut() {
            curr.deadline = curr
                .deadline
                .wrapping_add(to_virtual(slice_ns, curr.weight));
        }
    }

    /// Whether something queued should run instead of the running entity, now.
    ///
    /// What a wake-up asks: an entity that has just arrived with an earlier
    /// deadline than the running one, while eligible, preempts it — which is
    /// how a task that sleeps a lot gets the latency its small lag is owed,
    /// rather than waiting out whatever slice is in progress.
    #[must_use]
    pub fn should_preempt(&self) -> bool {
        let Some(curr) = self.curr.as_ref() else {
            return !self.tree.is_empty();
        };
        let Some(best) = self.tree.pick(|vruntime| self.is_eligible(vruntime)) else {
            return false;
        };
        !self.is_eligible(curr.vruntime) || before(best.deadline, curr.deadline)
    }

    /// Real nanoseconds until the running entity's slice is spent: what the
    /// timer should be armed for. Zero if it is already spent.
    #[must_use]
    pub fn remaining_ns(&self) -> Option<u64> {
        let curr = self.curr.as_ref()?;
        let left = curr.deadline.wrapping_sub(curr.vruntime) as i64;
        Some(if left <= 0 {
            0
        } else {
            to_real(left as u64, curr.weight)
        })
    }

    /// Take the running entity off the queue — it is blocking, exiting or
    /// moving — and hand back what it needs to come back with.
    pub fn remove_curr(&mut self) -> Option<(u64, T, EntityState)> {
        let entity = self.curr.take()?;
        Some(self.detach(entity))
    }

    /// Take entity `id` off the queue, running or waiting.
    pub fn remove(&mut self, id: u64) -> Option<(T, EntityState)> {
        if self.curr.as_ref().is_some_and(|curr| curr.id == id) {
            return self
                .remove_curr()
                .map(|(_, payload, state)| (payload, state));
        }
        let deadline = self.deadlines.remove(&id)?;
        let entity = self.tree.remove(Key { deadline, id })?;
        let (_, payload, state) = self.detach(entity);
        Some((payload, state))
    }

    /// Record an entity's lag, uncount it, and hand its parts back.
    ///
    /// The lag is taken while the entity is still counted, because it is a
    /// statement about the queue it is leaving.
    fn detach(&mut self, entity: Entity<T>) -> (u64, T, EntityState) {
        let limit = self.lag_limit(entity.weight);
        let vlag = self.lag_at(entity.vruntime).clamp(-limit, limit);
        self.sub_load(entity.vruntime, entity.weight);
        self.normalize();
        (
            entity.id,
            entity.payload,
            EntityState {
                weight: entity.weight,
                vlag,
                sum_exec: entity.sum_exec,
            },
        )
    }

    /// The running entity's data.
    #[must_use]
    pub fn current(&self) -> Option<&T> {
        self.curr.as_ref().map(|curr| &curr.payload)
    }

    /// The running entity's data, mutably.
    pub fn current_mut(&mut self) -> Option<&mut T> {
        self.curr.as_mut().map(|curr| &mut curr.payload)
    }

    /// The running entity's identifier.
    #[must_use]
    pub fn current_id(&self) -> Option<u64> {
        self.curr.as_ref().map(|curr| curr.id)
    }

    /// Entity `id`, running or waiting.
    #[must_use]
    pub fn get(&self, id: u64) -> Option<EntityView<'_, T>> {
        if let Some(curr) = self.curr.as_ref().filter(|curr| curr.id == id) {
            return Some(self.view(curr, true));
        }
        let deadline = *self.deadlines.get(&id)?;
        let entity = self.tree.get(Key { deadline, id })?;
        Some(self.view(entity, false))
    }

    /// Entity `id`'s lag, in virtual nanoseconds.
    #[must_use]
    pub fn lag(&self, id: u64) -> Option<i64> {
        self.get(id).map(|view| view.lag)
    }

    /// Visit every entity: the running one first, then the queued ones in
    /// deadline order.
    pub fn for_each(&self, mut visit: impl FnMut(EntityView<'_, T>)) {
        if let Some(curr) = self.curr.as_ref() {
            visit(self.view(curr, true));
        }
        self.tree.for_each(|entity| visit(self.view(entity, false)));
    }

    /// The queued entity with the latest deadline whose data satisfies
    /// `movable`: what an idle CPU should take from this one.
    ///
    /// The latest deadline because it is the one this queue would run last,
    /// so moving it costs this queue's other entities nothing — and the
    /// running entity never, because it is running.
    #[must_use]
    pub fn latest_where(&self, movable: impl Fn(&T) -> bool) -> Option<u64> {
        self.tree
            .last_where(|entity| movable(&entity.payload))
            .map(|key| key.id)
    }

    /// An entity as the caller sees it.
    fn view<'a>(&self, entity: &'a Entity<T>, running: bool) -> EntityView<'a, T> {
        EntityView {
            id: entity.id,
            weight: entity.weight,
            vruntime: entity.vruntime,
            deadline: entity.deadline,
            sum_exec: entity.sum_exec,
            lag: self.lag_at(entity.vruntime),
            running,
            payload: &entity.payload,
        }
    }

    /// Check every claim the queue's bookkeeping rests on.
    ///
    /// The tree is ordered, balanced and its minima are current; the index of
    /// deadlines names exactly the entities in it; the running entity is in
    /// neither; and the weighted sum and the load, recomputed from scratch,
    /// are the ones the queue has been maintaining by increments. The last is
    /// the one that matters most — a sum that has drifted makes every
    /// eligibility decision subtly wrong, and nothing else would say so.
    ///
    /// # Errors
    ///
    /// The first claim that does not hold, as a sentence.
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        let counted = self.tree.check()?;
        if counted != self.deadlines.len() || counted != self.tree.len() {
            return Err("the tree and its index of deadlines disagree about what is queued");
        }
        for (id, deadline) in &self.deadlines {
            if self
                .tree
                .get(Key {
                    deadline: *deadline,
                    id: *id,
                })
                .is_none()
            {
                return Err("an indexed deadline names nothing in the tree");
            }
        }
        if let Some(curr) = self.curr.as_ref()
            && self.deadlines.contains_key(&curr.id)
        {
            return Err("the running entity is also queued");
        }
        self.check_sums()
    }

    /// Recompute the load and the weighted sum, and require them to match.
    fn check_sums(&self) -> Result<(), &'static str> {
        let mut load = 0u64;
        let mut sum = 0i128;
        self.for_each(|view| {
            load += u64::from(view.weight);
            sum += i128::from(view.weight) * self.relative(view.vruntime);
        });
        if load != self.load {
            return Err("the queue's load is not the sum of its entities' weights");
        }
        if sum != self.sum {
            return Err("the queue's weighted sum has drifted from its entities");
        }
        if self.load == 0 && self.sum != 0 {
            return Err("an empty queue has a weighted sum");
        }
        if self.load != 0 && !(0..i128::from(self.load)).contains(&self.sum) {
            return Err("the queue's base is not at its virtual time");
        }
        Ok(())
    }
}
