//! Physical memory and the kernel heap.
//!
//! Stage 2 of `docs/ROADMAP.md`. Two things happen here, and the second is what
//! the rest of the kernel is waiting for:
//!
//! 1. the buddy allocator is given every usable frame firmware reported, and
//! 2. `alloc` starts working, so `Box`, `Vec` and `BTreeMap` exist.
//!
//! Both allocators themselves live in `libs/` — see `ferrix_frame` and
//! `ferrix_heap` — where they are ordinary Rust that `cargo test`, Miri and a
//! fuzzer can drive. What is here is the part that genuinely needs a machine:
//! deciding where the per-frame array goes, and reaching physical memory
//! through the direct map.
//!
//! # The chicken and the egg
//!
//! The buddy allocator needs one [`PageEntry`] per frame, and that array has to
//! be allocated before there is an allocator. So it is carved out of the front
//! of the largest usable region firmware reported *that the direct map
//! reaches*, and that region is then handed to the allocator with the carved
//! part left out. The qualification matters only on a 32-bit machine with more
//! RAM than its direct map, and there it is the difference between an array
//! and a zeroed kernel image. After that, every
//! allocation goes through the allocator like anything else.
//!
//! # Locks
//!
//! Three globals, each behind an interrupt-masking lock, nesting in one fixed
//! order:
//!
//! * [`TABLES`] serialises every walk and change of the kernel's page tables.
//!   A mapping that needs a new table takes a frame for it, and an unmap
//!   writes down what it released in a `Vec`, so this is taken before both of
//!   the others.
//! * [`HEAP`] is the kernel heap. A slab that runs dry takes a page, so this is
//!   taken before [`FRAMES`] too.
//! * [`FRAMES`] is the buddy allocator, and the innermost lock in the kernel:
//!   nothing is taken while it is held.
//!
//! Interrupt-masking rather than plain, because the page fault handler maps
//! pages and so reaches the first and the last: a handler that interrupted a
//! CPU holding either would spin on its own CPU's lock. Masking interrupts does
//! not mask a *fault*, which is why nothing holding these touches the
//! on-demand window.
//!
//! One lock in front of each is the simplest correct shape, and the
//! architecture asks for more: per-CPU caches in front of the frame allocator
//! and the heap, so the common allocation takes no lock at all. Those need a
//! per-CPU area to live in, and come after it.

use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_bootinfo::{BootView, MemKind, MemRegion, PAGE_SIZE};
use ferrix_frame::{Frame, Frames, PageEntry};
use ferrix_heap::{Backing, Heap, Request};
use ferrix_paging::{
    Encoding, Leaf, MapFlags, Mapper, PhysAddr, PhysMem, Released, VirtAddr, WalkOutcome,
};
use ferrix_sync::IrqSpinLock;

/// Why memory could not be brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MemoryError {
    /// Firmware reported no usable memory at all.
    NoUsableMemory,
    /// No single usable region is large enough to hold the per-frame array.
    ///
    /// Carries the bytes needed. On a machine small enough for this, the array
    /// is most of what there is.
    NoRoomForPageArray(u64),
}

impl core::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryError::NoUsableMemory => f.write_str("firmware reported no usable memory"),
            MemoryError::NoRoomForPageArray(bytes) => {
                write!(f, "no usable region holds the {bytes}-byte page array")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

/// The buddy allocator, once [`init`] has run.
///
/// The innermost lock in the kernel; the module documentation gives the order.
static FRAMES: IrqSpinLock<Option<Frames<'static>>, crate::arch::Irq> = IrqSpinLock::new(None);

/// The kernel heap.
static HEAP: IrqSpinLock<Heap, crate::arch::Irq> = IrqSpinLock::new(Heap::new());

/// Held for every walk and every change of the kernel's page tables.
///
/// Guards no data of its own: the tables are physical memory reached through
/// the direct map, and what this lock owns is the right to walk them.
/// [`with_tables`] and [`sweep`] are the only places that take it.
static TABLES: IrqSpinLock<(), crate::arch::Irq> = IrqSpinLock::new(());

/// Base of the direct map, learned from the hand-off.
///
/// An atomic rather than another `UnsafeCell`: it is written once and read from
/// every allocation, and there is no reason to spend an `unsafe` on a `u64`.
static PHYSMAP: AtomicU64 = AtomicU64::new(0);

/// The physical address that appears at [`PHYSMAP`]: the lowest RAM address,
/// which is zero on x86-64 and a gibibyte on QEMU's Arm machines.
static PHYSMAP_PHYS: AtomicU64 = AtomicU64::new(0);

/// The virtual address at which physical address `phys` is readable.
fn physmap(phys: u64) -> u64 {
    PHYSMAP.load(Ordering::Relaxed) + (phys - PHYSMAP_PHYS.load(Ordering::Relaxed))
}

/// The physical address behind a direct-map virtual address.
fn unmap(virt: u64) -> u64 {
    virt - PHYSMAP.load(Ordering::Relaxed) + PHYSMAP_PHYS.load(Ordering::Relaxed)
}

/// Run `body` with the frame allocator, or return `None` before [`init`].
fn with_frames<T>(body: impl FnOnce(&mut Frames<'static>) -> T) -> Option<T> {
    FRAMES.lock().as_mut().map(body)
}

/// Run `body` with the heap.
fn with_heap<T>(body: impl FnOnce(&mut Heap) -> T) -> T {
    body(&mut HEAP.lock())
}

// ---------------------------------------------------------------------------
// Bring-up
// ---------------------------------------------------------------------------

/// What memory looks like once it is up.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stats {
    /// Frames the buddy allocator was given.
    pub(crate) managed_frames: u64,
    /// Frames currently free.
    pub(crate) free_frames: u64,
    /// Bytes spent on the per-frame array.
    pub(crate) page_array_bytes: u64,
    /// Physical address the per-frame array was placed at.
    pub(crate) page_array_at: u64,
}

/// Bring up the frame allocator and the heap.
pub(crate) fn init(view: &BootView<'_>) -> Result<Stats, MemoryError> {
    PHYSMAP.store(view.raw().physmap_base, Ordering::Relaxed);
    PHYSMAP_PHYS.store(view.raw().physmap_phys, Ordering::Relaxed);
    ROOT_TABLE.store(view.raw().root_table_phys, Ordering::Relaxed);

    // No higher than the direct map reaches: a frame the kernel cannot address
    // is not a frame it can hand out. Only a 32-bit machine with more RAM than
    // its direct map holds has any such frames, and `kmain` says how many.
    let highest = view
        .max_ram_address()
        .min(view.physmap_limit())
        .div_ceil(PAGE_SIZE);
    let lowest = lowest_ram_frame(view);
    if highest <= lowest {
        return Err(MemoryError::NoUsableMemory);
    }

    // One entry per frame of RAM -- from the *lowest* RAM frame, not from zero.
    // That distinction is worth an offset on every lookup: QEMU's AArch64
    // `virt` machine starts RAM at 1 GiB, so an array based at zero would spend
    // 4 MiB describing a gigabyte of nothing. It is the same array either way
    // on x86-64, where RAM does start at zero.
    //
    // This is still a flat array, which is the right shape up to a few hundred
    // gibibytes and the wrong one beyond: a machine with RAM in widely
    // separated banks wants the array split per bank. `Frames` takes a base
    // precisely so that change stays inside this function.
    let entries = Frames::entries_needed(lowest, highest);
    let bytes = (entries * size_of::<PageEntry>()) as u64;
    if view.usable_ram() == 0 {
        return Err(MemoryError::NoUsableMemory);
    }
    // Among usable regions clipped to the direct map, not merely the longest
    // one: the array is zeroed through the direct map, and on a 32-bit board
    // with more RAM than that map holds the longest region can lie above it.
    let host = view
        .page_array_host(bytes)
        .ok_or(MemoryError::NoRoomForPageArray(bytes))?;

    let array = place_page_array(host.base, entries);
    let mut frames = Frames::new(array, lowest);

    // Everything firmware called usable, minus the part the array now occupies.
    let array_end = host.base + bytes.next_multiple_of(PAGE_SIZE);
    for region in view.regions() {
        if region.kind.is_free_at_boot() {
            insert_region(&mut frames, region, host.base, array_end);
        }
    }

    let stats = Stats {
        managed_frames: frames.managed_frames(),
        free_frames: frames.free_frames(),
        page_array_bytes: bytes,
        page_array_at: host.base,
    };

    *FRAMES.lock() = Some(frames);
    Ok(stats)
}

/// The lowest frame of anything firmware called memory.
///
/// RAM rather than *usable* RAM: the array has to have an entry for the
/// kernel's own pages and for firmware's, because a refcount on a frame is how
/// copy-on-write will work and those frames get shared too.
fn lowest_ram_frame(view: &BootView<'_>) -> u64 {
    view.regions()
        .iter()
        .filter(|region| region.kind.is_ram())
        .map(|region| region.base / PAGE_SIZE)
        .min()
        .unwrap_or(0)
}

/// Zero `entries` records at the front of `base` and take them as a slice.
///
/// The array outlives everything, so `'static` is honest: it is carved out of
/// physical memory that is then excluded from the allocator, and there is no
/// path that gives it back.
fn place_page_array(base: u64, entries: usize) -> &'static mut [PageEntry] {
    let virt = physmap(base) as *mut PageEntry;
    let bytes = entries * size_of::<PageEntry>();

    // Zero first, then take the slice. Forming a `&mut [PageEntry]` over memory
    // holding whatever the last owner left would be undefined behaviour: `State`
    // is an enum, and an undefined discriminant is not merely a strange value.
    // `ferrix_frame` guarantees zero is a state it defines.
    //
    // SAFETY: `base` is the start of a run of RAM firmware called usable, and
    // which is about to be excluded from the allocator, so nothing else refers
    // to it. `BootView::page_array_host` chose that run from inside the direct
    // map, so all `bytes` of it are mapped and writable at `virt` -- which is
    // not the same as "the direct map covers all of RAM": on ARMv7-A it covers
    // 1.25 GiB, and the rest is not mapped anywhere.
    unsafe { core::ptr::write_bytes(virt.cast::<u8>(), 0, bytes) };

    // SAFETY: the range was just zeroed, is inside a usable region large enough
    // for it, is excluded from the allocator below, and is naturally aligned
    // because the region base is page aligned.
    unsafe { core::slice::from_raw_parts_mut(virt, entries) }
}

/// Hand one usable region to the allocator, skipping the page array.
fn insert_region(frames: &mut Frames<'static>, region: &MemRegion, hole: u64, hole_end: u64) {
    let start = region.base;
    let end = region.end();

    // The common case: the region has nothing to do with the array.
    if end <= hole || start >= hole_end {
        frames.insert_free(start / PAGE_SIZE, region.len / PAGE_SIZE);
        return;
    }

    // Otherwise add whatever lies either side of it. The array is placed at the
    // front of its host region, so in practice only the tail exists — but a
    // future placement policy should not silently lose the head.
    if start < hole {
        frames.insert_free(start / PAGE_SIZE, (hole - start) / PAGE_SIZE);
    }
    if end > hole_end {
        frames.insert_free(hole_end / PAGE_SIZE, (end - hole_end) / PAGE_SIZE);
    }
}

// ---------------------------------------------------------------------------
// The public allocation interface
// ---------------------------------------------------------------------------

/// Take `2^order` contiguous frames.
pub(crate) fn allocate_frames(order: u8) -> Option<Frame> {
    with_frames(|frames| frames.allocate(order))?
}

/// Take `2^order` contiguous frames lying wholly below frame `limit`.
///
/// For the one caller that cares where its memory is: x86-64's trampoline,
/// which real mode can only reach below one mebibyte.
#[allow(
    dead_code,
    reason = "only x86-64 starts processors from low memory; see the doc comment"
)]
pub(crate) fn allocate_frames_below(order: u8, limit: Frame) -> Option<Frame> {
    with_frames(|frames| frames.allocate_below(order, limit))?
}

/// Give back frames taken with [`allocate_frames`].
pub(crate) fn deallocate_frames(frame: Frame, order: u8) {
    let _ = with_frames(|frames| frames.deallocate(frame, order));
}

/// Record another reference to a frame, for a page two address spaces share.
///
/// Returns the new count, or `None` if the allocator refused — which it does
/// for a frame that is not allocated, or one whose count would wrap. Both are
/// kernel bugs rather than conditions a caller can recover from, so the caller
/// that maps the page must treat `None` as "do not map it".
pub(crate) fn share_frame(frame: Frame) -> Option<u32> {
    with_frames(|frames| frames.share(frame).ok())?
}

/// Drop one reference to a frame, freeing it if it was the last.
///
/// Returns true if the frame went back to the allocator. Anonymous memory is
/// freed through here rather than through [`deallocate_frames`], because a
/// copy-on-write page may still be mapped by somebody else and the allocator
/// refuses to free it while it is.
pub(crate) fn release_frame(frame: Frame) -> bool {
    // Qualified: `Released` is already `ferrix_paging`'s in this module, and
    // the two mean different things -- a page table given back versus a frame.
    with_frames(|frames| matches!(frames.release(frame), Ok(ferrix_frame::Released::Freed)))
        .unwrap_or(false)
}

/// How many references there are to a frame.
///
/// For the fault handler's one real decision: a copy-on-write fault on a page
/// nobody else holds any more does not need to copy anything.
pub(crate) fn frame_references(frame: Frame) -> u32 {
    with_frames(|frames| frames.entry(frame).map_or(0, PageEntry::refcount)).unwrap_or(0)
}

/// Frames currently free.
pub(crate) fn free_frames() -> u64 {
    with_frames(|frames| frames.free_frames()).unwrap_or(0)
}

/// Frames the allocator was given to manage: what `MemTotal` reports.
pub(crate) fn managed_frames() -> u64 {
    with_frames(|frames| frames.managed_frames()).unwrap_or(0)
}

/// Bytes currently out on the kernel heap.
pub(crate) fn heap_allocated() -> usize {
    with_heap(|heap| heap.allocated_bytes())
}

/// Pages the heap is holding for its size classes.
pub(crate) fn heap_pages() -> usize {
    with_heap(|heap| heap.slab_pages() + heap.large_pages())
}

// ---------------------------------------------------------------------------
// The heap's page supply
// ---------------------------------------------------------------------------

/// Gives the heap pages out of the buddy allocator, addressed through the
/// direct map.
struct KernelPages;

// SAFETY: `allocate_pages` returns frames the buddy allocator handed out, which
// are contiguous, page aligned and owned by nobody else, addressed through the
// direct map, which covers all of RAM for the life of the system. `read_link`
// and `write_link` are only ever called by the heap on addresses inside pages
// it took from `allocate_pages` and has not given back, so nothing aliases
// them.
unsafe impl Backing for KernelPages {
    fn allocate_pages(&mut self, order: u8) -> Option<u64> {
        let frame = allocate_frames(order)?;
        Some(physmap(frame * PAGE_SIZE))
    }

    fn deallocate_pages(&mut self, address: u64, order: u8) {
        deallocate_frames(unmap(address) / PAGE_SIZE, order);
    }

    fn read_link(&self, at: u64) -> u64 {
        // SAFETY: as documented on the impl. Eight-byte aligned because every
        // size class is a multiple of eight and slab pages are page aligned.
        unsafe { core::ptr::read_volatile(at as *const u64) }
    }

    fn write_link(&mut self, at: u64, value: u64) {
        // SAFETY: as `read_link`.
        unsafe { core::ptr::write_volatile(at as *mut u64, value) };
    }

    fn slab_free(&self, page: u64) -> u16 {
        with_frames(|frames| frames.slab_free(unmap(page) / PAGE_SIZE)).unwrap_or(0)
    }

    fn set_slab_free(&mut self, page: u64, objects: u16) {
        let _ = with_frames(|frames| frames.set_slab_free(unmap(page) / PAGE_SIZE, objects));
    }
}

/// The kernel's `alloc` implementation.
struct KernelAllocator;

// SAFETY: `alloc` returns either null or the address of a block of at least
// `layout.size()` bytes, aligned to `layout.align()`, which no other live
// allocation overlaps — that is `ferrix_heap`'s contract, and its tests are
// where it is checked. `dealloc` is only called with a pointer and layout from
// a matching `alloc`, which is what `GlobalAlloc` requires of its caller.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let request = Request::new(layout.size(), layout.align());
        with_heap(|heap| match heap.allocate(&mut KernelPages, request) {
            Ok(address) => address as *mut u8,
            // A null return is how `GlobalAlloc` reports failure; the caller
            // turns it into whatever it considers appropriate, which for `Box`
            // is the allocation error handler.
            Err(_) => core::ptr::null_mut(),
        })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let request = Request::new(layout.size(), layout.align());
        with_heap(|heap| heap.deallocate(&mut KernelPages, pointer as u64, request));
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

// ---------------------------------------------------------------------------
// Kernel mappings
// ---------------------------------------------------------------------------

/// Physical address of the root page table the loader installed.
static ROOT_TABLE: AtomicU64 = AtomicU64::new(0);

/// A window of kernel address space with nothing mapped in it, faulted in a
/// page at a time on first touch.
///
/// Stage 3 uses it to prove the fault path works end to end. It is the same
/// mechanism stage 6 uses for every anonymous user mapping, which is the point
/// of testing it this way: a fault is resolved by *making the mapping true* and
/// letting the instruction retry, never by stepping over it.
pub(crate) const DEMAND_WINDOW: u64 = crate::vmap::DEMAND_WINDOW;

/// Size of that window.
pub(crate) const DEMAND_WINDOW_SIZE: u64 = 2 * 1024 * 1024;

/// True if `address` is inside the on-demand window.
pub(crate) fn is_demand_window(address: u64) -> bool {
    (DEMAND_WINDOW..DEMAND_WINDOW + DEMAND_WINDOW_SIZE).contains(&address)
}

/// Physical memory as the kernel sees it once the frame allocator is up:
/// through the direct map, with page table frames from the buddy.
///
/// Replaces `EarlyMemory`'s pool in `.bss`, which only ever had sixteen frames
/// and existed to get the console mapped before there was an allocator.
struct KernelPhysMem;

// SAFETY: `read` and `write` go through the direct map, which covers every byte
// of RAM and is the only mapping of it the kernel holds, so nothing can alias
// them. `allocate_table` returns a frame from the buddy allocator, which hands
// each one out once; it is page aligned because frames are, and zeroed here
// before it is returned.
unsafe impl PhysMem for KernelPhysMem {
    fn read(&self, at: PhysAddr) -> u64 {
        // SAFETY: as documented on the impl; `at` is a descriptor address.
        unsafe { core::ptr::read_volatile(physmap(at.0) as *const u64) }
    }

    fn write(&mut self, at: PhysAddr, value: u64) {
        // SAFETY: as documented on the impl.
        unsafe { core::ptr::write_volatile(physmap(at.0) as *mut u64, value) };
    }

    fn allocate_table(&mut self) -> Option<PhysAddr> {
        let frame = allocate_frames(0)?;
        let address = frame * PAGE_SIZE;
        // A page table built on someone else's leftovers translates to wherever
        // they pointed, so this zeroing is not optional.
        //
        // SAFETY: the frame was just allocated to us and nothing else refers to
        // it; the direct map makes it writable.
        unsafe { core::ptr::write_bytes(physmap(address) as *mut u8, 0, PAGE_SIZE as usize) };
        Some(PhysAddr(address))
    }
}

/// Map `len` bytes of kernel address space at `virt` onto `phys`.
pub(crate) fn map_kernel(
    virt: u64,
    phys: u64,
    len: u64,
    flags: MapFlags,
) -> Result<(), ferrix_paging::MapError> {
    with_tables(|mapper| {
        mapper.map_range(
            &mut KernelPhysMem,
            VirtAddr(virt),
            PhysAddr(phys),
            len.next_multiple_of(PAGE_SIZE),
            flags,
        )
    })?;
    // The table walker is a separate observer of memory: on AArch64 it cannot
    // see a descriptor still sitting in a store buffer, and the barrier inside
    // this is what makes the mapping real.
    crate::arch::flush_tlb();
    Ok(())
}

/// Resolve a fault in the on-demand window by mapping a fresh zeroed page.
pub(crate) fn map_demand_page(address: u64) -> Result<(), MemoryError> {
    let page = address & !(PAGE_SIZE - 1);
    let frame = allocate_frames(0).ok_or(MemoryError::NoUsableMemory)?;

    // Anonymous memory reads as zero, and a page handed to a program still
    // holding the last owner's data is an information leak, so this is a
    // correctness requirement rather than tidiness.
    //
    // SAFETY: the frame was just allocated to us and the direct map covers it.
    unsafe { core::ptr::write_bytes(physmap(frame * PAGE_SIZE) as *mut u8, 0, PAGE_SIZE as usize) };

    map_kernel(page, frame * PAGE_SIZE, PAGE_SIZE, MapFlags::KERNEL_DATA)
        .map_err(|_| MemoryError::NoUsableMemory)
}

/// Translate a kernel virtual address the way the hardware would.
pub(crate) fn translate(virt: u64) -> Option<u64> {
    with_tables(|mapper| {
        mapper
            .translate(&KernelPhysMem, VirtAddr(virt))
            .map(|at| at.0)
    })
}

/// Translate an address through the tree rooted at `root`, the way the
/// hardware would if that root were installed.
///
/// The counterpart of [`map_in`], and it takes no lock for the same reason
/// that one does not: the caller owns the tree. An address space that other
/// processors may be faulting in holds its own lock across this.
pub(crate) fn translate_in(root: u64, virt: u64) -> Option<u64> {
    let mapper: Mapper<crate::arch::PageEncoding> = Mapper::new(PhysAddr(root));
    mapper
        .translate(&KernelPhysMem, VirtAddr(virt))
        .map(|at| at.0)
}

/// Physical address of the kernel's root page table, for a processor about to
/// install it.
pub(crate) fn root_table() -> u64 {
    ROOT_TABLE.load(Ordering::Relaxed)
}

/// Where physical address `phys` can be read and written: its alias in the
/// direct map.
pub(crate) fn direct_map(phys: u64) -> u64 {
    physmap(phys)
}

// ---------------------------------------------------------------------------
// Page tables beside the kernel's
// ---------------------------------------------------------------------------

/// Map `len` bytes at `virt` onto `phys` in the tree rooted at `root`.
///
/// What starting a processor needs: a mapping the kernel's own tables must not
/// have — an identity map of the instructions that turn the MMU on — in a tree
/// only that processor installs, and only for as long as it takes. The caller
/// allocates and zeroes `root`, because where it may live differs: anywhere on
/// `AArch64`, below 4 GiB for x86-64's trampoline.
///
/// Takes no lock: a tree no processor has installed is nobody else's to walk.
/// `root` must not be the kernel's, which [`map_kernel`] is for.
pub(crate) fn map_in(
    root: u64,
    virt: u64,
    phys: u64,
    len: u64,
    flags: MapFlags,
) -> Result<(), ferrix_paging::MapError> {
    let mapper: Mapper<crate::arch::PageEncoding> = Mapper::new(PhysAddr(root));
    mapper.map_range(
        &mut KernelPhysMem,
        VirtAddr(virt),
        PhysAddr(phys),
        len.next_multiple_of(PAGE_SIZE),
        flags,
    )
}

/// Make top-level `slots` of the tree rooted at `root` point where the
/// kernel's do, so that part of the address space looks the same through
/// either root.
///
/// Shared, not copied: the tables under those slots are the kernel's own. So
/// nothing may be mapped or unmapped through `root` inside them — [`map_in`]
/// and [`unmap_in`] on `root` have to stay in the slots that are its own.
///
/// x86-64 only, and deliberately not behind a conditional, for the reason
/// [`clear_root_slots`] gives: `AArch64` splits the halves across two base
/// registers, so a tree there never needs the kernel's half in it.
#[allow(
    dead_code,
    reason = "only x86-64 keeps both halves in one root; see the doc comment"
)]
pub(crate) fn share_kernel_slots(root: u64, slots: core::ops::Range<usize>) {
    let kernel = ROOT_TABLE.load(Ordering::Relaxed);
    with_tables(|_| {
        for slot in slots {
            let offset = (slot as u64) * 8;
            let entry = KernelPhysMem.read(PhysAddr(kernel + offset));
            KernelPhysMem.write(PhysAddr(root + offset), entry);
        }
    });
}

/// Take down what [`map_in`] built at `virt`, giving back every table under
/// `root` that it leaves empty.
///
/// Neither the root nor the frames the mappings pointed at are freed: the
/// caller allocated both and knows what they are.
pub(crate) fn unmap_in(root: u64, virt: u64, len: u64) -> Result<(), ferrix_paging::MapError> {
    let mapper: Mapper<crate::arch::PageEncoding> = Mapper::new(PhysAddr(root));
    let _ = mapper.unmap_range(
        &mut KernelPhysMem,
        VirtAddr(virt),
        len.next_multiple_of(PAGE_SIZE),
        |freed| {
            if let Released::Table { phys } = freed {
                deallocate_frames(phys.0 / PAGE_SIZE, 0);
            }
        },
    )?;
    Ok(())
}

/// Map the 4 KiB page `phys` at I/O address `iova` in the IOMMU tree rooted at
/// `root`, whose descriptors `E` encodes.
///
/// The IOMMU counterpart of [`map_in`]: the tables come from the frame
/// allocator through the direct map, and the caller owns the tree, so no lock
/// is taken. One page at a time, so no block mapping is ever built, which a
/// unit without superpages could not walk.
pub(crate) fn map_io<E: Encoding>(
    root: u64,
    iova: u64,
    phys: u64,
    flags: MapFlags,
) -> Result<(), ferrix_paging::MapError> {
    let mut mapper: Mapper<E> = Mapper::new(PhysAddr(root));
    mapper.pages_only();
    mapper.map_range(
        &mut KernelPhysMem,
        VirtAddr(iova),
        PhysAddr(phys),
        PAGE_SIZE,
        flags,
    )
}

/// Take down the page [`map_io`] put at `iova`, giving back every table under
/// `root` it leaves empty. The frame the page pointed at is not freed: the
/// caller knows what it is, and must not free it before the unit's cached
/// translations are invalidated.
pub(crate) fn unmap_io<E: Encoding>(root: u64, iova: u64) -> Result<(), ferrix_paging::MapError> {
    let mapper: Mapper<E> = Mapper::new(PhysAddr(root));
    let _ = mapper.unmap_range(&mut KernelPhysMem, VirtAddr(iova), PAGE_SIZE, |freed| {
        if let Released::Table { phys } = freed {
            deallocate_frames(phys.0 / PAGE_SIZE, 0);
        }
    })?;
    Ok(())
}

/// Where I/O address `iova` leads in the IOMMU tree rooted at `root`, walked
/// as the unit would.
pub(crate) fn translate_io<E: Encoding>(root: u64, iova: u64) -> Option<u64> {
    let mapper: Mapper<E> = Mapper::new(PhysAddr(root));
    mapper
        .translate(&KernelPhysMem, VirtAddr(iova))
        .map(|at| at.0)
}

/// Run `body` with a mapper over the live kernel page tables, holding
/// [`TABLES`] throughout.
///
/// Reads take the lock as well as writes. A walk racing an unmap on another
/// CPU can follow a descriptor into a table that has just been freed and
/// handed to somebody else, and read whatever they wrote there as though it
/// were a translation.
fn with_tables<T>(body: impl FnOnce(Mapper<crate::arch::PageEncoding>) -> T) -> T {
    let _held = TABLES.lock();
    body(Mapper::new(PhysAddr(ROOT_TABLE.load(Ordering::Relaxed))))
}

/// Remove `len` bytes of kernel mapping at `virt`.
///
/// `released` is called once per leaf with the frame number and the order of
/// the block it was mapped at, and is where the caller decides whether the
/// memory behind the mapping goes back to the buddy allocator. A `vmap` of
/// anonymous pages frees them; a device window never allocated them.
///
/// **Unmap, invalidate everywhere, and only then free** — in that order. A
/// frame handed back before every processor has dropped its translation to it
/// can be allocated to somebody else while another processor still reads and
/// writes it through the old one. That is not a leak or a crash; it is two
/// owners of one page, and the symptom appears in whichever of them notices
/// first. So nothing is released until the shootdown returns — not even the
/// page tables, which a processor's walker may have cached as well.
///
/// Must not be called holding any lock, for the reason
/// [`crate::smp::flush_tlb_everywhere`] gives.
pub(crate) fn unmap_kernel(
    virt: u64,
    len: u64,
    mut released: impl FnMut(Frame, u8),
) -> Result<u64, ferrix_paging::MapError> {
    let mut pages: Deferred<(Frame, u8), 32> = Deferred::new((0, 0));
    let mut tables: Deferred<Frame, 8> = Deferred::new(0);

    let removed = with_tables(|mapper| {
        mapper.unmap_range(
            &mut KernelPhysMem,
            VirtAddr(virt),
            len.next_multiple_of(PAGE_SIZE),
            |freed| match freed {
                Released::Page { phys, level } => {
                    // Levels run root-to-leaf and orders run small-to-large,
                    // so the conversion is a subtraction rather than a table:
                    // a level-3 leaf is order 0 and a 2 MiB block is order 9.
                    let order = (ferrix_paging::Level::PAGE.depth() - level.depth()) * 9;
                    pages.push((phys.0 / PAGE_SIZE, order));
                }
                // A page table the mapper allocated through `KernelPhysMem`,
                // which took it from the buddy allocator. It goes straight
                // back there rather than to the caller: the caller asked to
                // unmap a range and has no idea a table existed, and telling
                // it about one would make every `released` closure in the
                // tree have to know.
                Released::Table { phys } => tables.push(phys.0 / PAGE_SIZE),
            },
        )
    });

    // Whatever was removed before an error is just as unmapped, and just as
    // cached in somebody's TLB, as it would have been after a success.
    crate::smp::flush_tlb_everywhere();
    for &(frame, order) in pages.iter() {
        released(frame, order);
    }
    for &frame in tables.iter() {
        deallocate_frames(frame, 0);
    }
    removed
}

/// What an unmap released, held until every TLB has forgotten it.
///
/// Inline for an unmap of up to `N` things, which is every unmap the kernel
/// makes today, and spilling to the heap past that. **Not a `Vec` from the
/// start**, and not for speed: an unmap that allocated would, whenever its
/// size class had no slab page, take one from the frame allocator and keep it
/// — and the stage 2 checks, which require an unmap to give back exactly the
/// frames its map took, would be measuring the heap's bookkeeping instead.
#[derive(Debug)]
struct Deferred<T: Copy, const N: usize> {
    /// The first `N` items.
    inline: [T; N],
    /// How many of `inline` are in use.
    held: usize,
    /// The rest, for an unmap larger than `N`.
    spill: Vec<T>,
}

impl<T: Copy, const N: usize> Deferred<T, N> {
    /// An empty list; `blank` fills the unused inline slots.
    const fn new(blank: T) -> Self {
        Deferred {
            inline: [blank; N],
            held: 0,
            spill: Vec::new(),
        }
    }

    /// Hold on to `item`.
    fn push(&mut self, item: T) {
        if let Some(slot) = self.inline.get_mut(self.held) {
            *slot = item;
            self.held += 1;
        } else {
            self.spill.push(item);
        }
    }

    /// Everything held, in the order it was pushed.
    fn iter(&self) -> impl Iterator<Item = &T> {
        self.inline.iter().take(self.held).chain(self.spill.iter())
    }
}

/// Change what an existing kernel mapping permits.
pub(crate) fn protect_kernel(
    virt: u64,
    len: u64,
    flags: MapFlags,
) -> Result<(), ferrix_paging::MapError> {
    with_tables(|mapper| {
        mapper.protect_range(
            &mut KernelPhysMem,
            VirtAddr(virt),
            len.next_multiple_of(PAGE_SIZE),
            flags,
        )
    })?;
    // Everywhere: a permission narrowed on one processor and still wide in
    // another's TLB is not narrowed.
    crate::smp::flush_tlb_everywhere();
    Ok(())
}

/// Visit every leaf in the tree rooted at `root`, in address order, holding
/// [`TABLES`] throughout.
///
/// Takes a root rather than going through [`with_tables`] because the W^X
/// sweep also walks the loader's identity map, which on `AArch64` is a second
/// tree with a root of its own.
fn sweep(root: u64, visit: impl FnMut(Leaf) -> bool) -> WalkOutcome {
    let _held = TABLES.lock();
    let mapper: Mapper<crate::arch::PageEncoding> = Mapper::new(PhysAddr(root));
    mapper.for_each_leaf(&KernelPhysMem, visit)
}

/// What `virt` is mapped as, or `None` if it is not mapped.
pub(crate) fn permissions_of(virt: u64) -> Option<MapFlags> {
    let mut found = None;
    let _ = sweep(ROOT_TABLE.load(Ordering::Relaxed), |leaf| {
        if virt >= leaf.virt.0 && virt < leaf.virt.0 + leaf.bytes() {
            found = Some(leaf.flags);
            return false;
        }
        true
    });
    found
}

// ---------------------------------------------------------------------------
// W^X
// ---------------------------------------------------------------------------

/// A mapping that is both writable and executable.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WriteExecute {
    /// Where it starts.
    pub(crate) virt: u64,
    /// How much of the address space it covers.
    pub(crate) len: u64,
}

/// What the W^X sweep found.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WxReport {
    /// Mappings the hardware can see.
    pub(crate) leaves: u64,
    /// Of those, how many are executable at all.
    pub(crate) executable: u64,
}

/// Walk the live page tables and require that nothing is writable *and*
/// executable.
///
/// **A measurement of the machine, not of the kernel's intentions.** Every
/// other check of this kind in the tree asserts that a function was called
/// with the right flags; this one reads the descriptors the hardware is going
/// to walk, including the ones the loader wrote and the ones an earlier stage
/// installed and forgot about. The loader's identity map is exactly such a
/// mapping — it has to be writable and executable, because the instruction
/// after the page table switch is fetched through it — so this sweep only
/// passes once that map has been dropped, which is why the two land in the
/// same stage.
///
/// # Errors
///
/// The first offending mapping, which is enough: the fix for one is the fix
/// for all of them, and reporting the address of the first is what makes it
/// findable.
pub(crate) fn check_w_xor_x(view: &BootView<'_>) -> Result<WxReport, WriteExecute> {
    let mut report = WxReport {
        leaves: 0,
        executable: 0,
    };
    let mut offender = None;

    // Every root the hardware can translate through, not only the kernel's.
    // On `AArch64` the identity map is a second regime with its own base
    // register, so a sweep of the kernel's tables alone would report a clean
    // machine while the CPU could still fetch from a writable page.
    let roots = [
        Some(ROOT_TABLE.load(Ordering::Relaxed)),
        crate::arch::identity_root(view),
    ];

    for root in roots.into_iter().flatten() {
        let outcome = sweep(root, |leaf| {
            if leaf.flags.execute {
                report.executable += 1;
            }
            if leaf.is_write_execute() {
                offender = Some(WriteExecute {
                    virt: leaf.virt.0,
                    len: leaf.bytes(),
                });
                return false;
            }
            true
        });
        report.leaves += outcome.leaves;
        if offender.is_some() {
            break;
        }
    }

    match offender {
        Some(found) => Err(found),
        None => Ok(report),
    }
}

// ---------------------------------------------------------------------------
// Reclaiming early boot
// ---------------------------------------------------------------------------

/// What reclaiming early-boot memory recovered.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Reclaimed {
    /// Frames the loader's own code and data occupied.
    pub(crate) loader_frames: u64,
    /// Frames the ACPI tables occupied.
    pub(crate) acpi_frames: u64,
}

impl Reclaimed {
    /// Frames recovered in total.
    pub(crate) const fn total(&self) -> u64 {
        self.loader_frames + self.acpi_frames
    }
}

/// Give the frame allocator the memory early boot has finished with.
///
/// Two kinds, and the ordering constraint on each is the whole of the risk.
///
/// * [`MemKind::Loader`] is the loader's own code and data. The kernel stopped
///   executing it at the jump, and nothing in the hand-off points into it —
///   the memory map and the command line were copied into the `BootInfo`
///   region, which is a different kind and is **not** reclaimed.
/// * [`MemKind::AcpiReclaim`] holds the firmware tables. Those are read
///   through the direct map by `crate::acpi`, so this must not run until the
///   last parse has finished; it is called from `kmain` after interrupt
///   bring-up for exactly that reason.
///
/// Deliberately *not* reclaimed: [`MemKind::PageTables`], which the kernel is
/// running on; [`MemKind::BootStack`], which it is running on too; and
/// [`MemKind::BootInfo`], which `BootView` borrows for the life of the system.
///
/// # Safety
///
/// Every reference into loader or ACPI-reclaim memory must be dead. In this
/// kernel that means being called from `kmain` after the last use of
/// `crate::acpi::Firmware`, and it is called exactly once.
pub(crate) unsafe fn reclaim_boot_memory(view: &BootView<'_>) -> Reclaimed {
    let mut reclaimed = Reclaimed::default();

    let _ = with_frames(|frames| {
        for region in view.regions() {
            if !region.kind.is_reclaimable() {
                continue;
            }
            // A region that ends below where the per-frame array starts, or
            // begins above where it ends, has no entry to mark — and handing
            // the allocator a frame it has no record of would be a write past
            // the end of that array. `Frames::insert_free` clamps, but saying
            // so here is what makes the clamp a decision rather than luck.
            let first = region.base / PAGE_SIZE;
            let count = region.len / PAGE_SIZE;
            if count == 0 || first < frames.base() || first >= frames.end() {
                continue;
            }
            let count = count.min(frames.end() - first);

            frames.insert_free(first, count);
            match region.kind {
                MemKind::Loader => reclaimed.loader_frames += count,
                _ => reclaimed.acpi_frames += count,
            }
        }
    });

    reclaimed
}

/// Clear a span of slots in the root page table.
///
/// Dead on `AArch64`, and deliberately not behind a conditional: the identity
/// map there is a whole second translation regime that is switched off at
/// `TCR_EL1`, so there is nothing to clear. A `cfg` here would be the first
/// crack in the rule that says architecture differences live under
/// `kernel/src/arch/`.
///
/// The one operation that cannot be expressed as unmapping a range: dropping
/// the loader's identity map on x86-64 means removing *everything* below the
/// upper half, and walking it page by page would be half a million
/// descriptors to discover what one store per top-level slot achieves.
#[allow(
    dead_code,
    reason = "only x86-64 drops its identity map this way; see the doc comment"
)]
pub(crate) fn clear_root_slots(slots: core::ops::Range<usize>) {
    let root = ROOT_TABLE.load(Ordering::Relaxed);
    with_tables(|_| {
        for slot in slots {
            KernelPhysMem.write(PhysAddr(root + (slot as u64) * 8), 0);
        }
    });
    crate::smp::flush_tlb_everywhere();
}

/// Zero a frame through the direct map.
///
/// Every path that hands memory to somebody — a fresh page table, an anonymous
/// page, a kernel stack — goes through this rather than repeating the
/// `write_bytes`, because a page handed out still holding the last owner's
/// data is an information leak and from stage 6 the last owner is another
/// process.
pub(crate) fn zero_frame(frame: Frame) {
    // SAFETY: the caller has just taken `frame` from the buddy allocator, so
    // nothing else refers to it, and the direct map covers every frame of RAM
    // and is writable.
    unsafe { core::ptr::write_bytes(physmap(frame * PAGE_SIZE) as *mut u8, 0, PAGE_SIZE as usize) };
}

/// Copy a whole frame through the direct map.
///
/// The copy in copy-on-write, and the only reason this is not
/// [`zero_frame`]'s neighbour by accident: both exist because a frame handed
/// to somebody must hold exactly what that somebody is entitled to see, and
/// this is the case where that is the sharer's current contents rather than
/// zeroes.
///
/// `destination` must be a frame the caller has just allocated and nothing
/// else refers to; `source` may be shared with any number of readers, which is
/// the situation that made the copy necessary.
pub(crate) fn copy_frame(destination: Frame, source: Frame) {
    // SAFETY: the direct map covers every frame of RAM and is writable. The
    // two frames are distinct -- the caller allocated `destination` while
    // `source` was already committed -- so the regions do not overlap, and
    // nothing else refers to `destination`.
    unsafe {
        core::ptr::copy_nonoverlapping(
            physmap(source * PAGE_SIZE) as *const u8,
            physmap(destination * PAGE_SIZE) as *mut u8,
            PAGE_SIZE as usize,
        );
    }
}
