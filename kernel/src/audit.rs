//! The audit record: what the TSF decided, kept where only it writes
//! (finding F-21b, `docs/certification/AUDIT.md`).
//!
//! # What is kept
//!
//! One fixed 64-byte [`Record`] per event the kernel decided: a capability
//! refused, authority handed over, a process ended from outside, a device
//! quiesced, TSF data changed, and the boot's own configuration. Each is
//! recorded at the one place that decision is made, so recording it adds a
//! call and changes nothing about the decision. The layout is
//! `libs/proto/audit`'s, which a reader shares.
//!
//! # Where
//!
//! In two rings of static storage, so that nothing on a recording path
//! allocates (finding F-23) and a record can be taken before the heap is up:
//!
//! * the **high-value ring**, [`HIGH_RECORDS`] long, for every class but
//!   refusals: they are rare, and a flood of refusals can never evict one;
//! * the **refusal ring**, [`REFUSAL_RECORDS`] long, with fairness: past
//!   [`PER_BUDGET_PER_SECOND`] refusals charged to one budget in a second,
//!   that budget's further refusals are counted, not kept, and one
//!   *suppressed n* record says how many once its second has ended.
//!
//! # Whose budget
//!
//! A refusal is charged to its job's audit budget, [`Subject::budget`],
//! which the caller takes from [`budget_of`]: the job at or above the
//! refusing one that its members' own authority could not have made. A
//! program that makes sub-jobs to refuse from -- anonymous ones, or named
//! ones in a cgroup delegated to it -- charges them all to the one budget,
//! so it writes at most [`PER_BUDGET_PER_SECOND`] refusals a second however
//! many jobs it makes, and cannot push another unit's refusals out faster
//! than that.
//!
//! # Numbering
//!
//! Each ring numbers its records from zero, per boot, without gaps, so a
//! reader who finds a number missing knows a record was lost, and is told
//! how many. A full ring overwrites its oldest record: refusing new ones
//! would let an attacker fill it first and then act unrecorded, and
//! stopping the TSF would make audit a lever for denial of service
//! (`AUDIT.md` §3).
//!
//! Each ring's lock is an [`IrqSpinLock`]: an IOMMU fault is recorded from
//! its interrupt handler, and an OOM kill can be decided with the
//! allocator's locks held, so a lock that left interrupts on could be taken
//! by a handler on the processor already holding it. Under it is a 64-byte
//! copy and two counters; nothing blocks or allocates.
//!
//! # The boot's audit id
//!
//! A random 128-bit number drawn at start-up ([`start`]) and carried in the
//! start-up record and in every read, so that the records of two boots can
//! never be spliced into one sequence. The kernel's random generator is the
//! item's, above this module, so bring-up draws it and hands it in.

pub(crate) mod check;

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(crate) use ferrix_audit::{
    BOOTED, CONFIG, Class, Config, Event, NO_UID, Outcome, Record, SUPPRESSED,
};
use ferrix_sync::IrqSpinLock;

use crate::object::job::Job;

/// The high-value ring's length: 32 KiB of records.
pub(crate) const HIGH_RECORDS: usize = 512;

/// The refusal ring's length: 256 KiB of records.
pub(crate) const REFUSAL_RECORDS: usize = 4096;

/// How many refusals charged to one budget a second are kept before the
/// rest are folded into a *suppressed n* record.
pub(crate) const PER_BUDGET_PER_SECOND: u32 = 64;

/// How many budgets' fairness windows are tracked at once. A budget that
/// finds every slot taken takes the one whose window began first, whose
/// count is written out before it goes, so eviction never loses a count.
const FAIR_SLOTS: usize = 32;

/// A fairness window's length, in nanoseconds.
const SECOND: u64 = 1_000_000_000;

/// Who a record is about: the process and its job, which the TSF attests,
/// and beside them the uid the personality says, which it does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Subject {
    /// The process, 0 for the kernel itself.
    pub(crate) pid: u32,
    /// The uid the personality gave, or [`NO_UID`]: personality-supplied
    /// data, never the TSF's identity of the subject (`AUDIT.md` §2).
    pub(crate) uid: u32,
    /// The process's job, 0 for the kernel itself.
    pub(crate) job: u64,
    /// The job its refusals are charged to ([`budget_of`]). Not recorded:
    /// it keys the refusal ring's fairness.
    pub(crate) budget: u64,
}

impl Subject {
    /// The kernel, deciding for itself.
    pub(crate) const KERNEL: Subject = Subject {
        pid: 0,
        uid: NO_UID,
        job: 0,
        budget: 0,
    };
}

/// The id of the job a refusal of `job`'s members is charged to in the
/// refusal ring's fairness (`docs/certification/AUDIT.md` §3): `job` or the
/// nearest job above it that its members' own authority could not have
/// made.
///
/// Two kinds of job are theirs to make, and count as the job above them: an
/// anonymous one, which anyone holding a job handle that allows it makes
/// with `job_create`, and a named one in a directory someone other than
/// root may write -- a delegated cgroup's, where the delegatee makes its own
/// with `mkdir`. The walk stops at a named job made in a directory only root
/// may write, which is a unit's own cgroup that init made, or at the tree's
/// root. So a program that makes sub-jobs to refuse from shares its unit's
/// one budget however many it makes.
pub(crate) fn budget_of(job: &Job) -> u64 {
    let mut at = job;
    while let Some(parent) = at.parent() {
        if at.name().is_some() && !others_may_make_in(parent) {
            break;
        }
        at = parent;
    }
    at.id()
}

/// Whether someone other than root may make a named job in `job`'s cgroupfs
/// directory: the directory `chown`ed away from root, or its mode letting its
/// group or everyone write.
fn others_may_make_in(job: &Job) -> bool {
    job.node(0)
        .is_some_and(|directory| directory.uid != 0 || directory.permissions & 0o022 != 0)
}

/// What a record's decision was about: an object's kind and identity, a
/// device's location, a resource. Zero where nothing is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// What kind of thing, numbered by the event.
    pub(crate) kind: u32,
    /// Which one.
    pub(crate) id: u64,
}

impl Target {
    /// Nothing in particular.
    pub(crate) const NONE: Target = Target { kind: 0, id: 0 };
}

/// The record of `event`, numbered later by the ring that keeps it.
fn record_of(
    time: u64,
    event: Event,
    outcome: Outcome,
    status: i16,
    subject: Subject,
    target: Target,
    detail: [u32; 3],
) -> Record {
    let mut record = Record {
        time,
        class: event.class as u16,
        code: event.code,
        outcome: outcome as u16,
        status,
        pid: subject.pid,
        uid: subject.uid,
        job: subject.job,
        target_kind: target.kind,
        detail,
        ..Record::EMPTY
    };
    record.set_target(target.id);
    record
}

/// Which of the two rings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    /// Everything but refusals.
    High,
    /// Refusals, and the *suppressed n* records that count those not kept.
    Refusals,
}

/// What one read took out of a ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    /// Records copied to the front of the reader's buffer.
    pub(crate) copied: usize,
    /// The sequence number to read from next.
    pub(crate) next: u64,
    /// Records between the number asked for and the first one copied that
    /// the ring no longer held.
    pub(crate) lost: u64,
    /// The boot's audit id.
    pub(crate) id: u128,
}

/// A ring of `N` records, numbered from zero.
struct Ring<const N: usize> {
    /// The record numbered `n`, in slot `n % N` until `n + N` replaces it.
    records: [Record; N],
    /// How many records have ever been kept: the next one's number.
    next: u64,
}

impl<const N: usize> Ring<N> {
    /// Its length, as a count.
    const LENGTH: u64 = {
        assert!(N > 0, "an audit ring holds at least one record");
        N as u64
    };

    /// An empty ring.
    const fn new() -> Self {
        Ring {
            records: [Record::EMPTY; N],
            next: 0,
        }
    }

    /// Keep `record` as the next number, over the oldest when full.
    fn put(&mut self, mut record: Record) {
        record.sequence = self.next;
        let slot = (self.next % Self::LENGTH) as usize;
        if let Some(kept) = self.records.get_mut(slot) {
            *kept = record;
        }
        self.next = self.next.wrapping_add(1);
    }

    /// Copy records from number `from` on into `out`, as many as fit and
    /// are there; one overwritten before the reader came for it is counted
    /// as lost and skipped. Answers how many were copied, the number to
    /// read from next, and how many were lost.
    fn read(&self, from: u64, out: &mut [Record]) -> (usize, u64, u64) {
        let oldest = self.next.saturating_sub(Self::LENGTH);
        let start = from.max(oldest);
        let lost = start.saturating_sub(from);
        let mut at = start;
        let mut copied = 0;
        for slot in out.iter_mut() {
            if at >= self.next {
                break;
            }
            if let Some(record) = self.records.get((at % Self::LENGTH) as usize) {
                *slot = *record;
            }
            at += 1;
            copied += 1;
        }
        (copied, at, lost)
    }
}

/// One budget's fairness window: how many refusals it has had kept in the
/// second that began at `began`, and how many it has had counted instead.
#[derive(Clone, Copy)]
struct Window {
    /// Whether the slot is in use.
    used: bool,
    /// The budget, a job's id.
    budget: u64,
    /// When the window began.
    began: u64,
    /// Refusals kept in it.
    kept: u32,
    /// Refusals past the limit, counted and not kept.
    suppressed: u32,
}

impl Window {
    /// A slot in no use.
    const FREE: Window = Window {
        used: false,
        budget: 0,
        began: 0,
        kept: 0,
        suppressed: 0,
    };
}

/// The refusal ring and the fairness windows it is kept by, under one lock.
struct Refusals<const N: usize> {
    ring: Ring<N>,
    windows: [Window; FAIR_SLOTS],
}

impl<const N: usize> Refusals<N> {
    /// None kept, no window open.
    const fn new() -> Self {
        Refusals {
            ring: Ring::new(),
            windows: [Window::FREE; FAIR_SLOTS],
        }
    }

    /// Keep `record`, a refusal charged to `budget`, if the budget's window
    /// has room; count it otherwise.
    fn keep(&mut self, now: u64, budget: u64, record: Record) {
        let at = self.window_of(now, budget);
        let Some(window) = self.windows.get_mut(at) else {
            return;
        };
        if window.kept < PER_BUDGET_PER_SECOND {
            window.kept += 1;
            self.ring.put(record);
        } else {
            window.suppressed = window.suppressed.saturating_add(1);
        }
    }

    /// The slot of `budget`'s current window: its own, opened afresh if its
    /// second has ended, or a free one, or the one begun first -- each
    /// closed first, so that what it counted is written out.
    fn window_of(&mut self, now: u64, budget: u64) -> usize {
        let own = self
            .windows
            .iter()
            .position(|window| window.used && window.budget == budget);
        let at = own.unwrap_or_else(|| {
            self.windows
                .iter()
                .position(|window| !window.used)
                .unwrap_or_else(|| self.first_begun())
        });
        let fresh = self.windows.get(at).is_none_or(|window| {
            !window.used || window.budget != budget || now.saturating_sub(window.began) >= SECOND
        });
        if fresh {
            self.close(now, at);
            if let Some(window) = self.windows.get_mut(at) {
                *window = Window {
                    used: true,
                    budget,
                    began: now,
                    kept: 0,
                    suppressed: 0,
                };
            }
        }
        at
    }

    /// The slot whose window began first.
    fn first_begun(&self) -> usize {
        let mut first = 0;
        for (at, window) in self.windows.iter().enumerate() {
            if self
                .windows
                .get(first)
                .is_some_and(|f| window.began < f.began)
            {
                first = at;
            }
        }
        first
    }

    /// Close the window in slot `at`: write its *suppressed n* record if it
    /// counted any, and free the slot.
    fn close(&mut self, now: u64, at: usize) {
        let Some(window) = self.windows.get_mut(at) else {
            return;
        };
        let closed = *window;
        *window = Window::FREE;
        if closed.used && closed.suppressed > 0 {
            let subject = Subject {
                pid: 0,
                uid: NO_UID,
                job: closed.budget,
                budget: closed.budget,
            };
            self.ring.put(record_of(
                now,
                SUPPRESSED,
                Outcome::Refused,
                0,
                subject,
                Target::NONE,
                [closed.suppressed, PER_BUDGET_PER_SECOND, 0],
            ));
        }
    }

    /// Close every window whose second has ended, so a reader sees its
    /// count without waiting for the budget's next refusal.
    fn close_ended(&mut self, now: u64) {
        for at in 0..FAIR_SLOTS {
            let ended = self
                .windows
                .get(at)
                .is_some_and(|window| window.used && now.saturating_sub(window.began) >= SECOND);
            if ended {
                self.close(now, at);
            }
        }
    }
}

/// An audit store: the two rings and the boot's id. The kernel's is
/// [`STORE`]; the boot check drives small ones of its own.
pub(crate) struct Store<const H: usize, const R: usize> {
    high: IrqSpinLock<Ring<H>, crate::arch::Irq>,
    refusals: IrqSpinLock<Refusals<R>, crate::arch::Irq>,
    /// The audit id's two halves, set once by [`Store::start`].
    id: [AtomicU64; 2],
    started: AtomicBool,
}

impl<const H: usize, const R: usize> Store<H, R> {
    /// The start-up record, before it is numbered and stamped: its lengths
    /// checked when the store's type is made, so the record can carry them.
    const START_FIELDS: () = assert!(
        H <= ferrix_audit::MAX_RING && R <= ferrix_audit::MAX_RING,
        "each ring's length fits the half word the start-up record gives it"
    );

    /// Empty, and not started.
    pub(crate) const fn new() -> Self {
        let () = Self::START_FIELDS;
        Store {
            high: IrqSpinLock::new(Ring::new()),
            refusals: IrqSpinLock::new(Refusals::new()),
            id: [AtomicU64::new(0), AtomicU64::new(0)],
            started: AtomicBool::new(false),
        }
    }

    /// Take `id` as the boot's audit id and write the start-up record, the
    /// first of the high-value ring when nothing was recorded before it: it
    /// carries the whole id and both ring lengths (`Record::start`). Only
    /// the first call counts.
    pub(crate) fn start(&self, now: u64, id: u128) -> bool {
        if self.started.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.id[0].store(id as u64, Ordering::Release);
        self.id[1].store((id >> 64) as u64, Ordering::Release);
        if let Some(mut record) = Record::start(id, H, R) {
            record.time = now;
            self.high.lock().put(record);
        }
        true
    }

    /// The boot's audit id, 0 before [`Store::start`].
    pub(crate) fn id(&self) -> u128 {
        u128::from(self.id[0].load(Ordering::Acquire))
            | (u128::from(self.id[1].load(Ordering::Acquire)) << 64)
    }

    /// Record `event` at `now`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the record's parts and its time, which the check sets and `record` reads"
    )]
    pub(crate) fn record_at(
        &self,
        now: u64,
        event: Event,
        outcome: Outcome,
        status: i16,
        subject: Subject,
        target: Target,
        detail: [u32; 3],
    ) {
        let record = record_of(now, event, outcome, status, subject, target, detail);
        if event.class == Class::Refused {
            self.refusals.lock().keep(now, subject.budget, record);
        } else {
            self.high.lock().put(record);
        }
    }

    /// Copy records of ring `which`, from number `from` on, into `out`.
    /// Windows of the refusal ring whose second has ended are closed first,
    /// so their counts are there to read.
    pub(crate) fn read_at(&self, now: u64, which: Which, from: u64, out: &mut [Record]) -> Read {
        let (copied, next, lost) = match which {
            Which::High => self.high.lock().read(from, out),
            Which::Refusals => {
                let mut refusals = self.refusals.lock();
                refusals.close_ended(now);
                refusals.ring.read(from, out)
            }
        };
        Read {
            copied,
            next,
            lost,
            id: self.id(),
        }
    }
}

/// The kernel's audit store: 288 KiB of static storage on every
/// architecture.
pub(crate) static STORE: Store<HIGH_RECORDS, REFUSAL_RECORDS> = Store::new();

/// Start the kernel's audit function with the boot's audit `id`, drawn by
/// bring-up from the random generator. False if it had already started.
pub(crate) fn start(id: u128) -> bool {
    STORE.start(crate::timer::now_nanos(), id)
}

/// Record `event` in the kernel's store, now.
pub(crate) fn record(
    event: Event,
    outcome: Outcome,
    status: i16,
    subject: Subject,
    target: Target,
    detail: [u32; 3],
) {
    STORE.record_at(
        crate::timer::now_nanos(),
        event,
        outcome,
        status,
        subject,
        target,
        detail,
    );
}

/// Record one item of the boot's configuration.
pub(crate) fn config(key: Config, value: u32, more: u32) {
    record(
        CONFIG,
        Outcome::Done,
        0,
        Subject::KERNEL,
        Target::NONE,
        [key as u32, value, more],
    );
}

/// Read the kernel's store: see [`Store::read_at`].
pub(crate) fn read(which: Which, from: u64, out: &mut [Record]) -> Read {
    STORE.read_at(crate::timer::now_nanos(), which, from, out)
}
