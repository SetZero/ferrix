# Memory and timing

The two determinism arguments the safety standards ask for, against the item in
[ITEM.md](ITEM.md): what the item allocates and what happens when it cannot,
and what it promises about time.

Both analyses conclude that the property is **not** achieved. That is the point
of writing them: findings F-23 and F-24 were vague statements that something
was missing, and this replaces them with measured statements of exactly what is
missing and what it would cost.

---

## 1. Dynamic memory — F-23

### 1.1 What the item allocates

Measured over the `core` and `item` rings, product code only: **225 allocation
sites across 40 files**. The heaviest are `devmgr.rs` and `object/job.rs` at 19
each, `user/space.rs` at 19, `user/vmo.rs` at 15, `device.rs` at 13.

The item carries four allocators beneath those sites:

| Allocator | Role |
|---|---|
| Buddy (`libs/frame`) | physical frames |
| Kernel heap (`libs/heap`) | `GlobalAlloc` behind `Box`, `Vec`, `Arc`, the maps |
| `vmap` arena | kernel virtual address space for device windows and stacks |
| Demand paging / CoW (`user/vmo.rs`) | a program's pages, on first touch |

### 1.2 What happens when allocation fails

This is the part that matters and it is worse than "unbounded".

`KernelAllocator::alloc` returns a null pointer on failure, which is what
`GlobalAlloc` requires. **There is no `#[alloc_error_handler]` in the tree.**
So a failing `Box::new` or `Vec::push` reaches Rust's default handler, and in a
`no_std` binary that aborts — a kernel panic.

Allocation failure in the certified item is therefore **fatal, not
recoverable**, at 225 sites. The heap's own API is fallible (`HeapError::
OutOfMemory`, and `libs/heap` returns it properly), but the `GlobalAlloc`
adapter above it discards that distinction for every ordinary Rust container.

### 1.3 Against the standards

* **EN 50716 Annex A** discourages dynamic memory at SIL 2 and above. The item
  uses it pervasively and on paths that a program can drive.
* **DO-178C** requires an argument that allocation cannot fail in a way that
  defeats a safety requirement — covering exhaustion, fragmentation and
  timing. None of the three has been analysed.
* **IEC 62304 §5.5** wants the failure behaviour of each unit stated. Here it
  is uniform and it is "panic", which is at least simple to state.

### 1.4 What would close it

**First, a constraint that rules out the obvious answer.** The usual fix is an
`#[alloc_error_handler]` that reports the failure properly instead of aborting
generically. **It is not available.** The attribute is an unstable library
feature (rust-lang issue #51540), verified against this tree's pinned 1.97.1,
and `kernel/` and `boot/` use no unstable features by policy — a policy that
`docs/certification/TOOLS.md` leans on, since it is part of why the toolchain
story is as clean as it is.

`Box::try_new` and `Arc::try_new` are unstable for the same reason. What *is*
stable is `Vec::try_reserve`, which covers growth but not the `Box` and `Arc`
allocations that dominate the 225 sites.

So the item cannot make allocation failure recoverable without either adopting
a nightly feature — which would cost more assurance than it buys — or
hand-rolling fallible construction at each site. That is worth knowing before
anybody plans this work, and it interacts with F-17: a Ferrocene toolchain
would not change it either.

In increasing order of cost, and none of it done:

1. **Bound the pre-user-mode item.** Everything the item allocates before the
   first program runs is a fixed, measurable set. Measuring it would let the
   item claim a bounded working set for its own bring-up even while the paths
   a program drives stay unbounded.
2. **Make the driveable paths fallible.** The sites a program can reach in a
   loop — `object/job.rs`, `object/channel.rs`, `user/space.rs`,
   `syscall/futex.rs` — are the ones that matter for T.EXHAUST (V-05 in
   [VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md)). On stable this
   means hand-rolled fallible construction, not a global handler. Plus a
   quota. It is a large change.
3. **Preallocate.** What a SIL 4 or DAL A item would do, and incompatible with
   an OS that also hosts a compiler.

**Verdict: F-23 stands.** It is now a measured finding rather than an
impression: 225 sites, four allocators, failure is fatal, no handler possible
on stable, no bound.

---

## 2. Worst-case execution time — F-24

### 2.1 What is claimed

Nothing, and deliberately. `docs/ARCHITECTURE.md` §5 states it plainly: the
`HardRt` domain *"does not promise a certified worst-case execution time for
the whole kernel — no OS that also hosts LLVM can offer that, and claiming it
would be the kind of statement that gets believed."*

That sentence is correct and this section exists to give it consequences rather
than leave it as a remark in a design document.

### 2.2 What the item does promise

Real, and narrower than a WCET:

* **Admission control.** EDF with a CBS test that refuses an unschedulable set
  — a bound on *acceptance*, not on execution.
* **Partitioned scheduling** in `HardRt`, no work stealing, which is what makes
  the admission test valid at all.
* **Bounded critical sections on the RT path**, by construction rather than by
  measurement.
* **Preemptible kernel**, so a long section delays rather than blocks.
* **Interrupts that cannot steal unaccounted time.**

### 2.3 What is missing, per standard

* **DO-178C DAL C** does not require WCET as such, but does require that
  timing-related requirements be verifiable. The item has no stated timing
  requirements to verify, which is a symptom of F-15.
* **EN 50716 SIL 2** expects timing behaviour to be analysed where a safety
  function depends on it. No safety function is defined (F-20), so there is
  nothing to hang the analysis on.
* **ASIL D / SIL 3-4**, the ratchet's destination, would require it outright,
  and the `core` ring is where it would have to be attempted — the Linux
  personality and the filesystem are the parts that make it impossible, and
  they are outside the item already.

### 2.4 An honest note on the boundary

The item boundary helps here more than anywhere else. A WCET argument over
93,646 lines including btrfs and a TCP stack is not a project anybody would
start. Over the 38,989-line `core` ring, with no dynamic allocation on the RT
path and no recursion anywhere (`scripts/check-complexity.py` establishes the
second), it is at least conceivable. It has not been started.

**Verdict: F-24 stands**, now with its scope stated: no WCET is claimed, the
narrower guarantees that are claimed are listed, and the ring where an attempt
would have to be made is named.

---

## 3. Why neither finding is closed

A document that says "the property does not hold" is not a closed finding, and
recording it as one would be the exact failure this directory exists to avoid.
Both entries in [FINDINGS.md](FINDINGS.md) are restated against this analysis
rather than struck out.
