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
    /// The object's size in pages, fixed at creation.
    len: u64,
}

impl Vmo {
    /// An anonymous object of `pages` pages, with nothing committed.
    pub(crate) fn new_anonymous(pages: u64) -> Arc<Vmo> {
        Arc::new(Vmo {
            pages: SpinLock::new(BTreeMap::new()),
            len: pages,
        })
    }

    /// The object's size in pages.
    pub(crate) fn len_pages(&self) -> u64 {
        self.len
    }

    /// The object's size in bytes.
    pub(crate) fn len_bytes(&self) -> u64 {
        self.len * PAGE_SIZE
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
        if index >= self.len {
            return Err(VmoError::OutOfRange {
                index,
                pages: self.len,
            });
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
