//! Where a task should run, and how loaded a processor is.
//!
//! Stage 5 left the scheduler fair on each processor and naive across them: a
//! task started wherever it was created, and the only thing that ever moved
//! one was a processor going idle and taking it. That is enough to pass a test
//! in which every processor eventually runs out of work, and not enough for a
//! machine where one does not.
//!
//! Four things live here, and all four are arithmetic over numbers the kernel
//! hands in — no processor is named, nothing is read from hardware, and every
//! one of them is decided by a function `cargo test` can call.
//!
//! * [`Load`], a decaying average of how busy something has been, which is
//!   what every decision below is made against. A queue's length is the
//!   obvious alternative and the wrong one: four tasks that sleep most of the
//!   time are not the load that one spinner is.
//! * [`place`], which picks the processor a waking or new task should go to.
//! * [`imbalance`] and [`busiest`], which say whether it is worth moving a
//!   task that is already somewhere, and from where.
//! * [`slice_for`], which shares a target latency out among however many
//!   entities are runnable, instead of giving each of them a fixed slice.

use crate::domain::CpuSet;

// ---------------------------------------------------------------------------
// Load tracking
// ---------------------------------------------------------------------------

/// One nice-0 entity's worth of demand.
///
/// [`Load::average`] is measured in these: a processor with one nice-0 entity
/// permanently runnable reads `LOAD_SCALE`, one with four reads about
/// `4 * LOAD_SCALE`, and one with a single entity that is runnable a quarter
/// of the time reads about `LOAD_SCALE / 4`.
///
/// **Demand, not occupancy, and the difference is the whole point.** A
/// processor is either running something or it is not, so "busy" saturates
/// the moment it has one task and says nothing thereafter — which makes every
/// busy processor look identical and gives a balancer nothing to compare.
/// Weighted demand keeps counting.
///
/// A power of two so the arithmetic below is shifts, and equal to
/// [`NICE_0_WEIGHT`](crate::NICE_0_WEIGHT), which is Linux's choice for the
/// same quantity.
pub const LOAD_SCALE: u64 = 1024;

/// How long a load period is, in nanoseconds.
///
/// Just over a millisecond, and a power of two so that dividing by it is a
/// shift. Linux uses 1024 microseconds for exactly that reason.
pub const LOAD_PERIOD_NS: u64 = 1 << 20;

/// The numerator of the per-period decay, over [`DECAY_DEN`].
///
/// `4008/4096` is `0.978515625`, and raised to the 32nd power it is `0.4991` —
/// so the half-life is 32 periods, near enough 33 milliseconds. That is the
/// same shape as Linux's `y^32 = 0.5`, in arithmetic that is exact in
/// integers rather than a table of magic constants.
const DECAY_NUM: u64 = 4008;
/// The denominator of the decay. A power of two, so the division is a shift.
const DECAY_DEN: u64 = 4096;

/// Extra bits the average is carried with internally.
///
/// **Not a detail: without them the average never reaches its own fixed
/// point.** Each step is `average * 4008/4096 + utilisation * 88/4096`, and
/// each of those divisions truncates. At full utilisation the loss from
/// truncating the first term equals the gain from the second at about 978 of
/// 1024, so a permanently busy processor would report 95% busy forever and
/// the balancer would compare every processor against a ceiling it could not
/// reach. Ten extra bits put the truncation error a thousand times below the
/// increment, and the fixed point comes out exact.
const PRECISION: u32 = 10;

/// How many periods of decay are worth applying before the answer is zero.
///
/// After this many the average has been multiplied by about `2^-11`, which
/// takes even a full [`LOAD_SCALE`] below one. Bounding the loop matters
/// because a processor that has been idle for a second would otherwise decay
/// a thousand times to reach a number it reached after a hundred and fifty.
const MAX_DECAY_PERIODS: u32 = 512;

/// A decaying average of how busy something has been.
///
/// Reads as a fraction of [`LOAD_SCALE`]: `LOAD_SCALE` for something that has
/// been busy throughout, zero for something that has been idle for long
/// enough, and the steady-state duty cycle for anything in between.
///
/// # Why an average and not a count
///
/// Every decision in this module is "which processor has the most work", and
/// the cheap answer — how many entities are on its queue — is wrong in the
/// case that matters. Four tasks that each run a hundredth of the time are a
/// quarter the work of one that never sleeps, and a balancer that believed
/// otherwise would move the spinner onto the processor already running one
/// and call the machine balanced.
///
/// # Why it decays
///
/// A total would remember a burst of work forever. What the balancer wants to
/// know is what this processor is doing *now*, with enough memory not to chase
/// every millisecond of noise — which is exactly a geometric decay, and the
/// half-life is the knob.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Load {
    /// The average, in units of [`LOAD_SCALE`] shifted up by [`PRECISION`].
    scaled: u64,
    /// Nanoseconds accumulated towards the period in progress.
    elapsed_ns: u64,
    /// Demand summed over those nanoseconds: the level times the time it was
    /// held for.
    weighted_ns: u64,
}

impl Load {
    /// Idle, and never yet busy.
    #[must_use]
    pub const fn new() -> Load {
        Load {
            scaled: 0,
            elapsed_ns: 0,
            weighted_ns: 0,
        }
    }

    /// What the average reads, in units of [`LOAD_SCALE`].
    #[must_use]
    pub const fn average(&self) -> u64 {
        self.scaled >> PRECISION
    }

    /// Account `elapsed_ns` of wall time during which demand stood at `level`.
    ///
    /// `level` is in units of [`LOAD_SCALE`]: what a run queue's total weight
    /// comes to, so nothing runnable is zero, one nice-0 entity is
    /// `LOAD_SCALE`, and four of them are four times that. It is a *level*,
    /// held for the whole of `elapsed_ns`, not an amount consumed during it —
    /// so a caller that samples on every tick and one that samples on every
    /// context switch describe the same history.
    ///
    /// Called with whatever the caller has: a timer tick, a context switch, a
    /// processor going idle. The period is closed only when a whole one has
    /// accumulated, so calling this often and calling it rarely give the same
    /// answer.
    pub const fn accumulate(&mut self, elapsed_ns: u64, level: u64) {
        self.elapsed_ns = self.elapsed_ns.saturating_add(elapsed_ns);
        self.weighted_ns = self
            .weighted_ns
            .saturating_add(level.saturating_mul(elapsed_ns));

        // Whole periods only. A partial one stays in the accumulators and is
        // folded in when it completes, so the result does not depend on how
        // finely the caller chopped up the time.
        while self.elapsed_ns >= LOAD_PERIOD_NS {
            // This period's share of the accumulated demand, which for a
            // level held throughout is that level.
            let share = self.weighted_ns / self.elapsed_ns;
            let contribution = share << PRECISION;

            // Decay the old and add the new share. Written that way rather
            // than as a weighted mean because the second form needs a
            // division that rounds a light period's contribution to nothing.
            self.scaled = (self.scaled * DECAY_NUM / DECAY_DEN)
                + (contribution * (DECAY_DEN - DECAY_NUM) / DECAY_DEN);

            self.weighted_ns = self.weighted_ns.saturating_sub(share * LOAD_PERIOD_NS);
            self.elapsed_ns -= LOAD_PERIOD_NS;
        }
    }

    /// Age the average by `elapsed_ns` of doing nothing.
    ///
    /// The same as [`accumulate`](Self::accumulate) with no busy time, and
    /// bounded: a processor that has been idle for a second decays to the same
    /// place as one idle for a minute, and looping a thousand times to say so
    /// is work for nothing.
    pub const fn decay(&mut self, elapsed_ns: u64) {
        let periods = elapsed_ns / LOAD_PERIOD_NS;
        if periods > MAX_DECAY_PERIODS as u64 {
            self.scaled = 0;
            self.elapsed_ns = 0;
            self.weighted_ns = 0;
            return;
        }
        self.accumulate(elapsed_ns, 0);
    }
}

// ---------------------------------------------------------------------------
// What the kernel tells this module about a processor
// ---------------------------------------------------------------------------

/// One processor, as far as a placement decision is concerned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CpuLoad {
    /// Entities on its run queue, the running one included.
    pub queued: usize,
    /// Its decaying load average, from [`Load::average`].
    pub average: u64,
    /// Whether it is running its idle task.
    pub idle: bool,
}

impl CpuLoad {
    /// A processor with nothing on it.
    #[must_use]
    pub const fn idle() -> CpuLoad {
        CpuLoad {
            queued: 0,
            average: 0,
            idle: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// Choose the processor a task should run on.
///
/// `prefer` is where it ran last, or where it is being created — the tie is
/// broken towards it, because a task that goes back to the processor it just
/// left finds its own data in that processor's caches, and the cost of not
/// doing so is invisible to every counter in this module.
///
/// `allowed` is the task's affinity: the answer is always inside it, and
/// `None` means there is no processor it may run on at all.
///
/// # The order, and why
///
/// 1. **`prefer`, if it is idle.** Nothing beats staying put on a processor
///    with nothing else to do: no migration, no cold cache, no lock on another
///    processor's queue.
/// 2. **Any idle processor.** An idle processor is capacity going to waste,
///    and waking one is what stops a burst of new tasks queueing behind each
///    other on the processor that created them. This is the case stage 5 got
///    wrong by not looking at all.
/// 3. **The least loaded.** With nothing idle, the question is only where the
///    task will wait least, which is where the load average is lowest; ties go
///    to `prefer`, then to the lowest processor number so the answer does not
///    depend on iteration order.
#[must_use]
pub fn place(loads: &[CpuLoad], allowed: &CpuSet, prefer: usize) -> Option<usize> {
    let permitted = |cpu: usize| allowed.contains(cpu) && cpu < loads.len();

    if permitted(prefer) && loads.get(prefer).is_some_and(|load| load.idle) {
        return Some(prefer);
    }

    let mut best: Option<(usize, u64)> = None;
    for (cpu, load) in loads.iter().enumerate() {
        if !permitted(cpu) {
            continue;
        }
        if load.idle {
            return Some(cpu);
        }
        let score = load.average;
        let better = match best {
            None => true,
            // Strictly better, or an equal score on the processor the task
            // would rather have. Never merely equal, so the lowest-numbered
            // processor wins a tie and the choice is deterministic.
            Some((_, best_score)) => score < best_score || (score == best_score && cpu == prefer),
        };
        if better {
            best = Some((cpu, score));
        }
    }
    best.map(|(cpu, _)| cpu)
}

// ---------------------------------------------------------------------------
// Balancing what is already placed
// ---------------------------------------------------------------------------

/// How much busier a processor must be before taking work from it is worth
/// the migration, in units of [`LOAD_SCALE`].
///
/// Half a nice-0 entity's worth of demand. Below this the move costs more in
/// cold cache than it recovers, and — worse — two processors a hair apart
/// would hand the same task back and forth forever, each correctly observing
/// that the other is less loaded.
///
/// Half rather than a whole, because the imbalance moved is half the
/// difference: a threshold of a whole entity would refuse the last move that
/// levels two processors one entity apart.
pub const BALANCE_THRESHOLD: u64 = LOAD_SCALE / 2;

/// How much work would move if `to` took from `from`, or `None` if it is not
/// worth moving any.
///
/// Half the difference, which is the amount that makes the two equal — moving
/// the whole difference would simply exchange which of them is overloaded.
#[must_use]
pub const fn imbalance(from: &CpuLoad, to: &CpuLoad) -> Option<u64> {
    // A processor with one entity has only the running one, and taking it is
    // not balancing, it is stealing the work it is in the middle of.
    if from.queued < 2 {
        return None;
    }

    // **Both tests, and the second is what stops it thrashing.** The load
    // average is the better measure of demand — it knows about weights and
    // duty cycles, which a count does not — but it is deliberately slow, with
    // a half-life of thirty-odd milliseconds. Moving a task does not change
    // it for a long time afterwards, so a balancer that consulted only the
    // average would move one task, see the same imbalance, move another, and
    // keep going until the averages caught up: eight movable tasks were
    // observed moving over a thousand times.
    //
    // The queue count updates the instant a task moves. It is a worse measure
    // and a perfect brake, so it is used as one: a difference of at least two
    // means there is a move that leaves the two closer together rather than
    // merely swapping which is ahead.
    if from.queued < to.queued + 2 {
        return None;
    }

    if from.average <= to.average {
        return None;
    }
    let difference = from.average - to.average;
    if difference < BALANCE_THRESHOLD {
        return None;
    }
    Some(difference / 2)
}

/// The processor `me` should take work from, if any is worth taking.
///
/// Answers the busiest one that clears [`imbalance`], rather than the first,
/// so that a machine with one overloaded processor and several merely busy
/// ones converges on the overloaded one instead of shuffling between the
/// others.
#[must_use]
pub fn busiest(loads: &[CpuLoad], allowed: &CpuSet, me: usize) -> Option<usize> {
    let mine = loads.get(me)?;
    let mut best: Option<(usize, u64)> = None;

    for (cpu, load) in loads.iter().enumerate() {
        if cpu == me || !allowed.contains(cpu) {
            continue;
        }
        let Some(moving) = imbalance(load, mine) else {
            continue;
        };
        if best.is_none_or(|(_, most)| moving > most) {
            best = Some((cpu, moving));
        }
    }
    best.map(|(cpu, _)| cpu)
}

/// The processor `me` should give work to, if it has too much.
///
/// The mirror of [`busiest`], and on a tickless kernel it is the one that does
/// the work. A processor with a single runnable task is not interrupted at
/// all — there is nothing to switch to, so arming a timer would buy nothing —
/// which means an *under*-loaded processor never reaches the balancer to pull
/// anything towards itself. The overloaded one is interrupted constantly,
/// because it has tasks to switch between, so it is the one that is awake to
/// notice and the one that must push.
///
/// Answers the quietest processor that clears [`imbalance`] in this
/// direction, so the work goes where it helps most.
#[must_use]
pub fn quietest(loads: &[CpuLoad], allowed: &CpuSet, me: usize) -> Option<usize> {
    let mine = loads.get(me)?;
    let mut best: Option<(usize, u64)> = None;

    for (cpu, load) in loads.iter().enumerate() {
        if cpu == me || !allowed.contains(cpu) {
            continue;
        }
        // The same test as a pull, with the ends swapped: would moving work
        // from me to them level the two out by enough to be worth it?
        if imbalance(mine, load).is_none() {
            continue;
        }
        if best.is_none_or(|(_, quietest)| load.average < quietest) {
            best = Some((cpu, load.average));
        }
    }
    best.map(|(cpu, _)| cpu)
}

// ---------------------------------------------------------------------------
// Slices
// ---------------------------------------------------------------------------

/// The slice each of `runnable` entities should ask for, given a target
/// latency and a floor.
///
/// # Why a fixed slice is wrong
///
/// A slice is also a latency bound: the last of `n` runnable entities waits
/// `n` slices before it runs. With a fixed slice that wait grows without limit
/// — a thousand runnable tasks at a millisecond each is a second before the
/// last one is looked at, and stage 5's own test ran exactly a thousand.
///
/// So the slice is a share of a target *period* instead, which is Linux's
/// `sched_latency` divided by the number of runnable entities. Every entity
/// still gets its weighted share of the processor; what changes is that it is
/// delivered in smaller instalments.
///
/// # Why there is a floor
///
/// Taken to its conclusion the arithmetic asks for slices of nanoseconds, and
/// then the machine does nothing but switch. Below `minimum_ns` the period is
/// allowed to stretch instead — which is Linux's `sched_min_granularity`, and
/// is the admission that beyond some number of runnable tasks, latency has to
/// give.
#[must_use]
pub const fn slice_for(target_latency_ns: u64, minimum_ns: u64, runnable: usize) -> u64 {
    let count = if runnable == 0 { 1 } else { runnable as u64 };
    let share = target_latency_ns / count;
    if share < minimum_ns {
        minimum_ns
    } else {
        share
    }
}
