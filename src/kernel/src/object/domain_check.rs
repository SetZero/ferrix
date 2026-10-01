//! Speculation domains: which switches skip the predictor barrier
//! (`docs/OPAQUE-KERNEL.md` §9.2, §9.3, §9.3a).
//!
//! The rule is decided where the barrier is, in `arch::speculation`'s
//! `entered_space`, so the check drives that path itself: it installs the
//! address spaces of processes in and out of domains on this processor, with
//! interrupts masked, and counts the switches the processor decided needed
//! the barrier (`arch::barrier_decisions_on`). The count is of decisions,
//! not of barriers issued, because a processor the reference configuration
//! runs without one -- QEMU's Cortex-A72 -- issues none, and the rule must
//! still be seen to hold there.
//!
//! The cases, each a switch from one space to another and back:
//! 1. two processes born in one marked job: no barrier;
//! 2. one of them and a process of an unmarked job, and one of them and a
//!    process of another marked job: one each way;
//! 3. a member moved out of the job, with one left in it: one each way;
//! 4. `job_create` marking without MANAGE on the parent, or with an unknown
//!    option bit: refused, and no `DOMAIN` record; marking with it: one;
//! 5. a member that lost dumpability by a change of credentials, and one by
//!    `PR_SET_DUMPABLE`'s path, with a member: one each way;
//! 6. a member whose space a process of another domain shares: one each way;
//! 7. a marked job made inside a marked job: a domain of its own, so a switch
//!    between the two jobs' members is one each way.

use alloc::sync::Arc;

use ferrix_native_abi::nr;
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::status;
use ferrix_native_abi::types::JOB_SPECULATION_DOMAIN;
use ferrix_sync::IrqControl;

use super::check::Side;
use super::job::{self, Job};
use super::{Object, process::Host as _};
use crate::arch;
use crate::audit;
use crate::syscall::attributes;
use crate::syscall::process::{self, Process};
use crate::user::space::AddressSpace;

/// What the check found, for the boot line.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Switches between members of one domain, none of which needed the
    /// barrier.
    pub(crate) skipped: u64,
    /// Switches out of a domain, each of which did.
    pub(crate) kept: u64,
    /// Whether the processor issues a barrier at all: the count above is of
    /// decisions either way.
    pub(crate) hardened: bool,
}

/// Run every case.
///
/// Verifies: H.TRAP.16, L.object.106, L.object.107, L.object.108, L.object.109,
/// L.x86_64.124, L.aarch64.51
pub(crate) fn run() -> Result<Report, &'static str> {
    let mut report = Report {
        hardened: arch::HARDENED,
        ..Report::default()
    };
    // A build without the defences decides nothing at a switch.
    if !arch::HARDENED {
        return Ok(report);
    }
    let tree = Job::new_root().map_err(|_| "no memory for the domain check's jobs")?;
    let marked = tree
        .new_child_domain()
        .map_err(|_| "a job refused a marked child")?;
    let other = tree
        .new_child_domain()
        .map_err(|_| "a job refused a second marked child")?;
    let plain = tree.new_child().map_err(|_| "a job refused a child")?;
    if marked.domain() == 0 || other.domain() == 0 || marked.domain() == other.domain() {
        return Err("two marked jobs are not two speculation domains");
    }
    if plain.domain() != 0 {
        return Err("a job made unmarked is a speculation domain");
    }

    let first = born_in(&marked)?;
    let second = born_in(&marked)?;
    let stranger = born_in(&plain)?;
    let neighbour = born_in(&other)?;
    let mut made = alloc::vec![
        Arc::clone(&first),
        Arc::clone(&second),
        Arc::clone(&stranger),
        Arc::clone(&neighbour)
    ];

    // 1. Inside one domain.
    expect(
        &first,
        &second,
        0,
        "case 1: a switch between two members of one domain issued the barrier",
        &mut report,
    )?;
    // 2. Out of it, to no domain and to another.
    expect(
        &first,
        &stranger,
        2,
        "case 2: a switch between a member and a process in no domain skipped the barrier",
        &mut report,
    )?;
    expect(
        &first,
        &neighbour,
        2,
        "case 2: a switch between members of two domains skipped the barrier",
        &mut report,
    )?;

    // 3. A member that moves out leaves, wherever it went.
    let mover = born_in(&marked)?;
    made.push(Arc::clone(&mover));
    expect(
        &first,
        &mover,
        0,
        "case 3: a switch between two members of one domain issued the barrier",
        &mut report,
    )?;
    mover
        .core()
        .move_to(&marked)
        .map_err(|_| "a member could not stay in its job")?;
    mover
        .core()
        .move_to(&plain)
        .map_err(|_| "a member could not move out of its job")?;
    expect(
        &first,
        &mover,
        2,
        "case 3: a member that moved out of its domain still skipped the barrier",
        &mut report,
    )?;

    // 4. Marking: MANAGE on the parent, and no other option.
    check_marking(&tree)?;

    check_leavers(&first, &marked, &mut made, &mut report)?;

    for process in made {
        process.kill(job::KILLED_STATUS);
    }
    Ok(report)
}

/// Cases 5 to 7: members that rose in privilege or share their space with
/// another domain are out of it, and a marked job inside a marked job is a
/// domain of its own. Each switched with `first`, a member of `marked`.
fn check_leavers(
    first: &Arc<Process>,
    marked: &Arc<Job>,
    made: &mut alloc::vec::Vec<Arc<Process>>,
    report: &mut Report,
) -> Result<(), &'static str> {
    // 5. A rise in privilege leaves the domain (A1).
    let promoted = born_in(marked)?;
    let hidden = born_in(marked)?;
    made.push(Arc::clone(&promoted));
    made.push(Arc::clone(&hidden));
    attributes::credentials_changed(&promoted);
    attributes::update(&hidden, |held| held.dumpable = false);
    expect(
        first,
        &promoted,
        2,
        "case 5: a member whose credentials changed still skipped the barrier",
        report,
    )?;
    expect(
        first,
        &hidden,
        2,
        "case 5: a member that is no longer dumpable still skipped the barrier",
        report,
    )?;

    // 6. A space shared with a process of another domain is in neither.
    let sharer = born_in(marked)?;
    made.push(Arc::clone(&sharer));
    let guest = Process::new(Arc::clone(sharer.core().space()))
        .map_err(|_| "no memory for a process sharing a space")?;
    let guest = crate::syscall::registry::register(guest);
    made.push(Arc::clone(&guest));
    expect(
        first,
        &sharer,
        2,
        "case 6: a member whose space a process of no domain shares skipped the barrier",
        report,
    )?;

    // 7. A marked job inside a marked job is its own domain.
    let nested = marked
        .new_child_domain()
        .map_err(|_| "a marked job refused a marked child")?;
    if nested.domain() == marked.domain() || nested.domain() == 0 {
        return Err("case 7: a marked job inside a marked job is not a domain of its own");
    }
    let inner = born_in(&nested)?;
    made.push(Arc::clone(&inner));
    expect(
        first,
        &inner,
        2,
        "case 7: a switch between members of a domain and of one inside it skipped the barrier",
        report,
    )
}

/// A process of the check's own, born in `job` as `process_create` makes
/// one: made in the root job and moved, unstarted, into its own.
fn born_in(job: &Arc<Job>) -> Result<Arc<Process>, &'static str> {
    let process = process::new_for_check().map_err(|_| "no process for the domain check")?;
    process
        .core()
        .move_new_to(job)
        .map_err(|_| "a job refused a new process")?;
    if process.core().speculation_domain() != job.domain() {
        return Err("a process made in a job is not in its job's speculation domain");
    }
    Ok(process)
}

/// Switch this processor from `from`'s space to `to`'s and back, and fail
/// with `why` unless exactly `wanted` of the two switches needed the barrier.
fn expect(
    from: &Process,
    to: &Process,
    wanted: u64,
    why: &'static str,
    report: &mut Report,
) -> Result<(), &'static str> {
    let decided = switch_and_back(from.core().space(), to.core().space());
    if decided != wanted {
        crate::console::println!(
            "  domain   {why}: {decided} of 2 switches needed the barrier, {wanted} wanted (domains {} and {})",
            from.core().space().domain(),
            to.core().space().domain(),
        );
        return Err(why);
    }
    if wanted == 0 {
        report.skipped += 2;
    } else {
        report.kept += 2;
    }
    Ok(())
}

/// Install `from`, then `to` over it and `from` again over that, as two
/// switches between programs would, and leave the processor with no user
/// space, as the kernel thread running this had it. Answers how many of the
/// two switches the processor decided needed the barrier.
fn switch_and_back(from: &Arc<AddressSpace>, to: &Arc<AddressSpace>) -> u64 {
    let saved = <arch::Irq as IrqControl>::disable();
    let cpu = crate::smp::this_cpu().map_or(0, |cpu| cpu.logical);
    // SAFETY: (TRANSLATE) both spaces are held by the caller for the whole of this
    // function, past the uninstall that ends it; interrupts are masked, so
    // nothing switches this processor meanwhile, and the kernel thread
    // running this touches no user address.
    unsafe { from.install(None) };
    let before = arch::barrier_decisions_on(cpu);
    // SAFETY: (TRANSLATE) as above.
    unsafe { to.install(Some(from)) };
    // SAFETY: (TRANSLATE) as above.
    unsafe { from.install(Some(to)) };
    let decided = arch::barrier_decisions_on(cpu).saturating_sub(before);
    // SAFETY: (TRANSLATE) as above: back to no user space.
    unsafe { from.uninstall() };
    <arch::Irq as IrqControl>::restore(saved);
    decided
}

/// Case 4: `job_create` marks only under MANAGE on the parent and with no
/// other option bit, and records each marking it makes, and only those.
fn check_marking(tree: &Arc<Job>) -> Result<(), &'static str> {
    let side = Side::new()?;
    let watcher = side
        .process
        .with_handles(|table| table.insert(Object::Job(Arc::clone(tree)), Rights::WAIT))
        .map_err(|_| "no room for a job handle")?;
    let manager = side
        .process
        .with_handles(|table| table.insert(Object::Job(Arc::clone(tree)), Rights::JOB))
        .map_err(|_| "no room for a job handle")?;
    let before = domain_records();
    if side.call(
        nr::JOB_CREATE,
        &[u64::from(watcher.0), JOB_SPECULATION_DOMAIN],
    ) != Err(status::ACCESS_DENIED)
    {
        return Err("case 4: job_create marked a job without MANAGE on its parent");
    }
    if side.call(
        nr::JOB_CREATE,
        &[u64::from(manager.0), JOB_SPECULATION_DOMAIN << 1],
    ) != Err(status::INVALID_ARGS)
    {
        return Err("case 4: job_create took an option it does not know");
    }
    if domain_records() != before {
        return Err("case 4: a refused job_create wrote a DOMAIN audit record");
    }
    if side
        .call(
            nr::JOB_CREATE,
            &[u64::from(manager.0), JOB_SPECULATION_DOMAIN],
        )
        .is_err()
    {
        return Err("case 4: job_create refused to mark a job under MANAGE on its parent");
    }
    if domain_records() != before + 1 {
        return Err("case 4: a job_create that marked a job wrote no DOMAIN audit record");
    }
    side.process.kill(job::KILLED_STATUS);
    Ok(())
}

/// How many `DOMAIN` records the high-value ring holds.
fn domain_records() -> u64 {
    let mut records = [audit::Record::EMPTY; 16];
    let mut from = 0;
    let mut count = 0;
    loop {
        let read = audit::read(audit::Which::High, from, &mut records);
        if read.copied == 0 {
            return count;
        }
        count += records
            .iter()
            .take(read.copied)
            .filter(|record| record.is(audit::DOMAIN))
            .count() as u64;
        from = read.next;
    }
}
