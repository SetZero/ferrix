//! The audit store's boot check: what a reader of the record is promised
//! (`docs/certification/AUDIT.md` §6), on small stores the check owns, and
//! what bring-up recorded, on the kernel's own.
//!
//! On stores of its own, whose rings it can drive past their length
//! without touching the boot's record:
//!
//! * **start-up**: the first start writes the start-up record, first in the
//!   high-value ring and carrying the whole id and both ring lengths, and a
//!   second start changes nothing;
//! * **gapless and lost**: a ring written past its length keeps its last
//!   records numbered without a gap, and a reader asking for what was
//!   overwritten is told exactly how many it lost;
//! * **partial read**: a reader with less room takes what fits and goes on
//!   from there;
//! * **the two rings**: a refusal goes to the refusal ring and every one of
//!   the five other classes to the high-value ring, so a flood of refusals
//!   leaves every grant readable;
//! * **budgets**: on a job tree of the check's own, the jobs a unit's
//!   program can make itself -- anonymous ones, and named ones in a cgroup
//!   delegated to it -- are charged to the unit's budget, and a job root
//!   made is its own;
//! * **fairness**: a unit whose program refuses from its job and from two
//!   levels of sub-jobs it made has the limit kept between all three, not
//!   per job, and the rest counted in one *suppressed n* record once the
//!   second has ended; another unit's refusal in the same second is kept;
//!   and a budget that finds every fairness slot taken closes the oldest
//!   window, its count written out rather than lost.
//!
//! On the kernel's store, that bring-up started it with the id it drew, and
//! recorded the boot's configuration as bring-up read it.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    BOOTED, CONFIG, Class, Config, Event, NO_UID, Outcome, PER_BUDGET_PER_SECOND, Read, Record,
    SECOND, SUPPRESSED, Store, Subject, Target, Which,
};
use crate::object::job::{Job, NodeAttributes};

/// A small store: four high-value records, a hundred and fifty refusals --
/// room for more than the fairness limit, so the limit and not the ring is
/// what stops a flood.
type Small = Store<4, 150>;

static SMALL: Small = Store::new();
static FAIR: Small = Store::new();
static CROWDED: Small = Store::new();
static ROUTED: Store<8, 8> = Store::new();

/// The id the check's stores start with.
const CHECK_ID: u128 = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;

/// A grant, for the high-value ring.
const GRANT: Event = Event::new(Class::Granted, 1);

/// A refusal, for the refusal ring.
const REFUSAL: Event = Event::new(Class::Refused, 2);

/// What bring-up says it recorded, for the check of the kernel's store.
#[derive(Debug)]
pub(crate) struct Expected {
    /// `ferrix.checks`: 1 when the checks run.
    pub(crate) checks: u32,
    /// `ferrix.devmgr`: 1 when pid 1 starts `devmgr`.
    pub(crate) devmgr: u32,
    /// The build's mitigations: 1 when hardened.
    pub(crate) mitigations: u32,
    /// The loader's KASLR state word.
    pub(crate) kaslr: u32,
}

/// What the check established, for the boot log.
#[derive(Debug)]
pub(crate) struct Report {
    /// Records the small ring kept after being written past its length.
    pub(crate) kept: usize,
    /// Records a reader of the start was told it lost.
    pub(crate) lost: u64,
    /// Jobs whose budget the check followed up the tree.
    pub(crate) budgets: usize,
    /// Refusals of the flooding unit kept, from its three jobs together.
    pub(crate) flood_kept: usize,
    /// Refusals of the flooding unit counted in its *suppressed n*.
    pub(crate) suppressed: u32,
    /// Configuration records found in the kernel's store.
    pub(crate) configs: usize,
}

/// The jobs the budget and fairness checks refuse from.
struct Tree {
    /// The tree's root, which holds the rest.
    _root: Arc<Job>,
    /// A unit's cgroup, made in a directory only root may write.
    unit: Arc<Job>,
    /// A job the unit's program made, and one it made inside that.
    made: Arc<Job>,
    made_inside: Arc<Job>,
    /// A second unit, its directory delegated to uid 1000, and a cgroup its
    /// program made in it.
    delegated: Arc<Job>,
    delegated_child: Arc<Job>,
    /// A cgroup made inside the first unit's directory, which only root may
    /// write: root made it, so it is a budget of its own.
    rooted: Arc<Job>,
}

/// A job's refusal at `now`, charged to its budget.
fn refuse<const H: usize, const R: usize>(store: &Store<H, R>, now: u64, job: &Job) {
    store.record_at(
        now,
        REFUSAL,
        Outcome::Refused,
        -1,
        Subject {
            pid: 7,
            uid: NO_UID,
            job: job.id(),
            budget: super::budget_of(job),
        },
        Target {
            kind: 3,
            id: job.id(),
        },
        [1, 0, 0],
    );
}

/// A refusal at `now` charged to `budget` directly, for the slots check.
fn refuse_as(store: &Small, now: u64, budget: u64) {
    store.record_at(
        now,
        REFUSAL,
        Outcome::Refused,
        -1,
        Subject {
            pid: 7,
            uid: NO_UID,
            job: budget,
            budget,
        },
        Target::NONE,
        [1, 0, 0],
    );
}

/// A grant at `now`, numbered `n` in its detail.
fn grant(store: &Small, now: u64, n: u32) {
    store.record_at(
        now,
        GRANT,
        Outcome::Done,
        0,
        Subject::KERNEL,
        Target::NONE,
        [n, 0, 0],
    );
}

/// Read ring `which` of `store` from `from` into a buffer of `room` records,
/// and answer what was copied.
fn read<const H: usize, const R: usize>(
    store: &Store<H, R>,
    now: u64,
    which: Which,
    from: u64,
    room: usize,
) -> (Read, Vec<Record>) {
    let mut out = vec![Record::EMPTY; room];
    let read = store.read_at(now, which, from, &mut out);
    out.truncate(read.copied);
    (read, out)
}

/// Run the check. `expected` is what bring-up recorded in the kernel's
/// store.
///
/// # Errors
///
/// The first property that did not hold.
pub(crate) fn run(expected: &Expected) -> Result<Report, &'static str> {
    let (kept, lost) = start_and_wrap()?;
    routing()?;
    let tree = tree()?;
    let budgets = budgets(&tree)?;
    let (flood_kept, suppressed) = fairness(&tree)?;
    crowded()?;
    let configs = kernel_store(expected)?;
    Ok(Report {
        kept,
        lost,
        budgets,
        flood_kept,
        suppressed,
        configs,
    })
}

/// Start-up, the gapless numbering, loss and a partial read, on [`SMALL`].
fn start_and_wrap() -> Result<(usize, u64), &'static str> {
    if !SMALL.start(10, CHECK_ID) {
        return Err("a store's first start was refused");
    }
    if SMALL.start(11, CHECK_ID ^ 1) {
        return Err("a store started twice");
    }
    if SMALL.id() != CHECK_ID {
        return Err("a second start changed the store's audit id");
    }
    let (first, out) = read(&SMALL, 12, Which::High, 0, 4);
    let start = out.first().copied().unwrap_or(Record::EMPTY);
    if first.copied != 1 || start.sequence != 0 {
        return Err("the start-up record is not the first of the high-value ring");
    }
    if start.start_fields() != Some((CHECK_ID, 4, 150)) {
        return Err("the start-up record does not carry the whole audit id and both lengths");
    }
    // Six grants after the start: seven numbered 0 to 6 in a ring of four,
    // so 3 to 6 are kept and a reader from 0 lost three.
    for n in 1..=6 {
        grant(&SMALL, 20 + u64::from(n), n);
    }
    let (read_all, out) = read(&SMALL, 30, Which::High, 0, 8);
    if read_all.lost != 3 || read_all.copied != 4 || read_all.next != 7 {
        return Err("a reader behind a wrapped ring was not moved on and told what it lost");
    }
    let numbered = out
        .iter()
        .zip(3_u64..)
        .all(|(record, n)| record.sequence == n && u64::from(record.detail[0]) == n);
    if !numbered {
        return Err("a wrapped ring's records are not the last four, numbered without a gap");
    }
    if read_all.id != CHECK_ID {
        return Err("a read did not answer the store's audit id");
    }
    let (part, _) = read(&SMALL, 31, Which::High, 3, 2);
    let (rest, out) = read(&SMALL, 32, Which::High, part.next, 8);
    let resumed = out.first().is_some_and(|record| record.sequence == 5);
    if part.copied != 2 || part.next != 5 || rest.copied != 2 || rest.lost != 0 || !resumed {
        return Err("a partial read did not resume where it stopped");
    }
    let (refusals, _) = read(&SMALL, 33, Which::Refusals, 0, 8);
    if refusals.copied != 0 {
        return Err("a grant was kept in the refusal ring");
    }
    Ok((read_all.copied, read_all.lost))
}

/// One event of every class, on [`ROUTED`]: each refusal in the refusal
/// ring, each of the other five in the high-value ring, in the order made.
fn routing() -> Result<(), &'static str> {
    let classes = [
        Class::Refused,
        Class::Granted,
        Class::Ended,
        Class::Device,
        Class::Changed,
        Class::System,
    ];
    for (at, class) in (1_u64..).zip(classes) {
        ROUTED.record_at(
            at,
            Event::new(class, 9),
            Outcome::Done,
            0,
            Subject::KERNEL,
            Target::NONE,
            [0; 3],
        );
    }
    let (high, kept_high) = read(&ROUTED, 10, Which::High, 0, 8);
    let (refused, kept_refused) = read(&ROUTED, 10, Which::Refusals, 0, 8);
    let in_order = kept_high
        .iter()
        .map(|record| record.class)
        .eq(classes.iter().skip(1).map(|&class| class as u16));
    let refusal = kept_refused
        .first()
        .is_some_and(|record| record.class == Class::Refused as u16);
    if high.copied != 5 || !in_order || refused.copied != 1 || !refusal {
        return Err("a class was kept in the wrong ring");
    }
    Ok(())
}

/// A job tree of the check's own: a root, a unit in it, a job the unit's
/// program made and one inside that; a second unit whose directory is
/// delegated to uid 1000, and a cgroup made in it; and a cgroup root made
/// inside the first unit.
fn tree() -> Result<Tree, &'static str> {
    let failed = "the check's job tree could not be made";
    let root = Job::new_root().map_err(|_| failed)?;
    let unit = root.new_named_child("unit.service").map_err(|_| failed)?;
    let made = unit.new_child().map_err(|_| failed)?;
    let made_inside = made.new_child().map_err(|_| failed)?;
    let delegated = root
        .new_named_child("delegated.service")
        .map_err(|_| failed)?;
    let chowned = NodeAttributes {
        uid: 1000,
        gid: 1000,
        permissions: 0o755,
    };
    delegated.set_node(0, chowned).map_err(|_| failed)?;
    let delegated_child = delegated.new_named_child("worker").map_err(|_| failed)?;
    let rooted = unit.new_named_child("root-made").map_err(|_| failed)?;
    Ok(Tree {
        _root: root,
        unit,
        made,
        made_inside,
        delegated,
        delegated_child,
        rooted,
    })
}

/// Each job's budget is what [`super::budget_of`] promises.
fn budgets(tree: &Tree) -> Result<usize, &'static str> {
    let wants = [
        (&tree.unit, &tree.unit),
        (&tree.made, &tree.unit),
        (&tree.made_inside, &tree.unit),
        (&tree.delegated, &tree.delegated),
        (&tree.delegated_child, &tree.delegated),
        (&tree.rooted, &tree.rooted),
    ];
    for (job, budget) in wants {
        if super::budget_of(job) != budget.id() {
            return Err(
                "a job's refusals are not charged to the budget its maker's authority sets",
            );
        }
    }
    Ok(wants.len())
}

/// The unit's program floods from its job and its two sub-jobs, the
/// delegated unit's worker refuses once, and a grant is made before the
/// flood, on [`FAIR`]: the unit keeps the limit between its three jobs and
/// the rest is counted.
fn fairness(tree: &Tree) -> Result<(usize, u32), &'static str> {
    let _ = FAIR.start(0, CHECK_ID);
    grant(&FAIR, 1, 1);
    let flooders = [&tree.unit, &tree.made, &tree.made_inside];
    let flood = PER_BUDGET_PER_SECOND + 40;
    for (n, job) in (0..flood).zip(flooders.iter().cycle()) {
        refuse(&FAIR, 100 + u64::from(n), job);
    }
    refuse(&FAIR, 500, &tree.delegated_child);
    let (early, out) = read(&FAIR, 600, Which::Refusals, 0, 150);
    let of_unit = out
        .iter()
        .filter(|record| flooders.iter().any(|job| job.id() == record.job))
        .count();
    if of_unit != PER_BUDGET_PER_SECOND as usize {
        return Err(
            "a unit's refusals past the limit in one second were kept, across its sub-jobs",
        );
    }
    if !out.iter().any(|record| record.job == tree.made_inside.id()) {
        return Err("a sub-job's refusals were not kept within its unit's budget");
    }
    if !out
        .iter()
        .any(|record| record.job == tree.delegated_child.id())
    {
        return Err("another unit's refusal was not kept beside a flood");
    }
    if out.iter().any(|record| record.is(SUPPRESSED)) {
        return Err("a budget's suppressed count was written before its second had ended");
    }
    // Its second over, a read closes the window and finds the count.
    let (_, out) = read(&FAIR, 100 + SECOND, Which::Refusals, early.next, 8);
    let Some(folded) = out.iter().find(|record| record.is(SUPPRESSED)) else {
        return Err("a flooding unit's suppressed count was never written");
    };
    if folded.job != tree.unit.id() || folded.detail[0] != flood - PER_BUDGET_PER_SECOND {
        return Err("a suppressed record does not say whose budget, or how many");
    }
    let (grants, out) = read(&FAIR, 100 + SECOND, Which::High, 0, 4);
    if !out.iter().any(|record| record.is(GRANT)) || grants.lost != 0 {
        return Err("a flood of refusals reached the high-value ring");
    }
    Ok((of_unit, folded.detail[0]))
}

/// More budgets than there are fairness slots, on [`CROWDED`]: the one that
/// finds every slot taken closes the window begun first, and that budget's
/// count is written, not dropped.
fn crowded() -> Result<(), &'static str> {
    let _ = CROWDED.start(0, CHECK_ID);
    // Budget 1 goes over by one, then 32 more budgets take a slot each; the
    // last finds every slot taken and closes budget 1's window, begun first.
    for _ in 0..=PER_BUDGET_PER_SECOND {
        refuse_as(&CROWDED, 10, 1);
    }
    for budget in 2..=(super::FAIR_SLOTS as u64 + 1) {
        refuse_as(&CROWDED, 10 + budget, budget);
    }
    let (_, out) = read(&CROWDED, 100, Which::Refusals, 0, 150);
    let folded = out
        .iter()
        .any(|record| record.is(SUPPRESSED) && record.job == 1 && record.detail[0] == 1);
    if !folded {
        return Err("a window closed to make room lost its suppressed count");
    }
    Ok(())
}

/// The kernel's store: started, with its id, and the configuration bring-up
/// read.
fn kernel_store(expected: &Expected) -> Result<usize, &'static str> {
    let mut out = vec![Record::EMPTY; 16];
    let read = super::read(Which::High, 0, &mut out);
    out.truncate(read.copied);
    if read.id == 0 {
        return Err("the kernel's audit store was not started, or its id is zero");
    }
    let start = out.first().copied().unwrap_or(Record::EMPTY);
    let lengths = (super::HIGH_RECORDS, super::REFUSAL_RECORDS);
    match start.start_fields() {
        Some((id, high, refusals)) if id == read.id && (high, refusals) == lengths => {}
        _ => return Err("the kernel's store does not begin with a start-up record of its id"),
    }
    let config = |key: Config| {
        out.iter()
            .find(|record| record.is(CONFIG) && record.detail[0] == key as u32)
    };
    let wants = [
        (Config::Checks, expected.checks),
        (Config::Devmgr, expected.devmgr),
        (Config::Mitigations, expected.mitigations),
        (Config::Kaslr, expected.kaslr),
    ];
    for (key, value) in wants {
        match config(key) {
            Some(record) if record.detail[1] == value => {}
            _ => return Err("the boot's configuration is not recorded as bring-up read it"),
        }
    }
    if out.iter().any(|record| record.is(BOOTED)) {
        return Err("the boot was recorded as brought up before it was");
    }
    Ok(wants.len())
}
