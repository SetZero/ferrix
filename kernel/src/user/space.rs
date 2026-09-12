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

        let page = address & !(PAGE_SIZE - 1);
        let into_region = page.saturating_sub(region.range.start());

        let (id, offset) = match region.backing {
            Backing::Anonymous { id, offset } => (id, offset.saturating_add(into_region)),
            // Everything else wants a page cache or a device, which are later
            // stages. Reported rather than panicked: an unhandled backing is a
            // kernel bug, but killing the process beats stopping the machine.
            _ => return Err(SpaceError::NotMapped(address)),
        };

        // Already present, and the fault was spurious: another processor
        // resolved this same page between the fault and this lock, or the
        // faulting processor walked a stale TLB entry. Both happen, and
        // neither is an error -- the instruction retries and succeeds.
        //
        // **When copy-on-write lands, its branch goes above this one**, not
        // below: a write to a present but deliberately read-only page is
        // exactly the fault that has to copy, and returning early here would
        // send the instruction back to fault forever. This is safe only while
        // every present page carries its own region's permissions, which holds
        // because nothing sets `cow` yet.
        if mm::translate_in(self.root * PAGE_SIZE, page).is_some() {
            return Ok(());
        }

        let vmo = Arc::clone(
            inner
                .objects
                .get(&id)
                .ok_or(SpaceError::NotMapped(address))?,
        );
        let frame = vmo
            .commit(offset / PAGE_SIZE)
            .map_err(SpaceError::Backing)?;

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
        let mut inner = self.inner.lock();

        let removed = inner.map.remove(range).map_err(|_| SpaceError::BadRange)?;
        for unmapping in removed {
            // Give the pages back, not just the mapping. A process that unmaps
            // half its heap expects the memory returned now, not when the
            // other half goes.
            //
            // Only if nothing else holds the object: a page of memory shared
            // with another address space is not this unmapper's to take away,
            // and a strong count of one means this map is the only holder.
            if let Backing::Anonymous { id, offset } = unmapping.backing
                && let Some(vmo) = inner.objects.get(&id)
                && Arc::strong_count(vmo) == 1
            {
                let _ = vmo.decommit_range(offset / PAGE_SIZE, unmapping.range.bytes() / PAGE_SIZE);
            }

            // The tables first, then the object: a page still reachable
            // through a stale translation while its frame goes back to the
            // allocator is the bug this order exists to make impossible.
            let _ = mm::unmap_in(
                self.root * PAGE_SIZE,
                unmapping.range.start(),
                unmapping.range.bytes(),
            );
        }

        // An object nothing maps any more is dropped here, which releases
        // every page it committed. Split borrows: the predicate reads the map
        // while the objects are being written.
        let Inner { map, objects, .. } = &mut *inner;
        objects.retain(|id, _| still_named(map, *id));
        Ok(())
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
