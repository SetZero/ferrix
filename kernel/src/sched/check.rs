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

use ferrix_sched::{CpuSet, LOAD_SCALE, NICE_0_WEIGHT, weight_of_nice};

use super::task::Task;
use super::{MIN_SLICE_NS, SLICE_NS, WaitQueue};
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

/// How long to watch confined tasks for a processor they should never reach.
const AFFINITY_WATCH_NANOS: u64 = 60_000_000;

/// How long to let a load average settle before reading it.
///
/// Three of `ferrix_sched`'s 33-millisecond half-lives, which takes a
/// permanently busy processor to about seven eighths of full — comfortably
/// past the half the check asks for, and not the near-perfect convergence it
/// used to wait for.
///
/// **Guest milliseconds are expensive.** Every one of them is emulated, and
/// this file's sleeps are what took the armv7a boot test from 23 seconds to
/// 64 against a 120-second timeout. A check that is three times more precise
/// than its own threshold needs is not three times better, it is three times
/// closer to a boot test that fails on a busy machine.
const LOAD_SETTLE_NANOS: u64 = 100_000_000;

/// How long to give periodic balancing to notice an imbalance.
///
/// Balancing runs at most once every 16 milliseconds per processor and moves
/// one task each time, so this is still several chances per processor rather
/// than one — enough to level a queue that is a few tasks too long, which is
/// what the check builds.
const BALANCE_SETTLE_NANOS: u64 = 120_000_000;

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
    /// Which ones, as a bit per processor. Printed beside the count because
    /// the count on its own invites being read as a capability: stealing is
    /// opportunistic, and a processor that lost every race for a task is not
    /// a processor that could not have run one. The fairness check below is
    /// the one that requires *every* processor to run something.
    pub(crate) processor_mask: u64,
    /// Spinners the fairness check ran.
    pub(crate) spinners: usize,
    /// The worst any task's service strayed from its share.
    pub(crate) worst_lag: u64,
    /// What that had to stay inside: a slice, plus the worst overrun served.
    pub(crate) bound: u64,
    /// How long the sleep check actually slept.
    pub(crate) slept: u64,
    /// Guest milliseconds each of the nine checks took, in order.
    pub(crate) spent_ms: [u64; 9],
    /// Spawns the placer sent to a processor other than the caller's.
    pub(crate) placed_elsewhere: u64,
    /// How many distinct processors new tasks were *placed* on, before any
    /// stealing or balancing could move them.
    pub(crate) placed_on: u32,
    /// The busiest and least busy load averages seen while every processor
    /// was running a spinner.
    pub(crate) load_high: u64,
    /// The load average of an idle processor, after it has decayed.
    pub(crate) load_low: u64,
    /// Tasks periodic balancing moved between processors that were all busy.
    pub(crate) balanced: u64,
    /// The slice handed out with one runnable task, and with many.
    pub(crate) slice_one: u64,
    /// As above, with the queue full.
    pub(crate) slice_many: u64,
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
    // Guest milliseconds per phase, collected and printed once at the end
    // rather than as each finishes: these checks cost the armv7a boot test
    // more than every other stage put together, and an attribution nobody can
    // see is one nobody will act on.
    let mut spent = [0u64; 9];
    let mut at = crate::timer::now_nanos();
    // A macro rather than a closure: a closure would borrow `spent` for the
    // whole of the phases below, and releasing that borrow to read it again
    // means either a scope around everything or a `drop` that lints.
    macro_rules! mark {
        ($index:expr) => {{
            let now = crate::timer::now_nanos();
            if let Some(slot) = spent.get_mut($index) {
                *slot = now.saturating_sub(at) / 1_000_000;
            }
            at = now;
        }};
    }

    one_task()?;
    mark!(0);
    sleeping(&mut report)?;
    mark!(1);
    many_tasks(topology, &mut report)?;
    mark!(2);
    fairness(topology, &mut report)?;
    mark!(3);
    placement(topology, &mut report)?;
    mark!(4);
    affinity_is_obeyed(topology)?;
    mark!(5);
    load_tracking(topology, &mut report)?;
    mark!(6);
    balancing(topology, &mut report)?;
    mark!(7);
    slice_scaling(&mut report)?;
    mark!(8);
    // The last `mark!` advances `at` for a phase that never comes; reading it
    // here is what says so, rather than an allow.
    let _ = at;
    report.spent_ms = spent;

    let summary = super::summary();
    report.switches = summary.switches;
    report.steals = summary.steals;
    super::check_invariants()?;
    Ok(report)
}

/// A new task is *placed*, not merely created where its parent happened to be.
///
/// The check is deliberately made before anything can run: it reads where each
/// task was put, not where it ended up. Stealing and balancing would spread
/// these out eventually, and that is exactly what this must not be allowed to
/// pass on — the question is whether the decision was made at all.
fn placement(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    let allocations = crate::vmap::usage().allocations;
    let online = topology.online();
    STOP.store(false, Ordering::Release);
    SPINNING.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);

    let placed_before = super::placed_elsewhere();
    let mut tasks = Vec::with_capacity(online);
    let mut landed = 0u64;

    for _ in 0..online {
        let task = super::spawn("placed", spinner, 0, NICE_0_WEIGHT)?;
        let cpu = task.cpu();
        if cpu < 64 {
            landed |= 1u64 << cpu;
        }
        tasks.push(task);
    }
    report.placed_on = landed.count_ones();

    STOP.store(true, Ordering::Release);
    wait_for(
        || DONE.load(Ordering::Acquire) >= online as u64,
        "a placed task never finished",
    )?;

    // **The decision, not the outcome.** Placement can only be observed where
    // it happens: an idle processor steals a new task within microseconds, so
    // a check that reads `task.cpu()` afterwards is measuring stealing. Tested
    // by removing placement entirely — a check on where the tasks ended up
    // passed regardless, on two processors and on four.
    //
    // This check used to require the tasks to spread over two processors or
    // more, on the premise that every processor was idle. On two processors
    // that premise is false: the task doing the spawning is running on one of
    // them. The first spawn goes to the idle processor, the second finds one
    // task on each and a tie the load average settles in favour of the
    // processor idle longer — both land together, the placer having done
    // exactly the right thing, and the check failed. Deterministically, on
    // hardware, where it was found.
    let placed = super::placed_elsewhere().saturating_sub(placed_before);
    report.placed_elsewhere = placed;
    if online > 1 && placed == 0 {
        crate::console::println!(
            "  place    none of {} spawns chose a processor other than the caller's",
            tasks.len(),
        );
        return Err("placement never sent a new task off the processor that created it");
    }

    reap_to(allocations, "placement")?;
    drop(tasks);
    Ok(())
}

/// A task with an affinity runs only inside it.
///
/// The one property that has to hold even when it costs throughput: a task
/// allowed on two processors of four must never be seen on the other two,
/// however idle they are and however loaded its own are.
fn affinity_is_obeyed(topology: &Topology) -> Result<(), &'static str> {
    let online = topology.online();
    if online < 2 {
        return Ok(());
    }
    let allocations = crate::vmap::usage().allocations;
    STOP.store(false, Ordering::Release);
    SPINNING.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);

    // Processors 0 and 1 only, and more tasks than that can comfortably hold,
    // so an unconstrained balancer would have every reason to spill them.
    let mut allowed = CpuSet::empty();
    allowed.insert(0).map_err(|_| "no processor 0")?;
    allowed.insert(1).map_err(|_| "no processor 1")?;

    let count = online * 2;
    let mut tasks = Vec::with_capacity(count);
    for index in 0..count {
        tasks.push(super::spawn_on(
            "confined",
            spinner,
            index,
            NICE_0_WEIGHT,
            index % 2,
            allowed,
        )?);
    }

    wait_for(
        || SPINNING.load(Ordering::Acquire) >= count as u64,
        "a confined task never started",
    )?;
    // Long enough for a balancer to have moved them if it were going to:
    // it looks every 16 milliseconds per processor, so this is several looks.
    super::sleep_for(AFFINITY_WATCH_NANOS);
    STOP.store(true, Ordering::Release);
    wait_for(
        || DONE.load(Ordering::Acquire) >= count as u64,
        "a confined task never stopped",
    )?;

    for task in &tasks {
        let ran_on = task.cpus_run_on();
        if ran_on & !0b11 != 0 {
            crate::console::println!("  affinity a task allowed on 0b11 ran on {ran_on:#b}",);
            return Err("a task ran on a processor its affinity excluded");
        }
    }

    reap_to(allocations, "affinity")?;
    drop(tasks);
    Ok(())
}

/// A processor running something reads as loaded; one running nothing decays.
fn load_tracking(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    let allocations = crate::vmap::usage().allocations;
    let online = topology.online();
    STOP.store(false, Ordering::Release);
    SPINNING.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);

    // One spinner pinned to every processor but the last, so the last is the
    // control: the same machine, the same moment, nothing to run.
    let busy_cpus = online.saturating_sub(1).max(1);
    let mut tasks = Vec::with_capacity(busy_cpus);
    for cpu in 0..busy_cpus {
        tasks.push(super::spawn_on(
            "loaded",
            spinner,
            cpu,
            NICE_0_WEIGHT,
            cpu,
            CpuSet::of(cpu),
        )?);
    }
    wait_for(
        || SPINNING.load(Ordering::Acquire) >= busy_cpus as u64,
        "a load-test task never started",
    )?;

    // Several half-lives, so the average is near its steady state rather than
    // still climbing.
    super::sleep_for(LOAD_SETTLE_NANOS);

    let mut lowest_busy = u64::MAX;
    for cpu in 0..busy_cpus {
        let load = super::cpu_report(cpu)
            .ok_or("a processor has no queue")?
            .load;
        lowest_busy = lowest_busy.min(load);
    }
    report.load_high = lowest_busy;
    if online > 1 {
        let idle = super::cpu_report(online - 1).ok_or("a processor has no queue")?;
        report.load_low = idle.load;
    }

    STOP.store(true, Ordering::Release);
    wait_for(
        || DONE.load(Ordering::Acquire) >= busy_cpus as u64,
        "a load-test task never stopped",
    )?;

    // Half full is a generous floor for a processor that has had a task
    // spinning on it for several half-lives; the point is to catch an average
    // that never moves, not to pin down its exact value.
    if report.load_high < LOAD_SCALE / 2 {
        return Err("a processor running a spinner did not read as loaded");
    }
    if online > 1 && report.load_low >= report.load_high {
        return Err("an idle processor read as loaded as a busy one");
    }

    reap_to(allocations, "load tracking")?;
    drop(tasks);
    Ok(())
}

/// Work moves between processors that are all busy.
///
/// The case work stealing cannot reach, and the reason periodic balancing
/// exists: stealing happens when a processor runs out of work, so a machine
/// where no processor ever does is a machine stealing never touches. Every
/// processor here has a spinner pinned to it, so none of them ever idles, and
/// the movable tasks all start on one.
fn balancing(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    let online = topology.online();
    if online < 2 {
        return Ok(());
    }
    let allocations = crate::vmap::usage().allocations;
    let before = super::balanced_count();
    STOP.store(false, Ordering::Release);
    SPINNING.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);

    let everywhere = CpuSet::first(online).map_err(|_| "too many processors for a set")?;
    let movable = online * 2;
    let total = online + movable;

    let mut tasks = Vec::with_capacity(total);
    // The floor: one per processor, pinned, so nothing ever goes idle.
    for cpu in 0..online {
        tasks.push(super::spawn_on(
            "anchor",
            spinner,
            cpu,
            NICE_0_WEIGHT,
            cpu,
            CpuSet::of(cpu),
        )?);
    }

    // **Every anchor must be *running* before the imbalance is created.**
    // Until then the other processors are still idle, and an idle processor
    // steals — so the movable tasks would be spread by the mechanism this
    // check is meant to exclude, and it would pass without periodic balancing
    // existing at all. That is how this check first failed: not because
    // balancing was broken, but because stealing beat it to the work.
    wait_for(
        || SPINNING.load(Ordering::Acquire) >= online as u64,
        "an anchor task never started",
    )?;

    // The imbalance: all of them on processor 0, free to move. No processor
    // will go idle from here until STOP, so nothing but `balance` can move
    // them.
    for index in 0..movable {
        tasks.push(super::spawn_on(
            "movable",
            spinner,
            index,
            NICE_0_WEIGHT,
            0,
            everywhere,
        )?);
    }

    wait_for(
        || SPINNING.load(Ordering::Acquire) >= total as u64,
        "a balancing task never started",
    )?;
    super::sleep_for(BALANCE_SETTLE_NANOS);

    let moved = super::balanced_count().saturating_sub(before);
    report.balanced = moved;

    // Sampled while the tasks are still running: after STOP every queue is
    // empty and every load average is on its way to zero, which says nothing
    // about the state the balancer was looking at.
    let mut spread = Vec::with_capacity(online);
    for cpu in 0..online {
        spread.push(super::cpu_report(cpu).map(|report| (report.load, report.queued)));
    }

    STOP.store(true, Ordering::Release);
    wait_for(
        || DONE.load(Ordering::Acquire) >= total as u64,
        "a balancing task never stopped",
    )?;

    if moved == 0 {
        for (cpu, sample) in spread.iter().enumerate() {
            if let Some((load, queued)) = sample {
                crate::console::println!("  balance  cpu {cpu} load {load} queued {queued}");
            }
        }
        crate::console::println!(
            "  balance  {movable} movable tasks stayed on one processor of {online}",
        );
        return Err("no task was balanced away from an overloaded processor");
    }

    reap_to(allocations, "balancing")?;
    drop(tasks);
    Ok(())
}

/// The slice shrinks as more becomes runnable, and stops at the floor.
fn slice_scaling(report: &mut Report) -> Result<(), &'static str> {
    let allocations = crate::vmap::usage().allocations;
    let here = super::current()
        .ok_or("the checking task is not running")?
        .cpu();

    report.slice_one = super::cpu_report(here).ok_or("no queue here")?.slice_ns;

    STOP.store(false, Ordering::Release);
    SPINNING.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);

    // Enough on this one processor to take the slice to its floor, which the
    // target latency reaches at eight runnable. More would only cost guest
    // milliseconds to demonstrate the same floor.
    let crowd = 8;
    let mut tasks = Vec::with_capacity(crowd);
    for index in 0..crowd {
        tasks.push(super::spawn_on(
            "crowd",
            spinner,
            index,
            NICE_0_WEIGHT,
            here,
            CpuSet::of(here),
        )?);
    }
    wait_for(
        || SPINNING.load(Ordering::Acquire) >= crowd as u64,
        "a crowding task never started",
    )?;
    report.slice_many = super::cpu_report(here).ok_or("no queue here")?.slice_ns;

    STOP.store(true, Ordering::Release);
    wait_for(
        || DONE.load(Ordering::Acquire) >= crowd as u64,
        "a crowding task never stopped",
    )?;

    if report.slice_many >= report.slice_one {
        return Err("the slice did not shrink as the run queue filled");
    }
    if report.slice_many < MIN_SLICE_NS {
        return Err("the slice fell below the floor");
    }

    reap_to(allocations, "slice scaling")?;
    drop(tasks);
    Ok(())
}

/// Tell anything waiting that a counter it might be watching has moved.
///
/// **Every store to a counter a `wait_for` predicate reads must be followed by
/// one of these.** The wait queue is the only thing that can end a wait early;
/// without a notify the waiter sleeps its entire deadline and *then* finds the
/// condition true, so the check passes and costs exactly `PATIENCE_NANOS`.
///
/// That is not hypothetical, and it is why this file needed a comment rather
/// than a convention. `SPINNING` was incremented without one, and all seven
/// waits for "a spinner never started" paid twenty seconds each — around forty
/// seconds of the armv7a boot test, on a run that reported success. It took
/// per-phase timings to see it at all, because a silent tax looks exactly like
/// a slow machine.
fn notify() {
    FINISHED.wake_all();
}

/// Count this task as finished, and wake whoever is waiting for the phase.
fn finish() {
    let _ = DONE.fetch_add(1, Ordering::AcqRel);
    notify();
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
    // The counter every "a spinner never started" wait watches. Without this
    // the waiter has nothing to wake it and sleeps its whole budget.
    notify();
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
fn reap_to(allocations: usize, what: &str) -> Result<(), &'static str> {
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
            let usage = crate::vmap::usage();
            crate::console::println!(
                "  tasks    after {what}: arena holds {} allocations, expected {allocations}",
                usage.allocations,
            );
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

    reap_to(allocations, "one task")?;
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
    report.processor_mask = processors;

    if topology.online() > 1 && report.processors < 2 {
        return Err("every thread ran on one processor: work stealing moved nothing");
    }

    reap_to(allocations, "a thousand tasks")?;
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
    if let Err(problem) = wait_for(
        || SPINNING.load(Ordering::Acquire) >= total,
        "a spinner never started",
    ) {
        crate::console::println!(
            "  fair     {} of {} spinners started",
            SPINNING.load(Ordering::Acquire),
            total,
        );
        for (index, task) in spinners.iter().enumerate() {
            crate::console::println!(
                "  fair     spinner {} wanted cpu {}, is on cpu {}, ran on {:#b}, {} switches",
                index,
                index / SPINNERS_PER_CPU,
                task.cpu(),
                task.cpus_run_on(),
                task.switches(),
            );
        }
        super::report_queues();
        return Err(problem);
    }

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

    reap_to(allocations, "fairness")?;
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
                CpuSet::of(cpu),
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
