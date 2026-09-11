//! Stage 4's exit criterion, and the checks on the way to it.
//!
//! Every check here runs on every processor at once, through
//! [`run_everywhere`], because that is the only way to test anything about a
//! multiprocessor: a property that holds on each processor alone is exactly
//! the kind that fails when they run together.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_paging::MapFlags;
use ferrix_sync::{Once, SpinLock};

use super::{PerCpu, Topology, run_everywhere};
use crate::{arch, mm, vmap};

/// What the checks found, for the boot log.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Report {
    /// Rounds of work every processor ran.
    pub(crate) rounds: u64,
    /// Inter-processor interrupts the secondaries took doing it.
    pub(crate) ipis: u64,
    /// Times a page was moved to another frame under every processor.
    pub(crate) remaps: u64,
    /// Shootdowns run by interrupt while it was. None where the architecture
    /// invalidates every processor's TLB itself.
    pub(crate) shootdowns: u64,
    /// Grace periods the writer waited for.
    pub(crate) grace_periods: u64,
    /// Read-side sections the readers ran meanwhile.
    pub(crate) reads: u64,
    /// What the contended counter came to.
    pub(crate) counter: u64,
    /// What it had to come to.
    pub(crate) expected: u64,
    /// Processors whose increments overlapped another's in time.
    pub(crate) overlapping: u64,
    /// Updates the same count lost without the lock.
    pub(crate) lost: u64,
}

/// Run the checks.
///
/// # Errors
///
/// The first one that fails, as a sentence.
pub(crate) fn run(topology: &Topology) -> Result<Report, &'static str> {
    let mut report = Report::default();
    everywhere(topology, &mut report)?;
    shootdown(&mut report)?;
    grace(topology, &mut report)?;
    contended(topology, &mut report)?;
    Ok(report)
}

/// The contended counter, kept under a ticket lock.
static COUNTER: SpinLock<u64> = SpinLock::new(0);

/// The same count kept with no lock at all: a load and a store, not an atomic
/// increment. It loses an update whenever two processors increment it at the
/// same moment, and how many it loses measures how much "at the same moment"
/// there really was.
static UNLOCKED: AtomicU64 = AtomicU64::new(0);

/// Processors at the start line.
static AT_START: AtomicU64 = AtomicU64::new(0);

/// The first and last value of [`COUNTER`] each processor produced, by
/// logical number.
static SPANS: Once<Vec<(AtomicU64, AtomicU64)>> = Once::new();

/// Increments each processor makes.
const INCREMENTS: u64 = 25_000;

/// Increment the counter, from every processor at once.
fn count(me: &'static PerCpu) {
    // Everyone starts together, so the counter is contended from the first
    // increment rather than taken in turns as each processor wakes.
    let everyone = super::TOPOLOGY
        .get()
        .map_or(1, |topology| topology.online() as u64);
    let _ = AT_START.fetch_add(1, Ordering::SeqCst);
    while AT_START.load(Ordering::SeqCst) < everyone {
        core::hint::spin_loop();
    }

    let mut first = 0;
    let mut last = 0;
    for increment in 0..INCREMENTS {
        let value = {
            let mut counter = COUNTER.lock();
            *counter += 1;
            *counter
        };
        if increment == 0 {
            first = value;
        }
        last = value;

        let seen = UNLOCKED.load(Ordering::Relaxed);
        UNLOCKED.store(seen + 1, Ordering::Relaxed);
    }

    if let Some((start, end)) = SPANS.get().and_then(|spans| spans.get(me.logical)) {
        start.store(first, Ordering::Relaxed);
        end.store(last, Ordering::Relaxed);
    }
}

/// Whether two processors' shares of the count overlapped in time.
///
/// Measured in counter values rather than nanoseconds: the lock hands the
/// counter out one increment at a time, so its value *is* the order things
/// happened in, and needs no clock the processors would have to agree on.
const fn overlaps(one: (u64, u64), other: (u64, u64)) -> bool {
    one.0 < other.1 && other.0 < one.1
}

/// Stage 4's exit criterion: every processor increments one counter under one
/// lock, all at once, and the total has to come out right.
///
/// The total alone is not enough, because processors that took turns would
/// get it right too. So two more things are measured. Each processor's first
/// and last increment bound its share, and at least two shares have to
/// overlap — or the lock was never contended, and the test passed without
/// testing it. And the same count is kept beside it with no lock at all,
/// whose shortfall is reported: the updates the lock is what prevents losing.
fn contended(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    let spans = SPANS.call_once(|| {
        (0..topology.count())
            .map(|_| (AtomicU64::new(0), AtomicU64::new(0)))
            .collect()
    });

    run_everywhere(count)?;

    let processors = topology.online() as u64;
    let expected = INCREMENTS * processors;
    let total = *COUNTER.lock();
    if total != expected {
        return Err("the contended counter came out wrong: the lock let two processors in at once");
    }

    let shares: Vec<(u64, u64)> = spans
        .iter()
        .map(|(first, last)| (first.load(Ordering::Relaxed), last.load(Ordering::Relaxed)))
        .collect();
    let overlapping = shares
        .iter()
        .enumerate()
        .filter(|&(index, &share)| {
            shares
                .iter()
                .enumerate()
                .any(|(other, &theirs)| other != index && overlaps(share, theirs))
        })
        .count();
    if processors > 1 && overlapping == 0 {
        return Err("no two processors' increments overlapped, so the lock was never contended");
    }

    report.counter = total;
    report.expected = expected;
    report.overlapping = overlapping as u64;
    report.lost = expected.saturating_sub(UNLOCKED.load(Ordering::Relaxed));
    Ok(())
}

/// What a grace-period reader finds through [`PUBLISHED`].
#[derive(Debug)]
struct Payload {
    /// [`LIVE`] while published, [`POISON`] once retired.
    value: AtomicU64,
}

/// The object the readers read, replaced by the writer every round.
static PUBLISHED: AtomicPtr<Payload> = AtomicPtr::new(core::ptr::null_mut());

/// Objects the writer has retired and poisoned, by address.
///
/// Kept, not freed, until every reader has stopped: a freed object's memory
/// would be the next one's, holding [`LIVE`] again, and a reader that should
/// have seen the poison would see a perfectly good value instead.
static RETIRED: SpinLock<Vec<usize>> = SpinLock::new(Vec::new());

/// Readers that have started reading.
static READERS_READY: AtomicU64 = AtomicU64::new(0);

/// Set by the writer when it is done.
static STOP: AtomicBool = AtomicBool::new(false);

/// Read-side sections run.
static READS: AtomicU64 = AtomicU64::new(0);

/// Reads that found a poisoned object.
static POISONED_READS: AtomicU64 = AtomicU64::new(0);

/// A published object's value.
const LIVE: u64 = 0x0B1E_C700_0000_0001;
/// A retired object's value.
const POISON: u64 = 0xDEAD_DEAD_DEAD_DEAD;

/// Grace periods the writer waits for.
const GRACE_ROUNDS: u64 = 100;

/// How long a reader holds what it loaded before it reads it.
///
/// Long enough that a grace period which ended early would find readers still
/// holding the object it had just poisoned; short enough that a section is
/// still the short thing a section has to be.
const HOLD_SPINS: u32 = 256;

/// The boot processor writes, and every other processor reads.
fn publish_and_read(me: &'static PerCpu) {
    if me.logical == 0 {
        write_side();
    } else {
        read_side();
    }
}

/// Read the published object, over and over, until the writer is done.
fn read_side() {
    let _ = READERS_READY.fetch_add(1, Ordering::SeqCst);
    while !STOP.load(Ordering::Acquire) {
        super::read_section(|| {
            let payload = PUBLISHED.load(Ordering::Acquire);
            if payload.is_null() {
                return;
            }
            for _ in 0..HOLD_SPINS {
                core::hint::spin_loop();
            }
            // SAFETY: every object this check ever publishes stays allocated
            // until every reader has stopped, so `payload` points at a live
            // `Payload` whatever the grace periods do. Whether it is still the
            // *current* one is what this check measures.
            let value = unsafe { &*payload }.value.load(Ordering::Relaxed);
            if value != LIVE {
                let _ = POISONED_READS.fetch_add(1, Ordering::Relaxed);
            }
        });
        let _ = READS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Replace the published object, wait a grace period, and poison the old one,
/// a hundred times.
fn write_side() {
    // Not until the readers are reading: a grace period with nothing to wait
    // for passes whether or not it works.
    let readers = super::TOPOLOGY
        .get()
        .map_or(0, |topology| topology.online() as u64 - 1);
    while READERS_READY.load(Ordering::SeqCst) < readers {
        core::hint::spin_loop();
    }

    for _ in 0..GRACE_ROUNDS {
        let fresh = Box::into_raw(Box::new(Payload {
            value: AtomicU64::new(LIVE),
        }));
        let old = PUBLISHED.swap(fresh, Ordering::AcqRel);
        super::synchronize();
        // SAFETY: `old` was published by this check and is still allocated —
        // nothing is freed while readers run — and after the grace period no
        // reader is holding it.
        unsafe { &*old }.value.store(POISON, Ordering::Relaxed);
        RETIRED.lock().push(old as usize);
    }
    STOP.store(true, Ordering::Release);
}

/// A writer replaces an object that every other processor is reading, waits
/// a grace period each time, and poisons the object it replaced — and no
/// reader may ever find the poison.
///
/// The failure this is built to see is a grace period that ends too soon. A
/// reader holds what it loaded for a while before reading it, so a writer
/// that poisoned early would find one still holding, and that reader would
/// read the poison.
fn grace(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    let first = Box::into_raw(Box::new(Payload {
        value: AtomicU64::new(LIVE),
    }));
    PUBLISHED.store(first, Ordering::Release);
    let before = super::grace_periods();

    run_everywhere(publish_and_read)?;

    // Every reader has stopped, so nothing can hold any of these now.
    let current = PUBLISHED.swap(core::ptr::null_mut(), Ordering::AcqRel);
    let mut retired = RETIRED.lock();
    for address in retired.drain(..).chain(core::iter::once(current as usize)) {
        // SAFETY: each address came from `Box::into_raw` in this module, is
        // freed exactly once, here, and no processor is reading: the work that
        // read them has returned on every one.
        drop(unsafe { Box::from_raw(address as *mut Payload) });
    }
    drop(retired);

    if POISONED_READS.load(Ordering::Relaxed) != 0 {
        return Err("a reader found an object poisoned by a grace period that should have waited");
    }
    let reads = READS.load(Ordering::Relaxed);
    if topology.count() > 1 && reads == 0 {
        return Err("no reader read anything, so the grace periods waited for nothing");
    }
    report.grace_periods = super::grace_periods() - before;
    report.reads = reads;
    Ok(())
}

/// The page [`read_probe`] reads, while [`shootdown`] runs.
static PROBE: AtomicU64 = AtomicU64::new(0);

/// What it must find there.
static EXPECTED: AtomicU64 = AtomicU64::new(0);

/// Reads that found something else.
static STALE: AtomicU64 = AtomicU64::new(0);

/// Where the markers [`shootdown`] writes start from.
const MARKER: u64 = 0x5407_D0A1_0000_0000;

/// Read the probe page and compare it with what it should hold.
fn read_probe(_me: &'static PerCpu) {
    let at = PROBE.load(Ordering::Acquire);
    // SAFETY: `shootdown` maps a page at `at` before handing this out, and
    // moves it only once every processor has finished reading it.
    let seen = unsafe { core::ptr::read_volatile(at as *const u64) };
    if seen != EXPECTED.load(Ordering::Acquire) {
        let _ = STALE.fetch_add(1, Ordering::Relaxed);
    }
}

/// A page is moved to another frame, again and again, and every processor
/// has to see the frame it was moved to each time.
///
/// Each round has every processor read the page first, which is what puts a
/// translation for it into each TLB, and then moves it. The old frame keeps
/// its old contents and stays allocated, so a processor still translating
/// through a stale entry does not fault: it reads the old value, quietly —
/// which is what a missing shootdown looks like in a running kernel, and
/// exactly what this counts.
fn shootdown(report: &mut Report) -> Result<(), &'static str> {
    const ROUNDS: u64 = 20;

    let page =
        vmap::allocate(1, MapFlags::KERNEL_DATA).map_err(|_| "no page for the shootdown check")?;
    let mut mapped =
        mm::translate(page.base).ok_or("the shootdown check's page is not mapped")? / PAGE_SIZE;
    let mut spare = mm::allocate_frames(0).ok_or("no second frame for the shootdown check")?;
    PROBE.store(page.base, Ordering::Release);
    let shootdowns_before = super::shootdowns();

    // SAFETY: the page was allocated a moment ago, writable, and is nobody
    // else's.
    unsafe { core::ptr::write_volatile(page.base as *mut u64, MARKER) };

    for round in 0..ROUNDS {
        EXPECTED.store(MARKER + round, Ordering::Release);
        run_everywhere(read_probe)?;

        // The next round's marker goes into the spare frame, and the page is
        // moved onto it. `unmap_kernel` is told to release nothing: the old
        // frame has to stay, holding the old marker, for a stale read to find.
        let next = mm::direct_map(spare * PAGE_SIZE);
        // SAFETY: `spare` is a frame this check owns and nothing maps it; the
        // direct map makes it writable.
        unsafe { core::ptr::write_volatile(next as *mut u64, MARKER + round + 1) };
        let _removed = mm::unmap_kernel(page.base, PAGE_SIZE, |_, _| {})
            .map_err(|_| "the shootdown check's page could not be unmapped")?;
        mm::map_kernel(
            page.base,
            spare * PAGE_SIZE,
            PAGE_SIZE,
            MapFlags::KERNEL_DATA,
        )
        .map_err(|_| "the shootdown check's page could not be remapped")?;
        core::mem::swap(&mut mapped, &mut spare);
    }
    EXPECTED.store(MARKER + ROUNDS, Ordering::Release);
    run_everywhere(read_probe)?;

    // `vmap::free` gives back whichever frame is mapped now; the other one is
    // this check's to return.
    vmap::free(page.base).map_err(|_| "the shootdown check's page could not be freed")?;
    mm::deallocate_frames(spare, 0);

    if STALE.load(Ordering::Relaxed) != 0 {
        return Err(
            "a processor read a page through a translation a shootdown should have dropped",
        );
    }
    report.remaps = ROUNDS;
    report.shootdowns = super::shootdowns() - shootdowns_before;
    Ok(())
}

/// How many times each processor ran [`tally_run`], by logical number.
static RUNS: Once<Vec<AtomicU64>> = Once::new();

/// Runs of [`tally_run`] on a processor whose record named another.
static MISPLACED: AtomicU64 = AtomicU64::new(0);

/// Count this run, and check the processor running it is the one its record
/// names.
///
/// The second half is the per-CPU register tested under load: `me` came
/// through that register, and `arch::hardware_id` asks the hardware.
fn tally_run(me: &'static PerCpu) {
    if me.hardware_id != arch::hardware_id() {
        let _ = MISPLACED.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(slot) = RUNS.get().and_then(|runs| runs.get(me.logical)) {
        let _ = slot.fetch_add(1, Ordering::Relaxed);
    }
}

/// Every processor runs the work it is handed, once per round, a hundred
/// times over — and every secondary is woken for it by an interrupt.
///
/// A hundred rounds rather than one because each round is a secondary going
/// to sleep and being woken, and the way that goes wrong is a lost wake-up: an
/// interrupt that arrives between a processor deciding to sleep and sleeping.
/// That is a race, and one round would have to be lucky to lose it.
fn everywhere(topology: &Topology, report: &mut Report) -> Result<(), &'static str> {
    const ROUNDS: u64 = 100;

    let runs = RUNS.call_once(|| (0..topology.count()).map(|_| AtomicU64::new(0)).collect());
    let ipis_before: u64 = topology.cpus().iter().map(PerCpu::ipis_taken).sum();

    for _ in 0..ROUNDS {
        run_everywhere(tally_run)?;
    }

    if MISPLACED.load(Ordering::Relaxed) != 0 {
        return Err("work ran on a processor whose per-CPU record names another");
    }
    if runs
        .iter()
        .any(|count| count.load(Ordering::Relaxed) != ROUNDS)
    {
        return Err("a processor did not run its share of the work once per round");
    }
    // Not "once per round": two interrupts sent to a processor before it
    // takes the first are delivered as one, and that is correct behaviour.
    // What must hold is that each secondary takes them at all.
    if topology
        .cpus()
        .iter()
        .skip(1)
        .any(|cpu| cpu.ipis_taken() == 0)
    {
        return Err("a secondary processor never took an inter-processor interrupt");
    }

    report.rounds = ROUNDS;
    report.ipis = topology.cpus().iter().map(PerCpu::ipis_taken).sum::<u64>() - ipis_before;
    Ok(())
}
