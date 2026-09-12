/*
 * Ferrix — SysML v2 model, part 4: memory.
 *
 * docs/ARCHITECTURE.md §4. The physical allocator, the heap, the page-table
 * arithmetic and the kernel arena run today; VMOs, process address spaces,
 * copy-on-write and reclaim are stage 6 and after.
 */
package FerrixMemory {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;

    // -----------------------------------------------------------------------
    // Physical frames (libs/frame, kernel/src/mm.rs)
    // -----------------------------------------------------------------------

    enum def FrameState { Reserved; Free; FreeTail; Allocated; }

    attribute def PageEntry {
        doc /* One per frame, in a side array, so the allocator is index
             * arithmetic and forbid(unsafe_code). Not a concession to testing:
             * the refcount is what copy-on-write needs, and the free-object
             * count is what lets the heap return slab pages. Linux calls the
             * same conclusion struct page. */
        attribute next : Natural;
        attribute previous : Natural;
        attribute refcount : Natural;
        attribute order : Natural;
        attribute frameState : FrameState;
        #planned attribute owner : Natural {
            @stage { number = 6; }
            doc /* The owning VMO, for reclaim and copy-on-write. */
        }
        #planned attribute flags : Natural {
            @stage { number = 6; }
        }
    }

    #implemented part def FrameAllocator {
        @stage { number = 2; }
        doc /* Buddy allocator over the memory map, orders 0 to 10 (up to
             * 4 MiB blocks). Blocks split when a smaller one is needed and
             * merge with their buddy when freed. allocate_below finds frames
             * under a limit, which the x86-64 AP trampoline needs. */
        attribute maxOrder : Natural = 10;
        attribute freeFrames : Natural;
        attribute managedFrames : Natural;
        attribute entries : PageEntry[0..*] ordered;
        action allocateBlock;
        action allocateBelow;
        action deallocate;
        action insertFree;
    }

    #implemented part def PhysicalMemory {
        @stage { number = 2; }
        doc /* kernel/src/mm.rs: where the per-frame array goes, reaching
             * physical memory through the direct map, and the kernel's own
             * page tables. The per-frame array is carved from the front of the
             * largest usable region before there is an allocator to make it.
             * Three globals behind interrupt-masking locks in a fixed order:
             * TABLES, then FRAMES, then HEAP. */
        part frames : FrameAllocator;
        part heap : KernelHeap;
        part tables : KernelPageTables;
        action allocateFrames;
        action deallocateFrames;
        action mapKernel;
        action unmapKernel {
            doc /* Unmap, invalidate everywhere, and only then free — holding
                 * what was released inline so the stage 2 checks' exact frame
                 * accounting still holds. */
        }
        action protectKernel;
        action translate;
        action checkWriteXorExecute;
        action reclaim;
        action mapDemandPage;

        part perCpuCaches : PerCpuFrameCache {
            @deferred { reason = "Each allocator is one lock, which is correct; the per-CPU magazines are a performance change waiting for a workload that can measure them."; }
        }
    }

    part def PerCpuFrameCache;

    // -----------------------------------------------------------------------
    // The kernel heap (libs/heap)
    // -----------------------------------------------------------------------

    interface def HeapBacking {
        doc /* The unsafe trait behind which a free-list allocator's loads and
             * stores live, so the allocator body is pure and host-testable. A
             * test implements it over a map; the kernel over the direct map
             * and the buddy allocator. */
        action allocatePages;
        action deallocatePages;
        action readLink;
        action writeLink;
    }

    #implemented part def KernelHeap {
        @stage { number = 2; }
        doc /* Segregated free lists over a page supply: power-of-two size
             * classes from 8 to 2048 bytes, refilled a page at a time; larger
             * requests go to the backing as whole pages. Alignment up to the
             * class size comes free from page-aligned slabs. Empty slab pages
             * are returned to the buddy, with the free-object count in the
             * per-frame record; the last page of a class stays so a workload
             * oscillating across a page boundary does not pay a buddy call
             * per cycle. Makes Box, Vec and BTreeMap work. */
        attribute classSizes : Natural[9] = (8, 16, 32, 64, 128, 256, 512, 1024, 2048);
        attribute allocatedBytes : Natural;
        attribute pagesHeld : Natural;

        #planned part objectSlabs : ObjectSlab[0..*] {
            @stage { number = 5; }
            doc /* Kernel object types get their own slabs, so a Task
                 * allocation is a pop off a list. Per-CPU free lists for the
                 * general path. */
        }
    }

    part def ObjectSlab;

    // -----------------------------------------------------------------------
    // Page tables (libs/paging)
    // -----------------------------------------------------------------------

    attribute def MapFlags {
        attribute read : Boolean;
        attribute write : Boolean;
        attribute execute : Boolean;
        attribute user : Boolean;
        attribute global : Boolean;
        attribute device : Boolean;
    }

    attribute def TableGeometry {
        doc /* Every architecture uses 512 eight-byte descriptors over a 4 KiB
             * granule; what differs is the number of levels and the address
             * width. Four over 48 bits on the 64-bit pair, three over 32 on
             * ARMv7-A, whose LPAE format is AArch64's descriptor with a
             * narrower physical address. */
        attribute levels : Natural;
        attribute virtualBits : Natural;
    }

    interface def Encoding {
        doc /* The roughly forty lines of bit layout each architecture
             * supplies: table and leaf descriptors, presence, leafness at a
             * level, the address in an entry, block support per level. */
        action tableDescriptor;
        action leafDescriptor;
        action isPresent;
        action isLeaf;
        action address;
        action supportsBlock;
        action leafFlags;
    }

    interface def PhysMem {
        doc /* Reading and writing table entries at physical addresses, and
             * allocating a table. The loader implements it over its pool,
             * the kernel over the direct map and the buddy. */
        action read;
        action write;
        action allocateTable;
    }

    #implemented part def Mapper {
        @stage { number = 1; }
        doc /* The walk, written once, generic over an Encoding and a
             * geometry. PhysAddr and VirtAddr are distinct newtypes with no
             * arithmetic between them: confusing the two is this project's
             * characteristic bug and the one the compiler catches for free. */
        attribute geometry : TableGeometry;
        action mapRange;
        action unmapRange {
            doc /* Reports what it released — leaves and pruned tables — so
                 * the caller can invalidate first and free afterwards. */
        }
        action protectRange;
        action translate;
        action forEachLeaf {
            doc /* What the W^X sweep walks. */
        }
    }

    #implemented part def KernelPageTables {
        @stage { number = 2; }
        doc /* The kernel's root, shared into every secondary's tree and, from
             * stage 6, into every process's upper half through
             * share_kernel_slots. */
        part mapper : Mapper;
    }

    // -----------------------------------------------------------------------
    // The kernel arena (kernel/src/vmap.rs)
    // -----------------------------------------------------------------------

    #implemented part def VmapArena {
        @stage { number = 2; }
        doc /* "Give me a range of addresses and let me decide later what goes
             * behind it": device windows, non-contiguous buffers, guard-paged
             * stacks. The arena is a libs/vma AddressSpace over the vmap area
             * past the reserved windows, because a kernel arena and a process
             * address space are the same problem. Allocation identity lives in
             * a second map beside it, because the arena merges adjacent equal
             * ranges, which is right for a process and wrong for an allocator. */
        attribute stackPages : Natural = 4;
        part ranges : VmaMap;
        action allocateRange {
            doc /* A page-aligned range with an unmapped guard page either side,
                 * mapped with the flags asked for. */
        }
        action free {
            doc /* Unmap first, then release the address; the unmapping cannot
                 * happen under the arena lock because it waits for processors
                 * that cannot answer while spinning for it, so the address is
                 * reserved across the gap. Stage 5 found the other order. */
        }
        action mapDevice;
        action unmapDevice;
        action allocateStack {
            doc /* Guard-paged kernel stacks: what every per-CPU record, every
                 * secondary and, from stage 5, every task runs on. */
        }
        action checkInvariants;
    }

    // -----------------------------------------------------------------------
    // Virtual memory areas (libs/vma)
    // -----------------------------------------------------------------------

    enum def BackingKind { Anonymous; File; Device; }

    attribute def Backing {
        doc /* Every Vma names the object it maps and where in it, anonymous
             * memory included: Anonymous carries an id and an offset exactly
             * as File does, so both sides of a fork point at one VMO and copy
             * on write per page, and MAP_SHARED|MAP_ANONYMOUS needs nothing
             * unpicked when it arrives. An id of zero denotes private
             * anonymous memory with no shared object; by convention its
             * offset is the mapping's own start address (what Linux's
             * vm_pgoff holds for an anonymous VMA), so that adjacent private
             * regions remain contiguous and mergeable. */
        attribute kind : BackingKind;
        attribute id : Natural;
        attribute offset : Natural;
    }

    attribute def PageRange {
        doc /* Page-aligned, non-empty, does not wrap: validated once on the
             * way in, so a value of this type is a standing promise. */
        attribute start : Natural;
        attribute limit : Natural;
    }

    attribute def Vma {
        attribute range : PageRange;
        attribute flags : MapFlags;
        attribute backing : Backing;
        attribute cow : Boolean {
            doc /* Copy on write: the fault handler installs the page
                 * read-only when this is set. Read by the handler today; set
                 * by nothing until fork exists. */
        }
    }

    #implemented part def VmaMap {
        @stage { number = 6; }
        doc /* A sorted, non-overlapping set of regions in a Vec searched by
             * binary search — a span of adjacent regions is what every
             * operation works on, which a contiguous index range expresses
             * directly. Every operation is total: it succeeds or returns an
             * error, never panics. Adjacent regions with equal flags and
             * contiguous backing merge, without which an mprotect loop leaks
             * regions until the next mmap fails. Reached at stage 2 as the
             * vmap arena; stage 6 is its intended consumer. */
        attribute regions : Vma[0..*] ordered;
        action insert;
        action mapFixed { doc /* mmap with MAP_FIXED. */ }
        action remove { doc /* munmap: splits at both edges. */ }
        action protect { doc /* mprotect: splits and re-permissions. */ }
        action findFree { doc /* Top-down: the highest gap that fits. */ }
        action find;
    }

    // -----------------------------------------------------------------------
    // Stage 6 and after
    // -----------------------------------------------------------------------

    #inProgress part def Vmo {
        @stage { number = 6; }
        doc /* kernel/src/user/vmo.rs. A pageable memory object: pages, not a
             * mapping. Anonymous memory, page-cache pages, shared memory and
             * DMA buffers are all VMOs. The load-bearing unification: a block
             * driver filling a page-cache page fills the VMO the cache already
             * holds, with no copy, which is what makes userspace drivers
             * affordable for a compiler workload. Only the anonymous kind
             * exists today; file VMOs want the page cache (stage 8) and DMA
             * VMOs an IOMMU domain (stage 10), and the shape is the one they
             * extend rather than unpick. Frames go back through the per-frame
             * refcount. */
        attribute pages : Natural[0..*] ordered {
            doc /* Sparse: a map from page index to frame, absence meaning
                 * uncommitted, with commit on demand. A reservation costs
                 * nothing until it is touched, which the stage 6 self-check
                 * measures. */
        }
        attribute size : Natural;
        action commitPage;
        action lookupPage;
        action replacePage;
        action decommitRange {
            doc /* What munmap of part of a mapping does to the object behind
                 * it: the pages are gone, not merely unmapped, because a
                 * process that unmaps half its heap expects the memory back.
                 * Called only when the object has a single holder: a page of
                 * a shared object is not one unmapper's to take away, and the
                 * address space, not the VMO, decides. */
        }
        #planned action fileFill { @stage { number = 8; } }
    }

    #inProgress part def ProcessAddressSpace {
        @stage { number = 6; }
        doc /* kernel/src/user/space.rs: a root frame, a libs/vma AddressSpace,
             * and a map from id to VMO. Each Vma names a VMO, an offset, a
             * protection and a share mode. VMOs are shared through Arc, so
             * this references them and never owns them. The kernel half is
             * shared into the root by prepare_user_root. */
        part tables : Mapper;
        part vmas : VmaMap;
        ref part vmos : Vmo[0..*];
        action mapAnonymous;
        action fault : DemandFault;
        action install {
            @stage { number = 6; }
            doc /* Put this space's root in the processor's root register, so
                 * that the MMU walks in hardware what the mapper has until now
                 * only walked in software through the direct map. It does not
                 * change privilege level: a processor translates through a
                 * user space while still running kernel code, and has to — a
                 * fault taken in user mode is handled in the space that
                 * faulted. Something must hold a reference for as long as the
                 * root is installed, or the tables are freed underneath a
                 * processor still walking them. */
        }
        action unmap {
            doc /* Reshape the map, change the tables to match, and decommit
                 * the object's pages only if this space is its sole holder. */
        }
        action forkSpace {
            @stage { number = 6; }
            doc /* Mark both sides read-only and copy on the first write fault;
                 * the per-frame refcount is what makes it tractable. No memory
                 * is copied: each private object is cloned page list and all,
                 * with a reference taken on every committed frame, so a write
                 * on either side copies that page into its own object. The
                 * object is cloned rather than shared through one Arc because
                 * two spaces holding one Arc cannot diverge -- replacing a
                 * page would replace it for both, and a child's first write
                 * would be visible to its parent. A MAP_SHARED object is the
                 * case where sharing the Arc is right.
                 *
                 * Marking the map is not enough: the parent's tables still
                 * hold writable translations to the pages it has agreed to
                 * share, and a write through one never reaches the fault
                 * handler. So every region newly marked is unmapped in the
                 * parent and re-faults read-only. Unmapping rather than
                 * write-protecting in place is what the mapper offers -- map_in
                 * refuses to overwrite a live mapping by design and there is
                 * no protect_in -- and costs the parent one extra fault per
                 * page. */
        }
        #planned action mmap { @stage { number = 7; } }
        #planned action mprotect { @stage { number = 7; } }
        #planned action brk { @stage { number = 7; } }

        assert constraint spaceLock {
            doc /* One lock per address space, never a global one. The kernel's
                 * own map_in takes no lock because the tree it was written
                 * for is one nobody has installed; a user space is installed
                 * on the processors running its threads, whose MMUs walk it
                 * in hardware while another processor maps into it. The lock
                 * is held across the pair that must not be split — reshaping
                 * the map and changing the tables — so a fault between them
                 * cannot find a region the tables lack or a mapping the map
                 * does not know. Per space, so two processes faulting at once
                 * contend for nothing, which a compiler workload needs. */
        }
    }

    #inProgress action def DemandFault {
        @stage { number = 6; }
        doc /* The stage 3 handler generalised: find the Vma, then either
             * allocate a zeroed page (anonymous, lazy), fault from the page
             * cache VMO (file mapping, so a mapped file and a read file are
             * the same pages), copy a shared page whose refcount is above one
             * (copy-on-write), or deliver SIGSEGV. Built today: find the Vma,
             * commit the page in its VMO, install it, read-only when the Vma
             * is marked cow. */
        in fault : FerrixBoot::PageFault;
        doc /* The guard below is the region's cow mark and *not* whether the
             * page is present, which is the obvious guess and is wrong in both
             * directions. A copy-on-write page that has not been faulted in
             * yet is absent and still has to copy on a write; and a present
             * page that is not copy-on-write needs nothing, because a fault on
             * it is spurious -- another processor resolved it, or this one
             * walked a stale entry. Presence decides only whether the
             * read-only entry has to come down before the writable one goes
             * in. */
        action findVma;
        action anonymousZeroPage;
        #planned action pageCacheFill { @stage { number = 8; } }
        action copyOnWrite {
            @stage { number = 6; }
            doc /* Copy the page, replace it in this space's own object, and
                 * install it writable. A page with one holder left is let
                 * through uncopied: the other side has copied, unmapped or
                 * exited, so duplicating it would allocate a frame to hold a
                 * second copy of data nobody else can see. That refcount test
                 * is also why a second write to an already-copied page
                 * terminates, because a region keeps its cow mark forever --
                 * it is a property of the region and there is no per-page flag.
                 *
                 * This branch is above the handler's present-page early
                 * return, not below it. A write to a page that is present but
                 * deliberately read-only is exactly the fault that must copy,
                 * so returning early because the page translates sends the
                 * instruction back to fault forever. The boot check fails when
                 * the order is wrong rather than trusting the comment. */
        }
        #planned action deliverSigsegv { @stage { number = 7; } }
        first start;
        then findVma;
        then decide;
            if fault.write and vma.cow then copyOnWrite;
            else anonymousZeroPage;
    }

    #inProgress part def VirtualMemory {
        @stage { number = 6; }
        doc /* kernel/src/user: the VM subsystem above stage 2's allocators.
             * VMOs and address spaces exist and are self-checked at boot, a
             * space installed on a processor is walked by its MMU, and fork
             * shares a process's memory copy-on-write; still owed by stage 6
             * are a Task carrying an address space with the root swap in
             * choose_next, a TLB shootdown for user spaces (which cannot bite
             * until that lands, and is the first thing that will), the ELF
             * loader, set_kernel_stack, the ring-3/EL0/USR transition and the
             * syscall vectors. Reclaim scoped by cgroup is stage 13's. */
        part vmos : Vmo[0..*];
        part spaces : ProcessAddressSpace[0..*];
        #planned part reclaim : Reclaim;
        #planned part elfLoader : UserElfLoader;
    }

    #planned part def Reclaim {
        @stage { number = 13; }
        doc /* Two-list LRU (active/inactive) with a shrinker interface for the
             * caches. rustc will exhaust memory on a small machine, so this is
             * correctness: an OOM kill scoped by Job and cgroup, never a
             * livelock. */
        part activeList : LruList;
        part inactiveList : LruList;
        action shrink;
        action oomKill;
    }

    part def LruList;

    #planned part def UserElfLoader {
        @stage { number = 6; }
        doc /* libs/elf over a file, mapping segments as Vmas of a file VMO.
             * The first user process is a hand-written static binary that
             * makes one syscall. */
    }
}
