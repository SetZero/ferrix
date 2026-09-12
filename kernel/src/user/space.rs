//! A process's address space: page tables, and the map that says what is in
//! them.
//!
//! `docs/ARCHITECTURE.md` §3 names this as one of the nine kernel objects and
//! §4 says what it is made of — "a red-black interval tree of `Vma`s, each
//! naming a VMO, an offset, a protection and a share mode". The tree is
//! `libs/vma`, host-tested for four stages as the vmap arena's allocator and
//! used here for the first time as what it was written for.
//!
//! # Why this has a lock and the kernel's tables do not need one of their own
//!
//! [`crate::mm::map_in`] documents itself as taking no lock, because the tree
//! it was written for — a processor's identity map during bring-up — is one
//! nobody has installed and therefore nobody can walk. A user address space is
//! the opposite: it is installed on whichever processors are running its
//! threads, and their `MMU`s walk it in hardware while another processor maps
//! into it. So each address space carries a lock of its own, held across the
//! pair of operations that must not be split — reshaping the map and changing
//! the tables to match — because a fault that arrives between them would find
//! a region the tables do not have or a mapping the map does not know about.
//!
//! The lock is per address space rather than global: two processes faulting at
//! once contend for nothing, which is the property a compiler workload needs.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use ferrix_bootinfo::{PAGE_SIZE, USER_VIRT_END, is_user_address};
use ferrix_frame::Frame;
use ferrix_paging::MapFlags;
use ferrix_sync::SpinLock;
use ferrix_vma::{Backing, PageRange, VmaFlags};

use crate::arch;
use crate::mm;
use crate::user::vmo::{Vmo, VmoError};

/// Why an address space operation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SpaceError {
    /// No frame was available for a page table or a root.
    OutOfMemory,
    /// The range is not in the half of the address space user code gets.
    NotUserRange(u64),
    /// The range overlaps something already mapped, or is malformed.
    BadRange,
    /// The faulting address is in no region: the process gets a `SIGSEGV`.
    NotMapped(u64),
    /// The access is not one the region's permissions allow.
    Refused(u64),
    /// The backing object could not produce the page.
    Backing(VmoError),
}

impl fmt::Display for SpaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpaceError::OutOfMemory => f.write_str("no frame available"),
            SpaceError::NotUserRange(at) => write!(f, "{at:#x} is not a user address"),
            SpaceError::BadRange => f.write_str("the range is malformed or already mapped"),
            SpaceError::NotMapped(at) => write!(f, "nothing is mapped at {at:#x}"),
            SpaceError::Refused(at) => write!(f, "the access at {at:#x} is not permitted"),
            SpaceError::Backing(why) => write!(f, "the backing object refused: {why}"),
        }
    }
}

/// What a fault was trying to do, which decides whether it is allowed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Access {
    /// The access wrote.
    pub(crate) write: bool,
    /// The access fetched an instruction.
    pub(crate) execute: bool,
}

impl Access {
    /// A read.
    pub(crate) const READ: Access = Access {
        write: false,
        execute: false,
    };
    /// A write.
    pub(crate) const WRITE: Access = Access {
        write: true,
        execute: false,
    };
}

/// The parts that the lock protects.
#[derive(Debug)]
struct Inner {
    /// What is mapped where.
    map: ferrix_vma::AddressSpace,
    /// The objects those regions name, by the id the region carries.
    objects: BTreeMap<u64, Arc<Vmo>>,
    /// The next object id to hand out. Never zero, which `Backing` reserves
    /// for private memory with no named object.
    next_id: u64,
}

/// One process's address space.
#[derive(Debug)]
pub(crate) struct AddressSpace {
    /// Physical frame of the root table.
    root: Frame,
    inner: SpinLock<Inner>,
}

impl AddressSpace {
    /// An empty address space, with the kernel reachable from it.
    ///
    /// # Errors
    ///
    /// [`SpaceError::OutOfMemory`] if there is no frame for the root.
    pub(crate) fn new() -> Result<Arc<AddressSpace>, SpaceError> {
        let root = mm::allocate_frames(0).ok_or(SpaceError::OutOfMemory)?;
        mm::zero_frame(root);

        // Before anything is mapped into it: on x86-64 this is what puts the
        // kernel's half in reach, and a trap taken in this address space
        // before it ran would have nowhere to go.
        arch::prepare_user_root(root * PAGE_SIZE);

        let map = ferrix_vma::AddressSpace::new(0, USER_VIRT_END).map_err(|_| {
            // The window is a compile-time constant of the layout, so this is
            // unreachable in practice; reported rather than panicked because a
            // kernel has no supervisor to restart it.
            SpaceError::BadRange
        })?;

        Ok(Arc::new(AddressSpace {
            root,
            inner: SpinLock::new(Inner {
                map,
                objects: BTreeMap::new(),
                next_id: 1,
            }),
        }))
    }

    /// The physical address of the root table, for whoever installs it.
    pub(crate) fn root_table(&self) -> u64 {
        self.root * PAGE_SIZE
    }

    /// Install this address space on the processor that is running.
    ///
    /// Every page faulted in so far has been reachable only through the direct
    /// map, because the kernel was walking these tables in software. After
    /// this the *hardware* walks them, and a user virtual address means
    /// something on this processor.
    ///
    /// What this does not do is change privilege level. That is the other half
    /// of stage 6 and a separate thing entirely: a processor can translate
    /// through a user address space while still running the kernel's own code,
    /// and it has to be able to — a fault taken in user mode is handled by
    /// kernel code running in the address space that faulted.
    ///
    /// # Safety
    ///
    /// Something must hold a reference to this address space for as long as it
    /// stays installed. The tables are freed when the last [`Arc`] goes, and a
    /// processor whose root register still names freed frames is walking
    /// memory the allocator has handed to somebody else.
    ///
    /// The caller must also not be preempted into a context expecting a
    /// different address space, which until the scheduler knows about address
    /// spaces means masking interrupts across the window.
    pub(crate) unsafe fn install(&self) {
        // SAFETY: the root was made by `new`, so `prepare_user_root` has run
        // on it and the kernel is reachable through it on the architecture
        // that needs that; the caller guarantees it outlives the installation.
        unsafe { arch::install_user_root(self.root * PAGE_SIZE) };
    }
}

/// Leave whatever address space this processor was translating through.
///
/// After this no user address translates here, which is the state a kernel
/// thread runs in.
///
/// # Safety
///
/// Nothing on this processor may still need a user address.
pub(crate) unsafe fn uninstall() {
    // SAFETY: the caller guarantees no user address is wanted, and the kernel
    // is reachable without one on every architecture.
    unsafe { arch::uninstall_user_root() };
}

impl AddressSpace {
    /// How many regions the map holds.
    pub(crate) fn region_count(&self) -> usize {
        self.inner.lock().map.region_count()
    }

    /// Map `len` bytes of fresh anonymous memory at `at`, and return the id of
    /// the object created for it.
    ///
    /// Nothing is committed: the pages arrive on first touch, through
    /// [`AddressSpace::fault`]. That is what makes a large `mmap` cheap, and
    /// it is why the region is inserted before any frame is allocated.
    ///
    /// # Errors
    ///
    /// [`SpaceError::NotUserRange`] outside the user half,
    /// [`SpaceError::BadRange`] if it overlaps or is malformed, and
    /// [`SpaceError::OutOfMemory`] if the object cannot be made.
    pub(crate) fn map_anonymous(
        &self,
        at: u64,
        len: u64,
        flags: VmaFlags,
    ) -> Result<u64, SpaceError> {
        if !is_user_address(at) || at.checked_add(len).is_none_or(|end| end > USER_VIRT_END) {
            return Err(SpaceError::NotUserRange(at));
        }
        let range = PageRange::from_len(at, len).map_err(|_| SpaceError::BadRange)?;

        let mut inner = self.inner.lock();
        let id = inner.next_id;
        inner.next_id = inner.next_id.saturating_add(1);

        let pages = len.div_ceil(PAGE_SIZE);
        let vmo = Vmo::new_anonymous(pages);

        inner
            .map
            .insert(range, flags, Backing::Anonymous { id, offset: 0 })
            .map_err(|_| SpaceError::BadRange)?;
        let _ = inner.objects.insert(id, vmo);
        Ok(id)
    }

    /// A second address space holding everything this one does, shared
    /// copy-on-write: the memory half of `fork`.
    ///
    /// Nothing is copied. Every private writable region is marked
    /// copy-on-write in *both* spaces — `libs/vma`'s `clone_for_fork` does the
    /// marking, and it must be both, because the parent's own writes have to
    /// stop reaching pages the child can now see. Each private object is
    /// cloned page-list and all, with a reference taken on every committed
    /// frame, so a write on either side copies the page into that side's own
    /// object and leaves the other's alone. A `MAP_SHARED` object is not
    /// cloned but shared, since writes through it are meant to be visible.
    ///
    /// # Why the parent's mappings are taken down
    ///
    /// Marking the map is not enough. The parent's page tables still hold
    /// *writable* translations to the pages it has just agreed to share, and a
    /// write through one of those would never reach [`AddressSpace::fault`] at
    /// all — it would land in a page the child can read. So every region now
    /// marked copy-on-write is unmapped in the parent, and the next access
    /// re-faults and is reinstalled read-only because `cow` is set.
    ///
    /// Unmapping rather than write-protecting in place costs the parent one
    /// extra fault per page — a read that would have hit now faults once — and
    /// is what [`crate::mm`] offers today: `map_in` refuses to overwrite a live
    /// mapping, by design, and there is no `protect_in`. Adding one is the
    /// obvious improvement and changes nothing about what is correct here.
    ///
    /// # What this does not yet do
    ///
    /// Invalidate other processors' translations. Nothing runs in a user
    /// address space yet, so there are none to invalidate; when threads arrive
    /// this needs the shootdown [`crate::smp::flush_tlb_everywhere`] does for
    /// the kernel's own tables, scoped to the processors running this space.
    /// Until then a parent that had been installed somewhere would keep a
    /// stale writable entry, which is the one thing between this and a working
    /// `fork(2)`.
    ///
    /// # Errors
    ///
    /// [`SpaceError::OutOfMemory`] if there is no frame for the child's root,
    /// and [`SpaceError::Backing`] if an object cannot take a reference on one
    /// of its pages. Both leave the parent usable: the child's root is freed,
    /// and a map marked copy-on-write with nobody to share with merely costs
    /// the parent a fault per page that then declines to copy anything,
    /// because the refcount says it is the only holder.
    pub(crate) fn fork(&self) -> Result<Arc<AddressSpace>, SpaceError> {
        let mut inner = self.inner.lock();

        // The child's root first, because it is the step most likely to fail
        // and the only one that fails without leaving a trace.
        let root = mm::allocate_frames(0).ok_or(SpaceError::OutOfMemory)?;
        mm::zero_frame(root);
        arch::prepare_user_root(root * PAGE_SIZE);

        // Then the objects, while the map is still untouched, so that a
        // failure here has changed nothing the parent can observe.
        let mut objects = BTreeMap::new();
        for (&id, vmo) in &inner.objects {
            let forked = if shared_object(&inner.map, id) {
                Arc::clone(vmo)
            } else {
                match vmo.fork() {
                    Ok(forked) => forked,
                    Err(why) => {
                        mm::deallocate_frames(root, 0);
                        return Err(SpaceError::Backing(why));
                    }
                }
            };
            let _ = objects.insert(id, forked);
        }

        // Only now the parent's map is marked and copied, and its writable
        // translations to the shared pages taken down.
        let map = inner.map.clone_for_fork();
        for region in inner.map.iter() {
            if region.cow {
                let _ = mm::unmap_in(
                    self.root * PAGE_SIZE,
                    region.range.start(),
                    region.range.bytes(),
                );
            }
        }

        // Read before the lock goes, because the child inherits it.
        let next_id = inner.next_id;

        // The parent's writable translations to the shared pages are out of
        // its tables, but may still be in some processor's TLB — and a write
        // through one of those would reach a page the child can read, which is
        // the whole thing fork just promised would not happen. Dropped here,
        // where the lock is about to go out of scope, rather than inside the
        // loop above: one invalidation covers every region.
        drop(inner);
        invalidate();

        Ok(Arc::new(AddressSpace {
            root,
            inner: SpinLock::new(Inner {
                map,
                objects,
                // Continued rather than restarted, so that an id means the
                // same object in a parent and a child for as long as they
                // share one. Two spaces may hand out the same id afterwards,
                // which is fine: an id is only ever looked up in its own
                // space's table.
                next_id,
            }),
        }))
    }

    /// Resolve a fault at `address`, and let the faulting instruction retry.
    ///
    /// This is demand paging, and stage 3 already proved the mechanism on the
    /// kernel's own tables: a fault arrives, the handler maps the faulting
    /// address, the instruction runs again. What is new is that the decision
    /// of *what* to map comes from the region map rather than from a fixed
    /// window — find the region, ask its object for the page, install it with
    /// the region's permissions.
    ///
    /// # Errors
    ///
    /// [`SpaceError::NotMapped`] if no region covers the address, which is the
    /// segmentation fault, and [`SpaceError::Refused`] if the region does not
    /// permit the access, which is the other one.
    pub(crate) fn fault(&self, address: u64, access: Access) -> Result<(), SpaceError> {
        let inner = self.inner.lock();

        let region = *inner
            .map
            .find(address)
            .ok_or(SpaceError::NotMapped(address))?;

        if access.write && !region.flags.write {
            return Err(SpaceError::Refused(address));
        }
        if access.execute && !region.flags.execute {
            return Err(SpaceError::Refused(address));
        }
        // A plain read needs a readable region. Without this, a read of a
        // `PROT_NONE` region -- every guard page, and every `mprotect` to
        // nothing -- commits a zero page and maps it, handing the program
        // memory where it should get `SIGSEGV`. Silent in the way a mapping
        // more permissive than the map always is: nothing that worked stops
        // working, so nothing notices.
        if !access.write && !access.execute && !region.flags.read {
            return Err(SpaceError::Refused(address));
        }

        let page = address & !(PAGE_SIZE - 1);
        let into_region = page.saturating_sub(region.range.start());

        let (id, offset) = match region.backing {
            Backing::Anonymous { id, offset } => (id, offset.saturating_add(into_region)),
            // Everything else wants a page cache or a device, which are later
            // stages. Reported rather than panicked: an unhandled backing is a
            // kernel bug, but killing the process beats stopping the machine.
            _ => return Err(SpaceError::NotMapped(address)),
        };

        let vmo = Arc::clone(
            inner
                .objects
                .get(&id)
                .ok_or(SpaceError::NotMapped(address))?,
        );
        let index = offset / PAGE_SIZE;

        // Copy-on-write, and **this branch is above the present-page check on
        // purpose**. A write to a page that is present but deliberately
        // read-only is exactly the fault that has to copy; returning early
        // because the page translates would send the instruction back to fault
        // forever. The region's own `flags.write` was already checked above,
        // so reaching here means the process is entitled to write and the
        // read-only entry is the kernel's device rather than the region's
        // permission.
        if access.write && region.cow {
            let shared = vmo.commit(index).map_err(SpaceError::Backing)?;

            // The one real decision. A page nobody else holds any more needs
            // no copy: the other side has already copied it, or unmapped, or
            // exited, and copying would allocate a frame in order to duplicate
            // data this space is the sole owner of.
            let frame = if mm::frame_references(shared) > 1 {
                let copy = mm::allocate_frames(0).ok_or(SpaceError::OutOfMemory)?;
                mm::copy_frame(copy, shared);
                // Replacing gives back this object's reference to the shared
                // page, which is what leaves the other holder as the last one.
                let _ = vmo.replace(index, copy);
                copy
            } else {
                shared
            };

            // `map_in` refuses to overwrite a live mapping, deliberately, so
            // the read-only entry comes down before the writable one goes in.
            // The frames are the object's and are not freed by this.
            let _ = mm::unmap_in(self.root * PAGE_SIZE, page, PAGE_SIZE);
            mm::map_in(
                self.root * PAGE_SIZE,
                page,
                frame * PAGE_SIZE,
                PAGE_SIZE,
                MapFlags {
                    read: true,
                    write: true,
                    execute: region.flags.execute,
                    user: true,
                    global: false,
                    device: false,
                },
            )
            .map_err(|_| SpaceError::OutOfMemory)?;

            // A translation that existed a moment ago has just been replaced
            // by a more permissive one, so every processor's cached copy of
            // the old one has to go. **This is not optional and it is not a
            // performance matter**: the entry that was there says read-only,
            // and the instruction that faulted is about to retry its write.
            // If it finds the stale entry it faults again, arrives here again,
            // finds one holder and no copy to make, installs the same writable
            // entry again, and retries into the same stale entry — forever.
            // The symptom is a hang with no message, which is the hardest kind
            // to attribute.
            drop(inner);
            invalidate();
            return Ok(());
        }

        // Already present, and the fault was spurious: another processor
        // resolved this same page between the fault and this lock, or the
        // faulting processor walked a stale TLB entry. Both happen, and
        // neither is an error -- the instruction retries and succeeds.
        //
        // Safe to return early only because the copy-on-write case above has
        // already been taken: every page that reaches here carries its own
        // region's permissions, so a fault on a present one asked for nothing
        // the mapping does not already grant.
        if mm::translate_in(self.root * PAGE_SIZE, page).is_some() {
            return Ok(());
        }

        let frame = vmo.commit(index).map_err(SpaceError::Backing)?;

        // A copy-on-write region is installed read-only however writable the
        // region is, so that the *next* write faults here again and can copy.
        // Until fork exists nothing sets `cow`, and this is what will make it
        // work when it does.
        let writable = region.flags.write && !region.cow;
        let flags = MapFlags {
            read: true,
            write: writable,
            execute: region.flags.execute,
            user: true,
            global: false,
            device: false,
        };

        mm::map_in(
            self.root * PAGE_SIZE,
            page,
            frame * PAGE_SIZE,
            PAGE_SIZE,
            flags,
        )
        .map_err(|_| SpaceError::OutOfMemory)?;

        drop(inner);
        Ok(())
    }

    /// Unmap `len` bytes at `at`, giving back the pages and the tables.
    ///
    /// # Errors
    ///
    /// [`SpaceError::BadRange`] if the range is malformed.
    pub(crate) fn unmap(&self, at: u64, len: u64) -> Result<(), SpaceError> {
        let range = PageRange::from_len(at, len).map_err(|_| SpaceError::BadRange)?;
        // Three phases, and the order between them is the whole point:
        // translations down, then every processor told, and only then the
        // frames given back. It used to be one phase that freed the pages
        // *before* it took their translations down, under a comment claiming
        // it did the opposite — which is the bug `mm::unmap_kernel` documents
        // at length: a frame handed to the allocator while another processor
        // can still reach it through a cached translation is two owners of one
        // page, and the symptom turns up in whichever of them writes second.
        //
        // Phase one, under the lock: take the range out of the map and take
        // its translations out of the tables. What was removed is remembered
        // rather than acted on, because the acting has to happen after the
        // invalidation and the invalidation may not hold a lock.
        let mut freeing: Vec<(u64, u64, u64)> = Vec::new();
        {
            let mut inner = self.inner.lock();
            let removed = inner.map.remove(range).map_err(|_| SpaceError::BadRange)?;
            for unmapping in removed {
                let _ = mm::unmap_in(
                    self.root * PAGE_SIZE,
                    unmapping.range.start(),
                    unmapping.range.bytes(),
                );
                if let Backing::Anonymous { id, offset } = unmapping.backing {
                    freeing.push((id, offset / PAGE_SIZE, unmapping.range.bytes() / PAGE_SIZE));
                }
            }
        }

        // Phase two. Nothing can reach these pages through this address space
        // any more, on any processor.
        invalidate();

        // Phase three: give the pages back, not just the mapping. A process
        // that unmaps half its heap expects the memory returned now, not when
        // the other half goes.
        //
        // Only if nothing else holds the object: a page of memory shared with
        // another address space is not this unmapper's to take away, and a
        // strong count of one means this map is the only holder.
        let mut inner = self.inner.lock();
        for (id, first, pages) in freeing {
            if let Some(vmo) = inner.objects.get(&id)
                && Arc::strong_count(vmo) == 1
            {
                let _ = vmo.decommit_range(first, pages);
            }
        }

        // An object nothing maps any more is dropped here, which releases
        // every page it committed. Split borrows: the predicate reads the map
        // while the objects are being written.
        let Inner { map, objects, .. } = &mut *inner;
        objects.retain(|id, _| still_named(map, *id));
        Ok(())
    }
}

/// Drop every processor's cached translations for this address space.
///
/// Called after a change that takes a translation down or makes it *less*
/// permissive. Adding a translation where there was none needs nothing: a
/// processor that faults on an absent page walks the tables and finds the new
/// entry, because no architecture here caches the absence of one.
///
/// # Why this is the global flush, which the switch path must never use
///
/// `arch::flush_tlb` discards the kernel's global entries as well, and
/// broadcasts. In the address-space switch path that would be simply wrong —
/// the global entries are exactly what has to survive a root register write,
/// and stage 4 has the writeup of the bug that comes from getting it wrong. It
/// is right *here* for the opposite reason: a mapping change has to reach
/// processors that are not this one, and this is the call that already knows
/// how — broadcast in hardware on the Arm pair, by inter-processor interrupt on
/// x86-64. [`crate::mm::unmap_kernel`] uses it for the kernel's own tables on
/// the same argument.
///
/// It is coarser than it needs to be, in two ways worth naming because both
/// are straightforward to fix and neither is a correctness question. It
/// invalidates everything rather than the one page that changed, and it tells
/// every processor rather than the ones with this space installed — which
/// wants a `CpuSet` on the address space, maintained by
/// [`AddressSpace::install`] and [`uninstall`]. Until a user thread exists
/// there is nothing to measure the difference on.
///
/// Must not be called holding this address space's lock, for the reason
/// [`crate::smp::flush_tlb_everywhere`] gives about locks in general.
fn invalidate() {
    crate::smp::flush_tlb_everywhere();
}

/// Whether object `id` is named by a region that shares it.
///
/// One region is enough. `MAP_SHARED` is a property of the mapping rather than
/// of the object, so in principle an object could be mapped shared in one
/// place and private in another; when that happens the object is the shared
/// one and the private mapping of it has to see the shared writes, because
/// that is what the other mapping was promised.
fn shared_object(map: &ferrix_vma::AddressSpace, id: u64) -> bool {
    map.iter().any(|region| {
        region.flags.shared
            && match region.backing {
                Backing::Anonymous { id: named, .. } | Backing::File { id: named, .. } => {
                    named == id
                }
                Backing::Device { .. } => false,
            }
    })
}

// ---------------------------------------------------------------------------
// What `mmap`, `munmap` and `mprotect` need on top of the above.
//
// Kept in an impl block of its own because it was added by stage 7 against a
// stage 6 interface that was already working: nothing here changes the
// behaviour of anything above it.
// ---------------------------------------------------------------------------

impl AddressSpace {
    /// Reserve `len` bytes wherever they fit, and say where that was.
    ///
    /// `mmap` with a null address. The search and the insertion happen under
    /// one lock, which is the whole reason this is a method rather than
    /// `find_free` followed by [`AddressSpace::map_anonymous`]: two threads
    /// calling `mmap` at once would otherwise be told about the same hole and
    /// the second insertion would fail, or worse, succeed.
    ///
    /// `hint` is advisory. A program that passes a non-null address without
    /// `MAP_FIXED` is asking, not telling, and Linux is free to answer
    /// somewhere else — so a hint that does not fit is not an error.
    ///
    /// # Errors
    ///
    /// [`SpaceError::BadRange`] if the length is malformed, and
    /// [`SpaceError::OutOfMemory`] if there is no hole big enough.
    pub(crate) fn map_anywhere(
        &self,
        hint: Option<u64>,
        len: u64,
        flags: VmaFlags,
    ) -> Result<u64, SpaceError> {
        if len == 0 {
            return Err(SpaceError::BadRange);
        }
        let mut inner = self.inner.lock();

        let at = inner
            .map
            .find_free(len, PAGE_SIZE, hint)
            .ok_or(SpaceError::OutOfMemory)?;
        if !is_user_address(at) || at.checked_add(len).is_none_or(|end| end > USER_VIRT_END) {
            return Err(SpaceError::NotUserRange(at));
        }
        let range = PageRange::from_len(at, len).map_err(|_| SpaceError::BadRange)?;

        let id = inner.next_id;
        inner.next_id = inner.next_id.saturating_add(1);
        let vmo = Vmo::new_anonymous(len.div_ceil(PAGE_SIZE));

        inner
            .map
            .insert(range, flags, Backing::Anonymous { id, offset: 0 })
            .map_err(|_| SpaceError::BadRange)?;
        let _ = inner.objects.insert(id, vmo);
        Ok(at)
    }

    /// Change the permissions of an already-mapped range.
    ///
    /// # Why the translations are taken down rather than rewritten
    ///
    /// The same reason `clone_for_fork` takes the parent's down. The page
    /// tables hold translations carrying the *old* permissions, and a region
    /// that has just become read-only is still writable through every one of
    /// them until something invalidates it. Rewriting each leaf in place would
    /// be faster and is what a later stage should do; unmapping costs one
    /// fault per touched page and is correct with the primitives `crate::mm`
    /// offers today, which is the right trade while there is no benchmark to
    /// answer to.
    ///
    /// Note this is `mprotect`'s semantics and not `mmap`'s: the range must
    /// already be mapped, and a hole in it is an error rather than a
    /// reservation.
    ///
    /// # Errors
    ///
    /// [`SpaceError::BadRange`] if the range is malformed, or is not wholly
    /// mapped.
    pub(crate) fn protect(&self, at: u64, len: u64, flags: VmaFlags) -> Result<(), SpaceError> {
        if !is_user_address(at) || at.checked_add(len).is_none_or(|end| end > USER_VIRT_END) {
            return Err(SpaceError::NotUserRange(at));
        }
        let range = PageRange::from_len(at, len).map_err(|_| SpaceError::BadRange)?;
        let mut inner = self.inner.lock();

        inner
            .map
            .protect(range, flags)
            .map_err(|_| SpaceError::BadRange)?;

        // Every page in the range re-faults and is reinstalled with the
        // permissions the map now carries.
        let _ = mm::unmap_in(self.root * PAGE_SIZE, range.start(), range.bytes());
        Ok(())
    }

    /// The highest address any region reaches, or `None` for an empty space.
    ///
    /// `brk` needs it to place the heap above everything the ELF loader
    /// mapped, without the loader and the heap having to agree on a number.
    pub(crate) fn highest_mapped(&self) -> Option<u64> {
        self.inner
            .lock()
            .map
            .iter()
            .map(|vma| vma.range.end())
            .max()
    }
}

/// Whether any region still names object `id`.
fn still_named(map: &ferrix_vma::AddressSpace, id: u64) -> bool {
    map.iter().any(|region| match region.backing {
        Backing::Anonymous { id: named, .. } | Backing::File { id: named, .. } => named == id,
        Backing::Device { .. } => false,
    })
}

impl Drop for AddressSpace {
    /// Tear the whole space down: every mapping, every object, every table.
    ///
    /// The objects go first and the tables second. Dropping an object releases
    /// its frames, and a frame released while a translation to it still exists
    /// is only safe because nothing is running in this address space — an
    /// `AddressSpace` is dropped when its last reference goes, and a running
    /// thread is a reference.
    fn drop(&mut self) {
        let inner = self.inner.get_mut();

        for region in inner.map.iter() {
            let _ = mm::unmap_in(
                self.root * PAGE_SIZE,
                region.range.start(),
                region.range.bytes(),
            );
        }
        inner.objects.clear();

        // The root itself. On x86-64 its upper half names the kernel's own
        // tables, which are emphatically not this space's to free -- but
        // `unmap_in` only ever walked the ranges above, all of which are in
        // the user half, so nothing of the kernel's was ever reached.
        mm::deallocate_frames(self.root, 0);
    }
}
