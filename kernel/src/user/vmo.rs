//! Pageable memory objects: pages, not mappings.
//!
//! Stage 6 of `docs/ROADMAP.md`, and `docs/ARCHITECTURE.md` §3 calls the VMO
//! unification the load-bearing idea of the object model. One page list serves
//! anonymous memory, the page cache and driver DMA buffers, so that a block
//! driver reading into a cached page is filling the object the cache already
//! holds rather than copying into a buffer and then into the cache.
//!
//! Only the anonymous kind exists here. File-backed VMOs want a page cache and
//! a filesystem, which are stage 8 and stage 11; DMA VMOs want an IOMMU
//! domain, which is stage 10. What matters now is that the *shape* is the one
//! those will extend — a sparse list of frames indexed by page number, with
//! commit-on-demand — rather than a shape they would have to unpick.
//!
//! # Why the list is sparse
//!
//! An anonymous VMO is created at its full size and almost never filled. A
//! process maps eight megabytes of stack and touches three pages of it; a
//! `rustc` invocation reserves far more than it writes. Storing a frame per
//! page up front would allocate the whole reservation at `mmap` time, which is
//! the opposite of what lazy anonymous memory means, so a page with no frame
//! is simply absent and the fault handler commits it on first touch.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_frame::Frame;
use ferrix_sync::SpinLock;

use crate::mm;

/// Why a VMO operation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VmoError {
    /// The page index is past the end of the object.
    OutOfRange {
        /// The index asked for.
        index: u64,
        /// How many pages the object has.
        pages: u64,
    },
    /// No frame was available to commit.
    OutOfMemory,
}

impl fmt::Display for VmoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmoError::OutOfRange { index, pages } => {
                write!(f, "page {index} is outside a {pages}-page object")
            }
            VmoError::OutOfMemory => f.write_str("no frame available to commit"),
        }
    }
}

/// A pageable memory object.
///
/// Reference-counted, because the whole point is that more than one mapping
/// can name the same pages: two processes sharing memory, and from stage 8 a
/// mapped file and a `read` file, are the same object seen twice.
#[derive(Debug)]
pub(crate) struct Vmo {
    /// Which pages hold a frame, and which of those are held in place.
    pages: SpinLock<Pages>,
    /// The object's size in pages. Set at creation and only ever raised, by
    /// [`Vmo::grow_to`]: an index that was inside the object stays inside it.
    len: AtomicU64,
}

/// A VMO's pages, under one lock.
#[derive(Debug, Default)]
struct Pages {
    /// Page index to the frame holding it. Absent means not yet committed.
    frames: BTreeMap<u64, Frame>,
    /// Page index to how many [`Held`] guards hold it. Absent means none.
    held: BTreeMap<u64, u32>,
}

impl Vmo {
    /// An anonymous object of `pages` pages, with nothing committed.
    pub(crate) fn new_anonymous(pages: u64) -> Arc<Vmo> {
        Arc::new(Vmo {
            pages: SpinLock::new(Pages::default()),
            len: AtomicU64::new(pages),
        })
    }

    /// The object's size in pages.
    pub(crate) fn len_pages(&self) -> u64 {
        self.len.load(Ordering::Relaxed)
    }

    /// The object's size in bytes.
    pub(crate) fn len_bytes(&self) -> u64 {
        self.len_pages() * PAGE_SIZE
    }

    /// How many pages currently hold a frame.
    ///
    /// The difference between this and [`Vmo::len_pages`] is the reservation
    /// that has been promised and not yet paid for, which is what overcommit
    /// means and what the exit criterion measures.
    pub(crate) fn committed(&self) -> usize {
        self.pages.lock().frames.len()
    }

    /// The frame holding page `index`, if it has been committed.
    pub(crate) fn page(&self, index: u64) -> Option<Frame> {
        self.pages.lock().frames.get(&index).copied()
    }

    /// A second object naming every page this one holds, for `fork`.
    ///
    /// The pages are *shared*, not copied: each committed frame gains a
    /// reference and both objects name it. What makes that safe is that the
    /// regions mapping either object are marked copy-on-write at the same
    /// moment, so the first write through either one copies the page and
    /// replaces it in *its own* object, leaving the other's untouched.
    ///
    /// Which is the reason fork clones the object rather than sharing one
    /// `Arc`. Two address spaces holding one `Arc<Vmo>` cannot diverge —
    /// [`Vmo::replace`] would swap the page for *both* of them, and a child's
    /// first write would be visible to its parent, which is precisely what
    /// `MAP_PRIVATE` promises will not happen. Sharing the `Arc` is right for
    /// `MAP_SHARED`, and [`crate::user::space::AddressSpace::fork`] is where
    /// the two cases are told apart.
    ///
    /// A page [`Vmo::hold`] holds is copied into the new object instead:
    /// sharing it would make the next write through either side a
    /// copy-on-write fault, and one that swapped the held frame out of this
    /// object would leave a device writing to a page nobody reads.
    ///
    /// # Errors
    ///
    /// [`VmoError::OutOfMemory`] if the allocator refuses a reference on a
    /// page — which means the frame is not allocated or its count would wrap,
    /// both kernel bugs rather than conditions to recover from — or has no
    /// frame to copy a held page into. Nothing is shared when that happens:
    /// the references and copies taken so far are given back, so a failed
    /// fork costs nothing.
    pub(crate) fn fork(&self) -> Result<Arc<Vmo>, VmoError> {
        let pages = self.pages.lock();

        let mut frames = BTreeMap::new();
        for (&index, &frame) in &pages.frames {
            let given = if pages.held.contains_key(&index) {
                mm::allocate_frames(0).inspect(|&copy| mm::copy_frame(copy, frame))
            } else {
                mm::share_frame(frame).map(|_| frame)
            };
            let Some(given) = given else {
                // Unwind, or the pages this got through would be held by an
                // object that is never built and never dropped.
                for &taken in frames.values() {
                    let _ = mm::release_frame(taken);
                }
                return Err(VmoError::OutOfMemory);
            };
            let _ = frames.insert(index, given);
        }

        Ok(Arc::new(Vmo {
            pages: SpinLock::new(Pages {
                frames,
                held: BTreeMap::new(),
            }),
            len: AtomicU64::new(self.len_pages()),
        }))
    }

    /// The frame holding page `index`, allocating and zeroing one if this is
    /// the first touch.
    ///
    /// Zeroed rather than merely allocated: a fresh anonymous page that still
    /// held the last owner's data is an information leak, and from here the
    /// last owner is another process. [`crate::mm::zero_frame`] is the single
    /// path every such hand-off goes through.
    ///
    /// # Errors
    ///
    /// [`VmoError::OutOfRange`] past the end of the object, and
    /// [`VmoError::OutOfMemory`] when the allocator has nothing left.
    pub(crate) fn commit(&self, index: u64) -> Result<Frame, VmoError> {
        let len = self.len_pages();
        if index >= len {
            return Err(VmoError::OutOfRange { index, pages: len });
        }

        let mut pages = self.pages.lock();
        if let Some(&frame) = pages.frames.get(&index) {
            return Ok(frame);
        }

        let frame = mm::allocate_frames(0).ok_or(VmoError::OutOfMemory)?;
        mm::zero_frame(frame);
        let _ = pages.frames.insert(index, frame);
        Ok(frame)
    }

    /// Copy `out.len()` bytes out of page `index`, starting `offset` into it.
    ///
    /// A page never committed reads as zeros and stays uncommitted: reading a
    /// reservation must not pay for it. The copy is made under the object's
    /// lock, which is what keeps the frame from being decommitted and freed
    /// half-way through; the caller copies onwards to user memory after it is
    /// released.
    ///
    /// # Errors
    ///
    /// [`VmoError::OutOfRange`] past the end of the object or of the page.
    pub(crate) fn read_page(
        &self,
        index: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), VmoError> {
        self.check_span(index, offset, out.len())?;
        let pages = self.pages.lock();
        match pages.frames.get(&index) {
            None => out.fill(0),
            Some(&frame) => {
                let at = mm::direct_map(frame * PAGE_SIZE) as usize + offset;
                // SAFETY: the object holds a reference on `frame` for as long
                // as its lock is held, `check_span` kept `offset + out.len()`
                // inside the page, and the direct map covers all of RAM.
                let source = unsafe { core::slice::from_raw_parts(at as *const u8, out.len()) };
                out.copy_from_slice(source);
            }
        }
        Ok(())
    }

    /// Copy `data` into page `index`, starting `offset` into it, committing
    /// the page first if this is its first touch.
    ///
    /// A page this object shares with another — one `fork` left in both — is
    /// copied before it is written, so that a write through this object is
    /// never visible through the other. A held page never is: it is written
    /// where the device reads it. The mappings of *this* object are
    /// not this function's to fix up; the native ABI creates no mapping of a
    /// VMO yet, and `vmo_map` will have to invalidate what it maps when it
    /// arrives.
    ///
    /// # Errors
    ///
    /// [`VmoError::OutOfRange`], or [`VmoError::OutOfMemory`].
    pub(crate) fn write_page(
        &self,
        index: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), VmoError> {
        self.check_span(index, offset, data.len())?;
        let mut pages = self.pages.lock();
        let frame = match pages.frames.get(&index).copied() {
            Some(frame) if mm::frame_references(frame) <= 1 || pages.held.contains_key(&index) => {
                frame
            }
            Some(_) | None => pages.exclusive(index)?,
        };
        let at = mm::direct_map(frame * PAGE_SIZE) as usize + offset;
        // SAFETY: the frame is this object's alone (copied above if it was
        // not) and held under its lock; `check_span` kept the write inside
        // the page; the direct map is writable for all of RAM.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), at as *mut u8, data.len()) };
        Ok(())
    }

    /// Refuse a byte range that leaves page `index` or the object.
    fn check_span(&self, index: u64, offset: usize, len: usize) -> Result<(), VmoError> {
        let end = offset.checked_add(len);
        let pages = self.len_pages();
        if index >= pages || end.is_none_or(|end| end as u64 > PAGE_SIZE) {
            return Err(VmoError::OutOfRange { index, pages });
        }
        Ok(())
    }

    /// Replace the frame holding page `index`, giving back the reference the
    /// object held on the old one.
    ///
    /// What a copy-on-write fault does once it has copied: the object now
    /// names the private copy, and the shared original is one holder lighter.
    /// Returns the frame that was displaced.
    ///
    /// A held page is not replaced, and `None` comes back with `frame` still
    /// the caller's. No fault reaches here for one — [`Vmo::hold`] leaves each
    /// page it holds unshared, and [`Vmo::fork`] copies rather than shares it
    /// — so this is the refusal that keeps a device's page from being freed
    /// under it if that ever stops being true.
    pub(crate) fn replace(&self, index: u64, frame: Frame) -> Option<Frame> {
        let mut pages = self.pages.lock();
        if pages.held.contains_key(&index) {
            return None;
        }
        let old = pages.frames.insert(index, frame);
        if let Some(old) = old {
            let _ = mm::release_frame(old);
        }
        old
    }

    /// Give back `pages` pages from `first`, and report how many held a frame.
    ///
    /// What `munmap` of part of a mapping does to the object behind it. The
    /// pages are gone rather than merely unmapped: a process that unmaps half
    /// its heap expects the memory back, and an object that kept them until
    /// the rest of the mapping went would hold them for as long as the
    /// process lived.
    ///
    /// Only the caller knows whether that is right — a page of a *shared*
    /// object is not one unmapper's to take away — so this does as it is told
    /// and [`crate::user::space::AddressSpace`] decides. A held page stays,
    /// and is not counted.
    pub(crate) fn decommit_range(&self, first: u64, pages: u64) -> usize {
        let mut held = self.pages.lock();
        let mut given = 0;
        for index in first..first.saturating_add(pages) {
            if held.held.contains_key(&index) {
                continue;
            }
            if let Some(frame) = held.frames.remove(&index) {
                let _ = mm::release_frame(frame);
                given += 1;
            }
        }
        given
    }

    /// Give back every page from `first` to the end, and report how many held
    /// a frame.
    ///
    /// What truncating a file does. Not [`Vmo::decommit_range`] over the rest
    /// of the object, because a file's object is sized for the largest file
    /// the filesystem allows -- hundreds of millions of pages -- and a loop
    /// over indices would visit every one of them to find the handful that are
    /// committed. Splitting the sparse list visits only those. A held page
    /// stays, and is not counted.
    pub(crate) fn decommit_from(&self, first: u64) -> usize {
        let released = {
            let mut pages = self.pages.lock();
            let mut released = pages.frames.split_off(&first);
            pages.keep_held(&mut released);
            released
        };
        for frame in released.values() {
            let _ = mm::release_frame(*frame);
        }
        released.len()
    }
}

impl Vmo {
    /// Make the object at least `pages` pages long.
    ///
    /// What `mremap` growing a shared mapping does to the object behind it,
    /// as Linux grows the shmem file behind `MAP_SHARED | MAP_ANONYMOUS`. The
    /// new pages are uncommitted, so they read as zeros and cost nothing until
    /// touched. Never shrinks: another mapping of the object may still reach
    /// the pages a shorter length would put out of range.
    pub(crate) fn grow_to(&self, pages: u64) {
        let _ = self.len.fetch_max(pages, Ordering::Relaxed);
    }

    /// Move `count` pages of `from`, starting at page `first`, into this
    /// object at pages `0..count`.
    ///
    /// Moved, not copied or shared: each frame leaves `from`'s list and joins
    /// this one with the reference it already had, so no page is allocated,
    /// copied or released. What `mremap` does with a private mapping it
    /// resizes, so that the region can be given an object of its own length
    /// without the cost of its contents. A frame a `fork` child still shares
    /// keeps its count above one, and the moved region keeps its
    /// copy-on-write marking, so the next write copies it as it would have.
    ///
    /// Pages past this object's end stay in `from`, and so does any page
    /// `from` holds. `from` must not be this object: the two locks are taken
    /// one after the other, never together.
    pub(crate) fn adopt_pages(&self, from: &Vmo, first: u64, count: u64) {
        let count = count.min(self.len_pages());
        let moved = {
            let mut source = from.pages.lock();
            let mut moved = source.frames.split_off(&first);
            let mut beyond = moved.split_off(&first.saturating_add(count));
            source.frames.append(&mut beyond);
            source.keep_held(&mut moved);
            moved
        };
        let mut pages = self.pages.lock();
        for (index, frame) in moved {
            let _ = pages.frames.insert(index - first, frame);
        }
    }

    /// Hold `pages` pages from `first` in place, for a device to be given
    /// their addresses.
    ///
    /// Each page is committed, and copied first if a `fork` left it shared,
    /// so that the frame a device reads and writes is the one this object
    /// names and no one else's. While a [`Held`] for a page lives, the page
    /// keeps that frame: decommitting skips it, a copy-on-write replace is
    /// refused, and a `fork` copies it rather than sharing it. Holds nest; a
    /// page goes back to ordinary when the last one is dropped.
    ///
    /// The guard keeps the object alive, and with it every frame it holds, so
    /// forgetting a guard — what a caller does when a device may still hold
    /// the addresses — keeps the frames out of the allocator for good.
    ///
    /// # Errors
    ///
    /// [`VmoError::OutOfRange`] for no pages or a range past the end, and
    /// [`VmoError::OutOfMemory`] when a frame cannot be had. Pages committed
    /// before a failure stay committed, as [`Vmo::commit`] would leave them,
    /// and none is held.
    pub(crate) fn hold(self: &Arc<Self>, first: u64, pages: u64) -> Result<Held, VmoError> {
        let len = self.len_pages();
        let end = first
            .checked_add(pages)
            .filter(|&end| pages > 0 && end <= len)
            .ok_or(VmoError::OutOfRange {
                index: first,
                pages: len,
            })?;
        let count = usize::try_from(pages).map_err(|_| VmoError::OutOfMemory)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(count)
            .map_err(|_| VmoError::OutOfMemory)?;

        let mut state = self.pages.lock();
        if (first..end).any(|index| state.held.get(&index) == Some(&u32::MAX)) {
            return Err(VmoError::OutOfMemory);
        }
        for index in first..end {
            let frame = match state.frames.get(&index).copied() {
                Some(frame) if mm::frame_references(frame) <= 1 => frame,
                Some(_) | None => state.exclusive(index)?,
            };
            frames.push(frame);
        }
        for index in first..end {
            *state.held.entry(index).or_insert(0) += 1;
        }
        drop(state);

        Ok(Held {
            vmo: Arc::clone(self),
            first,
            frames,
        })
    }
}

impl Pages {
    /// The frame page `index` can be written through without anyone else
    /// seeing it: a zeroed one if the page has none, or a copy of the one a
    /// `fork` left it sharing, whose reference this gives back.
    fn exclusive(&mut self, index: u64) -> Result<Frame, VmoError> {
        let fresh = mm::allocate_frames(0).ok_or(VmoError::OutOfMemory)?;
        match self.frames.insert(index, fresh) {
            Some(shared) => {
                mm::copy_frame(fresh, shared);
                let _ = mm::release_frame(shared);
            }
            None => mm::zero_frame(fresh),
        }
        Ok(fresh)
    }

    /// Put back into this list every page of `taken` that is held.
    fn keep_held(&mut self, taken: &mut BTreeMap<u64, Frame>) {
        let held: Vec<u64> = self
            .held
            .keys()
            .copied()
            .filter(|index| taken.contains_key(index))
            .collect();
        for index in held {
            if let Some(frame) = taken.remove(&index) {
                let _ = self.frames.insert(index, frame);
            }
        }
    }
}

/// Pages of a VMO held in place, from [`Vmo::hold`]. Dropping it lets them go.
#[derive(Debug)]
pub(crate) struct Held {
    /// The object, kept alive with its frames.
    vmo: Arc<Vmo>,
    /// The first page held.
    first: u64,
    /// The frame holding each page, in page order.
    frames: Vec<Frame>,
}

impl Held {
    /// The frame holding each page, in page order.
    pub(crate) fn frames(&self) -> &[Frame] {
        &self.frames
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut pages = self.vmo.pages.lock();
        for index in (self.first..).take(self.frames.len()) {
            match pages.held.get(&index).copied() {
                Some(1) => {
                    let _ = pages.held.remove(&index);
                }
                Some(count) => {
                    let _ = pages.held.insert(index, count - 1);
                }
                None => {}
            }
        }
    }
}

impl Drop for Vmo {
    /// Give every committed page back.
    ///
    /// Through [`crate::mm::release_frame`] rather than the allocator's
    /// `deallocate`, because a page this object shares with another is not
    /// this object's alone to free — the allocator refuses that, and the
    /// refusal is the point.
    fn drop(&mut self) {
        for frame in self.pages.get_mut().frames.values() {
            let _ = mm::release_frame(*frame);
        }
    }
}
