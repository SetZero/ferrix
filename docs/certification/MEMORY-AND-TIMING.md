# Memory and timing

The two determinism arguments the safety standards ask for, against the item in
[ITEM.md](ITEM.md): what the item allocates and what happens when it cannot,
and what it promises about time.

Both analyses began by concluding that the property is **not** achieved. That
was the point of writing them: findings F-23 and F-24 were vague statements
that something was missing, and this replaced them with measured statements of
exactly what was missing and what it would cost. Since 2026-09-26 the first
half holds for the item: allocation failure is reported rather than fatal, and
a gate keeps it so (§1). What is still not claimed is a bound on memory, and
the second half, time, is unchanged.

---

## 1. Dynamic memory — F-23

### 1.1 What the item allocates

The item carries four allocators:

| Allocator | Role |
|---|---|
| Buddy (`libs/frame`) | physical frames |
| Kernel heap (`libs/heap`) | `GlobalAlloc` behind `Box`, `Vec`, `Arc`, the maps |
| `vmap` arena | kernel virtual address space for device windows and stacks |
| Demand paging / CoW (`user/vmo.rs`) | a program's pages, on first touch |

The frame allocator, the arena and demand paging always reported failure, as
`Option` or an error. The heap did too, and `GlobalAlloc` turned that into a
null pointer that every ordinary container sent to the allocation error
handler. So the question is the heap's callers. On 2026-09-25 they were
**225 sites across 40 files**, measured by hand, and every one was fatal.

They are now counted by `scripts/check-fallible-alloc.py`, the "fallible
allocation" step of `cargo xtask check`. It finds every call to an allocating
standard-library API in the item's product code: the constructors, `vec!` and
`format!`, and every method that can grow a collection; a `Type::default()`
of a type whose `Default` allocates, wherever in the kernel that `Default` is
written; and a derived `Clone` over an owned heap field. It also reads three
load files as it reads the item, the ones the item's own `process_create` and
`process_start` run (§1.3). It fails on a site that is not argued. On
2026-09-26 it reads:

| | Sites |
|---|---:|
| Unmarked in the item: an infallible allocation | **0** |
| Unmarked in the load files the item runs (`syscall/process.rs`) | 14, recorded as debt |
| `NOALLOC:` — cannot allocate: room reserved just before, or a type that only looks like a collection | 36 |
| `FALLIBLE:` — a first-party method named like a standard one, that reports failure | 13 |
| `FATAL-ALLOC:` — bring-up, fatal by design (§1.3) | 73 |

Everything else in the item allocates through `kernel/src/fallible.rs`, and
the gate does not flag it. Its ratchet baseline,
`scripts/fallible-alloc-baseline.json`, records `process.rs`'s 14 and nothing
else: a count may fall and not rise, and a new unmarked site fails the build.

### 1.2 How failure is reported

The obvious fix is unavailable. `#[alloc_error_handler]` is an unstable
library feature (rust-lang #51540), and so are `Box::try_new`, `Arc::try_new`
and `BTreeMap::try_insert`, verified against the pinned 1.97.1. `kernel/` and
`boot/` use no unstable features by policy, and `TOOLS.md` leans on that.
Only `Vec::try_reserve` is stable. So fallible construction is built from
stable parts, in two kinds:

* **What can be made fallible directly.** `libs/fallible` (`ferrix-fallible`)
  gives `Box`, `Vec`, `VecDeque` and `String` fallible constructors.
  `try_box` allocates `Layout::new::<T>()` through the global allocator and
  makes the box with `Box::from_raw`: `Box`'s documentation makes that
  conversion part of its contract. `try_boxed_slice` and `try_boxed_str`
  allocate exactly once. The collection helpers reserve with `try_reserve`
  before they grow. Room already there is not an allocation, so it is never
  failed, even under injection. The crate is host-tested against a recording
  and refusing global allocator, and run under Miri in CI.
* **What cannot.** `Arc::new`, `Arc::new_cyclic` and a map insert allocate
  inside `alloc`, with layouts it does not publish. They run in a *reserved
  section* (`kernel/src/mm/reserve.rs`). Entering the section masks this
  processor's interrupts, then fills its reserve to 16 objects of every heap
  size class, plus one block for an `Arc` too large for a class. It fails
  with `AllocError` if the heap cannot supply them, and that failure is the
  one the caller reports. Inside the section, an allocation the heap refuses
  is served from the reserve. So the operation either never starts, or it
  runs to the end on memory set aside for it. The depth argument: an `Arc` is
  one allocation of `arc_layout::<T>()`, and a B-tree insert is at most height
  + 2 nodes of at most `btree_node_bound`. The host tests measure both against
  the pinned standard library, and `fallible.rs` checks each map's node size
  against the largest class at compile time. A tree of height 14 has more
  than 10^11 entries. Soundness does not rest on the bound: a reserve block is
  handed out only for a request of its own class, so a wrong bound would let
  the allocation fail as it did before (a stop, FX-0008), and would corrupt
  nothing.

Each caller turns `AllocError` into the answer its interface has:
`NO_MEMORY` from a native call, `ENOMEM` from a Linux one (`mmap`, `mremap`,
`fork`, a page fault that must copy), `EAGAIN` from `madvise`, and a refused
step at bring-up. Where a change has several steps, each takes its room
before the first changes anything, or undoes what went before. Mapping an
object inserts into the table, attaches, and places the region, and takes all
three back when the last one fails. `fork` copies every table fallibly before
it marks the parent's pages copy-on-write. A child it then cannot finish is
let go, and the parent keeps the marks, which only make it copy what it
writes. `mremap` reserves room in the map for both removals and the region's
return before it takes anything out.

Three paths were rebuilt so that they need no memory at all:

* **The scheduler.** It used to allocate a tree node on every enqueue, and so
  allocated with the run queue locked and from interrupt context. Every task
  now lends the run queue and the sleepers' timeline a node of its own, made
  when the task is made. `libs/sched/tests/no_allocation.rs` counts the
  allocations of a queue and a timeline at work under a counting global
  allocator, and requires none.
* **Taking pages out of an object** (munmap, madvise, truncation, mremap,
  copy-on-write). The frames go into a list whose room is had before the
  first frame leaves the object, so a frame is never out of one list and not
  in the other. A decommit that cannot be refused, and so cannot fail, falls
  back to 32 pages at a time on the stack. With no memory to list the spaces
  that map the object, it asks them one at a time, in the order of a key that
  does not move, each with a shootdown of its own.
* **Closing an object.** A drop that would recurse is queued. When the queue
  cannot grow, the object is dropped in place, at most four deep, and only
  past that is it given up and counted.

Where a path cannot report failure and cannot avoid allocating, it keeps what
it held rather than allocate: an unmap that cannot note a range leaves its
pages with an object that no region shows them through. Each such path is
counted, and every counter stays zero while memory lasts: `RANGES_KEPT`,
`SPANS_KEPT`, `LOST_TO_UNMAPS`, `ZOMBIES_LOST`, `MISSING_SLOTS`, `ABANDONED`
and `UNRECORDED`.

**The negative control.** Every boot runs `object/alloc_check.rs` at stage 9
(FX-0902). With the heap made to refuse every allocation inside a section, an
`Arc`, a large `Arc` and 200 map inserts must complete on the reserve alone.
With the reserve refused its filling, they must fail before they start. Then
one process drives rounds of native calls that allocate, while every *n*th
allocation of its task fails, for six prime periods. Every call must succeed
or answer `NO_MEMORY`, and no port may lose a promised packet. A clean round
must then succeed, and no frame may have leaked. Last, a decommit of 80 pages
of a mapped object must give every page back with every allocation failing,
through both fallbacks above, and leave no translation. It reads the same on
all three architectures: *"486 native calls with 162 allocations failed under
them: 150 answered NO_MEMORY, the rest succeeded, nothing leaked; 35
allocations served from a reserve; 80 pages decommitted with none"*.

### 1.3 What stays fatal

**Bring-up**, by design. 73 sites run before the first program or while a
processor comes up, where there is nothing to return an error to. Each is
marked `FATAL-ALLOC:` and listed by the gate's `--report`:

| File | Sites | What |
|---|---:|---|
| `device.rs` | 17 | the device registry, from the firmware's tables |
| `devmgr.rs` | 12 | the device manager's start: driver list and arguments |
| `sched/mod.rs` | 11 | per-processor run queues and idle tasks |
| `iommu.rs` | 10 | translation units and their domains |
| `pci.rs` | 9 | bus enumeration |
| `smp.rs`, `arch/*/smp.rs` | 12 | per-processor data and secondary start-up |
| `init.rs`, `vmap.rs` | 2 | the first program's arguments; the arena |

A failure there stops the machine, and the panic handler names it. It knows
std's *"memory allocation of N bytes failed"* message, when the heap has
refused, and reports **FX-0007** before the boot completes.

**The load.** The uncertified load's allocations are still infallible, and it
shares the heap. One that fails after boot stops the machine with **FX-0008**.
That is an application condition, AoU-5, not a property of the item.

**The load the item runs.** That line is clean for the Linux personality,
which is load from its entry, and not clean for two native calls, which are
item calls that run load code. `process_create` makes a POSIX process through
the `Processes::load` hook (`syscall/launch.rs`), and `process_start` makes
its first thread. F-23 was scoped to the item's source, and on that scope it
holds. But a refused allocation in that load code stops the kernel on an item
path, which the scope does not excuse and the claim of "every site a program can
reach" did not allow for. Found on 2026-09-26: `Signals::default` built the
signal tables with `vec!`, under every process and thread. It is now fallible,
checked at stage 7 with a negative control, and the gate reads `signal.rs`,
`thread.rs` and `process.rs`. What those two calls still reach that cannot
report failure:

| Where | What |
|---|---|
| `syscall/process.rs` | the program name and arguments recorded (`record_exec`), the thread and task lists a start pushes onto -- among the 14 the gate records |
| `syscall/registry.rs` | the `Arc` the new process is registered in |
| `syscall/fd.rs` | `standard_streams`, which stops the kernel explicitly (`CONSOLE_DESCRIPTORS`) if the console's descriptors cannot be made |
| `syscall/load.rs`, `syscall/exec.rs` | the ELF loader's lists |

Each of these is the load's and covered by AoU-5, as it was before; the
difference is that the list is now written down and the three files the item
leans on most are gated. Converting the rest is the load-side work §1.5's
third item declines, done one path at a time as the item comes to depend on
it.

**What the gate cannot see.** It says so in its docstring:

* It matches methods by name, not type, which is what `NOALLOC:` is for.
* `.clone()` is not flagged. The item's 18 were audited by hand on 2026-09-26,
  and none allocates. Each is an `Arc`, a `Weak`, an `Option` of one, an
  `Object` (an enum of `Arc`s) or a `FileMapping` (an `Arc` and a flag).
* It does not see conversions that allocate, `write!` into a `String`, or
  allocation inside a callee. `libs/vma`, `libs/objects`, `libs/sched` and
  `libs/sync` were converted with the item. The other libraries the item calls
  allocate nothing on its paths. The load's callees are the table above.

### 1.4 Against the standards

* **EN 50716 Annex A** discourages dynamic memory at SIL 2 and above. The item
  still uses it, on paths a program can drive. What changed is the failure
  mode: exhaustion is now an error the caller sees. It was a stop of the
  machine.
* **DO-178C** requires an argument that allocation cannot fail in a way that
  defeats a safety requirement: exhaustion, fragmentation and timing.
  Exhaustion is now argued: it is reported, at every site, and checked by the
  build and on every boot. Fragmentation and the time an allocation takes are
  not analysed.
* **IEC 62304 §5.5** wants each unit's failure behaviour stated. It is now
  per interface, and it is an error return, except at bring-up.

### 1.5 What is not claimed

1. **A bound.** Nothing bounds what the item allocates. Measuring the
   pre-user-mode working set would give bring-up a bound. The paths a program
   drives would still be unbounded.
2. **A quota on the heap.** Nothing limits the heap one program may use. A
   job's limits are on its depth and its descendants, a channel's queue and a
   port's registrations are capped, and a handle table only by the width of
   its index. A program that drives an allocation in a loop now meets
   `ENOMEM` and the machine keeps running. It still denies the heap to
   everything else: V-05 in
   [VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md).
3. **The load.** Converting the load ring the same way would take AoU-5's
   second half away. It is outside the item and not attempted.
4. **Preallocation.** What a SIL 4 or DAL A item would do, and incompatible
   with an OS that also hosts a compiler.

**Verdict: F-23 is closed** for what it measured: allocation failure in the
item's own source is reported, not fatal, and the build says so. Two item
calls still run load code whose allocations are fatal (§1.3), which the
closure did not claim and the first version of this section implied. The bound it also names is
not claimed, and is exported to the integrator as AoU-5.

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

### 2.2a The one bound the item puts on its own waiting

A processor that interrupts the others and waits for each to answer -- a TLB
shootdown or a grace period, `kernel/src/smp.rs` `wait_for` and `take_turn` --
gives up and stops the machine (FX-0001 to FX-0003) if one never answers.
That bound is a liveness diagnosis, not a safety property: waiting longer never
frees memory early, so its one job is to report a processor that will never
answer without ever calling a live one stuck.

Until 2026-09-26 it was one second of wall-clock time, and that measured the
host rather than the guest. Under QEMU's coverage plugin, which runs every
translated block through one process-wide lock, `test-compositor` stopped on
FX-0001 with nothing stuck. The bound is now two conditions together: the
wall-clock floor it always had, and a count of the waiter's own polls, which
slows exactly as the machine does and stands still while the waiter is not
running. A processor made to stop answering is still found: in 1.8 s under
KVM, 5.1 s under `tcg` and 32 s under the plugin. The residual is a host that
starves one virtual processor while it runs the waiter; that ends the wait
early, which costs availability and never integrity.

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

## 3. Why F-24 is not closed, and F-23 is

A document that says "the property does not hold" is not a closed finding, and
recording it as one would be the exact failure this directory exists to avoid.
F-24 is restated against this analysis rather than struck out.

F-23 closed on 2026-09-26 because what it described stopped being true, and a
gate and a boot check say so: no allocation in the item's product code is
fatal except at bring-up. What it did not describe, a bound, stays unclaimed,
and says so in §1.5.
