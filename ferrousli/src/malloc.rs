//! `malloc` and its family: `free`, `calloc`, `realloc`, `reallocarray`,
//! `aligned_alloc`, `posix_memalign`, `memalign`, `valloc`, `pvalloc` and
//! `malloc_usable_size`.
//!
//! Memory comes from `mmap` alone, never `brk`: it is the simplest contract a
//! new kernel can offer, and it is the one Ferrix offers first.
//!
//! # Layout
//!
//! Every pointer the allocator returns has a 16-byte [`Header`] right below it,
//! so every block is `16 + payload` bytes and starts at a multiple of 16. The
//! payload is therefore aligned to 16, which is `max_align_t`'s alignment on
//! x86-64 (`include/bits/alltypes.h`).
//!
//! ```text
//!   block ─► ┌──────────────────────────┐
//!            │ tag  = block ^ KEY ^ kind │  8 bytes
//!            │ info (depends on kind)    │  8 bytes
//! pointer ─► ├──────────────────────────┤
//!            │ payload                   │  a free small block keeps the
//!            │ …                         │  next free block's address here
//!            └──────────────────────────┘
//! ```
//!
//! The tag binds the header to its own address, so a pointer the allocator
//! never returned almost never finds a valid tag below it. The kinds are:
//!
//! * [`SMALL`], a block in use from a size class. `info` is the class. Small
//!   and medium requests, whose block is at most [`MAX_SMALL_BLOCK`], are
//!   carved from 1 MiB regions ([`REGION`]) by a bump pointer, and kept on a
//!   free list per class once freed.
//! * [`FREE`], a small block on its class's free list. `info` is still the
//!   class.
//! * [`LARGE`], a block with a mapping of its own, which starts at the header.
//!   `info` is the mapping's length. `free` unmaps it, and `realloc` moves it
//!   with `mremap`.
//! * [`ALIGNED`], a stub inside a larger block, below a pointer returned by an
//!   aligned allocation that the block's own payload did not already satisfy.
//!   `info` is the distance from the block's payload up to the pointer.
//!
//! Size classes run in steps of 16 bytes up to 128, then in four steps per
//! doubling up to 128 KiB: 32, 48, … 128, 160, 192, 224, 256, 320, … 131072.
//! A request rounds up to at most a quarter more than it asked for.
//!
//! # The large threshold
//!
//! A block larger than 128 KiB is mapped on its own. Below that, the two system
//! calls and the page faults a mapping costs outweigh what it saves, and page
//! rounding would waste a large fraction of the request. Above it, a size
//! class would waste up to a quarter of the request, which a mapping rounds
//! down to less than a page, and returning the memory to the kernel on `free`
//! starts to matter. glibc's default `M_MMAP_THRESHOLD` and musl's are the same
//! size, so programs tuned for either see the same trade-off.
//!
//! # What corruption is caught
//!
//! The program is stopped with a message and `abort` when `free`, `realloc` or
//! `malloc_usable_size` meets:
//!
//! * a pointer that is not a multiple of 16;
//! * a header whose tag does not match its address, which catches most
//!   pointers the allocator never returned, interior pointers, and a header
//!   overwritten by an overflow from the block below it;
//! * a small block that is already free: a double free, as long as the block
//!   has not been handed out again since;
//! * a second free of an aligned pointer, whose stub the first free erased.
//!
//! `malloc` also checks the tag of the block it takes from a free list, which
//! catches a use after free that overwrote the list's link with something that
//! is not a free block.
//!
//! Not caught: a double free after the block was reused, which frees the new
//! owner's memory; a second free of a large block, which faults with `SIGSEGV`
//! on its unmapped header instead; an overflow that stays inside a block's
//! payload or its rounding; and a pointer whose reading of the header faults,
//! which also ends in `SIGSEGV`.
//!
//! # Threads
//!
//! One [`Arena`] holds all state, behind a [`SpinLock`]. The lock is never held
//! across a system call: a new region is mapped with it released. Per-thread
//! arenas can come later as more `Arena`s. A small block would then record its
//! arena in the high bits of `info`, which hold nothing today.
//!
//! # Recursion
//!
//! `calloc` and `realloc` zero and copy with [`string::memset`] and
//! [`string::memcpy`], and nothing here moves a value larger than two words,
//! as `CONVENTIONS.md` requires of code the `string` functions might reach.

use core::ffi::{c_int, c_void};
use core::mem::size_of;
use core::ptr::{null_mut, with_exposed_provenance_mut};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::lock::SpinLock;
use crate::syscall::{self, nr};
use crate::{errno, signal, string};

/// The granule of the mapping arithmetic: 4 KiB, the smallest page of all
/// three architectures. A kernel with larger pages -- AArch64's 16 or 64 KiB --
/// rounds every mapping length up the same way on `mmap`, `munmap` and
/// `mremap`, and every mapping here is made whole and released whole, so only
/// `valloc` and `pvalloc` see the difference, and they ask the auxiliary
/// vector for the real page.
const PAGE_SIZE: usize = 4096;

/// `PROT_READ | PROT_WRITE`, from `asm-generic/mman-common.h`.
const PROT_READ_WRITE: usize = 0x1 | 0x2;
/// `MAP_PRIVATE | MAP_ANONYMOUS`, from `linux/mman.h` and
/// `asm-generic/mman-common.h`.
const MAP_PRIVATE_ANONYMOUS: usize = 0x02 | 0x20;
/// `MREMAP_MAYMOVE`, from `linux/mman.h`.
const MREMAP_MAYMOVE: usize = 1;

/// The alignment of every pointer returned: `max_align_t`'s on x86-64.
const ALIGN: usize = 16;
/// The size of a [`Header`].
const HEADER: usize = 16;
/// The smallest block: a header and 16 bytes, which also holds a free block's
/// link.
const MIN_BLOCK: usize = 32;
/// The largest block served from a size class. Larger ones are mapped alone.
const MAX_SMALL_BLOCK: usize = 128 * 1024;
/// How many size classes there are: 7 in steps of 16 up to 128, then 4 per
/// doubling from 128 to 131072.
const CLASSES: usize = 7 + 4 * 10;
/// How much is mapped at once to carve small blocks from.
const REGION: usize = 1 << 20;
/// The largest request that is attempted. Anything above leaves room for the
/// rounding below without overflow, and no mapping that size can succeed.
const MAX_REQUEST: usize = isize::MAX.cast_unsigned() - 4 * REGION;

/// Mixed into every tag. Its low four bits are zero, where the kind goes. A
/// 32-bit target keeps the low half.
const KEY: usize = 0x5f3c_9a1e_7d24_b860_u64 as usize;
/// A block in use from a size class.
const SMALL: usize = 1;
/// A small block on its class's free list.
const FREE: usize = 2;
/// A block with a mapping of its own.
const LARGE: usize = 3;
/// A stub below an aligned pointer, pointing back at its block.
const ALIGNED: usize = 4;

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Header>() == HEADER);
const _: () = assert!(KEY.is_multiple_of(ALIGN));
const _: () = assert!(class_size(CLASSES - 1) == MAX_SMALL_BLOCK);
const _: () = assert!(class_of(MAX_SMALL_BLOCK) == CLASSES - 1);
const _: () = assert!(REGION >= MAX_SMALL_BLOCK && REGION.is_multiple_of(PAGE_SIZE));

/// The 16 bytes below every pointer the allocator returns.
#[repr(C)]
#[derive(Debug)]
struct Header {
    /// The block's address, [`KEY`] and the kind, XORed together.
    tag: usize,
    /// The class, the mapping's length or the stub's offset, by kind.
    info: usize,
}

/// The allocator's state for small blocks.
#[derive(Debug)]
struct Arena {
    /// Held while any other field is read or written.
    lock: SpinLock,
    /// Each class's first free block, or zero.
    free: [AtomicUsize; CLASSES],
    /// Where the next fresh block is carved.
    bump: AtomicUsize,
    /// The end of the region `bump` carves from.
    end: AtomicUsize,
}

/// The only arena, until threads have their own.
static ARENA: Arena = Arena::new();

/// The tag of a header at `block` of this `kind`.
const fn tag(block: usize, kind: usize) -> usize {
    block ^ KEY ^ kind
}

/// The kind a header at `block` with this `tag` claims. Anything but the four
/// kinds means the header is not the allocator's.
const fn kind_of(block: usize, tag: usize) -> usize {
    tag ^ block ^ KEY
}

/// `value` rounded up to a multiple of `align`, a power of two. The caller
/// keeps `value + align` from overflowing.
const fn round_up(value: usize, align: usize) -> usize {
    (value + (align - 1)) & !(align - 1)
}

/// The block that holds a payload of `n` bytes, or `None` if `n` is too large
/// to try.
const fn block_for(n: usize) -> Option<usize> {
    if n > MAX_REQUEST {
        return None;
    }
    let block = round_up(n + HEADER, ALIGN);
    Some(if block < MIN_BLOCK { MIN_BLOCK } else { block })
}

/// The smallest class whose blocks hold `block` bytes, which is at most
/// [`MAX_SMALL_BLOCK`].
const fn class_of(block: usize) -> usize {
    if block <= 128 {
        let block = if block < MIN_BLOCK { MIN_BLOCK } else { block };
        block.div_ceil(16) - 2
    } else {
        // 2^k < block <= 2^(k+1), and the classes split that into quarters.
        let k = (usize::BITS - 1 - (block - 1).leading_zeros()) as usize;
        let quarter = 1 << (k - 2);
        let steps = (block - (1 << k)).div_ceil(quarter);
        7 + (k - 7) * 4 + steps - 1
    }
}

/// The block size of `class`, which is below [`CLASSES`].
const fn class_size(class: usize) -> usize {
    if class < 7 {
        MIN_BLOCK + 16 * class
    } else {
        let j = class - 7;
        let k = 7 + j / 4;
        (1 << k) + (j % 4 + 1) * (1 << (k - 2))
    }
}

/// The largest class whose block fits in `space` bytes, if any does.
const fn class_within(space: usize) -> Option<usize> {
    if space < MIN_BLOCK {
        return None;
    }
    let space = if space > MAX_SMALL_BLOCK {
        MAX_SMALL_BLOCK
    } else {
        space
    };
    let class = class_of(space);
    Some(if class_size(class) > space {
        class - 1
    } else {
        class
    })
}

/// The header at `block`.
fn header(block: usize) -> *mut Header {
    with_exposed_provenance_mut(block)
}

/// The tag and the info of the header at `block`.
///
/// # Safety
///
/// `block` must be readable for 16 bytes.
unsafe fn read_header(block: usize) -> (usize, usize) {
    let h = header(block);
    // SAFETY: the caller vouches for the 16 bytes.
    let tag = unsafe { (*h).tag };
    // SAFETY: as above.
    let info = unsafe { (*h).info };
    (tag, info)
}

/// Where a free small block at `block` keeps the next free block's address.
fn link(block: usize) -> *mut usize {
    with_exposed_provenance_mut(block + HEADER)
}

/// Stops the program because the heap's metadata is not what the allocator
/// wrote. Nothing may hold the arena's lock, since a `SIGABRT` handler may
/// allocate.
#[cold]
fn corrupt(what: &'static [u8]) -> ! {
    const PREFIX: &[u8] = b"ferrousli: ";
    // SAFETY: both are statics, and the kernel only reads them.
    let _ = unsafe { syscall::syscall3(nr::WRITE, 2, PREFIX.as_ptr().addr(), PREFIX.len()) };
    // SAFETY: as above.
    let _ = unsafe { syscall::syscall3(nr::WRITE, 2, what.as_ptr().addr(), what.len()) };
    signal::abort()
}

/// Sets `errno` to `ENOMEM` and returns the failed allocation.
fn out_of_memory() -> (usize, bool) {
    errno::set(errno::ENOMEM);
    (0, false)
}

/// A new anonymous mapping of `len` bytes, or zero.
fn map(len: usize) -> usize {
    // SAFETY: a new anonymous private mapping aliases nothing. The file
    // descriptor is -1 as `mmap` requires for anonymous memory.
    let ret = unsafe {
        syscall::mmap(
            0,
            len,
            PROT_READ_WRITE,
            MAP_PRIVATE_ANONYMOUS,
            usize::MAX,
            0,
        )
    };
    errno::decode(ret).unwrap_or(0)
}

/// Gives a large block's mapping back to the kernel.
///
/// # Safety
///
/// Nothing may use the mapping afterwards.
unsafe fn unmap(block: usize, len: usize) {
    // SAFETY: the caller gives up the mapping.
    let _ = unsafe { syscall::syscall2(nr::MUNMAP, block, len) };
}

impl Arena {
    /// An arena with no regions and empty free lists.
    const fn new() -> Self {
        Self {
            lock: SpinLock::new(),
            free: [const { AtomicUsize::new(0) }; CLASSES],
            bump: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
        }
    }

    /// A block of `class`, as its payload's address and whether that is fresh
    /// from `mmap` and so zero. The address is zero, with `errno` set, if no
    /// memory could be mapped.
    fn allocate(&self, class: usize) -> (usize, bool) {
        let Some(head) = self.free.get(class) else {
            return out_of_memory();
        };
        loop {
            if let Some(found) = self.take(head, class) {
                return found;
            }
            // Spinning waiters must not wait out a system call.
            let region = map(REGION);
            if region == 0 {
                return out_of_memory();
            }
            let _guard = self.lock.lock();
            self.retire();
            self.bump.store(region, Ordering::Relaxed);
            self.end.store(region + REGION, Ordering::Relaxed);
        }
    }

    /// Takes the first block on `class`'s free list `head`, or carves a fresh
    /// one from the current region. `None` if the list is empty and the region
    /// is used up.
    fn take(&self, head: &AtomicUsize, class: usize) -> Option<(usize, bool)> {
        let guard = self.lock.lock();
        let block = head.load(Ordering::Relaxed);
        if block != 0 {
            // SAFETY: a block on a free list lies in a region this arena
            // mapped and never unmaps.
            let (tag_now, info) = unsafe { read_header(block) };
            if tag_now != tag(block, FREE) || info != class {
                drop(guard);
                corrupt(b"malloc(): a free block was overwritten\n");
            }
            // SAFETY: a free block's payload holds its link.
            let next = unsafe { link(block).read() };
            head.store(next, Ordering::Relaxed);
            // SAFETY: the block is this arena's, and now no list's.
            unsafe { (*header(block)).tag = tag(block, SMALL) };
            return Some((block + HEADER, false));
        }
        let size = class_size(class);
        let bump = self.bump.load(Ordering::Relaxed);
        let end = self.end.load(Ordering::Relaxed);
        if end - bump < size {
            return None;
        }
        self.bump.store(bump + size, Ordering::Relaxed);
        // SAFETY: `bump..bump + size` is an unused part of a region this arena
        // mapped.
        unsafe {
            header(bump).write(Header {
                tag: tag(bump, SMALL),
                info: class,
            });
        }
        Some((bump + HEADER, true))
    }

    /// Carves what is left of the current region into free blocks, largest
    /// first, so that none of it is lost when a new region replaces it. The
    /// lock must be held.
    fn retire(&self) {
        let mut at = self.bump.load(Ordering::Relaxed);
        let end = self.end.load(Ordering::Relaxed);
        while let Some(class) = class_within(end - at) {
            self.push(at, class);
            at += class_size(class);
        }
        self.bump.store(end, Ordering::Relaxed);
    }

    /// Puts the block at `block` of `class` on its free list. The lock must be
    /// held, and `class` must be below [`CLASSES`].
    fn push(&self, block: usize, class: usize) {
        let Some(head) = self.free.get(class) else {
            corrupt(b"free(): invalid size class\n");
        };
        // SAFETY: the block lies in one of this arena's regions, and the
        // caller hands it over.
        unsafe {
            header(block).write(Header {
                tag: tag(block, FREE),
                info: class,
            });
        }
        // SAFETY: the payload is at least 16 bytes, and no longer the
        // program's.
        unsafe { link(block).write(head.load(Ordering::Relaxed)) };
        head.store(block, Ordering::Relaxed);
    }

    /// Frees the small block at `block`, catching a double free.
    fn release(&self, block: usize) {
        let guard = self.lock.lock();
        // SAFETY: `owner` found a small block's header here.
        let (tag_now, class) = unsafe { read_header(block) };
        if tag_now != tag(block, SMALL) {
            drop(guard);
            corrupt(b"double free\n");
        }
        self.push(block, class);
    }
}

/// A block holding `n` bytes, as its payload's address and whether that
/// memory is known to be zero. The address is zero, with `errno` set to
/// `ENOMEM`, on failure.
fn allocate(n: usize) -> (usize, bool) {
    let Some(block) = block_for(n) else {
        return out_of_memory();
    };
    if block <= MAX_SMALL_BLOCK {
        return ARENA.allocate(class_of(block));
    }
    let len = round_up(block, PAGE_SIZE);
    let at = map(len);
    if at == 0 {
        return out_of_memory();
    }
    // SAFETY: the mapping is new, and at least a page long.
    unsafe {
        header(at).write(Header {
            tag: tag(at, LARGE),
            info: len,
        });
    }
    (at + HEADER, true)
}

/// A block holding `n` bytes whose payload address is a multiple of `align`, a
/// power of two. Zero, with `errno` set to `ENOMEM`, on failure.
fn allocate_aligned(align: usize, n: usize) -> usize {
    if align <= ALIGN {
        return allocate(n).0;
    }
    // A block's payload is a multiple of 16, so the next multiple of `align`
    // is at most `align - 16` above it.
    let Some(total) = n.checked_add(align - ALIGN) else {
        return out_of_memory().0;
    };
    let base = allocate(total).0;
    if base == 0 {
        return 0;
    }
    // `total` was below `MAX_REQUEST`, so `align` is too, and the addition in
    // `round_up` cannot overflow.
    let aligned = round_up(base, align);
    if aligned != base {
        // The stub lies in the block's payload, below the aligned pointer.
        let stub = aligned - HEADER;
        // SAFETY: `base <= stub` and the payload holds `total` bytes, so the
        // stub is inside it.
        unsafe {
            header(stub).write(Header {
                tag: tag(stub, ALIGNED),
                info: aligned - base,
            });
        }
    }
    aligned
}

/// The header of the block that owns the pointer `p`, which the program says
/// the allocator returned. Stops the program if the metadata disagrees.
fn owner(p: usize) -> usize {
    if !p.is_multiple_of(ALIGN) || p < MIN_BLOCK {
        corrupt(b"invalid pointer\n");
    }
    let h = p - HEADER;
    // SAFETY: a pointer the allocator returned has its header here. For any
    // other pointer the program is already undefined, and this read either
    // faults or finds a tag that does not match.
    let (tag_now, info) = unsafe { read_header(h) };
    match kind_of(h, tag_now) {
        SMALL if info < CLASSES => h,
        LARGE if info > MAX_SMALL_BLOCK && info.is_multiple_of(PAGE_SIZE) => h,
        FREE => corrupt(b"double free\n"),
        ALIGNED if info.is_multiple_of(ALIGN) && info >= ALIGN && info < h => {
            let block = p - info - HEADER;
            // SAFETY: the stub says a block's header is here.
            let (tag_now, info) = unsafe { read_header(block) };
            match kind_of(block, tag_now) {
                SMALL if info < CLASSES => block,
                LARGE if info > MAX_SMALL_BLOCK => block,
                FREE => corrupt(b"double free\n"),
                _ => corrupt(b"invalid pointer\n"),
            }
        }
        _ => corrupt(b"invalid pointer\n"),
    }
}

/// The payload size of the block whose header `owner` returned.
fn usable(block: usize) -> usize {
    // SAFETY: `owner` validated this header.
    let (tag_now, info) = unsafe { read_header(block) };
    if kind_of(block, tag_now) == LARGE {
        info - HEADER
    } else {
        class_size(info) - HEADER
    }
}

/// Frees the pointer `p`, which is not null.
fn release(p: usize) {
    let block = owner(p);
    if p != block + HEADER {
        // Erase the stub, so a second free of the aligned pointer is caught.
        // SAFETY: `owner` found the stub here.
        unsafe { (*header(p - HEADER)).tag = 0 };
    }
    // SAFETY: `owner` validated the header.
    let (tag_now, len) = unsafe { read_header(block) };
    if kind_of(block, tag_now) == LARGE {
        // SAFETY: the program has given the block up.
        unsafe { unmap(block, len) };
    } else {
        ARENA.release(block);
    }
}

/// The address `addr` as a C pointer.
fn pointer(addr: usize) -> *mut c_void {
    with_exposed_provenance_mut(addr)
}

/// Allocates `size` bytes aligned for any object. `malloc(0)` returns a unique
/// pointer. On failure it returns null and sets `errno` to `ENOMEM`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn malloc(size: usize) -> *mut c_void {
    pointer(allocate(size).0)
}

/// Frees `p`, which the allocator returned. `free(NULL)` does nothing.
///
/// # Safety
///
/// `p` must be null or a live pointer from this allocator. The program is
/// stopped for some pointers that are not; see the module documentation.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn free(p: *mut c_void) {
    if !p.is_null() {
        release(p.addr());
    }
}

/// Allocates zeroed memory for `nmemb` objects of `size` bytes each. Fails with
/// `ENOMEM` if the product overflows.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn calloc(nmemb: usize, size: usize) -> *mut c_void {
    let Some(total) = nmemb.checked_mul(size) else {
        return pointer(out_of_memory().0);
    };
    let (addr, fresh) = allocate(total);
    let p = pointer(addr);
    if addr != 0 && !fresh {
        // SAFETY: the block's payload holds `total` bytes, and is the
        // program's alone.
        let _ = unsafe { string::memset(p, 0, total) };
    }
    p
}

/// Resizes the allocation at `p` to `size` bytes, keeping its contents up to
/// the smaller size.
///
/// `realloc(NULL, size)` is `malloc(size)`. `realloc(p, 0)` does what musl
/// does: it shrinks the allocation to zero bytes and returns a unique non-null
/// pointer, which may be `p`, that must still be freed. It returns null only if
/// memory ran out, and then `p` is untouched. glibc instead frees `p` and
/// returns null, and C23 leaves the case undefined.
///
/// A small allocation stays in place while the new size fits its block and
/// needs at least half of it. A large one stays large through `mremap`. On
/// failure the result is null, `errno` is `ENOMEM`, and `p` is untouched.
///
/// # Safety
///
/// `p` must be null or a live pointer from this allocator.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn realloc(p: *mut c_void, size: usize) -> *mut c_void {
    if p.is_null() {
        return malloc(size);
    }
    let addr = p.addr();
    let block = owner(addr);
    let offset = addr - block - HEADER;
    let old = usable(block) - offset;
    let Some(needed) = block_for(size) else {
        return pointer(out_of_memory().0);
    };

    if offset == 0 {
        // SAFETY: `owner` validated the header.
        let (tag_now, info) = unsafe { read_header(block) };
        if kind_of(block, tag_now) == SMALL {
            let have = class_size(info);
            if needed <= have && needed * 2 >= have {
                return p;
            }
        } else if needed > MAX_SMALL_BLOCK {
            let len = round_up(needed, PAGE_SIZE);
            if len == info {
                return p;
            }
            // SAFETY: the mapping is the block's alone, and the program hands
            // it over for the call. The kernel keeps its contents.
            let ret = unsafe { syscall::syscall4(nr::MREMAP, block, info, len, MREMAP_MAYMOVE) };
            let Ok(moved) = errno::decode(ret) else {
                return pointer(out_of_memory().0);
            };
            // SAFETY: the mapping now starts at `moved`, and the tag must name
            // its new address.
            unsafe {
                header(moved).write(Header {
                    tag: tag(moved, LARGE),
                    info: len,
                });
            }
            return pointer(moved + HEADER);
        }
    }

    let new = allocate(size).0;
    if new == 0 {
        return null_mut();
    }
    let keep = if size < old { size } else { old };
    // SAFETY: the old payload holds `old` bytes and the new one `size`, and
    // they are different blocks.
    let _ = unsafe { string::memcpy(pointer(new), p, keep) };
    release(addr);
    pointer(new)
}

/// `realloc` for `nmemb` objects of `size` bytes each, failing with `ENOMEM`
/// if the product overflows.
///
/// # Safety
///
/// As [`realloc`].
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn reallocarray(p: *mut c_void, nmemb: usize, size: usize) -> *mut c_void {
    let Some(total) = nmemb.checked_mul(size) else {
        return pointer(out_of_memory().0);
    };
    // SAFETY: the caller's contract is `realloc`'s.
    unsafe { realloc(p, total) }
}

/// Allocates `size` bytes at a multiple of `alignment`, a power of two.
///
/// As in musl, an alignment that is not a power of two fails with `EINVAL`,
/// and zero is taken as the default alignment.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn aligned_alloc(alignment: usize, size: usize) -> *mut c_void {
    if alignment & alignment.wrapping_sub(1) != 0 {
        errno::set(errno::EINVAL);
        return null_mut();
    }
    pointer(allocate_aligned(alignment, size))
}

/// The older name of [`aligned_alloc`], which `malloc.h` declares. glibc
/// rounds a bad alignment up to a power of two; musl, and this, fail with
/// `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn memalign(alignment: usize, size: usize) -> *mut c_void {
    aligned_alloc(alignment, size)
}

/// Allocates `size` bytes at a multiple of `alignment` and stores the pointer
/// in `*memptr`.
///
/// Returns `EINVAL`, without touching `errno` or `*memptr`, if `alignment` is
/// not a power of two at least `sizeof(void *)`. Returns `ENOMEM`, with
/// `errno` also set to it, if memory ran out.
///
/// # Safety
///
/// `memptr` must be valid for writing a pointer.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn posix_memalign(
    memptr: *mut *mut c_void,
    alignment: usize,
    size: usize,
) -> c_int {
    if !alignment.is_power_of_two() || alignment < size_of::<*mut c_void>() {
        return errno::EINVAL;
    }
    let addr = allocate_aligned(alignment, size);
    if addr == 0 {
        return errno::ENOMEM;
    }
    // SAFETY: the caller vouches for `memptr`.
    unsafe { memptr.write(pointer(addr)) };
    0
}

/// Allocates `size` bytes at a page boundary.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn valloc(size: usize) -> *mut c_void {
    pointer(allocate_aligned(crate::sysconf::page_size(), size))
}

/// Allocates `size` bytes rounded up to whole pages, at a page boundary, and at
/// least one page. musl's headers do not declare it; glibc exports it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn pvalloc(size: usize) -> *mut c_void {
    if size > MAX_REQUEST {
        return pointer(out_of_memory().0);
    }
    let page = crate::sysconf::page_size();
    let pages = round_up(size, page);
    let pages = if pages == 0 { page } else { pages };
    pointer(allocate_aligned(page, pages))
}

/// How many bytes the allocation at `p` can hold, which is at least what was
/// asked for. Zero for null.
///
/// # Safety
///
/// `p` must be null or a live pointer from this allocator.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn malloc_usable_size(p: *mut c_void) -> usize {
    if p.is_null() {
        return 0;
    }
    let addr = p.addr();
    let block = owner(addr);
    usable(block) - (addr - block - HEADER)
}

/// glibc's tuning knob: a trim threshold, a mapping threshold, an arena
/// count. This allocator has none of them, so every request is accepted and
/// changes nothing, and the answer is glibc's for success, 1. A program
/// calls it to tune, never to change what its allocations mean.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn mallopt(_param: c_int, _value: c_int) -> c_int {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_block_size_gets_the_smallest_class_that_holds_it() {
        for block in MIN_BLOCK..=MAX_SMALL_BLOCK {
            let class = class_of(block);
            assert!(class < CLASSES, "{block}");
            assert!(class_size(class) >= block, "{block}");
            if class > 0 {
                assert!(class_size(class - 1) < block, "{block}");
            }
        }
    }

    #[test]
    fn classes_grow_by_at_most_a_quarter_in_multiples_of_16() {
        assert_eq!(class_size(0), MIN_BLOCK);
        for class in 1..CLASSES {
            let (below, size) = (class_size(class - 1), class_size(class));
            assert!(size > below);
            assert!(size.is_multiple_of(16));
            assert!(size - below <= (below / 4).max(16), "{class}");
        }
        assert_eq!(class_size(7), 160);
        assert_eq!(class_size(10), 256);
        assert_eq!(class_size(11), 320);
    }

    #[test]
    fn the_largest_class_within_a_space_fits_it() {
        assert_eq!(class_within(31), None);
        assert_eq!(class_within(32), Some(0));
        assert_eq!(class_within(47), Some(0));
        assert_eq!(class_within(300), Some(10));
        assert_eq!(class_within(REGION), Some(CLASSES - 1));
    }

    #[test]
    fn a_block_holds_its_header_and_rounds_to_16() {
        assert_eq!(block_for(0), Some(32));
        assert_eq!(block_for(16), Some(32));
        assert_eq!(block_for(17), Some(48));
        assert_eq!(block_for(MAX_SMALL_BLOCK - HEADER), Some(MAX_SMALL_BLOCK));
        assert_eq!(block_for(MAX_REQUEST + 1), None);
        assert_eq!(block_for(usize::MAX), None);
    }

    #[test]
    fn a_tag_names_its_own_address_and_kind() {
        let block = 0x7f00_1234_5670_u64 as usize;
        for kind in [SMALL, FREE, LARGE, ALIGNED] {
            assert_eq!(kind_of(block, tag(block, kind)), kind);
            assert!(!matches!(
                kind_of(block + 16, tag(block, kind)),
                SMALL | FREE | LARGE | ALIGNED
            ));
        }
        assert!(!matches!(kind_of(block, 0), SMALL | FREE | LARGE | ALIGNED));
    }

    #[test]
    fn round_up_goes_to_the_next_multiple() {
        assert_eq!(round_up(0, 16), 0);
        assert_eq!(round_up(1, 16), 16);
        assert_eq!(round_up(4096, 4096), 4096);
        assert_eq!(round_up(4097, 4096), 8192);
    }

    /// Fills `n` bytes at `p` with `byte`.
    fn fill(p: *mut c_void, n: usize, byte: u8) {
        // SAFETY: the tests pass allocations of at least `n` bytes.
        let _ = unsafe { string::memset(p, c_int::from(byte), n) };
    }

    /// Whether all `n` bytes at `p` are `byte`.
    fn holds(p: *mut c_void, n: usize, byte: u8) -> bool {
        let bytes = p.cast::<u8>();
        // SAFETY: the tests pass allocations of at least `n` bytes.
        (0..n).all(|i| unsafe { bytes.wrapping_add(i).read() } == byte)
    }

    #[test]
    fn allocations_across_the_threshold_are_aligned_usable_and_distinct() {
        let sizes = [0, 1, 17, 1000, 131_056, 131_057, 1 << 20];
        let blocks: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| {
                let p = malloc(n);
                assert!(!p.is_null());
                assert!(p.addr().is_multiple_of(16));
                // SAFETY: `p` is live.
                assert!(unsafe { malloc_usable_size(p) } >= n);
                fill(p, n, i as u8);
                (p, n, i as u8)
            })
            .collect();
        for &(p, n, byte) in &blocks {
            assert!(holds(p, n, byte));
            // SAFETY: `p` is live, and freed once.
            unsafe { free(p) };
        }
    }

    #[test]
    fn realloc_keeps_contents_from_small_to_large_and_back() {
        let mut p = malloc(10);
        fill(p, 10, 0xa5);
        for n in [200, 200_000, 3 << 20, 150_000, 64, 10] {
            // SAFETY: `p` is live.
            p = unsafe { realloc(p, n) };
            assert!(!p.is_null());
            assert!(holds(p, 10, 0xa5), "{n}");
        }
        // SAFETY: as above.
        unsafe { free(p) };
    }

    #[test]
    fn calloc_zeroes_reused_memory_and_rejects_overflow() {
        let p = malloc(500);
        fill(p, 500, 0xff);
        // SAFETY: `p` is live.
        unsafe { free(p) };
        let q = calloc(5, 100);
        assert!(holds(q, 500, 0));
        // SAFETY: `q` is live.
        unsafe { free(q) };

        errno::set(0);
        assert!(calloc(usize::MAX / 2, 3).is_null());
        // SAFETY: this thread's `errno`.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::ENOMEM);
    }

    #[test]
    fn aligned_allocations_honour_the_alignment() {
        for shift in 0..=20 {
            let align = 1_usize << shift;
            let p = aligned_alloc(align, 100);
            assert!(p.addr().is_multiple_of(align));
            // SAFETY: `p` is live.
            assert!(unsafe { malloc_usable_size(p) } >= 100);
            fill(p, 100, 7);
            // SAFETY: as above.
            unsafe { free(p) };
        }
        let mut out = null_mut();
        // A power of two below a pointer's size.
        let small = size_of::<usize>() / 2;
        // SAFETY: `out` is a local.
        let below_a_pointer = unsafe { posix_memalign(&raw mut out, small, 1) };
        assert_eq!(below_a_pointer, errno::EINVAL);
        // SAFETY: as above.
        let not_a_power = unsafe { posix_memalign(&raw mut out, 24, 1) };
        assert_eq!(not_a_power, errno::EINVAL);
        assert!(out.is_null());
    }

    #[test]
    fn threads_share_the_arena() {
        let workers: Vec<_> = (0..4_u8)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..2000 {
                        let n = 1 + (i * 37) % 3000;
                        let p = malloc(n);
                        fill(p, n, t);
                        assert!(holds(p, n, t));
                        // SAFETY: `p` is live.
                        unsafe { free(p) };
                    }
                })
            })
            .collect();
        for worker in workers {
            assert!(worker.join().is_ok());
        }
    }
}
