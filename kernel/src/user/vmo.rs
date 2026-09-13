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
//!
//! # Who maps an object, and how a page leaves it
//!
//! An object does not know where it is mapped, but it knows *who* maps it:
//! every address space that has it in its object table, under the id that
//! space knows it by, in [`Vmo::attach`]'s list. A space holding the object
//! under two ids is in the list twice. The regions are not recorded — they
//! split and shrink under `mprotect` and `munmap`, and the space's own map
//! answers which of them name the object at the moment of asking.
//!
//! That list is what makes it safe for an object to take a frame away while
//! somebody maps it. Every such change — [`Vmo::decommit_range`],
//! [`Vmo::decommit_from`], [`Vmo::replace`], [`Vmo::adopt_pages`], and the copy
//! [`Vmo::write_page`] and [`Vmo::hold`] make of a page a `fork` left shared —
//! goes in three phases, and the order is the whole point:
//!
//! 1. **Under the pages lock**, the frames come out of the list into a
//!    [`Retired`]. A held page is skipped here, before anything is taken, so
//!    it is never in what the next phase invalidates.
//! 2. **With no VMO lock held**, every space in the list is asked to
//!    [`AddressSpace::forget_pages`]: it takes its translations of those pages
//!    down under its own lock and says which processors may still have them
//!    cached. One scoped shootdown goes to all of them together, and returns
//!    once every processor has answered.
//! 3. **Only then** are the frames released.
//!
//! A frame released before phase two ends is a frame some processor can
//! still write through, and the allocator hands it to somebody else: two
//! owners of one page, and the symptom turns up in whichever writes second.
//!
//! # Lock order
//!
//! An address space's lock, then an object's mapper list, then its pages. An
//! address space's lock is **never** taken while an object's lock is held, and
//! no shootdown waits while any of them is: phase two runs with all three
//! free, and a caller already holding a space's lock -- the copy-on-write fault,
//! `mremap`, `munmap`'s give-back -- does phase one under it and hands the rest
//! to [`Vmo::retire`] once it has let go, naming itself so that phase two does
//! not come back for its lock.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::SpinLock;
use ferrix_bootinfo::PAGE_SIZE;
use ferrix_frame::Frame;
use ferrix_sched::CpuSet;

use crate::mm;
use crate::smp::{self, TlbPages};
use crate::user::space::AddressSpace;

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

/// A move refused because a page of the range is held for a device.
///
/// A type of its own rather than a [`VmoError`] variant: only
/// [`Vmo::adopt_pages`] refuses for it, and every other caller matching on a
/// `VmoError` would have to answer a case it can never see.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct HeldPage {
    /// The first held page found.
    pub(crate) index: u64,
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
    /// How many pages from the start a fault through a file mapping may
    /// commit: those not wholly past the end of the file this object holds, as
    /// its filesystem last said. [`u64::MAX`] for an object that is not a
    /// file's. Stored before a truncation takes pages away and read under the
    /// pages lock by [`Vmo::commit_within`], which is what keeps a fault from
    /// committing a page the truncation has already passed.
    bound: AtomicU64,
    /// The address spaces that name this object, each with the id it names
    /// it by. Taken after a space's lock and before `pages`, never around
    /// either.
    mappers: SpinLock<Vec<Mapper>>,
}

/// How an address space maps an object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sharing {
    /// Private: copy-on-write across `fork`, which gives the child an object
    /// of its own. Such an object has exactly one mapper, ever.
    Private,
    /// Shared: every mapper sees every write.
    Shared,
}

/// One address space's name for an object.
#[derive(Debug)]
struct Mapper {
    /// Whether the space maps it privately or shared.
    sharing: Sharing,
    /// The space. Weak, so that an object does not keep alive the spaces that
    /// map it — they keep it alive, through their object tables.
    space: Weak<AddressSpace>,
    /// The id the space's regions name the object by.
    object: u64,
}

/// A VMO's pages, under one lock.
#[derive(Debug, Default)]
struct Pages {
    /// Page index to the frame holding it. Absent means not yet committed.
    frames: BTreeMap<u64, Frame>,
    /// Page index to how many [`Held`] guards hold it. Absent means none.
    held: BTreeMap<u64, u32>,
}

/// Frames phase one took out of an object's list, whose translations are still
/// to be invalidated.
///
/// Given to [`Vmo::retire`], by the object that made it, to finish. Dropped
/// without that, its frames are never released: a leak, and deliberately not
/// the other failure, a frame freed under a live translation.
#[derive(Debug)]
#[must_use = "the frames stay out of the allocator, and mapped, until the retirement is finished"]
pub(crate) struct Retired {
    /// Page index to the frame that held it.
    frames: BTreeMap<u64, Frame>,
    /// Whether the frames are released once invalidated. Not for a move, whose
    /// frames live on in another object.
    release: bool,
}

impl Retired {
    /// Nothing retired.
    fn nothing() -> Retired {
        Retired {
            frames: BTreeMap::new(),
            release: true,
        }
    }

    /// Whether nothing was taken out.
    pub(crate) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The frame page `index` held, if it was taken out.
    pub(crate) fn frame(&self, index: u64) -> Option<Frame> {
        self.frames.get(&index).copied()
    }

    /// The pages taken, as runs of `(first, count)`, lowest first.
    fn runs(&self) -> Vec<(u64, u64)> {
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for &index in self.frames.keys() {
            match runs.last_mut() {
                Some((first, count)) if first.saturating_add(*count) == index => *count += 1,
                _ => runs.push((index, 1)),
            }
        }
        runs
    }
}

/// The address space phase one ran under the lock of, which phase two must
/// therefore not visit.
#[derive(Debug)]
pub(crate) struct Own<'a> {
    /// The space.
    pub(crate) space: &'a AddressSpace,
    /// Its own shootdown, still to run: the processors it read after taking
    /// its translations down, and the pages, which it has counted pending.
    /// `None` when its shootdown has already returned.
    pub(crate) shootdown: Option<(CpuSet, TlbPages)>,
}

impl Vmo {
    /// An anonymous object of `pages` pages, with nothing committed.
    pub(crate) fn new_anonymous(pages: u64) -> Arc<Vmo> {
        Arc::new(Vmo {
            pages: SpinLock::new(Pages::default()),
            len: AtomicU64::new(pages),
            bound: AtomicU64::new(u64::MAX),
            mappers: SpinLock::new(Vec::new()),
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

    /// Record that `space` names this object as `object`.
    ///
    /// Called wherever an id goes into an address space's object table, with
    /// that space's lock held — which is the order: a space's lock, then this
    /// list. From here on a page this object takes away is forgotten in that
    /// space before its frame is released. A space that has gone since it was
    /// recorded is pruned whenever the list is next taken.
    ///
    /// # Panics
    ///
    /// If this would give an object backing a private region a second
    /// mapper, or a private mapper to an object already mapped: an invariant
    /// agreed with stage 9's `vmo_map`, and one `mremap`'s move of a private
    /// region's pages relies on to tell nobody else. `fork` gives a child its
    /// own object for every private region, so nothing in the kernel does
    /// this, and a caller that did would be silently wrong about which spaces
    /// see a page. Always on, not a debug assertion, for that reason.
    pub(crate) fn attach(&self, space: Weak<AddressSpace>, object: u64, sharing: Sharing) {
        let mut mappers = self.mappers.lock();
        mappers.retain(|mapper| mapper.space.strong_count() > 0);
        let joinable = mappers
            .iter()
            .all(|mapper| mapper.sharing == Sharing::Shared)
            && (sharing == Sharing::Shared || mappers.is_empty());
        assert!(
            joinable,
            "a VMO backing a private region would have more than one mapper"
        );
        mappers.push(Mapper {
            sharing,
            space,
            object,
        });
    }

    /// Record that the address space at `space` no longer names this object
    /// as `object`.
    ///
    /// Called wherever an id leaves an address space's object table, and for
    /// every object as a space is dropped. Takes the mapper list only, and
    /// compares addresses rather than following one, so a space in the middle
    /// of being dropped can name itself.
    pub(crate) fn detach(&self, space: *const AddressSpace, object: u64) {
        self.mappers.lock().retain(|mapper| {
            mapper.space.strong_count() > 0
                && !(core::ptr::eq(mapper.space.as_ptr(), space) && mapper.object == object)
        });
    }

    /// How many address spaces name this object, counting a space once per
    /// id it names it by.
    pub(crate) fn mapper_count(&self) -> usize {
        self.mappers
            .lock()
            .iter()
            .filter(|mapper| mapper.space.strong_count() > 0)
            .count()
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
    /// The new object is mapped by nobody yet; the space that takes it
    /// attaches itself.
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
            bound: AtomicU64::new(self.bound.load(Ordering::SeqCst)),
            mappers: SpinLock::new(Vec::new()),
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

    /// Put `frame`, filled by the caller, in page `index` if that page is
    /// inside the object and holds nothing yet. Whether it went in.
    ///
    /// What a page cache filled from a disk does once the fill returns: the
    /// fill ran with no lock held, so a write or a racing fill may have put a
    /// page there meanwhile, and that one wins. A frame that did not go in is
    /// still the caller's, to release. Nobody can have mapped a page that was
    /// absent, so nothing is taken down.
    pub(crate) fn insert_absent(&self, index: u64, frame: Frame) -> bool {
        if index >= self.len_pages() {
            return false;
        }
        let mut pages = self.pages.lock();
        if pages.frames.contains_key(&index) {
            return false;
        }
        let _ = pages.frames.insert(index, frame);
        true
    }

    /// The file this object holds is now `len` bytes long: a fault through a
    /// mapping of it may commit no page wholly past that.
    ///
    /// The filesystem calls it under its inode lock, after a write or grow
    /// extends the file, and *before* it takes a truncated file's pages away,
    /// so a fault racing the truncation sees the new end first.
    pub(crate) fn set_file_len(&self, len: u64) {
        self.bound.store(len.div_ceil(PAGE_SIZE), Ordering::SeqCst);
    }

    /// [`Vmo::commit`] for a fault through a file mapping: `None`, committing
    /// nothing, for a page wholly past the end of the file, which is the
    /// fault Linux answers with `SIGBUS`.
    ///
    /// The end is read under the pages lock. A truncation stores its new end
    /// before it takes that lock to take pages away, so either this sees the
    /// new end and refuses, or it commits first and the truncation's
    /// retirement then takes the page back out of every space that maps it,
    /// the faulting one included, once that space's lock is free.
    ///
    /// # Errors
    ///
    /// As [`Vmo::commit`].
    pub(crate) fn commit_within(&self, index: u64) -> Result<Option<Frame>, VmoError> {
        let len = self.len_pages();
        if index >= len {
            return Err(VmoError::OutOfRange { index, pages: len });
        }

        let mut pages = self.pages.lock();
        if index >= self.bound.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if let Some(&frame) = pages.frames.get(&index) {
            return Ok(Some(frame));
        }

        let frame = mm::allocate_frames(0).ok_or(VmoError::OutOfMemory)?;
        mm::zero_frame(frame);
        let _ = pages.frames.insert(index, frame);
        Ok(Some(frame))
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
    /// where the device reads it. The shared frame the copy displaced leaves
    /// in three phases, as a decommitted one does, so that a mapping of this
    /// object faults the copy in rather than keeping the original.
    ///
    /// Must not be called holding a spin lock: the displacement may wait for
    /// a shootdown.
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
        let displaced = {
            let mut pages = self.pages.lock();
            let (frame, displaced) = match pages.frames.get(&index).copied() {
                Some(frame)
                    if mm::frame_references(frame) <= 1 || pages.held.contains_key(&index) =>
                {
                    (frame, None)
                }
                Some(_) | None => pages.exclusive(index)?,
            };
            let at = mm::direct_map(frame * PAGE_SIZE) as usize + offset;
            // SAFETY: the frame is this object's alone (copied above if it was
            // not) and held under its lock; `check_span` kept the write inside
            // the page; the direct map is writable for all of RAM.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), at as *mut u8, data.len()) };
            displaced
        };
        if let Some(shared) = displaced {
            let mut retired = Retired::nothing();
            let _ = retired.frames.insert(index, shared);
            self.retire(retired, None);
        }
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
    /// Returns the frame that was displaced, once every mapping of it is gone
    /// and its reference given back.
    ///
    /// A held page is not replaced, and `None` comes back with `frame` still
    /// the caller's. No fault reaches here for one — [`Vmo::hold`] leaves each
    /// page it holds unshared, and [`Vmo::fork`] copies rather than shares it
    /// — so this is the refusal that keeps a device's page from being freed
    /// under it if that ever stops being true.
    ///
    /// Must not be called holding a spin lock; a caller holding an address
    /// space's lock uses [`Vmo::take_page`] and [`Vmo::retire`] instead.
    pub(crate) fn replace(&self, index: u64, frame: Frame) -> Option<Frame> {
        let retired = self.take_page(index, frame)?;
        let old = retired.frame(index);
        self.retire(retired, None);
        old
    }

    /// Phase one of [`Vmo::replace`]: put `frame` in page `index`, and take
    /// out the frame that was there.
    ///
    /// `None`, with `frame` still the caller's, if the page is held. May be
    /// called holding an address space's lock, and nothing else.
    pub(crate) fn take_page(&self, index: u64, frame: Frame) -> Option<Retired> {
        let mut pages = self.pages.lock();
        if pages.held.contains_key(&index) {
            return None;
        }
        let mut retired = Retired::nothing();
        if let Some(old) = pages.frames.insert(index, frame) {
            let _ = retired.frames.insert(index, old);
        }
        Some(retired)
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
    /// mapped wherever it was, and is not counted. Every other page is
    /// forgotten by every space that maps it before its frame goes back.
    ///
    /// Must not be called holding a spin lock.
    pub(crate) fn decommit_range(&self, first: u64, pages: u64) -> usize {
        let retired = self.take_range(first, pages);
        let given = retired.frames.len();
        self.retire(retired, None);
        given
    }

    /// Phase one of [`Vmo::decommit_range`]: take every committed page in
    /// `first..first + pages` that is not held out of the list.
    ///
    /// May be called holding an address space's lock, and nothing else.
    pub(crate) fn take_range(&self, first: u64, pages: u64) -> Retired {
        let end = first.saturating_add(pages);
        let mut state = self.pages.lock();
        // Held pages first, before anything is taken: a held page is never
        // among what the invalidation is told about.
        let taking: Vec<u64> = state
            .frames
            .range(first..end)
            .map(|(&index, _)| index)
            .filter(|index| !state.held.contains_key(index))
            .collect();
        let mut retired = Retired::nothing();
        for index in taking {
            if let Some(frame) = state.frames.remove(&index) {
                let _ = retired.frames.insert(index, frame);
            }
        }
        retired
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
    ///
    /// Must not be called holding a spin lock. An object nobody maps — every
    /// file's today — costs no more than the split: phase two has nobody to
    /// ask.
    pub(crate) fn decommit_from(&self, first: u64) -> usize {
        let retired = {
            let mut pages = self.pages.lock();
            let mut taken = pages.frames.split_off(&first);
            pages.keep_held(&mut taken);
            Retired {
                frames: taken,
                release: true,
            }
        };
        let given = retired.frames.len();
        self.retire(retired, None);
        given
    }

    /// Phases two and three: have every address space that maps this object
    /// forget the pages `retired` took, shoot down once on every processor
    /// any of them names, wait for every answer, and only then release the
    /// frames.
    ///
    /// `own` is the space a caller already did phase one under the lock of:
    /// it is not visited, its own shootdown (if it has one still to run) is
    /// folded into this one, and its pending count is dropped once that has
    /// returned.
    ///
    /// Must be called holding no spin lock at all — no space's, no object's.
    /// Phase two takes every mapping space's lock in turn, and the shootdown
    /// waits for other processors.
    pub(crate) fn retire(&self, retired: Retired, own: Option<Own<'_>>) {
        let mut cpus = CpuSet::empty();
        let mut pages = TlbPages::new();
        let except = own.as_ref().map(|own| own.space);
        if let Some((own_cpus, own_pages)) = own.as_ref().and_then(|own| own.shootdown.as_ref()) {
            smp::add_cpus(&mut cpus, own_cpus);
            pages.add_all(own_pages);
        }

        let runs = retired.runs();
        let Retired { frames, release } = retired;
        let mut forgotten: Vec<Arc<AddressSpace>> = Vec::new();
        if !runs.is_empty() {
            for (space, object) in self.mapped_by(except) {
                if let Some(theirs) = space.forget_runs(object, &runs, &mut pages) {
                    smp::add_cpus(&mut cpus, &theirs);
                    forgotten.push(space);
                }
            }
        }

        smp::flush_tlb_pages(&cpus, &pages);

        for space in &forgotten {
            space.flushed();
        }
        if let Some(own) = own
            && own.shootdown.is_some()
        {
            own.space.flushed();
        }
        // Dropped with no lock held: the last reference to a space that exited
        // meanwhile may be one of these, and dropping it detaches it from this
        // object's list.
        drop(forgotten);

        if release {
            for frame in frames.into_values() {
                let _ = mm::release_frame(frame);
            }
        }
    }

    /// Every live address space that maps this object, but `except`, with the
    /// id it maps it by. Spaces that have gone are pruned on the way.
    ///
    /// The list is copied and its lock let go before any space is visited, and
    /// the references come back to be dropped by the caller with no lock held.
    fn mapped_by(&self, except: Option<&AddressSpace>) -> Vec<(Arc<AddressSpace>, u64)> {
        let mut mappers = self.mappers.lock();
        mappers.retain(|mapper| mapper.space.strong_count() > 0);
        mappers
            .iter()
            .filter(|mapper| {
                except.is_none_or(|space| !core::ptr::eq(mapper.space.as_ptr(), space))
            })
            .filter_map(|mapper| Some((mapper.space.upgrade()?, mapper.object)))
            .collect()
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
    /// object at pages `0..count`, and hand back what `from` must still have
    /// forgotten.
    ///
    /// Moved, not copied or shared: each frame leaves `from`'s list and joins
    /// this one with the reference it already had, so no page is allocated,
    /// copied or released. What `mremap` does with a private mapping it
    /// resizes, so that the region can be given an object of its own length
    /// without the cost of its contents. A frame a `fork` child still shares
    /// keeps its count above one, and the moved region keeps its
    /// copy-on-write marking, so the next write copies it as it would have.
    ///
    /// The [`Retired`] names the pages that left `from`, which every other
    /// space mapping `from` must forget — not for this object's sake but for
    /// its own: when this object later releases one of them, it asks only the
    /// spaces that map *it*. It releases nothing; give it to `from`'s
    /// [`Vmo::retire`] once the caller's lock is gone.
    ///
    /// Pages past this object's end stay in `from`. `from` must not be this
    /// object: the two locks are taken one after the other, never together.
    ///
    /// # Errors
    ///
    /// [`HeldPage`], moving nothing, if any page of the range is held. A
    /// device reaches a held page at its frame, and a mapping moved to a new
    /// object would fault in a different frame at the same place: the program
    /// and the device would silently stop sharing the page. Leaving the held
    /// page behind in `from` is exactly that split, so the whole move is
    /// refused rather than part of it, as a held page's `replace` is.
    pub(crate) fn adopt_pages(
        &self,
        from: &Vmo,
        first: u64,
        count: u64,
    ) -> Result<Retired, HeldPage> {
        let count = count.min(self.len_pages());
        let end = first.saturating_add(count);
        let moved = {
            let mut source = from.pages.lock();
            if let Some((&index, _)) = source.held.range(first..end).next() {
                return Err(HeldPage { index });
            }
            let mut moved = source.frames.split_off(&first);
            let mut beyond = moved.split_off(&end);
            source.frames.append(&mut beyond);
            moved
        };
        {
            let mut pages = self.pages.lock();
            for (&index, &frame) in &moved {
                let _ = pages.frames.insert(index - first, frame);
            }
        }
        Ok(Retired {
            frames: moved,
            release: false,
        })
    }

    /// Undo [`Vmo::adopt_pages`]: move every page of this object back into
    /// `to`, at `first` onwards.
    ///
    /// For an `mremap` that fails after it has moved the pages: the frames
    /// never changed, so the translations to them were never wrong, and the
    /// [`Retired`] the move handed out is dropped rather than retired.
    pub(crate) fn return_pages(&self, to: &Vmo, first: u64) {
        let moved = core::mem::take(&mut self.pages.lock().frames);
        let mut pages = to.pages.lock();
        for (index, frame) in moved {
            let _ = pages.frames.insert(first.saturating_add(index), frame);
        }
    }

    /// Hold `pages` pages from `first` in place, for a device to be given
    /// their addresses.
    ///
    /// Each page is committed, and copied first if a `fork` left it shared,
    /// so that the frame a device reads and writes is the one this object
    /// names and no one else's. While a [`Held`] for a page lives, the page
    /// keeps that frame: decommitting skips it, a copy-on-write replace is
    /// refused, a `fork` copies it rather than sharing it, and `mremap`
    /// refuses to move it. Holds nest; a page goes back to ordinary when the
    /// last one is dropped.
    ///
    /// A copied page's shared original leaves in three phases, so a mapping
    /// that still reached the original faults the held copy in: the program
    /// and the device see one page.
    ///
    /// The guard keeps the object alive, and with it every frame it holds, so
    /// forgetting a guard — what a caller does when a device may still hold
    /// the addresses — keeps the frames out of the allocator for good.
    ///
    /// Must not be called holding a spin lock.
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

        let mut displaced = Retired::nothing();
        let outcome = {
            let mut state = self.pages.lock();
            if (first..end).any(|index| state.held.get(&index) == Some(&u32::MAX)) {
                return Err(VmoError::OutOfMemory);
            }
            let outcome = (first..end).try_for_each(|index| {
                frames.push(state.unshared(index, &mut displaced)?);
                Ok(())
            });
            if outcome.is_ok() {
                for index in first..end {
                    *state.held.entry(index).or_insert(0) += 1;
                }
            }
            outcome
        };
        // Whatever was copied before a failure is copied all the same, and
        // its original has to leave properly either way.
        if !displaced.is_empty() {
            self.retire(displaced, None);
        }
        outcome?;

        Ok(Held {
            vmo: Arc::clone(self),
            first,
            frames,
        })
    }
}

impl Pages {
    /// The frame page `index` can be held at: its own if nobody else holds a
    /// reference to it, or an exclusive copy otherwise, with the shared
    /// original put in `displaced` to be retired.
    fn unshared(&mut self, index: u64, displaced: &mut Retired) -> Result<Frame, VmoError> {
        if let Some(frame) = self
            .frames
            .get(&index)
            .copied()
            .filter(|&frame| mm::frame_references(frame) <= 1)
        {
            return Ok(frame);
        }
        let (frame, shared) = self.exclusive(index)?;
        if let Some(shared) = shared {
            let _ = displaced.frames.insert(index, shared);
        }
        Ok(frame)
    }

    /// The frame page `index` can be written through without anyone else
    /// seeing it: a zeroed one if the page has none, or a copy of the one a
    /// `fork` left it sharing — which is taken out and handed back second,
    /// still referenced, for the caller to retire.
    fn exclusive(&mut self, index: u64) -> Result<(Frame, Option<Frame>), VmoError> {
        let fresh = mm::allocate_frames(0).ok_or(VmoError::OutOfMemory)?;
        match self.frames.insert(index, fresh) {
            Some(shared) => {
                mm::copy_frame(fresh, shared);
                Ok((fresh, Some(shared)))
            }
            None => {
                mm::zero_frame(fresh);
                Ok((fresh, None))
            }
        }
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
    ///
    /// No shootdown: an object is dropped when the last space naming it lets
    /// it go, and every space takes its translations of an object down, and
    /// waits for their shootdown, before it does.
    fn drop(&mut self) {
        for frame in self.pages.get_mut().frames.values() {
            let _ = mm::release_frame(*frame);
        }
    }
}
