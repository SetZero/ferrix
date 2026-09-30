//! Wakes that move a task onto the waker's processor (`sched::wake_with`),
//! checked under stress and against affinity.
//!
//! The relay is the stress. On every processor sits an *anchor*, pinned there,
//! and between the anchors runs a *traveller*, free to run anywhere: the
//! anchor holding the token hands it to the traveller with a sync wake and
//! waits, and the traveller hands it to the next processor's anchor and
//! waits. So every hop a traveller makes is a sync wake from the processor it
//! is about to be asked to run on, and it arrives in whichever state the
//! traveller is in by then: all the way asleep on the last processor, which
//! is a move, or still part-way into its block, which must not be. One
//! traveller per processor, each with its own anchors, all at once.
//!
//! A task the move lost is on no run queue and in no sleeper set, so nothing
//! runs it again: its relay stops, and the check says which traveller stopped
//! at which hop. Every hop is counted by the party that took it, and the
//! counts must come out exactly: each traveller [`RELAY_HOPS`], its anchors as
//! many between them. A wake that went missing is not a lost task -- the
//! party's wait looks again every [`RECHECK_NANOS`], as every wait in the
//! kernel does -- and it is counted, not failed: a hand-over the recheck found
//! rather than a wake is printed in the `sync` line. (The relay found such
//! wakes going missing on AArch64 with a GICv3, with every wake made at home,
//! before any of this: `docs/BACKLOG.md`.)

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_sched::{CpuSet, NICE_0_WEIGHT};

use super::{Task, WaitQueue, Wake};

/// Hops each traveller makes: a few thousand wakes in all at four processors.
pub(super) const RELAY_HOPS: u64 = 1000;

/// Most processors the relay uses, and so most travellers.
const RELAY_CPUS: usize = 4;

/// How long the relay may take before a stopped one is called a loss.
/// Generous, because an emulated processor is a host thread that may not be
/// running; a lost task never finishes however long it is given.
const PATIENCE_NANOS: u64 = 30_000_000_000;

/// How long a wait in these checks goes before looking again of its own
/// accord: what `WaitQueue::wait_until_deadline` uses everywhere.
const RECHECK_NANOS: u64 = 5_000_000;

/// Relay hand-overs a party found at its recheck, no wake having taken it off
/// its queue.
static RECHECKED: AtomicU64 = AtomicU64::new(0);

/// Rounds of the affinity check.
const PINNED_ROUNDS: u64 = 4;

/// Where each traveller's token is: hop `2k` is anchor `k % n`'s to take, hop
/// `2k + 1` the traveller's.
static STEP: [AtomicU64; RELAY_CPUS] = [const { AtomicU64::new(0) }; RELAY_CPUS];

/// Where each traveller waits.
static TRAVELLER_WAITS: [WaitQueue; RELAY_CPUS] = [const { WaitQueue::new() }; RELAY_CPUS];

/// Where each traveller's anchors wait, by traveller and processor.
static ANCHOR_WAITS: [[WaitQueue; RELAY_CPUS]; RELAY_CPUS] =
    [const { [const { WaitQueue::new() }; RELAY_CPUS] }; RELAY_CPUS];

/// Hops each traveller took.
static TRAVELLER_HOPS: [AtomicU64; RELAY_CPUS] = [const { AtomicU64::new(0) }; RELAY_CPUS];

/// Hops each traveller's anchors took between them.
static ANCHOR_HOPS: [AtomicU64; RELAY_CPUS] = [const { AtomicU64::new(0) }; RELAY_CPUS];

/// Relay parties that have stopped, and those that stopped because their wait
/// ran out rather than because the relay was over.
static STOPPED: AtomicU64 = AtomicU64::new(0);
static TIMED_OUT: AtomicU64 = AtomicU64::new(0);

/// Processors in the relay, set before any party starts.
static CPUS: AtomicU64 = AtomicU64::new(0);

/// Where the checker waits for the relay and for the affinity check.
static CHECKER: WaitQueue = WaitQueue::new();

/// What the checks found, for the boot log.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SyncReport {
    /// Travellers, one per processor in the relay.
    pub(crate) travellers: usize,
    /// Hops made in all.
    pub(crate) hops: u64,
    /// Of the relay's wakes, those that moved a task onto its waker's
    /// processor.
    pub(crate) moved: u64,
    /// Processors the travellers ran on, a bit each.
    pub(crate) ran_on: u64,
    /// Hand-overs a party found at its recheck rather than by a wake.
    pub(crate) rechecked: u64,
    /// What the polling check saw; `None` where processors do not poll.
    pub(crate) poll: Option<PollReport>,
}

/// Run them. `Ok` with nothing measured on a machine of one processor.
///
/// # Errors
///
/// What did not hold, as a sentence.
pub(super) fn run(online: usize) -> Result<SyncReport, &'static str> {
    if online < 2 {
        return Ok(SyncReport::default());
    }
    let mut report = relay(online)?;
    a_sync_wake_keeps_a_task_inside_its_affinity()?;
    report.poll = a_polling_processor_needs_no_interrupt()?;
    Ok(report)
}

/// Rounds of the polling check.
const POLL_ROUNDS: u64 = 16;

/// How long the polling check watches for processor 1 to poll, each round:
/// half the longest poll, so that a round that misses it wakes a halt short
/// enough to grow processor 1's window rather than shrink it.
const POLL_WATCH_NANOS: u64 = super::HALT_POLL_NANOS / 2;

/// What the polling check saw.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PollReport {
    /// Rounds in which processor 1 was seen polling before its kick.
    pub(crate) seen: u64,
    /// Kicks that found a processor polling and sent no interrupt.
    pub(crate) absorbed: u64,
}

/// A kick that finds its processor polling sends no interrupt, and the task
/// it was for still runs.
///
/// A task confined to processor 1 waits [`POLL_ROUNDS`] times. Each round,
/// once it is all the way asleep, the checker on processor 0 watches up to
/// [`POLL_WATCH_NANOS`] for processor 1's idle task to poll
/// (`sched::halt_poll`), then wakes it with a plain wake. Every round must end
/// in the task running. If processor 1 was seen polling in any round, some
/// kick must have found it so and sent nothing: the one sent the moment it
/// was seen finds the mark up unless the poll ran out in between. A host can
/// hold the checker past every poll, and processor 1's window starts each
/// round where the last left it, so a machine where it was never seen checks
/// only that every task ran, and the boot log says so.
///
/// Verifies: L.sched.5
fn a_polling_processor_needs_no_interrupt() -> Result<Option<PollReport>, &'static str> {
    if !crate::arch::polls_before_halt() {
        return Ok(None);
    }
    let allocations = crate::vmap::usage().allocations;
    GO.store(0, Ordering::Release);
    ROUNDS_DONE.store(0, Ordering::Release);
    SLEEPER_ROUNDS.store(POLL_ROUNDS, Ordering::Release);
    let absorbed_before = super::poll_counts().2;
    let task = super::spawn_on(
        "poll sleeper",
        sleeper,
        0,
        NICE_0_WEIGHT,
        PINNED_CPU,
        CpuSet::of(PINNED_CPU),
    )?;
    let polling = || {
        super::POLLING
            .get()
            .and_then(|marks| marks.get(PINNED_CPU))
            .is_some_and(|mark| mark.load(Ordering::Acquire))
    };
    let mut report = PollReport::default();
    for round in 1..=POLL_ROUNDS {
        let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
        while !(task.is_blocked() && !task.is_queued() && SLEEPER_WAITS.listed() == 1) {
            if crate::timer::now_nanos() >= deadline {
                return Err("the polling check's sleeper never went to sleep");
            }
            core::hint::spin_loop();
        }
        let watch = crate::timer::now_nanos().saturating_add(POLL_WATCH_NANOS);
        let mut seen = polling();
        while !seen && crate::timer::now_nanos() < watch {
            core::hint::spin_loop();
            seen = polling();
        }
        report.seen += u64::from(seen);
        GO.store(round, Ordering::Release);
        SLEEPER_WAITS.wake_all();
        let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
        if !CHECKER.wait_until_deadline(|| ROUNDS_DONE.load(Ordering::Acquire) >= round, deadline) {
            return Err("a task kicked on a polling processor never ran");
        }
    }
    super::wait_until_gone(&task, PATIENCE_NANOS)?;
    SLEEPER_ROUNDS.store(PINNED_ROUNDS, Ordering::Release);
    drop(task);
    report.absorbed = super::poll_counts().2.saturating_sub(absorbed_before);
    if report.seen > 0 && report.absorbed == 0 {
        crate::console::println!(
            "  poll     processor {PINNED_CPU} was seen polling in {} of {POLL_ROUNDS} rounds, and \
             every kick sent it an interrupt anyway",
            report.seen,
        );
        return Err("kicks to a processor seen polling all sent an interrupt");
    }
    super::check::reap_to(allocations, "the polling check's sleeper")?;
    Ok(Some(report))
}

/// The relay: see the module documentation.
///
/// Verifies: L.sched.3
fn relay(online: usize) -> Result<SyncReport, &'static str> {
    let allocations = crate::vmap::usage().allocations;
    let cpus = online.min(RELAY_CPUS);
    CPUS.store(cpus as u64, Ordering::Release);
    STOPPED.store(0, Ordering::Release);
    TIMED_OUT.store(0, Ordering::Release);
    RECHECKED.store(0, Ordering::Release);
    for traveller in 0..cpus {
        STEP[traveller].store(0, Ordering::Release);
        TRAVELLER_HOPS[traveller].store(0, Ordering::Release);
        ANCHOR_HOPS[traveller].store(0, Ordering::Release);
    }
    let anywhere = *super::domain_cpus().ok_or("the scheduler has no domain")?;
    let moved_before = super::woken_here();

    let mut travellers: Vec<Arc<Task>> = Vec::with_capacity(cpus);
    let mut parties = 0_u64;
    for traveller in 0..cpus {
        for cpu in 0..cpus {
            let _anchor = super::spawn_on(
                "relay anchor",
                anchor,
                traveller * RELAY_CPUS + cpu,
                NICE_0_WEIGHT,
                cpu,
                CpuSet::of(cpu),
            )?;
            parties += 1;
        }
        // Started away from its first anchor, so its first hop is a move.
        travellers.push(super::spawn_on(
            "relay traveller",
            traveller_task,
            traveller,
            NICE_0_WEIGHT,
            (traveller + 1) % cpus,
            anywhere,
        )?);
        parties += 1;
    }

    let deadline =
        crate::timer::now_nanos().saturating_add(PATIENCE_NANOS.saturating_add(PATIENCE_NANOS / 4));
    let all_stopped =
        CHECKER.wait_until_deadline(|| STOPPED.load(Ordering::Acquire) >= parties, deadline);
    let mut report = SyncReport {
        travellers: cpus,
        ..SyncReport::default()
    };
    for (index, traveller) in travellers.iter().enumerate() {
        let hops = TRAVELLER_HOPS[index].load(Ordering::Acquire);
        let anchors = ANCHOR_HOPS[index].load(Ordering::Acquire);
        report.hops = report.hops.saturating_add(hops).saturating_add(anchors);
        report.ran_on |= traveller.cpus_run_on();
        if hops != RELAY_HOPS || anchors != RELAY_HOPS {
            crate::console::println!(
                "  sync     traveller {index} stopped at step {} of {}: it took {hops} hops and \
                 its anchors {anchors}, {RELAY_HOPS} each were due; {} parties of {parties} \
                 stopped, {} of them timed out",
                STEP[index].load(Ordering::Acquire),
                2 * RELAY_HOPS,
                STOPPED.load(Ordering::Acquire),
                TIMED_OUT.load(Ordering::Acquire),
            );
            return Err(if all_stopped {
                "a relay stopped short: its parties waited out their deadline"
            } else {
                "a task woken onto its waker's processor never ran again"
            });
        }
    }
    if !all_stopped || TIMED_OUT.load(Ordering::Acquire) != 0 {
        return Err("a relay party waited out its deadline");
    }
    report.moved = super::woken_here().saturating_sub(moved_before);
    report.rechecked = RECHECKED.load(Ordering::Acquire);
    if report.moved == 0 {
        return Err("no wake in the relay moved a task onto its waker's processor");
    }
    if report.ran_on.count_ones() < 2 {
        return Err("the relay's travellers never left one processor");
    }
    drop(travellers);
    super::check::reap_to(allocations, "the relay")?;
    Ok(report)
}

/// Wait on `queue` until `ready`, counting in [`RECHECKED`] a wait its recheck
/// ended rather than a wake. Each party waits on a queue of its own, so the
/// queue's count of waits a wake ended is the party's.
/// `false` if the relay's deadline passed first.
fn relay_wait(queue: &WaitQueue, mut ready: impl FnMut() -> bool, deadline: u64) -> bool {
    if ready() {
        return true;
    }
    let woken = queue.waits_ended_by_a_wake();
    let ended = WaitQueue::wait_on_any(&[queue], ready, deadline, RECHECK_NANOS);
    if ended && queue.waits_ended_by_a_wake() == woken {
        let _ = RECHECKED.fetch_add(1, Ordering::Relaxed);
    }
    ended
}

/// Stop as a relay party, saying whether the relay was over.
fn stop(over: bool) {
    if !over {
        let _ = TIMED_OUT.fetch_add(1, Ordering::AcqRel);
    }
    let _ = STOPPED.fetch_add(1, Ordering::AcqRel);
    CHECKER.wake_all();
}

/// An anchor: `argument` is its traveller times [`RELAY_CPUS`] plus its
/// processor. Takes the token when it is its turn and hands it on to the
/// traveller.
fn anchor(argument: usize) {
    let (traveller, cpu) = (argument / RELAY_CPUS, argument % RELAY_CPUS);
    let (Some(step), Some(waits), Some(hops), Some(next)) = (
        STEP.get(traveller),
        ANCHOR_WAITS.get(traveller).and_then(|waits| waits.get(cpu)),
        ANCHOR_HOPS.get(traveller),
        TRAVELLER_WAITS.get(traveller),
    ) else {
        stop(false);
        return;
    };
    let cpus = CPUS.load(Ordering::Acquire).max(1);
    let last = 2 * RELAY_HOPS;
    let mine = |at: u64| at >= last || (at % 2 == 0 && (at / 2) % cpus == cpu as u64);
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    loop {
        if !relay_wait(waits, || mine(step.load(Ordering::Acquire)), deadline) {
            stop(false);
            return;
        }
        if step.load(Ordering::Acquire) >= last {
            stop(true);
            return;
        }
        let _ = hops.fetch_add(1, Ordering::AcqRel);
        let _ = step.fetch_add(1, Ordering::AcqRel);
        // This anchor waits next: the traveller may have its processor.
        next.wake_all_with(Wake::Sync);
    }
}

/// A traveller: takes the token from an anchor and hands it to the next
/// processor's.
fn traveller_task(traveller: usize) {
    let (Some(step), Some(waits), Some(hops), Some(anchors)) = (
        STEP.get(traveller),
        TRAVELLER_WAITS.get(traveller),
        TRAVELLER_HOPS.get(traveller),
        ANCHOR_WAITS.get(traveller),
    ) else {
        stop(false);
        return;
    };
    let cpus = CPUS.load(Ordering::Acquire).max(1);
    let last = 2 * RELAY_HOPS;
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    loop {
        let ready = || {
            let at = step.load(Ordering::Acquire);
            at >= last || at % 2 == 1
        };
        if !relay_wait(waits, ready, deadline) {
            stop(false);
            return;
        }
        if step.load(Ordering::Acquire) >= last {
            break;
        }
        let _ = hops.fetch_add(1, Ordering::AcqRel);
        let at = step.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        if at >= last {
            break;
        }
        let next = usize::try_from((at / 2) % cpus).unwrap_or(0);
        if let Some(queue) = anchors.get(next) {
            queue.wake_all_with(Wake::Sync);
        }
    }
    // Over: every anchor looks, sees it, and stops.
    for queue in anchors {
        queue.wake_all();
    }
    stop(true);
}

/// The processor the pinned sleeper is confined to.
const PINNED_CPU: usize = 1;

/// Rounds the affinity check's sleepers have been let through, and have
/// finished.
static GO: AtomicU64 = AtomicU64::new(0);
static ROUNDS_DONE: AtomicU64 = AtomicU64::new(0);

/// Where the affinity check's sleeper waits.
static SLEEPER_WAITS: WaitQueue = WaitQueue::new();

/// A sync wake never moves a task outside its affinity, and does move one
/// inside it.
///
/// The checker runs on processor 0, alone there. A task confined to processor
/// 1 is woken with a sync wake [`PINNED_ROUNDS`] times, each once it is all
/// the way asleep: no wake may move it, and it must never run on processor 0.
/// Then the same with a task free to run anywhere, started on processor 1,
/// which at least one such wake must move: the control that shows the check
/// could see a move at all. Moves are counted where they are made
/// (`sched::woken_here`), nothing else wakes that way meanwhile, and where the
/// free task ran is not asked: an idle processor may steal it back from
/// behind the checker before it runs, which is a steal, not a wake.
///
/// Verifies: L.sched.4
fn a_sync_wake_keeps_a_task_inside_its_affinity() -> Result<(), &'static str> {
    let anywhere = *super::domain_cpus().ok_or("the scheduler has no domain")?;
    let allocations = crate::vmap::usage().allocations;
    for (affinity, confined) in [(CpuSet::of(PINNED_CPU), true), (anywhere, false)] {
        GO.store(0, Ordering::Release);
        ROUNDS_DONE.store(0, Ordering::Release);
        let moved_before = super::woken_here();
        let task = super::spawn_on(
            "sync sleeper",
            sleeper,
            0,
            NICE_0_WEIGHT,
            PINNED_CPU,
            affinity,
        )?;
        for round in 1..=PINNED_ROUNDS {
            // All the way asleep: blocked, and off every run queue.
            let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
            while !(task.is_blocked() && !task.is_queued() && SLEEPER_WAITS.listed() == 1) {
                if crate::timer::now_nanos() >= deadline {
                    return Err("the affinity check's sleeper never went to sleep");
                }
                // Spun, not slept: a checker asleep leaves processor 0 idle,
                // and an idle processor 0 steals the free sleeper before it
                // first runs, which then never sleeps anywhere but here and
                // gives the control nothing to move.
                core::hint::spin_loop();
            }
            GO.store(round, Ordering::Release);
            SLEEPER_WAITS.wake_all_with(Wake::Sync);
            let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
            if !CHECKER
                .wait_until_deadline(|| ROUNDS_DONE.load(Ordering::Acquire) >= round, deadline)
            {
                return Err("the affinity check's sleeper never woke");
            }
        }
        super::wait_until_gone(&task, PATIENCE_NANOS)?;
        let ran_on = task.cpus_run_on();
        let moved = super::woken_here().saturating_sub(moved_before);
        if confined && (ran_on != 1 << PINNED_CPU || moved != 0) {
            crate::console::println!(
                "  sync     a task confined to processor {PINNED_CPU} ran on {ran_on:#b} after \
                 {PINNED_ROUNDS} sync wakes from processor 0, {moved} of them moves"
            );
            return Err("a sync wake moved a task off the processors its affinity allows");
        }
        if !confined && moved == 0 {
            crate::console::println!(
                "  sync     a task free to run anywhere ran on {ran_on:#b} after \
                 {PINNED_ROUNDS} sync wakes from processor 0, {moved} of them moves"
            );
            return Err("a sync wake never brought a free task to its waker's processor");
        }
        drop(task);
    }
    super::check::reap_to(allocations, "the affinity check's sleepers")
}

/// Rounds the next sleeper waits for.
static SLEEPER_ROUNDS: AtomicU64 = AtomicU64::new(PINNED_ROUNDS);

/// The affinity and polling checks' sleeper: waits for each round, and says
/// it ran.
fn sleeper(_argument: usize) {
    for round in 1..=SLEEPER_ROUNDS.load(Ordering::Acquire) {
        let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
        let _ = WaitQueue::wait_on_any(
            &[&SLEEPER_WAITS],
            || GO.load(Ordering::Acquire) >= round,
            deadline,
            RECHECK_NANOS,
        );
        ROUNDS_DONE.store(round, Ordering::Release);
        CHECKER.wake_all();
    }
}
