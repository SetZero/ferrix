//! Stage 5's exit criterion.
//!
//! Four checks, and the last is the stage's own. The first shows a task can
//! be created, run, and cleaned up after. The second shows a sleep is a
//! sleep: the task is off the run queue and the processor is free, and it
//! comes back when it said it would. The third runs a thousand of them at
//! once and requires every stack back afterwards. The fourth measures
//! fairness against the bound EEVDF actually promises, rather than against
//! the eye.
//!
//! # Why the fairness check measures real service
//!
//! The scheduler's own virtual time is the thing under test, so a check
//! written in terms of it could only prove it is self-consistent. What is
//! measured instead is nanoseconds of CPU, counted by the same clock stage 3
//! calibrated, against each task's weighted share of what the group received.
//! The difference is the task's lag, and EEVDF's guarantee is that it stays
//! inside one request.
//!
//! A request is the slice, plus however late the timer cut it — under an
//! emulator whose host can deschedule a whole virtual processor, that lateness
//! is not a rounding error. So the bound is not a constant: the scheduler
//! records the worst overrun it actually served, and the check requires the
//! worst lag to be inside a slice plus that. Both numbers go in the boot log,
//! because a bound that moves is only honest if it is printed.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferrix_sched::{NICE_0_WEIGHT, weight_of_nice};

use super::task::Task;
use super::{SLICE_NS, WaitQueue};
use crate::smp::Topology;

/// Threads the many-task check runs.
const THREADS: u64 = 1000;

/// Rounds of arithmetic each one does: bounded work, and enough of it that a
/// processor which took a share of the thousand is busy for long enough to be
/// worth stealing from.
const WORK_ROUNDS: u64 = 20_000;

/// Spinners per processor in the fairness check.
const SPINNERS_PER_CPU: usize = 3;

/// How long the fairness window stays open.
const WINDOW_NANOS: u64 = 150_000_000;

/// How long the sleep check sleeps.
const SLEEP_NANOS: u64 = 20_000_000;

/// How long a wait loop gives the machine before calling it wedged.
///
/// Wall-clock rather than a spin count, because the wait blocks rather than
/// spins: what is being waited for is a thousand tasks getting through their
/// work, and how many times *this* task is woken meanwhile says nothing about
/// how long that took. Generous, because under an emulator it genuinely is;
/// finite, because a scheduler that has lost a task should be a sentence in
/// the boot log rather than the boot test's timeout.
const PATIENCE_NANOS: u64 = 20_000_000_000;

/// What the checks found, for the boot log.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Report {
    /// Threads that ran to completion.
    pub(crate) threads: u64,
    /// Context switches made along the way.
    pub(crate) switches: u64,
    /// Tasks moved between processors by work stealing.
    pub(crate) steals: u64,
    /// How many processors ran some of the thousand.
    pub(crate) processors: u32,
    /// Spinners the fairness check ran.
    pub(crate) spinners: usize,
    /// The worst any task's service strayed from its share.
    pub(crate) worst_lag: u64,
    /// What that had to stay inside: a slice, plus the worst overrun served.
    pub(crate) bound: u64,
    /// How long the sleep check actually slept.
    pub(crate) slept: u64,
}

/// Tasks that have finished a phase.
static DONE: AtomicU64 = AtomicU64::new(0);

/// What the workers computed, so their work cannot be optimised away.
static SUM: AtomicU64 = AtomicU64::new(0);

/// Spinners that have started spinning.
static SPINNING: AtomicU64 = AtomicU64::new(0);

/// Tells the spinners to stop.
static STOP: AtomicBool = AtomicBool::new(false);

/// Where the checking task waits for a phase to end.
static FINISHED: WaitQueue = WaitQueue::new();

/// Run them.
///
/// # Errors
///
/// The first check that fails, as a sentence.
pub(crate) fn run(topology: &Topology) -> Result<Report, &'static str> {
    let mut report = Report::default();
    one_task()?;
    sleeping(&mut report)?;
    many_tasks(topology, &mut report)?;
    fairness(topology, &mut report)?;

    let summary = super::summary();
    report.switches = summary.switches;
    report.steals = summary.steals;
    super::check_invariants()?;
    Ok(report)
}

/// Count this task as finished, and wake whoever is waiting for the phase.
fn finish() {
    let _ = DONE.fetch_add(1, Ordering::AcqRel);
    FINISHED.wake_all();
}

/// Bounded work, and a number at the end that nothing can fold away.
fn worker(argument: usize) {
    let mut value = argument as u64 | 1;
    for _ in 0..WORK_ROUNDS {
        value = value.wrapping_mul(2_654_435_761).rotate_left(7) ^ 0x9E37_79B9;
    }
    let _ = SUM.fetch_add(value & 0xFF, Ordering::Relaxed);
    finish();
}

/// Run until told to stop, never blocking: the only way one of these gets off
/// its processor is by being preempted, which is what the fairness check is
/// about.
fn spinner(_argument: usize) {
    let _ = SPINNING.fetch_add(1, Ordering::AcqRel);
    while !STOP.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    finish();
}

/// Wait for `ready`, giving the processor up meanwhile, and give up rather
/// than hang if it never becomes true.
fn wait_for(ready: impl FnMut() -> bool, what: &'static str) -> Result<(), &'static str> {
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    if FINISHED.wait_until_deadline(ready, deadline) {
        Ok(())
    } else {
        Err(what)
    }
}

/// Free every exited task's stack, and require the arena to come back to
/// where it started.
fn reap_to(allocations: usize) -> Result<(), &'static str> {
    // Still a yielding loop, and deliberately: reaping is work *this* task
    // does, so it has to keep being given the processor to do it. Blocking
    // here would wait for something nobody is going to do.
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    loop {
        let _ = super::reap();
        if crate::vmap::usage().allocations <= allocations {
            return Ok(());
        }
        if crate::timer::now_nanos() >= deadline {
            return Err("a task's stack was never given back");
        }
        super::yield_now();
    }
}

/// One task runs, computes what it was given, exits, and is cleaned up after.
fn one_task() -> Result<(), &'static str> {
    let allocations = crate::vmap::usage().allocations;
    DONE.store(0, Ordering::Release);
    SUM.store(0, Ordering::Release);

    let task = super::spawn("check-one", worker, 1, NICE_0_WEIGHT)?;
    wait_for(
        || DONE.load(Ordering::Acquire) >= 1,
        "a single task never finished",
    )?;
    if SUM.load(Ordering::Acquire) == 0 {
        return Err("a task finished without doing its work");
    }
    if task.switches() == 0 {
        return Err("a task finished without ever being switched to");
    }

    reap_to(allocations)?;
    drop(task);
    Ok(())
}

/// A sleep gives the processor up and comes back on time.
fn sleeping(report: &mut Report) -> Result<(), &'static str> {
    let started = crate::timer::now_nanos();
    super::sleep_for(SLEEP_NANOS);
    let elapsed = crate::timer::now_nanos().saturating_sub(started);
    report.slept = elapsed;

    if elapsed < SLEEP_NANOS {
        return Err("a sleep came back before its deadline");
    }
    if elapsed > SLEEP_NANOS * 20 {
        return Err("a sleep came back an order of magnitude late");
    }
    Ok(())
}

/// A thousand threads, started from one processor, run to completion — and
/// every stack comes back.
fn many_tasks(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    let allocations = crate::vmap::usage().allocations;
    DONE.store(0, Ordering::Release);
    SUM.store(0, Ordering::Release);

    // All of them onto the processor doing the spawning, so that the only way
    // the others get any is by taking them.
    let mut tasks = Vec::with_capacity(THREADS as usize);
    for index in 0..THREADS {
        tasks.push(super::spawn(
            "worker",
            worker,
            index as usize,
            NICE_0_WEIGHT,
        )?);
    }

    wait_for(
        || DONE.load(Ordering::Acquire) >= THREADS,
        "not every thread finished",
    )?;

    let mut processors = 0u64;
    for task in &tasks {
        if task.switches() == 0 {
            return Err("a thread finished without ever being switched to");
        }
        processors |= task.cpus_run_on();
    }
    report.threads = DONE.load(Ordering::Acquire);
    report.processors = processors.count_ones();

    if topology.online() > 1 && report.processors < 2 {
        return Err("every thread ran on one processor: work stealing moved nothing");
    }

    reap_to(allocations)?;
    drop(tasks);
    Ok(())
}

/// Tasks that never sleep get shares in proportion to their weights, and no
/// task's share strays further from its due than EEVDF allows.
fn fairness(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    let allocations = crate::vmap::usage().allocations;
    DONE.store(0, Ordering::Release);
    SPINNING.store(0, Ordering::Release);
    STOP.store(false, Ordering::Release);

    let spinners = start_spinners(topology)?;
    let total = spinners.len() as u64;
    wait_for(
        || SPINNING.load(Ordering::Acquire) >= total,
        "a spinner never started",
    )?;

    // The window opens once every spinner is running, and this task sleeps
    // through it: a processor whose queue holds only the spinners is what
    // makes the measurement a measurement of them.
    super::open_window(&spinners);
    super::sleep_for(WINDOW_NANOS);
    super::close_window(&spinners);

    STOP.store(true, Ordering::Release);
    wait_for(
        || DONE.load(Ordering::Acquire) >= total,
        "a spinner never stopped",
    )?;

    let summary = super::summary();
    report.spinners = spinners.len();
    report.worst_lag = summary.worst_lag;
    report.bound = SLICE_NS.saturating_add(summary.worst_overrun);

    if report.worst_lag > report.bound {
        return Err("a task's service strayed further from its share than EEVDF allows");
    }
    shares_are_proportional(topology, &spinners, report.bound)?;

    reap_to(allocations)?;
    drop(spinners);
    Ok(())
}

/// The weights the spinners on each processor are given: two at nice 0 and
/// one at nice -3, so the check covers equal shares and unequal ones.
fn spinner_weight(index: usize) -> u32 {
    if index == SPINNERS_PER_CPU - 1 {
        weight_of_nice(-3).unwrap_or(NICE_0_WEIGHT)
    } else {
        NICE_0_WEIGHT
    }
}

/// Start the spinners, pinned so that each processor's queue is a fixed set.
fn start_spinners(topology: &Topology) -> Result<Vec<Arc<Task>>, &'static str> {
    let mut spinners = Vec::new();
    for cpu in 0..topology.online() {
        for index in 0..SPINNERS_PER_CPU {
            spinners.push(super::spawn_on(
                "spinner",
                spinner,
                cpu,
                spinner_weight(index),
                cpu,
                true,
            )?);
        }
    }
    Ok(spinners)
}

/// Check, from outside the scheduler, that each processor's spinners got CPU
/// in proportion to their weights.
///
/// The same claim the scheduler measured for itself while the window was
/// open, recomputed here from the runtime counters — so a scheduler whose
/// own measurement was wrong in the same way as its accounting still has to
/// answer to this one.
fn shares_are_proportional(
    topology: &Topology,
    spinners: &[Arc<Task>],
    bound: u64,
) -> Result<(), &'static str> {
    for cpu in 0..topology.online() {
        let group: Vec<&Arc<Task>> = spinners
            .iter()
            .skip(cpu * SPINNERS_PER_CPU)
            .take(SPINNERS_PER_CPU)
            .collect();

        let total: u128 = group
            .iter()
            .map(|task| u128::from(task.since_baseline(task.runtime())))
            .sum();
        let weights: u128 = group
            .iter()
            .map(|task| u128::from(task.entity_state().weight))
            .sum();
        if total == 0 || weights == 0 {
            return Err("a processor's spinners ran for no time at all");
        }

        for task in group {
            let had = u128::from(task.since_baseline(task.runtime()));
            let share = total * u128::from(task.entity_state().weight) / weights;
            if share.abs_diff(had) > u128::from(bound) {
                // Named, because "a task" is one of a dozen and which one it
                // was says whether the weights or the accounting is at fault.
                crate::console::println!(
                    "  fair     {} on cpu {} had {} ns of {} due, bound {}",
                    task.name,
                    cpu,
                    had,
                    share,
                    bound,
                );
                return Err("a task's share of its processor was not its weight's share");
            }
        }
    }
    Ok(())
}
