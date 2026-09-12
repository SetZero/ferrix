//! Stage 6's self-checks, so far: the memory objects a process is built from,
//! and a processor translating through one.
//!
//! What most of these measure is not "does a VMO work" but the two properties
//! the rest of stage 6 will rest on: that a reservation costs nothing until it
//! is touched, and that every frame an object was given comes back when it is
//! dropped. The second is the one that fails silently — a process that leaks
//! its anonymous memory on exit leaks it at a rate nothing reports, and the
//! machine dies of it an hour into a `rustc` build.
//!
//! The last one measures something different in kind: that the *hardware*
//! agrees with the tables, which no amount of walking them in software can
//! establish. It is the first thing in the tree to put a root the kernel built
//! into a processor's root register.

use ferrix_bootinfo::{KERNEL_HALF_BASE, PAGE_SIZE};

use ferrix_sync::IrqControl;
use ferrix_vma::VmaFlags;

use crate::arch;
use crate::mm;
use crate::user::space::{self, Access, AddressSpace, SpaceError};
use crate::user::vmo::{Vmo, VmoError};

/// What the checks measured, for the boot log.
#[derive(Debug)]
pub(crate) struct Report {
    /// Pages a reservation promised.
    pub(crate) reserved: u64,
    /// Pages of it actually committed.
    pub(crate) committed: usize,
    /// Frames the whole check cost, once everything was dropped. Zero, or the
    /// check failed.
    pub(crate) leaked: i64,
    /// Pages faulted into an address space and read back.
    pub(crate) faulted: u64,
    /// Pages the processor itself translated, through an address space
    /// installed on it.
    pub(crate) walked: u64,
}

/// Run them. `Err` names the first thing that was not true.
pub(crate) fn run() -> Result<Report, &'static str> {
    let before = mm::free_frames();

    check_reservation_is_lazy()?;
    check_a_committed_page_is_zeroed()?;
    check_commit_is_idempotent()?;
    check_out_of_range_is_refused()?;
    check_a_shared_page_survives_one_drop()?;
    check_replacing_a_page_releases_the_old_one()?;

    let reserved = 2048;
    let committed = check_only_what_is_touched_is_paid_for(reserved)?;

    check_an_empty_space_maps_nothing()?;
    check_a_region_outside_the_user_half_is_refused()?;
    check_a_fault_outside_every_region_is_a_segfault()?;
    check_a_write_to_a_read_only_region_is_refused()?;
    let faulted = check_pages_arrive_on_demand_and_go_back()?;
    let walked = check_the_processor_walks_an_installed_space()?;

    // Everything above dropped its objects before returning, so the allocator
    // must be exactly where it started. Signed, because a check that somehow
    // *gained* frames is as wrong as one that lost them and the number should
    // say which.
    let after = mm::free_frames();
    let leaked = i64::try_from(before).unwrap_or(i64::MAX) - i64::try_from(after).unwrap_or(0);
    if leaked != 0 {
        return Err("the checks did not give back every frame they took");
    }

    Ok(Report {
        reserved,
        committed,
        leaked,
        faulted,
        walked,
    })
}

/// A fresh object holds no frames at all.
fn check_reservation_is_lazy() -> Result<(), &'static str> {
    let before = mm::free_frames();
    let vmo = Vmo::new_anonymous(1024);

    if vmo.committed() != 0 {
        return Err("a fresh object had pages committed");
    }
    if mm::free_frames() != before {
        return Err("reserving a thousand pages cost a frame");
    }
    if vmo.len_pages() != 1024 || vmo.len_bytes() != 1024 * PAGE_SIZE {
        return Err("an object's size in bytes disagrees with its size in pages");
    }
    Ok(())
}

/// A committed page reads back as zero, all the way across.
fn check_a_committed_page_is_zeroed() -> Result<(), &'static str> {
    let vmo = Vmo::new_anonymous(4);
    let frame = vmo.commit(2).map_err(|_| "committing a page failed")?;

    // Through the direct map, which is the only way the kernel can see a page
    // that belongs to an object it has not mapped anywhere.
    let base = mm::direct_map(frame * PAGE_SIZE) as *const u8;
    for offset in 0..PAGE_SIZE as usize {
        // SAFETY: `offset` is below `PAGE_SIZE` and `base` is the start of a
        // page, so the result is inside that page.
        let at = unsafe { base.add(offset) };
        // SAFETY: `frame` was just committed, so it is allocated and nothing
        // else refers to it; the direct map covers every frame of RAM.
        let byte = unsafe { at.read_volatile() };
        if byte != 0 {
            return Err("a freshly committed page was not zeroed");
        }
    }
    Ok(())
}

/// Committing the same page twice hands back the same frame and costs nothing.
fn check_commit_is_idempotent() -> Result<(), &'static str> {
    let vmo = Vmo::new_anonymous(8);
    let first = vmo.commit(3).map_err(|_| "committing a page failed")?;

    let between = mm::free_frames();
    let second = vmo.commit(3).map_err(|_| "re-committing a page failed")?;

    if first != second {
        return Err("committing a page twice produced two different frames");
    }
    if mm::free_frames() != between {
        return Err("re-committing a page allocated a second frame");
    }
    if vmo.page(3) != Some(first) {
        return Err("a committed page is not where the object says it is");
    }
    if vmo.page(4).is_some() {
        return Err("an untouched page reported a frame");
    }
    Ok(())
}

/// A page past the end of the object is refused rather than allocated.
fn check_out_of_range_is_refused() -> Result<(), &'static str> {
    let vmo = Vmo::new_anonymous(4);
    match vmo.commit(4) {
        Err(VmoError::OutOfRange { index: 4, pages: 4 }) => {}
        _ => return Err("a page past the end of an object was not refused"),
    }
    if vmo.committed() != 0 {
        return Err("a refused commit allocated a frame anyway");
    }
    Ok(())
}

/// A frame two objects hold is not freed when the first of them goes.
///
/// This is copy-on-write's invariant expressed at the object level, and the
/// check that would catch the whole class of bug the allocator's `StillShared`
/// guard exists for.
fn check_a_shared_page_survives_one_drop() -> Result<(), &'static str> {
    let before = mm::free_frames();

    let first = Vmo::new_anonymous(1);
    let frame = first.commit(0).map_err(|_| "committing a page failed")?;

    // A second holder, as `fork` would install.
    if mm::share_frame(frame) != Some(2) {
        return Err("sharing a committed page did not raise its count");
    }

    drop(first);
    if mm::frame_references(frame) != 1 {
        return Err("dropping one holder did not leave exactly one reference");
    }
    if mm::free_frames() != before - 1 {
        return Err("a page with a holder left was returned to the allocator");
    }

    // And the last holder gives it back.
    if !mm::release_frame(frame) {
        return Err("releasing the last reference did not free the frame");
    }
    if mm::free_frames() != before {
        return Err("the frame did not come back");
    }
    Ok(())
}

/// Replacing a page hands the old frame back, which is what a copy-on-write
/// fault does once it has copied.
///
/// The old frame must not be freed while the other holder still has it, and
/// must be freed when it does not — the same invariant as
/// [`check_a_shared_page_survives_one_drop`], reached through the object
/// rather than through the allocator.
fn check_replacing_a_page_releases_the_old_one() -> Result<(), &'static str> {
    let before = mm::free_frames();
    let vmo = Vmo::new_anonymous(1);
    let original = vmo.commit(0).map_err(|_| "committing a page failed")?;

    // A second holder, so the replace below must not free the original.
    if mm::share_frame(original) != Some(2) {
        return Err("sharing a committed page did not raise its count");
    }

    let copy = mm::allocate_frames(0).ok_or("no frame for the copy")?;
    mm::zero_frame(copy);

    if vmo.replace(0, copy) != Some(original) {
        return Err("replacing a page did not report the frame it displaced");
    }
    if vmo.page(0) != Some(copy) {
        return Err("the object still names the page it replaced");
    }
    if mm::frame_references(original) != 1 {
        return Err("replacing a shared page did not drop the object's reference");
    }

    // The other holder gives it back, and the object gives back the copy.
    if !mm::release_frame(original) {
        return Err("releasing the last reference to the original did not free it");
    }
    drop(vmo);
    if mm::free_frames() != before {
        return Err("replacing a page leaked either the original or the copy");
    }
    Ok(())
}

/// A large reservation costs exactly the pages that are touched.
fn check_only_what_is_touched_is_paid_for(reserved: u64) -> Result<usize, &'static str> {
    let before = mm::free_frames();
    let vmo = Vmo::new_anonymous(reserved);

    // Touch a scattered few, out of order, so a commit that quietly filled a
    // range rather than a page would show up here.
    let touched = [0, 1, 2, 700, 12, 2047, 699];
    for &index in &touched {
        let _ = vmo.commit(index).map_err(|_| "committing a page failed")?;
    }

    let committed = vmo.committed();
    if committed != touched.len() {
        return Err("committing scattered pages did not commit exactly those pages");
    }
    if mm::free_frames() != before - committed as u64 {
        return Err("a reservation cost more frames than the pages touched");
    }

    drop(vmo);
    if mm::free_frames() != before {
        return Err("dropping an object did not give back every page it held");
    }
    Ok(committed)
}

// ---------------------------------------------------------------------------
// Address spaces
// ---------------------------------------------------------------------------
//
// Most of these build address spaces and fault pages into them without ever
// installing one on a processor, which is deliberate: the mapper reaches any
// root through the direct map, so the whole of demand paging can be exercised
// before there is a thread at a lower privilege level to exercise it from. The
// last one closes that gap — it installs a space and makes the processor
// translate through it — and what is still not checked here is the privilege
// change, which is the next piece of stage 6.

/// A fresh space has a root, no regions, and the kernel in reach.
fn check_an_empty_space_maps_nothing() -> Result<(), &'static str> {
    let space = AddressSpace::new().map_err(|_| "could not make an address space")?;

    if space.region_count() != 0 {
        return Err("a fresh address space already had regions");
    }
    if space.root_table() == 0 || !space.root_table().is_multiple_of(PAGE_SIZE) {
        return Err("an address space's root is not a page-aligned frame");
    }
    Ok(())
}

/// A mapping outside the user half is refused rather than made.
///
/// The check that stops a process asking for kernel addresses and being given
/// them, which on x86-64 -- where both halves share a root -- would hand it the
/// kernel's own tables.
fn check_a_region_outside_the_user_half_is_refused() -> Result<(), &'static str> {
    let space = AddressSpace::new().map_err(|_| "could not make an address space")?;

    match space.map_anonymous(KERNEL_HALF_BASE, PAGE_SIZE, VmaFlags::READ_WRITE) {
        Err(SpaceError::NotUserRange(_)) => {}
        _ => return Err("a mapping in the kernel half was not refused"),
    }
    if space.region_count() != 0 {
        return Err("a refused mapping was inserted anyway");
    }
    Ok(())
}

/// A fault where nothing is mapped is the segmentation fault.
fn check_a_fault_outside_every_region_is_a_segfault() -> Result<(), &'static str> {
    let space = AddressSpace::new().map_err(|_| "could not make an address space")?;
    let _ = space
        .map_anonymous(0x10_000, 2 * PAGE_SIZE, VmaFlags::READ_WRITE)
        .map_err(|_| "mapping failed")?;

    // Just past the region, which is the off-by-one a fault handler gets wrong.
    match space.fault(0x10_000 + 2 * PAGE_SIZE, Access::READ) {
        Err(SpaceError::NotMapped(_)) => Ok(()),
        _ => Err("a fault outside every region was not a segmentation fault"),
    }
}

/// A write to a region that does not permit writing is refused.
fn check_a_write_to_a_read_only_region_is_refused() -> Result<(), &'static str> {
    let space = AddressSpace::new().map_err(|_| "could not make an address space")?;
    let _ = space
        .map_anonymous(0x20_000, PAGE_SIZE, VmaFlags::READ)
        .map_err(|_| "mapping failed")?;

    match space.fault(0x20_000, Access::WRITE) {
        Err(SpaceError::Refused(_)) => {}
        _ => return Err("a write to a read-only region was not refused"),
    }
    // And the read it does permit still works.
    space
        .fault(0x20_000, Access::READ)
        .map_err(|_| "a read of a readable region was refused")?;
    Ok(())
}

/// Pages arrive on the fault that needs them, hold what is written through the
/// space's own tables, and every frame goes back when the space is dropped.
fn check_pages_arrive_on_demand_and_go_back() -> Result<u64, &'static str> {
    let before = mm::free_frames();
    let base = 0x4000_0000;
    let pages = 4;

    let space = AddressSpace::new().map_err(|_| "could not make an address space")?;
    let _ = space
        .map_anonymous(base, pages * PAGE_SIZE, VmaFlags::READ_WRITE)
        .map_err(|_| "mapping failed")?;

    // A whole region mapped and not one frame spent on it yet.
    if mm::free_frames() != before - 1 {
        return Err("mapping a region cost more than the root table");
    }

    // Fault them in out of order, so a handler that mapped a fixed address
    // rather than the faulting one would fail here.
    for index in [2, 0, 3, 1] {
        space
            .fault(base + index * PAGE_SIZE, Access::WRITE)
            .map_err(|_| "a fault in a mapped region was not resolved")?;
    }

    // Each page must now translate through this space's root, be writable, and
    // hold what is put in it -- read back through the direct map, since the
    // kernel is not running in this address space and cannot use the address
    // the process would.
    for index in 0..pages {
        let virt = base + index * PAGE_SIZE;
        let phys = mm::translate_in(space.root_table(), virt)
            .ok_or("a faulted page does not translate in its own address space")?;

        let at = mm::direct_map(phys) as *mut u64;
        let written = 0xFEED_0000 + index;
        // SAFETY: `phys` is the frame the fault above committed for this page,
        // it is mapped nowhere else, and the direct map covers all of RAM.
        unsafe { at.write_volatile(written) };
        // SAFETY: the same address, just written.
        if unsafe { at.read_volatile() } != written {
            return Err("a faulted page did not hold what was written to it");
        }
    }

    // Re-faulting a page already present must not cost a second frame.
    let settled = mm::free_frames();
    space
        .fault(base, Access::WRITE)
        .map_err(|_| "re-faulting a present page failed")?;
    if mm::free_frames() != settled {
        return Err("re-faulting a present page allocated a second frame");
    }

    // Unmapping half the region gives back exactly those pages and leaves the
    // rest mapped, which is what `munmap` of part of a mapping has to do.
    let before_unmap = mm::free_frames();
    space
        .unmap(base, 2 * PAGE_SIZE)
        .map_err(|_| "unmapping part of a region failed")?;
    if mm::free_frames() < before_unmap + 2 {
        return Err("unmapping two pages did not give back two frames");
    }
    if mm::translate_in(space.root_table(), base).is_some() {
        return Err("an unmapped page still translates");
    }
    if mm::translate_in(space.root_table(), base + 2 * PAGE_SIZE).is_none() {
        return Err("unmapping part of a region unmapped the rest of it");
    }

    drop(space);
    if mm::free_frames() != before {
        return Err("dropping an address space did not give back every frame");
    }
    Ok(pages)
}

/// The processor translates through an address space installed on it.
///
/// Everything above walked page tables in software, through the direct map,
/// which proves the tables say the right thing and not that the hardware
/// agrees. This is the one that puts a root in `CR3` or `TTBR0` and then uses
/// an address that means nothing until it is there.
///
/// The proof is the alias. A write through the user virtual address must show
/// up in the frame the address space says backs that page, read back through
/// the direct map — an address that is nothing to do with the tables under
/// test. A processor that had ignored the new root, or found the page through
/// some leftover translation, would fail that comparison rather than merely
/// not crashing.
///
/// # Why it installs twice
///
/// Because on the Arm architectures the first install is the easy one and the
/// second is the real one. This runs before `drop_identity_map`, so the first
/// time round `TTBR0` still holds the loader's identity map and its regime is
/// *enabled* — a root written there walks whether or not anything thought
/// about `TCR.EPD0` / `TTBCR.EPD0`. Uninstalling switches the regime off, as
/// it must, and the second install is therefore the one that has to turn it
/// back on. That is the state every address space switch after the first one
/// happens in, and a version of `install_user_root` that wrote the root and
/// left `EPD0` alone passes the first round and fails the second.
///
/// One consequence worth naming: on Arm this leaves the lower half switched
/// off earlier than `drop_identity_map` would have. That is safe because
/// nothing has executed or read through the lower half since the secondary
/// processors finished starting, and the W^X sweep reaches the identity map's
/// tables through the direct map rather than through `TTBR0` — but it is a
/// real reordering and not an accident.
fn check_the_processor_walks_an_installed_space() -> Result<u64, &'static str> {
    let before = mm::free_frames();
    let base = 0x5000_0000;
    let pages = 2;

    let space = AddressSpace::new().map_err(|_| "could not make an address space")?;
    let _ = space
        .map_anonymous(base, pages * PAGE_SIZE, VmaFlags::READ_WRITE)
        .map_err(|_| "mapping failed")?;
    for index in 0..pages {
        space
            .fault(base + index * PAGE_SIZE, Access::WRITE)
            .map_err(|_| "a fault in a mapped region was not resolved")?;
    }

    // Interrupts masked across the whole window. The scheduler does not know
    // about address spaces yet, so a preemption here would resume some other
    // task on a processor translating through this space's tables — and that
    // task's own user addresses, when it has them, would mean the wrong thing.
    let state = <arch::Irq as IrqControl>::disable();

    let mut walked = Ok(());
    for round in 0..ROUNDS {
        walked = walk_through_installed(&space, base, pages, round);

        // Unconditionally, and before the mask is lifted or `space` is
        // dropped: a processor left translating through tables that are then
        // freed is walking memory the allocator has given to somebody else.
        //
        // SAFETY: nothing after this wants a user address — the next round
        // installs its own, and every read outside this loop goes through the
        // direct map.
        unsafe { space::uninstall() };

        if walked.is_err() {
            break;
        }
    }

    <arch::Irq as IrqControl>::restore(state);
    walked?;

    // The user addresses must be meaningless again. Not checked by touching
    // one, which would be a fault with no handler; checked by the kernel still
    // working, which the rest of boot does at length.
    drop(space);
    if mm::free_frames() != before {
        return Err("installing an address space leaked frames");
    }
    Ok(pages)
}

/// How many times the space is installed and used. Two, for the reason
/// [`check_the_processor_walks_an_installed_space`] gives at length.
const ROUNDS: u64 = 2;

/// Install `space`, use `pages` pages of it at `base`, and check each write
/// landed in the frame that backs it.
///
/// Split out so that the caller can uninstall on the way out whether this
/// returns `Ok` or `Err`: an early `?` inside the installed window would leave
/// the root on the processor.
///
/// `round` only varies what is written, so that a round finding the previous
/// round's value still in place is a failure rather than a pass.
fn walk_through_installed(
    space: &AddressSpace,
    base: u64,
    pages: u64,
    round: u64,
) -> Result<(), &'static str> {
    // SAFETY: `space` is borrowed for the whole of this call, so its tables
    // outlive the installation; the caller has masked interrupts and
    // uninstalls before going on.
    unsafe { space.install() };

    for index in 0..pages {
        let virt = base + index * PAGE_SIZE;
        let written = 0xC0FF_EE00_u64 + round * 0x100 + index;

        let at = virt as *mut u64;
        // SAFETY: the page at `virt` was faulted in by the caller, the region
        // is writable, and this address space is installed on this processor —
        // so this is a write to a page of RAM nothing else is using.
        unsafe { at.write_volatile(written) };
        // SAFETY: the same address, just written.
        if unsafe { at.read_volatile() } != written {
            return Err("a user address did not hold what the processor wrote to it");
        }

        // The alias, and the whole point of the check.
        let Some(phys) = mm::translate_in(space.root_table(), virt) else {
            return Err("a faulted page does not translate in its own address space");
        };
        let alias = mm::direct_map(phys) as *const u64;
        // SAFETY: `phys` is the frame this space says backs `virt`, and the
        // direct map covers every frame of RAM.
        if unsafe { alias.read_volatile() } != written {
            return Err("a write through a user address did not land in the frame behind it");
        }
    }
    Ok(())
}
