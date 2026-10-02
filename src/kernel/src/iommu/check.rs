//! The IOMMU's boot checks.
//!
//! * [`check_iommu`]: a device's domain pins and unpins as the rules say, and
//!   the quarantine holds a dead driver's pins, as stage 10 requires;
//! * [`check_dma_faults`]: last in the boot, no unit recorded a fault that no
//!   check provoked;
//! * [`run`]: the unit gate's waits, in the shapes a unit that answers at
//!   once -- every unit QEMU presents -- never puts them in: a look that
//!   outlasts its deadline, one that takes long enough for a waiter to start
//!   giving up its processor, and an operation that stays inside a gate past
//!   the next one's patience.

use alloc::sync::Arc;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_paging::MapFlags;

use crate::device::{self, DeviceNode, Location};
use crate::panic::{catalog, fatal};
use crate::{mm, object, println};

use super::gate::{self, Gate};
use super::{Domain, DomainError};

/// Run the gate's.
///
/// # Errors
///
/// The first property that did not hold, as a sentence.
///
/// Verifies: L.iommu.30, L.iommu.31
pub(crate) fn run() -> Result<(), &'static str> {
    // A deadline already past: one last look, and its answer.
    if gate::poll(|| false, 0) {
        return Err("a unit wait past its deadline said the unit had answered");
    }

    // A unit that answers on the hundredth look, to a waiter that may block:
    // it gives up its processor between looks and still sees the answer.
    let mut looks = 0_u32;
    let far = crate::timer::now_nanos().saturating_add(5_000_000_000);
    if !gate::poll(
        || {
            looks += 1;
            looks > 100
        },
        far,
    ) {
        return Err("a unit wait that yielded between looks missed the unit's answer");
    }

    // An operation inside the gate for longer than the next one's patience:
    // the next is refused rather than left waiting for good.
    let unit = Gate::new();
    let inside = unit
        .enter()
        .map_err(|_| "an empty gate could not be entered")?;
    // Through the same wait `enter` makes, with a patience of 10 ms, not
    // the unit's second.
    if unit.enter_within(10_000_000).is_ok() {
        return Err("a gate was entered twice at once");
    }
    drop(inside);
    if unit.enter().is_err() {
        return Err("a gate left by its holder could not be entered");
    }
    // As a translated domain's failure report prints it.
    if !alloc::format!("{unit:?}").starts_with("Gate { held: false") {
        return Err("a gate does not print whether it is held");
    }
    Ok(())
}

/// Stage 10: the ceiling on raised pin budgets, counted; then a device's
/// domain, and the budget and quarantine a driver's pins count against,
/// unless the boot was told to skip its checks.
///
/// Halts rather than returning, as every other stage's check does.
pub(crate) fn check_iommu() {
    // The ceiling on raised pin budgets, a quarter of RAM, counted here at
    // stage 10, checks or not: before devmgr can set a budget, and before
    // the budget's checks below set one (`object::pin`).
    object::pin::count_ceiling();
    if !crate::checks::run() {
        return;
    }
    let domains = match check_domains(device::devices()) {
        Ok(report) => report,
        // A change refused for a write not cleaned fails the domains'
        // check too; the cause is named first.
        Err(problem) => match check_cleaning(super::vtd::cleaning()) {
            Err(cause) => fatal!(
                catalog::STAGE10_IOMMU,
                "stage 10 self-check failed: {cause} ({problem})"
            ),
            Ok(_) => fatal!(
                catalog::STAGE10_IOMMU,
                "stage 10 self-check failed: {problem}"
            ),
        },
    };
    println!(
        "  iommu    {} pages pinned and unpinned through a device's {} domain, {} refusals \
         as specified, {} waits on a unit with interrupts on",
        domains.pinned,
        if domains.translated {
            "translated"
        } else {
            "untranslated"
        },
        domains.refusals,
        domains.waits,
    );
    match check_cleaning(super::vtd::cleaning()) {
        Ok(Some(line)) => println!("  iommu    {line}"),
        Ok(None) => {}
        Err(problem) => fatal!(
            catalog::STAGE10_IOMMU,
            "stage 10 self-check failed: {problem}"
        ),
    }
    let failed_kept = match object::pin::check::check_budget(device::devices()) {
        Ok(Some(report)) => {
            println!(
                "  iommu    pin budget: {} pins refused at a budget of 2 pages and at twice it, a \
                 dead driver's pins quarantined and released, {} page kept and still counted, \
                 and device_set_limit refused {} times -- without SET_LIMIT, under a live pin \
                 and past the ceiling of {} pages -- each audited",
                report.refusals,
                report.kept,
                report.set_refused,
                object::pin::ceiling(),
            );
            report.failed_kept
        }
        Ok(None) => 0,
        Err(problem) => fatal!(
            catalog::STAGE10_IOMMU,
            "stage 10 self-check failed: {problem}"
        ),
    };
    let planted = match check_completion_errors() {
        Ok(planted) => planted,
        Err(problem) => fatal!(
            catalog::STAGE10_IOMMU,
            "stage 10 self-check failed: {problem}"
        ),
    };
    match check_queue(super::invalidations(), failed_kept, planted) {
        Ok(Some(line)) => println!("  iommu    {line}"),
        Ok(None) => {}
        Err(problem) => fatal!(
            catalog::STAGE10_IOMMU,
            "stage 10 self-check failed: {problem}"
        ),
    }
    match object::pin::check::check_untranslated(device::devices()) {
        Ok(Some(done)) => println!(
            "  iommu    untranslated pins: {} a live driver closed given back at once, past twice \
             a budget of 2 pages; {} pages a dead driver left quarantined and freed at the next \
             HELLO",
            done.given_back, done.released,
        ),
        Ok(None) => {}
        Err(problem) => fatal!(
            catalog::STAGE10_IOMMU,
            "stage 10 self-check failed: {problem}"
        ),
    }
}

/// Every VT-d change on a unit whose walk does not snoop found its table
/// writes cleaned to memory before its own publish point (finding F-58): no
/// attach, detach, map or unmap was refused for a write noted and not
/// cleaned, and the boot's domains made some to clean. QEMU's unit reports
/// `ECAP.C` clear, so this runs on every x86-64 boot test. That every write
/// is noted rests on construction (`vtd.rs`'s module documentation), not on
/// this check.
///
/// `None` on a machine with no such unit.
///
/// # Errors
///
/// A change refused for a write not cleaned, or a unit that cleaned nothing
/// over a boot that attached and pinned through it.
///
/// Verifies: L.iommu.56
/// Verifies: L.iommu.57
fn check_cleaning(
    cleaning: super::vtd::Cleaning,
) -> Result<Option<alloc::string::String>, alloc::string::String> {
    if cleaning.uncleaned != 0 {
        return Err(alloc::format!(
            "{} of {} VT-d changes were refused for a table write not cleaned to memory",
            cleaning.uncleaned,
            cleaning.checked
        ));
    }
    if cleaning.units == 0 {
        return Ok(None);
    }
    if cleaning.entries == 0 || cleaning.tables == 0 || cleaning.checked == 0 {
        return Err(alloc::format!(
            "a VT-d unit that does not snoop cleaned {} entries and {} tables over {} changes",
            cleaning.entries,
            cleaning.tables,
            cleaning.checked
        ));
    }
    Ok(Some(alloc::format!(
        "{} entry writes and {} fresh tables noted and cleaned to memory on {} VT-d units that \
         do not snoop; {} changes each found them cleaned before its own publish point",
        cleaning.entries,
        cleaning.tables,
        cleaning.units,
        cleaning.checked
    )))
}

/// VT-d units on which [`check_firmware_left_on`] left the queue on as
/// firmware would and saw `open`'s path turn it off.
static FIRMWARE_QUEUES_STOPPED: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Stage 10, at bring-up, between a VT-d unit's `open` and its `enable`,
/// unless the boot was told to skip its checks:
/// the unit's invalidation queue turned on as firmware that used it would
/// leave it, and then turned off by the path `open` takes when it finds a
/// queue firmware left on, and read back off. No firmware QEMU boots leaves
/// one on, so without this the path would never run.
///
/// Halts rather than returning, as every other stage's check does.
///
/// Verifies: L.iommu.51
pub(crate) fn check_firmware_left_on(unit: &super::vtd::Unit) {
    if !crate::checks::run() {
        return;
    }
    if let Err(problem) = super::vtd::leave_queue_on_and_stop(unit) {
        fatal!(
            catalog::STAGE10_IOMMU,
            "stage 10 self-check failed: {problem}"
        );
    }
    let _ = FIRMWARE_QUEUES_STOPPED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

/// R6 and R7 (`docs/NVIDIA.md` §12.3): every VT-d invalidation of the boot
/// went through a unit's queue and was waited for, and none by register --
/// no unit reads back a register-based invalidation pending, which QEMU's
/// leaves set for good once the queue is on -- and the one invalidation check
/// R7 made fail, and nothing else, failed: its pin's `failed_kept` pages were
/// kept. No unit was marked failed.
///
/// `None` on a machine with no VT-d unit translating.
///
/// # Errors
///
/// A register-based invalidation pending, no invalidation of a kind the
/// boot makes, a failure R7 did not make, or a unit marked failed.
///
/// Verifies: L.iommu.47
/// Verifies: L.iommu.48
fn check_queue(
    queued: super::Invalidations,
    failed_kept: usize,
    planted: u64,
) -> Result<Option<alloc::string::String>, alloc::string::String> {
    let (units, pending) = super::register_invalidations_pending();
    if units == 0 {
        return Ok(None);
    }
    if pending != 0 {
        return Err(alloc::format!(
            "{pending} of {units} VT-d units read back a register-based invalidation pending"
        ));
    }
    if queued.context == 0 || queued.iotlb == 0 {
        return Err(alloc::format!(
            "the boot queued {} context-cache and {} IOTLB invalidations, where it makes both",
            queued.context,
            queued.iotlb
        ));
    }
    if queued.units_failed != 0 {
        return Err(alloc::format!(
            "{} VT-d units' invalidation queues stopped",
            queued.units_failed
        ));
    }
    if queued.completion_errors != planted || planted != units as u64 {
        return Err(alloc::format!(
            "{} completion errors were cleared where the check planted {planted} on {units} \
             VT-d units",
            queued.completion_errors
        ));
    }
    let provoked = u64::from(failed_kept != 0);
    let unplanted = queued.failed.saturating_sub(planted);
    if unplanted != provoked || queued.failed < planted {
        return Err(alloc::format!(
            "{} VT-d invalidations failed where check R7 made {provoked} fail and the planted \
             completion errors {planted}",
            queued.failed
        ));
    }
    let stopped = FIRMWARE_QUEUES_STOPPED.load(core::sync::atomic::Ordering::Relaxed);
    if stopped != units as u64 {
        return Err(alloc::format!(
            "a queue left on as firmware leaves it was turned off on {stopped} of {units} VT-d \
             units"
        ));
    }
    Ok(Some(alloc::format!(
        "{} context-cache and {} IOTLB invalidations queued and each waited for, none by \
         register on {units} VT-d units; {unplanted} failed as check R7 made it, its \
         {failed_kept} pages kept; {planted} completion errors planted, cleared, and the next \
         invalidation completed; a queue left on as firmware leaves it turned off on {stopped}",
        queued.context,
        queued.iotlb,
    )))
}

/// The consultant's condition 1 on N0g's slice 1: on every translating
/// VT-d unit, an invalidation completion error (`ICE`) the check plants
/// fails that invalidation, is cleared and counted, and the next
/// invalidation completes on a unit not marked failed -- so a completion
/// error is a failed invalidation, never a sticky bit that fails every
/// later one. Answers how many were planted.
///
/// # Errors
///
/// What did not hold, as a sentence.
///
/// Verifies: L.iommu.48
fn check_completion_errors() -> Result<u64, &'static str> {
    let Some(programmed) = super::PROGRAMMED.get() else {
        return Ok(0);
    };
    for unit in &programmed.vtd {
        super::vtd::check_planted_completion_error(unit)?;
    }
    Ok(programmed.vtd.len() as u64)
}

/// What the domain check found.
#[derive(Clone, Copy, Debug, Default)]
struct DomainReport {
    /// Pages pinned and unpinned.
    pinned: u64,
    /// Requests refused, each exactly as the rule requires.
    refusals: usize,
    /// Whether the domain checked was translated.
    translated: bool,
    /// Waits on a unit made with interrupts on, since boot.
    waits: u64,
}

/// Pin two frames through the first PCI node's domain, and require the node to
/// hand out one domain, the pin to give each frame an address, the domain to
/// count what it holds, and a pin to be refused by any domain but its own.
///
/// # Errors
///
/// The first thing that is not so. The frames are then kept out of the
/// allocator, since a pin that was not given back may still be reachable.
///
/// Verifies: L.device.8
fn check_domains(nodes: &[Arc<DeviceNode>]) -> Result<DomainReport, &'static str> {
    let mut report = DomainReport::default();
    let Some(node) = nodes
        .iter()
        .find(|node| matches!(node.location(), Location::Pci(_)))
    else {
        return Ok(report);
    };
    let domain = node
        .domain()
        .map_err(|_| "no memory for a device's domain")?;
    let again = node
        .domain()
        .map_err(|_| "no memory for a device's domain")?;
    if !Arc::ptr_eq(&domain, &again) {
        return Err("a device node handed out two domains");
    }
    let Some(first) = mm::allocate_frames(0) else {
        return Err("no frame to pin");
    };
    let Some(second) = mm::allocate_frames(0) else {
        mm::deallocate_frames(first, 0);
        return Err("no frame to pin");
    };
    pin_and_unpin(&domain, [first, second], &mut report)?;
    mm::deallocate_frames(first, 0);
    mm::deallocate_frames(second, 0);
    Ok(report)
}

/// The body of [`check_domains`], once it has its frames.
///
/// Verifies: L.iommu.19, L.iommu.20, L.iommu.21, L.iommu.22, L.iommu.29, H.DMA.3
fn pin_and_unpin(
    domain: &Domain,
    frames: [u64; 2],
    report: &mut DomainReport,
) -> Result<(), &'static str> {
    let before = domain.pinned_pages();
    let pinned = domain
        .pin(&frames, MapFlags::DMA)
        .map_err(|_| "a domain refused to pin two frames")?;
    let expected = frames.map(|frame| frame * PAGE_SIZE);
    // Every domain gives a page its physical address as its device address;
    // a translated one must also send the device there and nowhere else.
    let addressed = pinned.addresses() == expected.as_slice()
        && expected
            .iter()
            .all(|&phys| domain.resolve(phys) == Some(phys));
    let counted = domain.pinned_pages() == before + 2;

    let Err((DomainError::Foreign, pinned)) = Domain::untranslated().unpin(pinned) else {
        return Err("a domain unpinned a pin another domain took");
    };
    report.refusals += 1;
    if !addressed || !counted {
        pinned.leak();
        return Err(if addressed {
            "a domain miscounted the pages pinned into it"
        } else {
            "an untranslated domain gave a device address other than the frame's"
        });
    }
    if !matches!(domain.pin(&[], MapFlags::DMA), Err(DomainError::Empty)) {
        pinned.leak();
        return Err("a domain pinned nothing");
    }
    report.refusals += 1;
    let waits = gate::waits_with_interrupts_on();
    if domain.unpin(pinned).is_err() {
        return Err("a domain refused its own pin");
    }
    if domain.pinned_pages() != before {
        return Err("a domain still counted pages it had unpinned");
    }
    if domain.translated() && expected.iter().any(|&phys| domain.resolve(phys).is_some()) {
        return Err("a translated domain still reached a page it had unpinned");
    }
    // The boot task may block, so the unpin's wait for its unit must have
    // been made with interrupts on.
    if domain.translated() && gate::waits_with_interrupts_on() == waits {
        return Err("a translated domain's unpin waited on its unit with interrupts masked");
    }
    report.waits = gate::waits_with_interrupts_on();
    report.translated = domain.translated();
    report.pinned += 2;
    Ok(())
}

/// Stage 10: no IOMMU recorded a fault that no check provoked. Answers
/// whether any unit translates, for the audit record's end-of-boot check,
/// which runs after this since reading the faults is what records them.
///
/// Halts rather than returning, as every other stage's check does.
///
/// Verifies: H.DMA.6
pub(crate) fn check_dma_faults() -> bool {
    let audit = super::audit_faults();
    println!(
        "  iommu    {} DMA faults recorded that no check provoked, {} of them unit events other \
         than a refused access, across {} translating units; {} late faults from the \
         out-of-domain probe",
        audit.stray, audit.stray_events, audit.units, audit.provoked,
    );
    if audit.stray == 0 {
        return audit.units > 0;
    }
    if let Some(fault) = audit.first {
        println!("  iommu    the first read here: {fault}");
    }
    fatal!(
        catalog::STAGE10_DMA_FAULT,
        "stage 10 self-check failed: {} DMA faults no check provoked",
        audit.stray
    );
}
