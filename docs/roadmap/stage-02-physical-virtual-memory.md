# Stage 2 — Physical and virtual memory ✅

The arithmetic lives in `src/lib/` and is unit-tested on the host; the parts that
touch `CR3` or `TTBR1` do not.

**Done.** The buddy allocator (`src/lib/kernel/frame`) and the kernel heap
(`src/lib/kernel/heap`), both wired up and running on both architectures:

* `src/lib/kernel/frame` — buddy allocator over the UEFI memory map, orders 0 to 10.
  Free-list links live in a per-frame side array rather than in the free pages,
  which makes the whole allocator index arithmetic and therefore
  `#![forbid(unsafe_code)]`, host-testable and fuzzable. The side array is not a
  concession to testing: a refcount per frame is what copy-on-write will need.
* `src/lib/kernel/heap` — segregated free lists over a page supply, behind a `Backing`
  trait so the loads and stores that a free-list allocator needs are the
  implementor's problem rather than the allocator's. `Box`, `Vec` and
  `BTreeMap` now work in the kernel.
* The chicken-and-egg — the per-frame array has to exist before there is an
  allocator to make it — is resolved by carving it from the front of the
  largest usable region inside the direct map and handing that region over
  with the carved part excluded. Inside, because the array is zeroed through
  the direct map, and on ARMv7-A that map holds 1.25 GiB: a board with more
  RAM whose longest region lay above it would have zeroed the kernel image.
* Physical address 0 is never a frame, on any architecture: frame 0 is never
  given to the allocator, at bring-up or when boot memory is reclaimed, and
  the boot check requires it to be neither managed, free nor allocatable on
  all three. OVMF calls 0x0-0x9FFFF conventional memory, so on x86-64 frame 0
  was an ordinary free frame, and under KVM it once became an address space's
  root (FX-0601).

**Exit criterion met, and in the boot test on both architectures:** 4096 blocks
of assorted orders allocated and freed in an order that forces coalescing, with
the free-frame count required to return exactly to where it started; a `Box`, a
`Vec` grown through several reallocations, and a `BTreeMap` of two thousand
entries, all verified to hold what was put in them.

**Done — the virtual half, and the tidying the physical half was owed.**

* `src/kernel/src/vmap.rs` — an arena over `KERNEL_VMAP_BASE` that hands out
  virtual ranges with an unmapped guard page either side, and guard-paged
  kernel stacks on top of it. Free ranges are coalesced with their neighbours,
  so a stack allocated and freed a thousand times does not fragment the arena
  into a thousand holes.
* The loader's identity map is dropped, in the one order that works: the W^X
  sweep cannot pass while it is live — the loader mapped itself writable *and*
  executable, and correctly so, since it was executing out of pages it was
  still relocating — so the map goes first and the sweep runs after. The kernel
  then proves the map is gone by asking the architecture: on x86-64 by walking
  the one tree for address zero and the kernel's physical address, and on the
  Arm pair by reading `EPD0` and `TTBR0` back from the processor, because
  there the identity map is a regime of its own that no walk of the kernel's
  tables reaches, and a walk would have found nothing whether it had gone or
  not.
* The loader's own memory and the ACPI-reclaim regions are handed to the buddy
  allocator once nothing points into them, which on a 512 MiB QEMU machine is
  3 MiB on x86-64 and 2 MiB on AArch64 — small in absolute terms and the
  entire difference between a kernel that can reclaim boot memory and one that
  cannot.
* The W^X sweep walks the live tables through `Mapper::for_each_leaf` and
  asserts no leaf is both writable and executable. It reports what it swept as
  well as what it found, because a sweep that walks nothing also finds nothing.
* `src/lib/kernel/heap` returns empty slab pages to the buddy. The free-object count
  lives in the per-frame record, as the note here asked — not in a header
  stolen from the first object, which would have made the allocator's own
  metadata the thing a use-after-free corrupts first. The last page of a class
  stays, so a workload oscillating across a page boundary does not pay a buddy
  allocation per cycle.

**Exit criterion met, and in the boot test on both architectures.** On top of
stage 2's allocator hammering: a vmap allocation is written through and read
back, two allocations are required to be separated by their guard pages, the
pages either side of a range are required to translate to nothing, a kernel
stack is required to be 16-byte aligned and writable at both ends with guards
beyond each, and freeing it is required to return every frame it held. A
device window whose mapping fails part way is required to leave nothing of
itself mapped, and to remove nothing it did not map, before its address goes
back. Then the
identity map is dropped and its absence checked, the sweep reports 318 mappings
on x86-64 and 822 on AArch64 with none writable-and-executable, and the reclaim
reports the frames it recovered. (Those were the counts when this landed.
AArch64's fell to 311 when the direct map stopped covering the device hole
below RAM — see *ARMv7-A* — and stage 4's per-processor stacks and records
have raised all of them since.)

**Deferred to stage 4, because it needs a second CPU to mean anything:**
per-CPU frame caches. Deferred again there — see stage 4.

---

