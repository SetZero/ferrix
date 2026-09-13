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

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use ferrix_bootinfo::{PAGE_SIZE, USER_VIRT_END, is_user_address};
use ferrix_frame::Frame;
use ferrix_paging::MapFlags;
use ferrix_sync::SpinLock;
use ferrix_vma::{Backing, PageRange, Unmapping, Vma, VmaFlags};

use crate::arch;
use crate::mm;
use crate::user::vmo::{Vmo, VmoError};

/// The lowest address a program may map anything at: 64 KiB, the
/// `vm.mmap_min_addr` Linux distributions ship.
///
/// A null pointer the kernel follows must fault, not read memory a program
/// chose to put there, and with no SMAP or PAN a kernel dereference of a low
/// user address reads whatever is mapped at it. So the bottom of the user half
/// is kept out of every map, not merely left free: `MAP_FIXED` cannot place a
/// page there and a hint cannot round down into it. Nothing Linux would load is
/// refused for it — a static ARM binary is linked at exactly this address, and
/// the other two architectures link theirs higher.
pub(crate) const MMAP_MIN_ADDR: u64 = 0x1_0000;

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
    /// The objects `vmo_map` put here, which the Linux calls that reshape a
    /// region -- `mremap`, `mprotect` -- may not touch: a native VMO's size
    /// and the rights its protection stands for belong to its handle.
    native: BTreeSet<u64>,
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

        let map = ferrix_vma::AddressSpace::new(MMAP_MIN_ADDR, USER_VIRT_END).map_err(|_| {
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
                native: BTreeSet::new(),
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
    /// Device regions are shared, not copied: they own no frames, and
    /// `ferrix_vma`'s `needs_cow` never marks them copy-on-write, so a child
    /// that touches one faults in the same registers its parent reaches.
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

        // Read before the lock goes, because the child inherits them. A
        // native object is shared, so the child names the same one.
        let next_id = inner.next_id;
        let native = inner.native.clone();

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
                native,
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

        if !permits(region.flags, access) {
            return Err(SpaceError::Refused(address));
        }

        let page = address & !(PAGE_SIZE - 1);
        let into_region = page.saturating_sub(region.range.start());

        let (id, offset) = match region.backing {
            Backing::Anonymous { id, offset } => (id, offset.saturating_add(into_region)),
            // A device region has no object: its page is the device's own.
            Backing::Device { physical } => {
                return self.fault_device(page, physical.saturating_add(into_region), region.flags);
            }
            // A file wants a page cache, which is a later stage. Reported rather
            // than panicked: an unhandled backing is a kernel bug, but killing
            // the process beats stopping the machine.
            Backing::File { .. } => return Err(SpaceError::NotMapped(address)),
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

    /// Run `touch` on the direct-map address of the byte at `address`, with the
    /// page held where it is for as long as `touch` runs.
    ///
    /// What a copy to or from user memory goes through. Faulting the page in
    /// and then translating it is not enough on its own, because the lock goes
    /// between the two and again before the copy: a thread sharing this space
    /// can `munmap` the page in either gap, and the copy then reads or writes
    /// a frame that has been given back and handed to somebody else. So the
    /// translation is taken again under the lock, and `touch` runs before the
    /// lock goes. Every way a frame this space names is given back -- `unmap`,
    /// a copy-on-write replacement, `fork` re-sharing a page -- first takes its
    /// translation down under this same lock, so a translation found under it
    /// names a live frame until it is released.
    ///
    /// The page is faulted in again if what the lock finds does not do: gone
    /// since the fault, or, for a write, a copy-on-write page some other space
    /// still holds, which a `fork` in between would have left. The fault
    /// always makes progress on its own, so the retry ends unless another
    /// thread keeps undoing it.
    ///
    /// `touch` runs under a spin lock: it must not sleep, fault, or take this
    /// space's lock. A copy between the direct map and a kernel buffer does
    /// none of those.
    ///
    /// # Errors
    ///
    /// As [`AddressSpace::fault`].
    pub(crate) fn with_page<R>(
        &self,
        address: u64,
        access: Access,
        touch: impl FnOnce(u64) -> R,
    ) -> Result<R, SpaceError> {
        loop {
            self.fault(address, access)?;

            let inner = self.inner.lock();
            let region = *inner
                .map
                .find(address)
                .ok_or(SpaceError::NotMapped(address))?;
            if !permits(region.flags, access) {
                return Err(SpaceError::Refused(address));
            }
            let Some(physical) = mm::translate_in(self.root * PAGE_SIZE, address) else {
                continue;
            };
            if access.write && region.cow && mm::frame_references(physical / PAGE_SIZE) > 1 {
                continue;
            }
            let answer = touch(mm::direct_map(physical));
            drop(inner);
            return Ok(answer);
        }
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
        let mut freeing: Vec<Freeing> = Vec::new();
        {
            let mut inner = self.inner.lock();
            let removed = inner.map.remove(range).map_err(|_| SpaceError::BadRange)?;
            self.take_down(&removed, &mut freeing);
        }

        // Phase two. Nothing can reach these pages through this address space
        // any more, on any processor.
        invalidate();

        // Phase three: give the pages back, not just the mapping.
        self.give_back(freeing);
        Ok(())
    }

    /// Take the translations for `removed` out of the tables, and note the
    /// pages behind them in `freeing` for [`AddressSpace::give_back`].
    ///
    /// Phase one of an unmap, called with the lock held. Nothing is freed
    /// here, because nothing may be until every processor has been told.
    fn take_down(&self, removed: &[Unmapping], freeing: &mut Vec<Freeing>) {
        for unmapping in removed {
            let _ = mm::unmap_in(
                self.root * PAGE_SIZE,
                unmapping.range.start(),
                unmapping.range.bytes(),
            );
            if let Backing::Anonymous { id, offset } = unmapping.backing {
                freeing.push(Freeing {
                    id,
                    first: offset / PAGE_SIZE,
                    pages: unmapping.range.bytes() / PAGE_SIZE,
                });
            }
        }
    }

    /// Give back the pages `freeing` names, and drop every object no region
    /// names any more.
    ///
    /// Phase three of an unmap: after [`invalidate`], and taking the lock
    /// itself. A process that unmaps half its heap expects the memory returned
    /// now, not when the other half goes.
    ///
    /// Only if nothing else holds the object: a page of memory shared with
    /// another address space is not this unmapper's to take away, and a
    /// strong count of one means this map is the only holder.
    fn give_back(&self, freeing: Vec<Freeing>) {
        let mut inner = self.inner.lock();
        for Freeing { id, first, pages } in freeing {
            if let Some(vmo) = inner.objects.get(&id)
                && Arc::strong_count(vmo) == 1
            {
                let _ = vmo.decommit_range(first, pages);
            }
        }

        // An object nothing maps any more is dropped here, which releases
        // every page it committed. Split borrows: the predicate reads the map
        // while the objects are being written.
        let Inner {
            map,
            objects,
            native,
            ..
        } = &mut *inner;
        objects.retain(|id, _| still_named(map, *id));
        native.retain(|id| objects.contains_key(id));
    }
}

/// Pages of one object whose translations an unmap has taken down, and which
/// go back once every processor has been told.
#[derive(Clone, Copy, Debug)]
struct Freeing {
    /// The object.
    id: u64,
    /// Its first page.
    first: u64,
    /// How many.
    pages: u64,
}

/// Where `mremap` may put the region it resizes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Destination {
    /// Where it is, or nowhere: no `MREMAP_MAYMOVE`.
    InPlace,
    /// Where it is if it fits there, wherever it fits otherwise:
    /// `MREMAP_MAYMOVE`.
    Anywhere,
    /// Exactly here, replacing whatever is mapped there: `MREMAP_FIXED`.
    Fixed(u64),
}

impl AddressSpace {
    /// Resize the mapping of `old_len` bytes at `old` to `new_len` bytes,
    /// moving it if `destination` allows, and say where it now is.
    ///
    /// `mremap`. Both lengths are whole pages, and `old..old + old_len` must
    /// lie inside one region: one object, one set of flags, one marking.
    ///
    /// # Nothing is copied
    ///
    /// A private region is given an object of its own, of the new length, and
    /// the pages it had are *moved* into it -- frame numbers out of one list
    /// and into the other, references and all. That is what makes glibc's
    /// `realloc` of a large block cheap, and it is also what keeps a moved
    /// region honest: an object nothing else names cannot be aliased by
    /// another region of the old one, which a region that kept naming the old
    /// object at its old offsets could be once that object's other pieces had
    /// grown or moved too. The region keeps its copy-on-write marking, so a
    /// page it still shares with a `fork` child is copied on the next write
    /// exactly as it would have been.
    ///
    /// A shared region cannot do that -- its pages are the same pages for
    /// whoever else maps the object -- so it keeps its object and its offset,
    /// and the object is grown to cover it, as Linux grows the shmem file
    /// behind shared anonymous memory. Growing it over offsets another region
    /// of this space already names is refused with
    /// [`SpaceError::OutOfMemory`]: those pages would be mapped twice, and an
    /// unmap of either would give back pages the other still shows.
    ///
    /// Either way the old translations come down and the new ones arrive on
    /// fault, with the protection the region always had.
    ///
    /// # Errors
    ///
    /// [`SpaceError::NotMapped`] if the old range is not wholly inside one
    /// region; [`SpaceError::Refused`] if that region is not anonymous memory,
    /// or is a VMO `vmo_map` put there;
    /// [`SpaceError::NotUserRange`] for a fixed destination outside the user
    /// half or off a page boundary; [`SpaceError::BadRange`] for one that
    /// overlaps the old range, or a malformed length; and
    /// [`SpaceError::OutOfMemory`] when the region cannot grow where it is and
    /// may not move, or there is no gap it fits.
    pub(crate) fn remap(
        &self,
        old: u64,
        old_len: u64,
        new_len: u64,
        destination: Destination,
    ) -> Result<u64, SpaceError> {
        let old_range = user_range(old, old_len).ok_or(SpaceError::NotMapped(old))?;
        if new_len == 0 || !new_len.is_multiple_of(PAGE_SIZE) {
            return Err(SpaceError::BadRange);
        }

        let mut freeing: Vec<Freeing> = Vec::new();
        let placed = {
            let mut inner = self.inner.lock();

            let region = *inner.map.find(old).ok_or(SpaceError::NotMapped(old))?;
            if region.range.end() < old_range.end() {
                return Err(SpaceError::NotMapped(old));
            }
            let Backing::Anonymous { id, offset } = region.backing else {
                return Err(SpaceError::Refused(old));
            };
            if inner.native.contains(&id) {
                return Err(SpaceError::Refused(old));
            }
            // Where in the object the old range starts, in bytes.
            let first = offset
                .checked_add(old - region.range.start())
                .ok_or(SpaceError::BadRange)?;
            let shared = region.flags.shared || shared_object(&inner.map, id);

            let at = match destination {
                Destination::Fixed(target) => {
                    let target_range = user_range(target, new_len)
                        .filter(|_| target.is_multiple_of(PAGE_SIZE))
                        .ok_or(SpaceError::NotUserRange(target))?;
                    if target_range.start() < old_range.end()
                        && old_range.start() < target_range.end()
                    {
                        return Err(SpaceError::BadRange);
                    }
                    target
                }
                _ if new_len == old_len => return Ok(old),
                _ if new_len < old_len => old,
                _ if grows_in_place(&inner.map, old, old_len, new_len) => old,
                Destination::Anywhere => inner
                    .map
                    .find_free(new_len, PAGE_SIZE, None)
                    .filter(|&at| user_range(at, new_len).is_some())
                    .ok_or(SpaceError::OutOfMemory)?,
                Destination::InPlace => return Err(SpaceError::OutOfMemory),
            };
            let new_range = PageRange::from_len(at, new_len).map_err(|_| SpaceError::BadRange)?;

            if shared && new_len > old_len {
                let end = first.checked_add(new_len).ok_or(SpaceError::BadRange)?;
                if named_elsewhere(&inner.map, id, first + old_len, end) {
                    return Err(SpaceError::OutOfMemory);
                }
            }
            let vmo = Arc::clone(inner.objects.get(&id).ok_or(SpaceError::NotMapped(old))?);

            // Everything that can be refused has been. Whatever a fixed
            // destination covers goes, as `MAP_FIXED` would take it; then the
            // old range leaves the map and the tables, but not its pages.
            if let Destination::Fixed(_) = destination {
                let removed = inner
                    .map
                    .remove(new_range)
                    .map_err(|_| SpaceError::BadRange)?;
                self.take_down(&removed, &mut freeing);
            }
            let _ = inner
                .map
                .remove(old_range)
                .map_err(|_| SpaceError::BadRange)?;
            let _ = mm::unmap_in(self.root * PAGE_SIZE, old, old_len);

            let backing = if shared {
                vmo.grow_to((first + new_len) / PAGE_SIZE);
                if new_len < old_len {
                    freeing.push(Freeing {
                        id,
                        first: (first + new_len) / PAGE_SIZE,
                        pages: (old_len - new_len) / PAGE_SIZE,
                    });
                }
                Backing::Anonymous { id, offset: first }
            } else {
                let fresh_id = inner.next_id;
                inner.next_id = inner.next_id.saturating_add(1);
                let fresh = Vmo::new_anonymous(new_len / PAGE_SIZE);
                fresh.adopt_pages(&vmo, first / PAGE_SIZE, old_len.min(new_len) / PAGE_SIZE);
                // Whatever did not move -- the tail a shrink cut off -- is
                // still the old object's, and goes back with the rest.
                freeing.push(Freeing {
                    id,
                    first: first / PAGE_SIZE,
                    pages: old_len / PAGE_SIZE,
                });
                let _ = inner.objects.insert(fresh_id, fresh);
                Backing::Anonymous {
                    id: fresh_id,
                    offset: 0,
                }
            };
            // Back to the count the map alone holds, which `give_back` reads.
            drop(vmo);

            inner
                .map
                .insert_region(Vma {
                    range: new_range,
                    backing,
                    ..region
                })
                .map(|()| at)
                .map_err(|_| SpaceError::BadRange)
        };

        // The old translations are out of the tables but may still be in a
        // processor's TLB, and so may whatever a fixed destination replaced.
        invalidate();
        self.give_back(freeing);
        placed
    }
}

/// `at..at + len` as a range, if it is a well-formed one inside the user half.
fn user_range(at: u64, len: u64) -> Option<PageRange> {
    if !is_user_address(at) || at.checked_add(len).is_none_or(|end| end > USER_VIRT_END) {
        return None;
    }
    PageRange::from_len(at, len).ok()
}

/// Whether the mapping of `old_len` bytes at `old` can grow to `new_len`
/// bytes where it is: the pages after it are in the user half and unmapped.
///
/// Asked of `find_free` with the extension as its hint, which answers with
/// the hint exactly when that much fits there, and somewhere else otherwise.
fn grows_in_place(map: &ferrix_vma::AddressSpace, old: u64, old_len: u64, new_len: u64) -> bool {
    let Some(extension) = old.checked_add(old_len) else {
        return false;
    };
    let more = new_len.saturating_sub(old_len);
    user_range(extension, more).is_some()
        && map.find_free(more, PAGE_SIZE, Some(extension)) == Some(extension)
}

/// Whether any region names object `id` at a byte offset in `start..end`.
fn named_elsewhere(map: &ferrix_vma::AddressSpace, id: u64, start: u64, end: u64) -> bool {
    map.iter().any(|region| match region.backing {
        Backing::Anonymous { id: named, offset } if named == id => {
            offset < end && start < offset.saturating_add(region.range.bytes())
        }
        _ => false,
    })
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

/// Whether a region with `flags` permits `access`.
fn permits(flags: VmaFlags, access: Access) -> bool {
    if access.write && !flags.write {
        return false;
    }
    if access.execute && !flags.execute {
        return false;
    }
    // A plain read needs a readable region. Without this, a read of a
    // `PROT_NONE` region -- every guard page, and every `mprotect` to nothing
    // -- commits a zero page and maps it, handing the program memory where it
    // should get `SIGSEGV`. Silent in the way a mapping more permissive than
    // the map always is: nothing that worked stops working, so nothing
    // notices.
    access.write || access.execute || flags.read
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
    /// Place `len` bytes of device memory, starting at physical address
    /// `physical`, in this space: at `at` if one is given, otherwise wherever
    /// it fits. Returns where it went.
    ///
    /// What `io_mapping_map` does with an aperture. The region is backed by
    /// the device itself, [`Backing::Device`], so nothing is committed or
    /// copied: its pages are translated on first touch, by
    /// [`AddressSpace::fault`], to the device's own frames, uncached. It is
    /// shared, so a `fork` child reaches the same registers rather than a copy
    /// of them, and never executable, because nobody runs code out of a
    /// register window.
    ///
    /// Whether `physical..physical + len` is memory this space may have at
    /// all is not decided here: the caller holds an `Aperture` that says so.
    ///
    /// # Errors
    ///
    /// [`SpaceError::Refused`] if `flags` asks for execute;
    /// [`SpaceError::BadRange`] for a length or physical address that is not
    /// whole pages, or a range overlapping a mapping;
    /// [`SpaceError::NotUserRange`] outside the user half; and
    /// [`SpaceError::OutOfMemory`] if no gap that large is free.
    pub(crate) fn map_device(
        &self,
        at: Option<u64>,
        len: u64,
        physical: u64,
        flags: VmaFlags,
    ) -> Result<u64, SpaceError> {
        if flags.execute {
            return Err(SpaceError::Refused(at.unwrap_or(0)));
        }
        if len == 0 || !len.is_multiple_of(PAGE_SIZE) || !physical.is_multiple_of(PAGE_SIZE) {
            return Err(SpaceError::BadRange);
        }
        let mut inner = self.inner.lock();
        let at = match at {
            Some(at) => at,
            None => inner
                .map
                .find_free(len, PAGE_SIZE, None)
                .ok_or(SpaceError::OutOfMemory)?,
        };
        if !is_user_address(at) || at.checked_add(len).is_none_or(|end| end > USER_VIRT_END) {
            return Err(SpaceError::NotUserRange(at));
        }
        let range = PageRange::from_len(at, len).map_err(|_| SpaceError::BadRange)?;
        let flags = VmaFlags {
            execute: false,
            shared: true,
            grows_down: false,
            ..flags
        };
        inner
            .map
            .insert(range, flags, Backing::Device { physical })
            .map_err(|_| SpaceError::BadRange)?;
        Ok(at)
    }

    /// Map `len` bytes of `vmo`, from byte `offset`, at `at` or wherever there
    /// is room: what `vmo_map` does.
    ///
    /// Shared, always. The region names the object itself, not a copy of it,
    /// so a write through it is a write every handle and every other mapping
    /// of the object sees, and `fork` shares the object rather than marking
    /// the region copy-on-write. The region keeps the object alive, so the
    /// mapping outlives the handle it was made with. Nothing is committed
    /// here: pages arrive on first touch, as the object's own, and an unmap
    /// gives them back only if nothing else holds the object. `mremap` and
    /// `mprotect` refuse the region: the object's size is its handle's to
    /// set, and the protection stands for rights the handle carried.
    ///
    /// # Errors
    ///
    /// [`SpaceError::Refused`] for an executable mapping;
    /// [`SpaceError::BadRange`] for a length or offset that is not whole
    /// pages, a range past the object's end, or one that overlaps a mapping;
    /// [`SpaceError::NotUserRange`] outside the user half; and
    /// [`SpaceError::OutOfMemory`] if no free range is long enough.
    pub(crate) fn map_object(
        &self,
        at: Option<u64>,
        len: u64,
        vmo: Arc<Vmo>,
        offset: u64,
        flags: VmaFlags,
    ) -> Result<u64, SpaceError> {
        if flags.execute {
            return Err(SpaceError::Refused(at.unwrap_or(0)));
        }
        if len == 0
            || !len.is_multiple_of(PAGE_SIZE)
            || !offset.is_multiple_of(PAGE_SIZE)
            || offset
                .checked_add(len)
                .is_none_or(|end| end > vmo.len_bytes())
        {
            return Err(SpaceError::BadRange);
        }
        let mut inner = self.inner.lock();
        let at = match at {
            Some(at) => at,
            None => inner
                .map
                .find_free(len, PAGE_SIZE, None)
                .ok_or(SpaceError::OutOfMemory)?,
        };
        if !is_user_address(at) || at.checked_add(len).is_none_or(|end| end > USER_VIRT_END) {
            return Err(SpaceError::NotUserRange(at));
        }
        let range = PageRange::from_len(at, len).map_err(|_| SpaceError::BadRange)?;
        let flags = VmaFlags {
            execute: false,
            shared: true,
            grows_down: false,
            ..flags
        };
        let id = inner.next_id;
        inner.next_id = inner.next_id.saturating_add(1);
        inner
            .map
            .insert(range, flags, Backing::Anonymous { id, offset })
            .map_err(|_| SpaceError::BadRange)?;
        let _ = inner.objects.insert(id, vmo);
        let _ = inner.native.insert(id);
        Ok(at)
    }

    /// Translate one page of a device region to the device's own page.
    ///
    /// Called with the address space's lock held, as the anonymous path maps,
    /// so two processors faulting the same page cannot both install it. A page
    /// already present was resolved by another processor first, and the
    /// faulting instruction retries and succeeds, for the reason the anonymous
    /// path gives.
    fn fault_device(&self, page: u64, physical: u64, flags: VmaFlags) -> Result<(), SpaceError> {
        if mm::translate_in(self.root * PAGE_SIZE, page).is_some() {
            return Ok(());
        }
        mm::map_in(
            self.root * PAGE_SIZE,
            page,
            physical,
            PAGE_SIZE,
            MapFlags {
                read: flags.read,
                write: flags.write,
                execute: false,
                user: true,
                global: false,
                device: true,
            },
        )
        .map_err(|_| SpaceError::OutOfMemory)
    }

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
    /// mapped; [`SpaceError::Refused`] if it reaches a region `vmo_map` made,
    /// whose protection is its handle's to grant.
    pub(crate) fn protect(&self, at: u64, len: u64, flags: VmaFlags) -> Result<(), SpaceError> {
        if !is_user_address(at) || at.checked_add(len).is_none_or(|end| end > USER_VIRT_END) {
            return Err(SpaceError::NotUserRange(at));
        }
        let range = PageRange::from_len(at, len).map_err(|_| SpaceError::BadRange)?;
        let mut inner = self.inner.lock();

        let Inner { map, native, .. } = &*inner;
        let reaches_native = map.iter().any(|region| {
            region.range.start() < range.end()
                && range.start() < region.range.end()
                && matches!(region.backing, Backing::Anonymous { id, .. } if native.contains(&id))
        });
        if reaches_native {
            return Err(SpaceError::Refused(at));
        }

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

/// One region of an address space, as `/proc/<pid>/maps` reports it.
///
/// No name: the region map records none. `[heap]` and `[stack]` are things
/// the process knows about its regions, not things the map knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    /// First address.
    pub(crate) start: u64,
    /// First address past the end.
    pub(crate) end: u64,
    /// Permissions and mapping kind.
    pub(crate) flags: VmaFlags,
}

impl AddressSpace {
    /// Every region, lowest first, as it stands at the moment of the call.
    pub(crate) fn regions(&self) -> Vec<Region> {
        self.inner
            .lock()
            .map
            .iter()
            .map(|vma| Region {
                start: vma.range.start(),
                end: vma.range.end(),
                flags: vma.flags,
            })
            .collect()
    }

    /// Pages the objects this space maps have committed: its resident set.
    ///
    /// A page shared with another space after `fork` is counted in both, as
    /// Linux's `VmRSS` counts it.
    pub(crate) fn resident_pages(&self) -> u64 {
        self.inner
            .lock()
            .objects
            .values()
            .map(|vmo| vmo.committed() as u64)
            .sum()
    }
}
