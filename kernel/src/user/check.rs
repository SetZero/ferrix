//! Stage 6's self-checks, so far: the memory objects a process is built from.
//!
//! What these measure is not "does a VMO work" but the two properties the rest
//! of stage 6 will rest on: that a reservation costs nothing until it is
//! touched, and that every frame an object was given comes back when it is
//! dropped. The second is the one that fails silently — a process that leaks
//! its anonymous memory on exit leaks it at a rate nothing reports, and the
//! machine dies of it an hour into a `rustc` build.

use ferrix_bootinfo::PAGE_SIZE;

use crate::mm;
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
