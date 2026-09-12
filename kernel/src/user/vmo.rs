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
    /// Page index to the frame holding it. Absent means not yet committed.
    pages: SpinLock<BTreeMap<u64, Frame>>,
    /// The object's size in pages. Set at creation and only ever raised, by
    /// [`Vmo::grow_to`]: an index that was inside the object stays inside it.
    len: AtomicU64,
}

impl Vmo {
    /// An anonymous object of `pages` pages, with nothing committed.
    pub(crate) fn new_anonymous(pages: u64) -> Arc<Vmo> {
        Arc::new(Vmo {
            pages: SpinLock::new(BTreeMap::new()),
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
        self.pages.lock().len()
    }

    /// The frame holding page `index`, if it has been committed.
    pub(crate) fn page(&self, index: u64) -> Option<Frame> {
        self.pages.lock().get(&index).copied()
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
    /// # Errors
    ///
    /// [`VmoError::OutOfMemory`] if the allocator refuses a reference on a
    /// page — which means the frame is not allocated or its count would wrap,
    /// both kernel bugs rather than conditions to recover from. Nothing is
    /// shared when that happens: the references taken so far are given back,
    /// so a failed fork costs nothing.
    pub(crate) fn fork(&self) -> Result<Arc<Vmo>, VmoError> {
        let pages = self.pages.lock();

        for (taken, &frame) in pages.values().enumerate() {
            if mm::share_frame(frame).is_none() {
                // Unwind, or the pages this got through would be held by an
                // object that is never built and never dropped.
                for &frame in pages.values().take(taken) {
                    let _ = mm::release_frame(frame);
                }
                return Err(VmoError::OutOfMemory);
            }
        }

        Ok(Arc::new(Vmo {
            pages: SpinLock::new(pages.clone()),
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
        if let Some(&frame) = pages.get(&index) {
            return Ok(frame);
        }

        let frame = mm::allocate_frames(0).ok_or(VmoError::OutOfMemory)?;
        mm::zero_frame(frame);
        let _ = pages.insert(index, frame);
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
        match pages.get(&index) {
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
    /// never visible through the other. The mappings of *this* object are
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
        let frame = match pages.get(&index).copied() {
            Some(frame) if mm::frame_references(frame) <= 1 => frame,
            Some(shared) => {
                let copy = mm::allocate_frames(0).ok_or(VmoError::OutOfMemory)?;
                mm::copy_frame(copy, shared);
                let _ = pages.insert(index, copy);
                let _ = mm::release_frame(shared);
                copy
            }
            None => {
                let fresh = mm::allocate_frames(0).ok_or(VmoError::OutOfMemory)?;
                mm::zero_frame(fresh);
                let _ = pages.insert(index, fresh);
                fresh
            }
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
    pub(crate) fn replace(&self, index: u64, frame: Frame) -> Option<Frame> {
        let mut pages = self.pages.lock();
        let old = pages.insert(index, frame);
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
    /// and [`crate::user::space::AddressSpace`] decides.
    pub(crate) fn decommit_range(&self, first: u64, pages: u64) -> usize {
        let mut held = self.pages.lock();
        let mut given = 0;
        for index in first..first.saturating_add(pages) {
            if let Some(frame) = held.remove(&index) {
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
    /// committed. Splitting the sparse list visits only those.
    pub(crate) fn decommit_from(&self, first: u64) -> usize {
        let released = self.pages.lock().split_off(&first);
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
    /// Pages past this object's end stay in `from`. `from` must not be this
    /// object: the two locks are taken one after the other, never together.
    pub(crate) fn adopt_pages(&self, from: &Vmo, first: u64, count: u64) {
        let count = count.min(self.len_pages());
        let moved = {
            let mut source = from.pages.lock();
            let mut moved = source.split_off(&first);
            let mut beyond = moved.split_off(&first.saturating_add(count));
            source.append(&mut beyond);
            moved
        };
        let mut pages = self.pages.lock();
        for (index, frame) in moved {
            let _ = pages.insert(index - first, frame);
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
        for frame in self.pages.get_mut().values() {
            let _ = mm::release_frame(*frame);
        }
    }
}
