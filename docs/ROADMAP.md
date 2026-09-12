# Ferrix — roadmap

`docs/ARCHITECTURE.md` says what is being built. This says in what order, and
how each stage knows it is finished.

Two rules govern the ordering:

1. **Every stage ends in something that runs.** Not "the VM subsystem
   compiles" — a QEMU boot that demonstrates the new capability and stays in CI
   forever after. A stage with no observable exit criterion is a stage nobody
   can tell is broken.
2. **Nothing is stubbed that a later stage has to unpick.** A fixed-size process
   table, an in-memory-only filesystem or a cooperative scheduler would each
   save a week now and cost a rewrite later, because the goal at the end needs
   the real version of all three.

Sizes are order-of-magnitude, in the sense of "a weekend / a week / a month /
longer". This is a long program of work: stages 1–8 are a conventional kernel
bring-up, 9–14 are the parts this design chose to do properly, and 15–17 are the
goal. Nobody should read the table as a schedule.

**Where it stands:** stages 0–7 are done and in the boot test on all three
architectures, and the boot marker reads `FERRIX-BOOT-OK stages 1-7`.
ARMv7-A joined after stage 3 — see *ARMv7-A* after stage 4. Stage 7's exit is
somebody else's static musl busybox running a script on every architecture,
checked by `cargo xtask test-shell` rather than the boot test because it needs
a binary the repository does not carry. Since the exit, a program can
`fork`, `execve` and `wait4`; its section lists what the Linux surface still
owes. Stages 8 and 9 have both begun where the continuous rule
says a stage should: their byte-level halves are in `libs/` — the VFS in
`libs/vfs`, the handle table in `libs/objects`. Stage 9's first kernel objects
— handle tables, channels carrying handles, VMOs — are in the boot test, and
so is most of stage 8: the root filesystem unpacked from an initramfs, every
process's descriptor table, the calls that take a path, `/dev` and `/proc`.
Stage 8's exit test, `cargo xtask test-vfs`, passes as the criterion is written
— `ls -R /proc`, `cat /proc/self/maps` and one shell script, on all three
architectures — and the stage waits only on fixes for five bugs a review found
in its VFS.
Stage 10 has begun the same way, with PCI configuration space in `libs/pci`,
and its first kernel code — PCI enumeration, device nodes, and a device driven
by DMA from the boot check — is in the boot test.
Each stage's section below says what exists. The marker will not move until a
stage meets its exit criterion.

---

## Stage 0 — Foundation ✅

Workspace, the quality gates ported from Starling, CI, and the two host-testable
libraries.

**Exit:** `cargo xtask check` runs fmt, clippy on every freestanding target,
the layering check, the assembly allow-list, the unsafe audit and the panic
audit, and all pass on an empty tree.

---

## Stage 1 — Boot, both architectures ✅

UEFI loader in Rust: read the kernel from the ESP, parse ELF, build page
tables, take the memory map, `ExitBootServices`, switch to our own tables, jump
to the kernel's Rust entry point. Kernel writes to a serial port and shuts the
machine down.

Zero assembly at boot on any architecture — firmware calls `efi_main` with a
stack and the MMU on. The only assembly is the page-table switch itself.

**Exit, met:** `cargo xtask test-boot --arch all` boots firmware → loader →
kernel on each architecture, and the kernel verifies four things before it
reports success:

* the hand-off structure's magic, version and layout constants agree with what
  the kernel was built against;
* the memory map is sorted, non-overlapping, has usable RAM, and describes the
  loader's *own* allocations — without which the frame allocator would hand out
  the frames holding its own page tables;
* the direct map really does alias physical memory, checked by reading the
  kernel's own first bytes through both mappings;
* the kernel can walk the loader's page tables and *extend* them, which on
  AArch64 is not optional — the console is an `MMIO` register that has to be
  mapped as device memory before a byte can go out.

Two bugs this stage found, both of which would have been very hard to diagnose
later: firmware reports a reserved aperture at 1 TiB that must not be counted as
RAM, and `FERRIX` as a directory name is byte-identical to `FERRIX` as a FAT
volume label.

---

## Stage 2 — Physical and virtual memory ✅

The arithmetic lives in `libs/` and is unit-tested on the host; the parts that
touch `CR3` or `TTBR1` do not.

**Done.** The buddy allocator (`libs/frame`) and the kernel heap
(`libs/heap`), both wired up and running on both architectures:

* `libs/frame` — buddy allocator over the UEFI memory map, orders 0 to 10.
  Free-list links live in a per-frame side array rather than in the free pages,
  which makes the whole allocator index arithmetic and therefore
  `#![forbid(unsafe_code)]`, host-testable and fuzzable. The side array is not a
  concession to testing: a refcount per frame is what copy-on-write will need.
* `libs/heap` — segregated free lists over a page supply, behind a `Backing`
  trait so the loads and stores that a free-list allocator needs are the
  implementor's problem rather than the allocator's. `Box`, `Vec` and
  `BTreeMap` now work in the kernel.
* The chicken-and-egg — the per-frame array has to exist before there is an
  allocator to make it — is resolved by carving it from the front of the
  largest usable region and handing that region over with the carved part
  excluded.

**Exit criterion met, and in the boot test on both architectures:** 4096 blocks
of assorted orders allocated and freed in an order that forces coalescing, with
the free-frame count required to return exactly to where it started; a `Box`, a
`Vec` grown through several reallocations, and a `BTreeMap` of two thousand
entries, all verified to hold what was put in them.

**Done — the virtual half, and the tidying the physical half was owed.**

* `kernel/src/vmap.rs` — an arena over `KERNEL_VMAP_BASE` that hands out
  virtual ranges with an unmapped guard page either side, and guard-paged
  kernel stacks on top of it. Free ranges are coalesced with their neighbours,
  so a stack allocated and freed a thousand times does not fragment the arena
  into a thousand holes.
* The loader's identity map is dropped, in the one order that works: the W^X
  sweep cannot pass while it is live — the loader mapped itself writable *and*
  executable, and correctly so, since it was executing out of pages it was
  still relocating — so the map goes first and the sweep runs after. The kernel
  then proves the map is gone by translating the physical address it used to
  run at and requiring nothing back.
* The loader's own memory and the ACPI-reclaim regions are handed to the buddy
  allocator once nothing points into them, which on a 512 MiB QEMU machine is
  3 MiB on x86-64 and 2 MiB on AArch64 — small in absolute terms and the
  entire difference between a kernel that can reclaim boot memory and one that
  cannot.
* The W^X sweep walks the live tables through `Mapper::for_each_leaf` and
  asserts no leaf is both writable and executable. It reports what it swept as
  well as what it found, because a sweep that walks nothing also finds nothing.
* `libs/heap` returns empty slab pages to the buddy. The free-object count
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
beyond each, and freeing it is required to return every frame it held. Then the
identity map is dropped and its absence checked, the sweep reports 318 mappings
on x86-64 and 822 on AArch64 with none writable-and-executable, and the reclaim
reports the frames it recovered. (Those were the counts when this landed.
AArch64's fell to 311 when the direct map stopped covering the device hole
below RAM — see *ARMv7-A* — and stage 4's per-processor stacks and records
have raised all of them since.)

**Deferred to stage 4, because it needs a second CPU to mean anything:**
per-CPU frame caches. Deferred again there — see stage 4.

---

## Stage 3 — Traps, interrupts, time ✅

x86-64: GDT, TSS, IST stacks, IDT, exception handlers, LAPIC, IOAPIC, HPET/TSC
deadline. AArch64: `VBAR_EL1` vector table, synchronous/IRQ/FIQ/SError
handlers, GICv2 and GICv3, the architected generic timer.

Both behind one facade: `irq::register`, `timer::after`, `trap::Frame`.

**Done — the trap half.** Vectors are installed on both architectures as the
first thing after the stage 1 self-check, before anything can deliberately
fault. That ordering is not fastidiousness: until it runs the CPU is still
pointing at firmware's handlers, and those stopped existing at
`exit_boot_services`. A fault in that window is a jump into reclaimed memory,
which on x86-64 is a triple fault and a silent reset with nothing on the wire to
say why.

* x86-64 — GDT, TSS and a 256-entry IDT, every gate naming the kernel code
  selector the GDT installed a moment earlier. The double-fault gate runs on an
  IST stack of its own, because it is the one fault whose cause may be that the
  stack is unusable.
* AArch64 — the `VBAR_EL1` vector table and its handlers.
* One dispatch path above both (`kernel/src/trap.rs`), reached only through the
  architecture facade — `TrapFrame`, `classify`, `report_trap` — so generic code
  still never names a CPU.

**Exit criterion, half met, and in the boot test on both architectures.** A
breakpoint returns to the instruction after it, twice, with a canary in a
register the trap frame saves and restores required to come back intact — which
is what proves the entry path *restores* rather than merely arrives. Then a page
fault is resolved rather than reported: three pages in an unmapped window are
touched out of order, the handler maps the faulting address, the instruction
retries, and the pages are required to read back what was written, to be zeroed,
and to have cost exactly the frames they should — one per page plus one per
table level for the first fault into a fresh region, and exactly one for a fault
into a region already tabled. That last bound is what makes the accounting a
measurement rather than a shrug.

Resolving a fault by mapping a page and retrying *is* demand paging, arriving
here rather than at stage 6 because it is the only honest way to show the fault
path recovers. Stage 6 inherits it instead of writing it.

**Done — the interrupt and time halves.** Something now arrives that the
kernel did not ask for at the moment it arrives, which is the difference
between a program and an operating system.

* x86-64 — the local APIC is mapped from the MADT's address, enabled, and its
  task priority dropped to accept everything; the local APIC timer is
  calibrated against the HPET, whose period firmware states exactly in
  femtoseconds so that nothing has to be measured. A machine with no HPET
  table falls back to the TSC calibrated against the PIT, which is the only
  clock a PC is guaranteed to have. Every I/O APIC firmware described is
  mapped and every input masked — not configured, because nothing is wired to
  a device line until stage 10, but quiesced, because a line firmware left
  enabled would arrive at a vector chosen by whoever wrote the firmware.
* AArch64 — GICv2's distributor and CPU interface, from the MADT; the
  architected *virtual* timer, whose interrupt number comes from the GTDT.
  Virtual rather than physical because a kernel at EL1 is by definition below
  any hypervisor present, and on bare metal the two are the same counter.
* The other two thirds of the facade: `irq::register` and `timer::after`.
  One-shot is the primitive, because AArch64's timer has no periodic mode at
  all — it compares against an absolute instant — and stage 5's tickless
  scheduler wants one-shot anyway. Periodic is a re-arm inside the handler.

**Exit criterion met, and in the boot test on all three architectures.** A
thousand timer interrupts are counted, and the time they took is measured
with the *counter* rather than by multiplying the tick count by the rate they
were programmed at — which would be arithmetic that cannot fail rather than a
measurement. Against a requested 1000 Hz, all three now report 998 to 999. It
is a measurement, so it moves.

It used to report 969 to 992, and the shortfall was written up here as one
interrupt entry and exit per period under an emulator. That was the wrong
diagnosis of a real defect. The handler re-armed the timer for one interval
*from the moment it ran*, so every period was one interval plus however long
that interrupt had taken to arrive — an error that never averaged out, because
it was added afresh each time. On a fast host it hid inside the tolerance. On
QEMU's Windows build, whose timer resolution is about a millisecond, it
doubled the period outright and the self-check failed: 500 Hz against 1000
requested, which is what a kernel that measures its own latency and calls it a
frequency looks like.

A periodic timer is now a *schedule*: tick `n` is due at `start + n * interval`
and the instant tick `n - 1` happened to arrive has no say in it, so a late
tick is absorbed rather than propagated. Unbounded catch-up is its own hazard —
a kernel held off for a second owes a thousand interrupts — so past sixteen
intervals behind, the debt is written off and the schedule restarts. This is
also the shape stage 5 wants: a deadline, not a delay.

Before that, a one-shot is armed and the count required not to move for ten
further intervals. That check is there for one specific bug: AArch64's timer
interrupt is level triggered, so a handler that acknowledges the controller
without disarming the timer is re-entered immediately and forever. It does
not fail by producing a wrong number — it fails by never returning, with
nothing in the log after the line before it.

The boot marker reads `FERRIX-BOOT-OK stages 1-3`, and it is now the whole
truth.

**Deferred, and none of it is reachable from a machine Ferrix boots on
today.** The first two are hardware variants rather than gaps in the stage:
the facade above them does not change, and neither is on any later stage's
path. The third is stage 4's by definition.

* GICv3 and its redistributors. `gic::init` reads the version out of the
  MADT and refuses anything that is not a GICv2, rather than writing GICv2
  layouts into GICv3 registers and producing a machine that takes no
  interrupts for reasons nothing explains. QEMU's `virt` gives a GICv2 unless
  asked otherwise, so this needs a second boot-test configuration as much as
  it needs code.
* TSC-deadline mode, which replaces the local APIC timer's countdown with a
  comparator against the TSC and is how a tickless kernel avoids
  reprogramming a divider on every reschedule. The calibration it needs is
  already here.
* Per-CPU anything. The controller is brought up for the boot CPU because
  there is only one; stage 4 is where a redistributor per core and a local
  APIC per core start to mean something. *Done in stage 4*: every processor
  brings up its own local APIC or GIC CPU interface.

## Stage 4 — SMP ✅

x86-64 AP bring-up via INIT–SIPI–SIPI and a real-mode trampoline (the one place
in the project with 16-bit assembly). AArch64 via PSCI `CPU_ON`. Per-CPU data
areas, IPIs, TLB shootdown, and the RCU-like grace period the scheduler mode
switch later depends on.

**Done.**

**One correction, found later and worth recording.** On x86-64 `flush_tlb`
reloaded `CR3`, which does not invalidate global entries — and kernel text,
the direct map and every device window are mapped global, so the flush spared
very nearly everything it was asked to drop. The shootdown machinery above was
correct; what it invoked at the end was not. It now clears and restores
`CR4.PGE`, which is the architecturally defined way to invalidate global
entries.

This passed every boot test for as long as every boot test ran under `tcg`,
because an emulated `MMU` has no `TLB` to hold a stale entry. Under a hardware
accelerator it failed immediately, and in three different places: the stage 4
shootdown check caught it directly, and stage 3 twice read a fresh device
window through a dead translation left by a stage 2 check that had used the
same `vmap` address — reporting an `HPET` period no `HPET` can have, and a
local `APIC` frequency of 4.29 GHz. `cargo xtask test-boot --accel auto` is
how that class of bug is reachable at all.

* **Counting first.** Processors come from the MADT — local APIC and x2APIC
  entries on x86-64, GIC CPU interface entries on AArch64 — checked for
  duplicates and required to include the processor reading them. That last
  rule caught the stage's first bug before it could happen: `MPIDR_EL1` has
  bits the MADT does not store, so an unmasked read never matches its own
  processor's entry.
* **Every "one CPU" is a lock now.** The frame allocator, the heap, the IRQ
  table, the console — and one the stage 2 code did not know it needed, the
  kernel page tables, where two processors mapping at once would each install
  the same intermediate table. The lock order is written down in `mm.rs`. Once
  something is panicking the console waits a bounded time for its lock, so a
  fault while printing still produces its `FERRIX-PANIC` line.
* **A record per processor**, reached through `GS` on x86-64 and `TPIDR_EL1`
  on AArch64, holding its own address first so `gs:0` is a pointer. Every
  processor checks its record against what its own hardware says it is.
* **AArch64 secondaries** through PSCI `CPU_ON`, its conduit read from the
  FADT. A core starts with its MMU off at a physical address, so it enters
  through an identity map of the entry sequence alone, in a tree of its own —
  and loads every parameter before the MMU goes on, because afterwards the
  physical address they came from is not mapped.
* **x86-64 secondaries** through INIT–SIPI–SIPI and a trampoline that goes from
  real mode to long mode in one step, on a root table below 1 MiB that shares
  the kernel's upper half. `Frames::allocate_below` finds the two low frames,
  host-tested in `libs/frame`. Each processor gets its own GDT, TSS and
  guard-paged double-fault stack: a TSS cannot be shared, because loading one
  marks its descriptor busy. The trampoline's one bug was a triple fault — its
  GDT descriptors lacked the accessed bit, so loading a selector made the
  processor write to the read-only page the GDT lives in, with no IDT yet to
  take the fault.
* **Work on every processor.** `run_everywhere` hands a function to every
  processor; secondaries sleep between pieces in `sti; hlt` or `wfi`, whose
  wake-up cannot be lost, and a broadcast IPI wakes them.
* **TLB shootdown** on x86-64. AArch64 needs none: `tlbi vmalle1is` reaches
  every core in hardware. `unmap_kernel` now unmaps, invalidates everywhere,
  and only then frees, holding what it released inline rather than in a `Vec`
  so that the stage 2 checks' exact frame accounting still holds.
* **Grace periods.** A read-side section masks interrupts; `synchronize`
  interrupts every other processor and waits for each to take it, which none
  can inside a section. An interrupt handler is a section already, which is
  what will make unregistering one safe.

**Exit criterion met, and in the boot test on both architectures.** Every
vCPU QEMU is given comes online — four — and they increment one counter under
one ticket lock, 25,000 times each, released from a start line together. The
total is required to be exactly 100,000, and the four processors' shares are
required to overlap in time, measured in the counter's own values, because
processors that took turns would get the total right too. The same count kept
beside it with a plain load and store loses updates — 349 on x86-64 and 2,114
on AArch64 in the runs recorded when this landed — which is the measurement
that says the four really were running at once. Like any measurement, it
moves.

Before that, each piece has a check of its own: a hundred rounds of work on
every processor, each woken by an IPI; a page moved to another frame twenty
times, with every processor required to read the new frame each time; and a
hundred grace periods against readers holding what they loaded, with the
objects they retired poisoned rather than freed, so a reader that should have
been waited for reads the poison. Two of those were tested by breaking what
they test: with the shootdown disabled, x86-64 fails on a stale read; with
the grace period skipped, both architectures fail on a poisoned one.

The boot marker reads `FERRIX-BOOT-OK stages 1-5`.

**Deferred, none of it on stage 5's path:**

* Per-CPU frame and heap caches, deferred here from stage 2. Each allocator is
  one lock, which is correct; the caches are a performance change, and the
  per-CPU area they need now exists. They wait for a workload that can
  measure them.
* x2APIC mode. APIC IDs above 255 are refused with a message; QEMU's are 0–3.
* GICv3, still, as at stage 3. IPIs go through GICv2's `GICD_SGIR`.
* The PSCI parking protocol, for firmware without PSCI. It is refused, not
  guessed at.
* Taking a processor offline. Nothing does, and the per-CPU records and
  stacks are allocated once for the life of the machine.

---

## ARMv7-A — a third architecture ✅

32-bit ARMv7-A — the Cortex-A7 of the STM32MP157 — on QEMU's `virt` machine,
booted by U-Boot. `docs/arm32.md` is the plan and argues each decision; this
records what the port changed and what it proved. None of stages 1–3 was
rewritten to admit it.

* **The same loader, converted.** rustc has no 32-bit UEFI target, so the
  loader is built as an ELF static PIE and `xtask/src/pe.rs` rewrites it as a
  PE32 with base relocations, tested against an independent reader of its own
  output. U-Boot runs it as `BOOTARM.EFI`. No bootstrap assembly was added.
* **A 32-bit address space, argued rather than shrunk.** A 2/2 split with a
  1.25 GiB direct map; LPAE tables, which are AArch64's descriptors on a
  three-level walk, so `libs/paging` gained a geometry rather than a second
  mapper. The direct map now begins at the lowest RAM address on every
  architecture — which on AArch64 stopped it mapping the device hole below RAM
  as cacheable memory, and took that sweep from 822 leaves to 311.
* **One hand-off layout on every width.** `BootInfo` version 2 carries `u64`
  addresses where it had pointers, the direct map's physical origin, and a
  device tree copied into memory of its own kind, so that it outlives the
  reclaim that returns the firmware's copy.
* **Device tree only.** `libs/fdt` got its first consumer nine stages early:
  the console by `stdout-path`, the GICv2, the virtual timer's interrupt, and
  the PSCI conduit — which on QEMU is `hvc`, not the `smc` the plan first
  guessed; dumping the generated tree settled it before a line depended on it.
* **Traps without mode stacks.** Eight vector entries, and one path through
  `srsdb` and `rfeia` on the SVC stack, so no other processor mode is ever
  given a stack to get wrong.

**Exit criterion met, and in the boot test.** `cargo xtask test-boot --arch
armv7a` runs the same self-checks as the other two and reaches the same
marker: two breakpoints, four page faults with the exact frame bound, 1001
ticks measured against the counter at 920 Hz for a requested 1000, the
identity map dropped and its absence checked, 317 mappings swept with none
writable-and-executable, and 3 MiB reclaimed.

**And stage 4, which landed on `main` while the port was under way.** The
secondaries are found in the device tree's `/cpus` rather than the MADT and
started through PSCI `CPU_ON`, entering as AArch64's do: through an identity
map of their entry sequence, with every parameter loaded in one `ldm` before
the MMU goes on, and refused if the entry would sit above the 2 GiB that
`TTBR0` covers. TLB invalidation is broadcast by the hardware, as on AArch64,
so there is no shootdown IPI. The run recorded when it landed: four of four
online, 100 rounds of work woken by 300 IPIs, 100 grace periods against 34,900
reads with none stale, and the counter at exactly 100,000 with its shares
overlapping and 39,725 updates lost by the unlocked count beside it. The
sweep then found 341 mappings, and the reclaim 4 MiB.

**Deferred, with the reasons in `docs/arm32.md`:** RAM above 2 GiB physical,
which the board has and QEMU cannot place; RAM beyond the direct map; the
board's own UART; Thumb-2; VFP.

---

## Stage 5 — Tasks and the scheduler ✅

`Task`, kernel stacks, context switch, per-CPU runqueues, the class stack, and
the EEVDF fair class. Scheduling domains exist from the start with one mode
(`Throughput`) implemented; the other two are stage 14, but the domain
abstraction is not retrofitted.

**Done.**

* **The deciding is `libs/sched`**, host-tested, because a scheduler that is
  wrong is wrong in a way nothing on the machine can print. The EEVDF tree,
  the weights, the lag arithmetic and the domain partition are all reachable
  from `cargo test`; what is in `kernel/` is the part that needs a machine.
* **Deciding and switching are one operation.** The run queue's lock is taken
  before the decision and released *after* the switch, by whichever context
  ends up running. That is not an optimisation: it is what stops another
  processor picking up the outgoing task in the window between it going back
  on the queue and its registers being saved. `SpinLock::lock_manually` exists
  to say so where the type system cannot.
* **Preemption happens only on the way out of an interrupt.** The timer sets a
  flag and returns; the decision is made once the controller has been told the
  interrupt is done. Switching inside the handler would leave an interrupt in
  service for as long as the next task ran, and a controller still servicing
  one delivers nothing further.
* **The context switch** is the fourth and last entry on the assembly
  allow-list's "both architectures" half: a function that returns onto a
  different stack from the one it was called on, which Rust cannot express.

**Exit criterion met, and in the boot test on all three architectures.** A
thousand kernel threads, all spawned on one processor so that the only way the
others get any is by taking them, run bounded work to completion and give every
stack back — about 28,000 context switches and 1,000 steals in the runs
recorded when this landed. Then twelve spinners, three on each processor and
one of each three at a different weight, run inside a measured window, and each
task's service is required to stay within EEVDF's own bound of its weighted
share. The bound is not a constant: it is a slice plus the worst overrun the
scheduler actually served, and both numbers are printed, because a bound that
moves is only honest beside what it bounded.

**Three bugs it found, each invisible to the check before it:**

* **An idle processor is never told.** Work appearing on another processor's
  queue after an idle one has halted is invisible to it forever, so a thousand
  tasks ran on one processor with three asleep. Placing a task now wakes the
  idle, and `should_preempt` is not the only reason to: it compares an arrival
  against the fair queue, which the idle task is deliberately not in, so it
  answers false however urgent the arrival.
* **`vmap::free` freed the address before it unmapped the pages**, so another
  processor could be handed an address that was still mapped and have a
  perfectly ordinary allocation refused. The unmapping cannot happen under the
  arena lock — it waits for processors that cannot answer while spinning for
  that lock — so the two steps are separate now, with the address reserved
  across the gap.
* **Every private interrupt the boot core enables is off on every other core**,
  the timer included, because those enable bits are banked. A core whose timer
  is masked in the controller runs, takes inter-processor interrupts, and is
  never preempted: whatever it picks first, it runs forever. The GICv2 driver
  records what was enabled and gives each core the same set, which fixes the
  class rather than the instance.

Two of the three needed more than one processor and work that outlives a
timeslice, which is to say they needed this stage's own test to exist.

**Deferred:** a task that is not a kernel thread, which is stage 6; and the
other two domain modes, which are stage 14.

### Closing the distance to Linux

Stage 5 left the scheduler fair on each processor and naive across them, which
is enough to pass its own exit criterion and not enough to be called a
scheduler. Five things were added afterwards, all of them arithmetic in
`libs/sched` with the kernel supplying the numbers, and each with a check in
the boot test that fails without it.

* **Load tracking.** A decaying average with a 33-millisecond half-life, in the
  shape of Linux's PELT. It measures *weighted demand* rather than occupancy,
  which is the distinction the balancer lives on: a processor is either running
  something or not, so "busy" saturates at one task and says nothing after
  that.
* **Placement.** A new task goes where it should rather than where it was
  created — the processor it prefers if that one is idle, any idle processor
  otherwise, the least loaded if none is.
* **Affinity.** `pinned: bool` became a `CpuSet` per task, which is the field
  `sched_setaffinity` will want in stage 7 and is cheaper now than retrofitted.
  Stealing and balancing both honour it.
* **Periodic balancing**, for the case stealing structurally cannot reach:
  every processor busy, one of them much busier.
* **Slice scaling.** The slice is a share of a target latency rather than a
  constant, floored at a minimum granularity. A fixed slice is also a latency
  bound per task, and at a thousand runnable tasks that bound was seconds.

**Four bugs, three of which only a running machine could show.**

* The load average never reached its own fixed point. Two truncating integer
  divisions per step settled a permanently busy processor at 978 of 1024, so
  every processor was compared against a ceiling none could reach.
* Pulling work is useless on a tickless kernel. `arm_timer` deliberately leaves
  a processor alone when nothing is waiting, so an *under*-loaded processor is
  never interrupted and never reaches the balancer to pull anything towards
  itself. The overloaded one is interrupted constantly, precisely because it
  has tasks to switch between — so it is the only one awake to notice, and it
  has to push. Six thousand balance attempts moved nothing before this.
* **A task queued behind a running one did not re-arm its processor's timer.**
  A remote enqueue where `should_preempt` says no left a processor running one
  task forever with others waiting. A hang, not a fairness problem, and the
  cause of an intermittent failure that had been putting the `AArch64` boot
  test down about one run in three.
* Balancing thrashed: the load average is deliberately slow, so moving one task
  does not change it for tens of milliseconds and the balancer kept moving
  more. Eight movable tasks were observed moving 1,262 times. The queue length,
  which updates instantly, is now a brake on the decision the average makes.

Measured on the same boot test: worst-case fairness lag fell from 2,882 to
about 1,000 microseconds, and balance thrash from 1,262 moves to 7.

**Withdrawn, and worth saying why.** Choosing a processor for a task at the
moment it *wakes* — which is what Linux does, and better than placing only at
creation — was implemented and reverted, along with detaching a woken task from
its processor's sleeper set. Each made an already-flaky machine reliably worse;
the second wedged every run. The reason both are harder than they look is the
same: a blocked task is not an unattached one, and "blocked" covers several
states this code does not distinguish. A task can be on a wait queue, in a
sleeper set, part-way into `block` and in neither yet, or in both. A waker that
reasons about one of them moves a task something else still believes it owns.
Naming those states and giving them an order is a change of its own, and the
balancer covers the same ground less promptly in the meantime.

**Still missing against Linux**, none of it on stage 6's path: group scheduling
and bandwidth control, which are stage 13; the real-time classes, which are
stage 14; and NUMA and capacity awareness, which need a topology this kernel
does not yet parse.

---

## Stage 6 — User mode ✅

`AddressSpace`, VMOs, the VMA interval tree, demand paging, copy-on-write, the
ELF loader, and the ring-3/EL0 transition. The first user process is a
hand-written static binary that makes one syscall.

**Exit:** a boot test that runs a user binary which writes to fd 1 and exits,
with a page fault serviced along the way.

**Exit criterion met, and in the boot test on all three architectures.** Each
architecture boots a program at user privilege — through the ELF loader, a
startup stack built by `libs/ustack`, and its own way down: `sysretq` to ring 3
on x86-64, `eret` to EL0 on AArch64, `rfeia` to USR on ARMv7-A. The program is a
few dozen bytes of that architecture's machine code calling `write(1, …)` and
`exit_group(42)`, and the boot log carries both:

    hello from EL0
    usermode a program ran in user mode and exited with 42

The page fault is asserted rather than assumed: the check counts resolved faults
either side of the program and fails if none was serviced. Each architecture
services exactly one today — the text page, faulted back in after the loader
narrows its permissions — which is incidental rather than designed, and the
check says so, because a later change that narrows permissions in place would
remove that fault and the criterion's evidence with it. A program that stores
well below its stack pointer, into a page nothing has touched, is the sturdier
version.

x86-64 is also booted under KVM (`--accel kvm`), where the host processor checks
what `tcg` lets through, and that is how a missing RPL in `STAR` was found.
`SYSRET` forces RPL 3 into the CS it computes but loads SS as written, so a
program ran on an RPL-0 SS until its first exception, and the `iretq` back to
ring 3 refused it with `#GP(0x20)`. The default boot test cannot see that class,
so anything that touches ring 3 wants a KVM boot before it is called done.

**Done:**

* **The objects.** `Vmo` is a sparse page list, committed on first touch, so a
  reservation costs nothing until it is written — which is what makes a large
  `mmap` cheap and is measured rather than asserted: 2048 pages reserved, seven
  committed. `AddressSpace` is the `libs/vma` interval tree used for the first
  time as what it was written for, with a lock of its own rather than a global
  one, so two processes faulting at once contend for nothing. Anonymous memory
  carries an identity, because `MAP_SHARED|MAP_ANONYMOUS`, futexes resolving to
  one wait queue and `/proc/self/maps` all need to name the object behind a
  mapping.
* **Demand paging**, generalised from stage 3's fixed window: the fault handler
  finds the region, asks its object for the page, and installs it with that
  region's permissions.
* **The processor walks it.** `arch::install_user_root` puts a root the kernel
  built into `CR3` or `TTBR0`. The three architectures disagree about what a
  switch even is — one `CR3` write, whose own side effect drops the non-global
  entries, against writing `TTBR0`, clearing `EPD0` to re-enable a translation
  regime, and invalidating by `ASID`. No `ASID`s or `PCID`s are allocated: a
  full invalidation of user entries on switch is the correct baseline, and
  eliding it later makes the switch faster rather than unpicking anything.
* **fork and copy-on-write**, copying no memory at all. Each private object is
  cloned page list and all, with a reference taken on every committed frame, so
  a write on either side copies that page into its own object. A page with one
  holder left is let through uncopied — the refcount is what makes that
  decision, which is also why a second write to an already-copied page
  terminates.
* **Tasks carry address spaces.** `Task` holds an `Option<Arc<AddressSpace>>`
  and `sched::choose_next` swaps roots under the run queue lock, comparing by
  pointer so two threads of one process cost nothing. A kernel thread gets the
  user half switched off rather than left installed.
* **Into user mode.** `arch::enter_user` zeroes every general register and
  drops privilege from the program's own task, and does not return: a program
  leaves user mode through a trap or a system call, and for good through
  `exit_group`, which ends its task. The entry stack is the part that bites, and
  it is the task's own kernel stack on every architecture — `gs:8` and `RSP0` set
  on each switch on x86-64, and on the Arm pair simply the stack pointer at the
  return, since at EL1 `sp` *is* `SP_EL1` and ARMv7-A stores every exception from
  USR on the SVC stack. The first version parked registers where the first
  fault then pushed its frame, which x86-64 shipped and fixed. On the two Arm
  architectures `svc` arrives through the trap vector, so `Trap::SystemCall` asks
  the architecture through `arch::system_call`; x86-64's `SYSCALL` has an entry
  of its own. Interrupts are open in user mode — see stage 7, *Programs are
  scheduled tasks*.
* **The permissions the map states are the ones enforced.** A read of a region
  that permits nothing is refused rather than served a fresh zero page — the
  guard-page case, which nothing notices when it is wrong, because the mapping
  is more permissive than the map rather than less.

**Decisions worth not reversing silently.** No `ASID`s or `PCID`s: a full
invalidation of user entries on switch is the correct baseline, and eliding it
later makes the switch faster rather than unpicking anything. On the Arm pair it
is still an invalidation *by* `ASID` — every space is `ASID` zero — because that
is the only form that drops user entries and keeps the kernel's global ones. The
global broadcast flush is right for a mapping change, which has to reach other
processors, and wrong in the switch path. Anonymous memory names a VMO, so
shared anonymous mappings, futex keys and `/proc/self/maps` have an object to
name. One lock per address space, never a global one.

**Left for later, none of it on stage 6's path:**

* Scoping the user TLB shootdown: one page rather than all of them, told only to
  the processors running that space.
* `AddressSpace::protect` takes translations down without invalidating them, so
  after `mprotect` a stale entry can stay more permissive than the map. glibc's
  RELRO is the first real program to walk into it.
* `read_console_byte` returns `None` on both Arm architectures, whose UART
  drivers are write-only, so a program reading fd 0 there waits forever — and no
  check reads the console.

**What the boot test cannot see, written down rather than trusted.** A
copy-on-write fault replaces a live read-only translation with a writable one,
and leaving the stale entry makes the retrying instruction fault forever — a
hang with no message. Deleting that invalidation fails no boot test on any architecture — not under
`tcg`, and not under KVM on x86-64 either, where the host's own TLB is in play.
The check resolves the fault with the space installed and writes through the
faulting address, and it still passes; but it writes from the kernel, and a
stale read-only entry is only certain to fault a write made from user mode. A
program that writes to a copy-on-write page after its fault is the test that
would see this, and nothing runs one yet. The same holds for `protect` above,
and stage 4 has the converse writeup.
Relatedly: a check that reads or writes user memory through the direct map tests
no permission bit at all. To test the tables, ask `translate_in`, or install the
space and use the address.

---

## Stage 7 — The Linux syscall ABI ✅

The syscall entry path on every architecture, the dispatch table, and the core
of the surface: memory (`mmap`, `mprotect`, `brk`), files, process
(`clone`, `execve`, `wait4`, `exit_group`), threads and `futex`, signals with
`sigaltstack` and `rt_sigreturn`, time, and identity.

**Exit:** a static musl `busybox sh` starts, runs a script, and exits — the
first time somebody else's binary runs on Ferrix.

**Exit criterion met on all three architectures, with the script given to
`sh -c`.** `cargo xtask test-shell` builds the kernel with a static busybox
and a script, boots it, and requires the script's lines in order and its exit
status. Run against Alpine's `busybox-static` 1.37.0 — built by people who have
never heard of Ferrix — for x86-64, AArch64 and ARMv7-A:

    cargo xtask test-shell --arch all --init PATH/{arch}/busybox

      init     857 KiB program built in, starting `sh -c` with a built-in script
    script: started
    script: the sum is 15
    script: hello, ferrix
    script: test agrees
    script: case matched
    script: 3 positional parameters
      init     the shell exited with 7

The status is 7 rather than 0 so that a shell which died and reported success
cannot pass, and the lines are looked for after the boot marker so that the
kernel's own output cannot satisfy them.

Three decisions sit inside that, and each is a reading someone could dispute:

* **"Runs a script" is read as `-c`.** The script travels in `argv`, which
  needs no filesystem. A script *file* needs `openat`, and that is stage 8's —
  making this stage's exit wait on it would have put the first foreign binary
  behind a filesystem it does not otherwise need.
* **The script is builtins only.** Variables, arithmetic, a loop, a function,
  `test`, `case`, positional parameters and the exit status. Nothing forks,
  because `clone`, `execve` and `wait4` did not exist at the exit, and an
  external command would have measured their absence rather than the ABI.
* **The binary is not in the repository.** Which static busybox to trust is a
  decision for whoever runs the test, and a kernel that embedded a host's
  binary silently would stop building byte for byte the same. `--init` names
  it; without a script the same kernel starts `sh -i` and hands a person a
  prompt, which is how glibc busybox was first run by hand.

`test-shell` is not part of `cargo xtask check` or the boot test, because it
needs a binary the repository does not carry. The boot test covers every
handler below directly.

**Done:**

* **The numbers.** `libs/linux-abi` carries all three tables. ARMv7-A's EABI
  table is not the 64-bit calls renumbered: a 32-bit register cannot carry a
  file offset, a file size or a post-2038 `time_t`, so sixteen calls exist twice
  and the wide form is a *different call with a different signature* — `mmap2`
  counts its offset in pages, `_llseek` returns through a pointer. Two more,
  `set_tls` and `cacheflush`, live at `__ARM_NR_BASE` and have no 64-bit
  counterpart. The boot test asserts which table this build uses by its content:
  each architecture reports its own number for `getpid`, 39, 172 and 20, which
  is the one fact a host test cannot establish.
* **The startup stack.** `libs/ustack` writes `argc`, `argv`, `envp` and the
  auxiliary vector at both pointer widths, and reads them back; its fuzz target
  found that a string containing a NUL built a well-formed image which read back
  as a different, shorter string.
* **Dispatch.** `kernel/src/syscall/`: `SyscallArgs`, `Outcome` and `dispatch`,
  reached through `arch::decode_syscall`, and total — every number in `0..=600`
  answered with poisoned argument registers in the boot test.
* **The copy layer and the loader.** `copy_from_user` and `copy_to_user` resolve
  through the `AddressSpace` and its fault path rather than dereferencing, and
  refuse a kernel address before any length arithmetic. The ELF loader maps
  `PT_LOAD` segments with their own permissions and copies them in.
* **The calls a static binary makes.** Memory: `mmap`, `mmap2`, `munmap`,
  `mprotect`, `brk`. Threads: `set_tid_address`. Files: `read` on the console
  and `write`/`writev` to it, with iovecs read as native words. Time:
  `clock_gettime`, `clock_gettime64`, `gettimeofday`, `getrandom`. Identity:
  the credential calls, `getpid`, `gettid`, `getppid`, `uname`, `sched_yield`.
  Signals: `rt_sigaction`, `rt_sigprocmask`, `sigaltstack`. Answered in each
  architecture's trap path, because they are facts about the processor:
  `exit_group`, `arch_prctl(ARCH_SET_FS)` and `set_tls`.

**What running foreign binaries found**, none of which a hand-written test
program would have:

* glibc spun forever on `clock_gettime(CLOCK_MONOTONIC)` returning `ENOSYS`,
  with no output at all — there is no vDSO, so it makes the real call.
* `brk(0)` answered the top of the user half, because the heap was placed above
  the highest mapping and the highest mapping is the stack. The first `mmap`
  then landed in the page deliberately left unmapped above the stack.
* `uname` reports `sysname` as `Linux`, not `Ferrix`, because the programs that
  ask are choosing a code path. The identity goes in `nodename` and `release`:
  `Linux ferrix 6.1.0-ferrix`.
* ARMv7-A entered a Thumb-2 program in ARM state. An odd entry point is Thumb
  by the interworking convention, and Alpine's busybox enters at `0x1d1f9`.
* Neither Arm kernel let user mode use the FPU. ARMv7-A's busybox is hard-float,
  and AArch64's ran only because EDK2 happened to leave `CPACR_EL1.FPEN` open.
* busybox's `printf` asks `fcntl(1, F_GETFL)` before writing and prints nothing
  when it fails — confirmed by injecting `ENOSYS` into exactly that call on the
  host. `fcntl` belongs to stage 8's descriptor table, so `printf` is not in the
  test script yet.

**Signals are recorded, not delivered.** The dispositions, the blocked mask and
the alternate stack answer consistently — the old action a program reads back is
the one it set, at its own architecture's layout, which is three native words
and an 8-byte mask. Nothing in a clean run raises a signal. Delivery is the
first item below.

**Programs are scheduled tasks.** Each program runs as a task of its own that
carries its process, rather than as a guest of the boot task with interrupts
masked. The boot test shows two sharing one processor and a third ended from
outside:

      procs    two programs took turns on one processor, switched to 45 and 49 times
      kill     a spinning program was ended from outside and reported 137

The check program spins in user mode, so it is preempted there only if
interrupts are open; masking them makes the first program run to the end the
first time it is switched to, and the check fails naming that rather than
merely running slower. `process::load` builds a process and `process::start`
runs it, so a handle can be put between the two; `process::kill` ends one from
outside; ending is a level (`Process::is_terminated`) and a wake-up, and
whichever of `exit_group` and `kill` arrives first sets the status. Three
things a program owns stopped being the processor's: its kernel entry stack,
its thread pointer, and its floating-point and SIMD registers, which the
scheduler now saves and loads on every switch between user tasks — eagerly,
next to the address space. And one thing that had been hiding: the `SYSCALL`
MSRs were programmed only on a processor that had started a program, which is
harmless when programs never move and a `#UD` when they do.

**A program makes programs.** `fork`, `vfork` and `clone` without new threads,
`execve` with one `#!` level, `wait4` and `waitid`, and the process-group and
session calls work on all three architectures, with two programs of their own
in the boot test:

      fork     a program forked, waited for its child, and exited with 24
      execve   a program became another and exited with 42; with the file gone it got errno 2

A child resumes from a copy of its parent's saved registers (`arch::UserRegs`,
entered by `arch::resume_user`) in a copy-on-write copy of its memory.
`execve` refuses everything it can before its point of no return, then empties
the process's own address space and loads into it, so the process keeps its
identity; a failure after that ends it with `SIGSEGV`'s status, as on Linux.
`vfork` copies rather than lends memory, and the parent still sleeps until the
child execs or ends. A new thread is still `ENOSYS`.

**Left, and why it did not block the exit:**

* **Signal delivery and `rt_sigreturn`**, and `SIGSEGV` from the fault path,
  which is what rustc's stack-overflow guard needs. Owed with the first thing
  that has to kill a program.
* **`futex` and threads**, which nothing single-threaded calls.
* **Everything that opens a file**, `fcntl` included — stage 8.
* **Three stand-ins, each written down where it lives.** The console's `read`
  does a line discipline's job until stage 15 brings ttys; the real-time clocks
  read 1970 until something reads a clock chip; `getrandom` is xorshift seeded
  from a counter and says so.

---

## Stage 8 — VFS, initramfs, the pseudo-filesystems  ·  *month*

Inode and dentry caches, the mount table, file descriptors and their sharing
rules, tmpfs, devfs, procfs (`self/maps`, `self/exe`, `self/fd`, `cpuinfo`,
`meminfo`), and cpio initramfs unpacking.

**Done — the VFS, host-tested before the kernel calls it.** `libs/vfs` is
the half of the stage that needs no machine, written first for the reason the
continuous rule gives: path resolution over names a program chose is exactly
the code that should meet a fuzzer before it meets ring 0.

* **Dentries, with negative entries.** A name and the thing it names are
  separate objects, which is what `..`, `getcwd`, mount points and
  `/proc/self/fd` are answered from. A miss is cached like a hit. A child
  holds its parent and a parent holds its children weakly, so what keeps a
  dentry alive is a bounded queue of recent ones — the whole eviction policy,
  replaceable without touching the tree. A lookup racing a create cannot cache
  a stale miss: each directory carries a generation that every change bumps,
  and a lookup inserts only if it is unchanged.
* **Mounts and the walk.** One path walk for every call that takes a path,
  following links from a heap stack rather than by recursion, crossing mounts
  in both directions, and never climbing above a context's root. The
  namespace takes the root and working directory as an argument rather than
  knowing about processes, which is what lets stage 13's mount namespaces be
  more of the same type.
* **Open file descriptions and descriptor tables**, kept apart the way Linux
  keeps them: `dup` and `fork` share an offset, separate `open`s do not, and
  close-on-exec belongs to the number.
* **tmpfs**, whose file contents are not a byte vector but a page store the
  kernel supplies — a VMO there — so that `mmap` of a tmpfs file can later map
  the file's own pages. Directory cursors are never reused, so `rm -rf`
  reading a directory it is emptying sees every entry exactly once.
* **initramfs unpacking** through the same calls a program makes, hard links
  and device nodes included, and **the `getdents64` packer**, whose names start
  at byte 19 rather than at the structure's size of 24.

44 host tests, the `vfs_ops` fuzz target — which asserts that every name a
listing reports resolves to the inode the listing gave, the property a stale
cache entry breaks — and a Miri step.

**Done — the root, built at boot from what the loader hands over.** The
loader reads `FERRIX/INITRD.IMG` into memory nothing reclaims; it is optional,
so a card flashed without one boots as it did. xtask writes the archive itself,
the same bytes on every build. The kernel unpacks it into a tmpfs root through
the same VFS calls a program makes, and mounts a second tmpfs on `/tmp`. File
contents are VMO pages, created at a tebibyte and paid for by the page, so the
object `read` copies out of is the one `mmap` of the file will map. The boot
check reads the archive's marker back through its hard link and its symbolic
link, then writes a file across pages under `/tmp`, truncates into it, grows it
and removes it:

```
  initrd   2 KiB unpacked: 6 directories, 2 files, 1 hard links, 1 symbolic links, 0 refused, verified true
  tmpfs    4 pages written through a VMO and read back, 0 frames leaked
```

**Done — the calls that take a path.** `kernel/src/syscall/path.rs` and
`stat.rs`: `mkdirat`, `mknodat` (regular files, pipes and socket names; device
nodes are `EPERM` until devfs owns the numbers), `unlinkat`, `renameat2` with
`RENAME_NOREPLACE`, `symlinkat`, `linkat`, `readlinkat`, `chdir`, `fchdir`,
`getcwd`, `faccessat` and `faccessat2`, `chmod`, `chown`, `utimensat`, `umask`,
`getdents64`, and the stat family with `statx` — each with its pre-`*at` form
where the architecture has one. The one architecture-dependent fact is which
`struct stat` a stat call fills: x86-64's own 144 bytes, the generic 128, or
ARMv7-A's 104-byte `stat64`, whose EABI padding `libs/linux-abi` now names.
It is `arch::STAT_LAYOUT`, and all three encoders are compiled, and checked, on
every architecture. The umask is per process, 0o022 to start, and applies to
`openat`'s create too. The boot check makes each call by its number against
`/tmp`, decodes every record back out of user memory, lists forty names in
96-byte pieces, and runs twice:

```
  paths    145 path calls under /tmp, 42 names listed in 12 getdents64 calls, 0 frames leaked, dentry cache +0
```

**Done — descriptors, and the console as a file.** Every process has a
descriptor table and a root and working directory, each behind an `Arc` so
that `clone` can share them where `CLONE_FILES` and `CLONE_FS` ask and copy
them where they do not. A new process's descriptors 0, 1 and 2 are one open
description of `/dev/console`, whose inode carries the line discipline the
shell prompt depends on. `openat`, `close`, `read`, `write`, `readv`,
`writev`, `pread64`, `pwrite64`, `lseek` and `_llseek`, `dup`, `dup2`, `dup3`,
`fcntl` and `ftruncate` answer through it. `fcntl(F_GETFL)` reports `O_RDWR` on
the console, which is the one thing busybox's `printf` needed before it would
print, and stage 7's shell test has its `printf` line back. The `O_*` bits
x86-64 and the Arm architectures number differently are tables in
`libs/linux-abi`, chosen through the architecture facade. `ioctl` on the
console goes to `syscall/tty.rs`, which refuses `TCGETS` on purpose: with it
answered, `sh -i` switches the terminal to raw mode and echoes for itself,
doubling every character over the line discipline.

**Done — `/dev` and `/proc`.** devfs holds `null`, `zero`, `full`, `random`,
`urandom`, `tty` and `console`, numbered as Linux numbers them. procfs renders
every file at open, so a program reading `maps` in small pieces sees one
snapshot, and its directories opt out of the dentry cache, so a pid looked up
before its process existed is not remembered as missing. `/proc/self` links to
the caller's pid; each `/proc/<pid>` has `fd`, `status`, `comm`, `cmdline`,
`stat`, `maps` and `exe`; and `/proc` has `cpuinfo`, `meminfo`, `mounts`,
`filesystems`, `uptime` and `version`. The text is `libs/procfs`, pinned byte
for byte against lines taken from a real Linux `/proc`. Behind it, every
process now has a pid from a registry that finds a live process by it.

```
  devfs    7 nodes numbered as Linux numbers them; zero, null, full and urandom do what they are for
  procfs   20 names listed and walked back to, 4 maps lines parsed, 2 of them named
```

**The exit test, and how far it gets.** `cargo xtask test-vfs --init PATH`
puts a static musl busybox into the initramfs and has init run the exit
criterion's commands from it, checking their output after the boot marker. It
is a test of its own rather than part of `test-boot` for the reason stage 7's
is: the binary is not the repository's. Measured before a line of it was
written (`docs/STAGE8-WHAT-THE-EXIT-NEEDS.md`), this busybox runs every applet
that touches a file — `mkdir`, `mv`, `ln`, `rm`, `cat` — through `fork`,
`execve` and `wait4`. Until those existed the test ran eleven programs in the
script's place. With stage 7's `fork`, `execve`, `wait4` and `poll` on top, it
is the criterion's three commands: `ls -R /proc`, `cat /proc/self/maps`, and one
`sh -c` script whose file applets are real forked programs. Every command is
judged on its output as well as its status, and the script ends with a status
of its own, so a shell that died and reported success cannot pass. It passes on
all three architectures:

```
  x86_64: stage 8's exit programs all passed
  aarch64: stage 8's exit programs all passed
  armv7a: stage 8's exit programs all passed
```

**Why the stage is not marked done anyway.** A review of the VFS by another
session found five bugs, three reproduced on the host, and a stage whose exit
test passes over them is not finished. The worst lets a racing rename move a
directory inside itself: a lookup that loses a race with a create hands back a
second, uncached dentry for a directory, whose parent a later rename does not
update, so the ancestry check reads a stale chain. The others: a racing
`open(O_CREAT)` without `O_EXCL` can fail with `EEXIST`; `..` in a directory
listing reports the directory's own inode number; `openat` at the descriptor
limit creates the file before failing with `EMFILE`, leaving it behind; and a
rename over an empty directory keeps it alive. Fixes, each with a host test
that fails first, are in progress.

**Still to do.**

* The five VFS bugs above.
* Pipes and FIFOs over `libs/vfs`'s pipe buffer, `statfs`, `sync` and its
  kin, `truncate`, `fallocate`, `chroot`, `mount` and `umount2`, `sendfile`,
  and extended attributes: in progress.

**Exit:** `busybox ls -R /proc`, `cat /proc/self/maps` and a shell script that
manipulates files under tmpfs, all under the boot test.

---

## Stage 9 — The native ABI: handles, channels, ports, VMOs  ·  *week*

Handle tables, `Channel` with handle passing, `Port` event queues, `Interrupt`
objects, `IoMapping`, and `Job`. The syscalls in the `0x1000` range. This is
what stage 10 is written against.

**Done — the ABI written down, and the table under it.** Host-tested, fuzzed
and under Miri; not yet reached from the kernel.

* `libs/native-abi` — the numbers, handle values, rights, signals, error names
  and `repr(C)` layouts. One number table on every architecture, in
  `0x1000..=0x1FFF`, held clear of all three Linux tables by a test rather than
  by a comment. No argument is wider than a register — anything that must be
  64 bits on ARMv7-A goes through a pointer — so no native call exists twice
  the way sixteen Linux calls do there. Failures are `errno`, each native
  failure a distinct one, because `devmgr` is a musl program. Rights live on
  handles and only shrink, decided in one function.
* `libs/objects` — the handle table and a channel's message queue. A handle is
  a slot and a generation, and a slot is retired rather than let its
  generation wrap, so a closed handle *never* names anything again: the
  `handle_table` fuzz target checks that after every operation, against a
  model. Batch take and insert are all-or-nothing, and a read that does not
  fit takes nothing, because either failure half-done loses handles.

**Done — the first kernel objects, in the boot test on all three
architectures.**

* A handle table on every `Process`, behind a lock of its own, and the native
  dispatch: `dispatch` branches on the range before any Linux table is asked,
  so `arch::decode_syscall` never sees a native number.
* `handle_close`, `handle_duplicate`, `handle_replace`; `channel_create`,
  `channel_write` and `channel_read` with handle passing; `vmo_create`,
  `vmo_read`, `vmo_write`, `vmo_get_size`. A channel write takes the sender's
  handles out of its table only under the peer's queue lock, after the queue
  has accepted the message, so a refused send moves nothing.
* Objects that contain objects never drop them recursively. A closed endpoint
  hands its queue to `object::dispose`, which drops one level at a time, so a
  program that queues endpoints inside endpoints cannot turn a close into a
  kernel stack overflow. Jobs too, since a review found this was true only of
  channels: a child holds its parent, so a job's drop unwinds its chain of
  parents in a loop, and the check frees a chain of ten thousand.
* The self-check builds two processes and drives the handlers through raw
  registers: a message and a VMO handle cross from one table to the other and
  the handle that arrives reads back what the sender wrote; a read that does
  not fit is refused with the sizes and leaves the message queued; rights only
  shrink; a refused send keeps its handles; a full channel says wait; closing
  a reader frees a VMO still queued in it; a send that would leave two
  channels queued in each other is refused; and the frame count says nothing
  leaked.
* `libs/objects` now keeps every handle value below 2^31. A new handle comes
  back in the register an `errno` does, and on a 32-bit machine a larger value
  reads as negative.
* **A send that would close a cycle of channels is refused.** Endpoints keep
  each other alive only through their queues, so two endpoints each queued in
  the other would outlive every handle to both. A send carrying an endpoint
  walks from what it carries, through the endpoints queued in each, for the
  end it would land in (`libs/objects`'s `reaches`), and holds a lock only
  such sends take from the walk to the push, so two sends cannot build the
  loop between them. A walk past 1024 endpoints is refused as too big rather
  than allowed to hold that lock.
* **Signals and waiting.** Every object reports its signals as a level —
  a channel end `READABLE`, `WRITABLE`, `PEER_CLOSED`; a killed job
  `TERMINATED` — and names the queue woken when they may have changed.
  `object_wait_one` looks at the level before it sleeps, so a message that
  arrived before the wait is not lost, and also ends when the waiting process
  is killed rather than sleeping out its deadline. The check wakes a
  two-minute wait with a message written twenty milliseconds later.
* **`Job`.** `job_create` and `job_kill`, with a tree of jobs holding
  processes. A kill walks the tree without recursion, ends every process in
  the job and beneath it through `process::kill`, and marks each job under the
  lock that adding to it takes, so nothing can join a job being killed and
  survive it. The check kills the middle of a three-job tree of programs,
  each blocked in `object_wait_one` on a channel nobody writes to, and
  requires the two beneath to end with 137 and their tasks to stop, the one
  above to keep running, and a program outside every job to finish with its
  own status; then it kills the root. The members were spinning programs at
  first, and on a loaded four-processor ARMv7-A boot a spin of `u32::MAX`
  rounds finished before the kill it was meant to survive — so the check
  failed three boots in four on `main` until they became programs only a kill
  can end.
* **Device handles, `IoMapping` and `Interrupt`**, minted from stage 10's
  device nodes through `Aperture` and `Vector`, types only enumeration can
  construct, so a driver cannot name memory or a line its device does not
  have. `io_mapping_create` refuses a range outside one of the device's
  apertures with `ACCESS_DENIED`, and an aperture that is not whole pages with
  `INVALID_ARGS` rather than rounding it out: QEMU's ARM machines pack
  virtio-mmio transports several to a page, and rounding would hand a driver
  its neighbours' registers. `AddressSpace::map_device` inserts a
  `Backing::Device` region that commits nothing, is shared rather than copied
  across `fork`, and is never executable; its pages arrive on first fault,
  uncached. An `Interrupt` masks its line when it fires and holds `READABLE`
  until `interrupt_ack` unmasks it, through `arch::mask_interrupt`, backed by
  GICv2 on both Arm architectures. The check maps a whole-page aperture and
  requires its first fault to translate to the device's own physical page,
  from a forked child too, refuses one byte past it and a sub-page aperture,
  and on ARMv7-A claims a virtio-mmio vector once, runs the kernel's delivery
  path, waits, acknowledges, and claims the line again after closing. It
  first ran before the device nodes were published, found none, and passed on
  every machine having checked nothing; it now runs after them and fails if a
  machine with an aperture or a vector mapped or held none.

**The exit criterion is met in the boot test**, on all three architectures.
Two copies of `arch::USER_NATIVE_PROGRAM`, assembled per architecture, run
as user-mode tasks of their own: one creates a VMO, writes a secret into it and
sends a message carrying its handle; the other blocks in `object_wait_one`,
reads the message, reads the secret back through the handle it was sent, and
replies with it; and the first exits 0 only if the reply is the secret. The job
check is the other half: a `job_kill` ends every program in the job and beneath
it, blocked in a native wait or spinning alone on a processor, and none above
it. The exit test found, on four-processor ARMv7-A, that a program blocked in
a system call could resume with another program's user stack pointer; stage
7's `144b0cc` fixed it. The job check found that a kill never reached a
program spinning alone on its processor; `301aec4` fixed that. The stage stays
open for what stage 10 needs beyond the criterion.

**Still to do.**

* Ports and asynchronous waits (`object_wait_async`, `port_*`), binding an
  `Interrupt` to a port, and `vmo_map`.
* **An interrupt wakes its waiter within five milliseconds, not at once.** A
  wait queue takes a plain lock, which an interrupt handler may not, so the
  handler only masks and marks and the waiter notices through its wait's
  recheck. Waking from interrupt context needs an interrupt-safe wake.
* An `Interrupt` on x86-64, where device interrupts are MSI-X and are masked
  in the device's own table, which stage 10's `Vector::mask` will route to;
  and sub-page apertures, which need each access trapped.
* Process creation in the native ABI. `0x1030..=0x1037` is held for it.

**Exit:** two user processes exchange messages and a handle over a channel, and
a `Job` kill takes down a process tree.

---

## Stage 10 — Userspace drivers  ·  *month*

ACPI and device-tree enumeration in the kernel; IOMMU domains (VT-d, AMD-Vi,
SMMUv3); `devmgr`; the shared-ring block protocol; and the first driver —
virtio-blk — as a user process.

**Exit:** a boot test that reads a sector from a virtio disk through a driver
running in ring 3, with the IOMMU on and a deliberate out-of-domain DMA
attempt faulting.

**Done — configuration space, ahead of the kernel code that reads it.**
Started while stage 9 is still under way, because the exit criterion needs
stage 9's objects but most of what stands between here and it does not.

* `libs/pci` — configuration space as arithmetic over a `ConfigSpace` the
  caller implements, answering as hardware does: all ones for a function that
  is not there. `#![forbid(unsafe_code)]`, no allocation, no recursion.
  * **ECAM**: the window geometry, and an adapter that turns "read these bytes
    at this offset" into configuration space, so the kernel's half is its
    volatile access and nothing else.
  * **Headers**, types 0 and 1, and the class code.
  * **BARs**, decoded and sized. Sizing is where the two classic mistakes live
    — probing a BAR while its function still decodes, so it briefly answers
    for whatever sits at the top of the address space, and sizing or
    restoring only the lower half of a 64-bit BAR — and neither is visible on
    a machine with one device. The tests' fake bus records the first and
    compares every register before and after for the second.
  * **Both capability lists**, walked with a visited set over every offset the
    list could use, so a list that points back at itself ends in an error, not
    a kernel that never finishes enumerating. MSI-X decoded from its
    capability.
  * **The bus walk**: every function reachable from a root bus through bridges
    firmware numbered. A bridge whose numbers make no sense — not above its
    own bus, outside the window, already claimed — is reported and skipped,
    and no arrangement of bridges scans a bus twice. Bus numbers are
    firmware's and are not renumbered, because the number is also where the
    function sits in the ECAM window and in the IOMMU's tables.
  * **virtio's PCI transport**: the vendor capabilities that say where in its
    BARs a modern virtio device keeps each register block, and a check that
    each block fits the sized BAR it names — the kernel will map exactly those
    blocks into a driver, and a block that overhangs its BAR would map
    whatever is next to it.
* The `pci_walk` fuzz target builds configuration spaces from its input and
  requires every walk to end, every function to be found once, and sizing to
  restore every register without probing a decoding function; 5.1 million runs
  found nothing when it landed. Miri runs the host tests in CI.

**One bug, found in review before anything ran.** The first BAR sizing
required the bits that stuck to run all the way to bit 63. A device with a
64-bit BAR that decodes only 40 bits of address reads back zeros above bit 40
— which the specification allows and Linux's sizing expects — and would have
been refused as malformed. The
check now requires one contiguous run, and a test sizes a 40-bit decoder.

**Done — enumeration, in the boot test on all three architectures.**

* **Where configuration space is.** `libs/acpi` reads the MCFG, `libs/fdt` the
  `pci-host-ecam-generic` nodes. The two disagree about what their address
  means: an MCFG allocation's is where bus *zero* would be, whatever bus it
  starts at, and a device tree's `reg` is its first bus's. Both parsers hand
  over the first bus's, so `kernel/src/pci.rs` never has to remember which it
  read. Where the loader handed over ACPI tables the MCFG is authoritative;
  otherwise the device tree is read. A `bus-range` larger than its window is
  cut to what the window holds, as Linux does.
* **A bus at a time.** An ECAM window is a megabyte per bus, and every machine
  here describes 256 buses. Mapping all of it would take 256 MiB of address
  space — more than half the 32-bit kernel's arena — to reach a handful of
  functions on bus zero, so a bus's megabyte is mapped the first time the walk
  reads from it, and every window is given back afterwards.
* **The check** walks every host, sizes every BAR, walks both capability lists
  of every function and finds its virtio transport. It fails if a described
  host answers with nothing, if a bus cannot be mapped, or if `libs/pci`
  refuses anything a device presents. A machine that describes no host passes
  and says so, because the board has no PCI at all.
* **A virtio device on every test machine.** `virtio-rng-pci`, because it needs
  no backend and nothing depends on it, so the check meets a 64-bit BAR,
  MSI-X and virtio's vendor capabilities rather than only host bridges.

The run recorded when it landed: on x86-64, 6 functions from the MCFG with 8
BARs sized, 8 capabilities and one virtio transport; on AArch64, 2 functions
from the MCFG with 3 BARs, 6 capabilities and one virtio transport; on
ARMv7-A the same 2 from the device tree, reaching a window at
`0x40_1000_0000` — above 4 GiB, through LPAE.

**Done — device nodes, and the only way to name a device's memory.**
`kernel/src/device.rs`, written to the contract agreed with stage 9: its
`IoMapping` and `Interrupt` take an `Aperture` and a `Vector`, and only this
module can make either — from a sized memory BAR, from a `virtio,mmio` node's
`reg` and GIC interrupts, or afterwards from `DeviceNode::aperture`, which
answers only for a range inside *one* of the device's apertures. "Nothing
outside it" is then a type rather than a comparison a handler can forget.

* **The device tree is an allowlist** — `virtio,mmio` only — because most
  nodes with a `reg` are devices the kernel drives itself, and a node for the
  console would let a driver map it. An aperture overlapping the boot
  framebuffer is withheld; on x86-64 that is the display adapter's BAR, which
  is where a panic is drawn.
* **Not every aperture is whole pages.** QEMU packs its 32 virtio-mmio
  transports 0x200 bytes apart, several to a page, so a page mapping of one
  would hand a driver its neighbours' registers. An `Aperture` says whether it
  is whole pages, and `IoMapping` is to refuse one that is not. On QEMU's Arm
  machines that makes virtio-pci the transport a ring-3 driver can be given.
* **The check** asks every node for each aperture, its last byte, a range
  across each edge, an empty range and one that wraps the address space, and
  for every vector and the one past the end, before anything is published.

The run recorded when it landed: x86-64 publishes 6 nodes with 4 apertures, 1
withheld, and 24 refusals as specified; AArch64 2 nodes with 2 apertures;
ARMv7-A 34 nodes — its 32 virtio-mmio transports among them — with 34
apertures, 32 of them not whole pages, 32 edge-triggered vectors and 170
refusals, at four processors and at two.

**Done — a device driven by DMA, from the boot check.** The kernel does not
drive devices, but it owns everything a driver stands on — the BAR mappings,
the capability locations, and the physical memory a device reads and writes —
and enumeration proves none of that, because it only reads configuration
space. So once at boot `kernel/src/pci/virtio.rs` plays driver for the
simplest device there is, virtio-rng: it maps the common and notification
blocks the capabilities name, turns on bus mastering, gives the device a queue
in a page of its own, asks for 64 bytes, and requires the device to write them
into the page whose physical address it was given. Then it resets the device
— before the pages are freed, so the device holds no address into memory that
is given back — and restores the command register.

* `libs/virtio` gains the PCI transport's common configuration: the status
  protocol, feature negotiation and queue activation, host-tested against a
  device that behaves as virtio 1.2 §4.1.4.3 says. A reset that never finishes
  times out rather than hanging, a device that drops `FEATURES_OK` has refused,
  and `FAILED` is set before either error is returned.
* This is also the harness the next two items need. MSI-X is proven when this
  completion arrives as an interrupt rather than by polling, and an IOMMU
  domain when a descriptor pointing outside it faults.

The run recorded when it landed: 64 bytes read by DMA on x86-64, AArch64 and
ARMv7-A, at four processors and, on ARMv7-A, at two.

**Done — MSI-X tables kept out of a driver's reach.** An MSI-X interrupt is a
write the device makes, to an address and of a value the kernel put in a table
in one of the device's own BARs — so whoever can write the table chooses which
interrupt the device raises, the kernel's included. A PCI device node now cuts
the pages holding its MSI-X table and pending-bit array out of whichever BAR
holds them, mints apertures only from what is left, and records the withheld
ranges; the boot check requires every one of them, and its first and last
byte, to be refused. virtio-rng keeps its table in a BAR of its own, which
therefore stops being an aperture at all.

* `libs/pci::msix` is the arithmetic — the withheld and mappable ranges of a
  BAR, rounded out to pages, and the table entry layout — and the messages the
  table will be programmed with: the local APIC's on x86-64, and a `GICv2m`
  frame's on the Arm machines, read from QEMU's own `arm_gicv2m.c` rather than
  remembered — `MSI_SETSPI_NS` takes the GIC identifier itself, and
  `MSI_TYPER` reports the first identifier and the count. The `pci_walk` fuzz
  target now also requires a BAR's mappable and withheld ranges to partition
  it exactly, with every withheld range on page boundaries.
* `libs/acpi` reads the MADT's GIC MSI frame entries, and `libs/fdt` the
  `arm,gic-v2m-frame` nodes, which is where those frames are described.

The run recorded when it landed: x86-64 publishes 3 apertures where it had 4,
with one MSI-X range withheld; AArch64 and ARMv7-A publish one PCI aperture
each where they had two.

**Still to do, in the order it can be done:**

* **MSI-X vectors for PCI nodes.** A PCI node has apertures and no vectors.
  The messages and the table layout are written; what is missing is the
  allocator behind the architecture facade — local APIC vectors on x86-64, the
  `GICv2m` frame's SPIs on the Arm machines — built on stage 9's
  `arch::mask_interrupt`, and the boot check's completion arriving as an
  interrupt rather than by polling.
* **IOMMU domains, which need nothing from stage 9 either.** Where the
  hardware is can already be read: `libs/acpi` decodes the DMAR — each VT-d
  unit's register block and the devices behind it — and the IORT, following a
  requester ID from the root complex to the `SMMUv3` that translates it and
  the stream ID it arrives as. Both are tested against the exact layouts
  QEMU's own `build_dmar_q35` and `build_iort` produce. Still missing: VT-d on
  `q35` with `intel-iommu`, `SMMUv3` on `virt` with `iommu=smmuv3` — which
  this QEMU also offers on the 32-bit machine — and the deliberate
  out-of-domain DMA fault, driven from the boot check's virtio-rng harness.
* **Everything that runs in ring 3, which does.** Device-node handles
  (`Object::Device`); `Interrupt` and `IoMapping`, which stage 9 writes against
  `device.rs`'s tokens; `devmgr`, the ring protocol, and virtio-blk as a
  process. One consequence for the test machine:
  under ACPI, QEMU describes AArch64's virtio-mmio devices only in the DSDT,
  which is AML, which Ferrix will not interpret — so the disk that driver
  reads has to be `virtio-blk-pci` there.

---

## Stage 11 — Block core and btrfs, read  ·  *month*

Request queues, merging, the I/O scheduler. Then btrfs stage A: superblock,
chunk tree, root tree, fs trees, extents inline and regular, crc32c, and
zstd/zlib/lzo.

The item parsing is `libs/` code — pure functions over bytes, fuzzed against
images `mkfs.btrfs` produced.

**Exit:** Ferrix mounts an image made by real `mkfs.btrfs`, and reads a file
tree out of it that byte-for-byte matches what the host wrote.

---

## Stage 12 — btrfs, write  ·  *longer*

Copy-on-write allocation through the extent tree, delayed refs, transaction
commit against both superblocks with correct flush/FUA ordering, the free-space
tree, and log-tree replay.

**Exit, and it is a strict one:** Ferrix writes a tree, and host `btrfs check`
finds nothing. Then the power-fail test — kill QEMU at a random point inside a
transaction, remount, replay, `btrfs check` again — over hundreds of seeds.

---

## Stage 13 — Namespaces, cgroups v2, seccomp  ·  *month*

All eight namespaces, the unified hierarchy with `cpu`/`memory`/`io`/`pids`,
cgroupfs, and classic-BPF seccomp with the interpreter in `libs/`.

**Exit:** an unprivileged user namespace runs a process whose pid is 1 inside
it, under a memory limit that triggers scoped reclaim and a scoped OOM kill,
with a seccomp filter that blocks a syscall.

---

## Stage 14 — Real-time domains  ·  *month*

The `SoftRt` and `HardRt` modes: FIFO/RR classes, threaded interrupts,
priority-inheritance mutexes, EDF with constant-bandwidth admission control,
and the runtime domain-mode switch with its quiescence protocol.

**Exit:** a cyclictest-shaped boot test measuring wake-up latency on a `HardRt`
domain while a `Throughput` domain on other cores is saturated, reporting a
maximum inside the stated bound; plus an admission test that refuses an
unschedulable set instead of missing deadlines.

---

## Stage 15 — A real userland  ·  *week*

Static musl busybox as `/bin`, a working init, job control, ttys, pipes. Enough
of a system to be used rather than demonstrated.

**Exit:** an interactive shell over the serial console that a person can use.

---

## Stage 16 — `rustc`  ·  *the goal*

The remaining syscall surface, the memory scale, the process spawn path for
`rust-lld`, and a sysroot on btrfs. Then run it.

**Exit:** `rustc hello.rs && ./hello` on Ferrix, in CI.

---

## Stage 17 — Self-hosting

Build Ferrix on Ferrix. At that point the acceptance test writes itself: the
image produced by the Ferrix-hosted compiler boots and passes every test above.

---

## Written ahead of their stage

The rule at the bottom of this file — anything expressible as a pure function of
bytes goes to `libs/` *before* it is called from the kernel — has a consequence
worth stating plainly, because otherwise the tree looks further along than it is:
several crates for stages that have not started are already written and tested.

They are parsers and data structures, not subsystems. None of them counts
towards the stage that will consume it, and most are still unreachable from
the kernel. Four are the exception, which is what the rule was for:
`libs/acpi` as of stage 3 — the MADT walk the interrupt controller needed was
already written, tested and fuzz-shaped before a line of controller code
existed — `libs/fdt`, which the ARMv7-A port reached from stage 1 because that
machine has no ACPI to read, and, as of stage 2, `libs/vma` and `libs/sync`,
whose `AddressSpace` and `IrqSpinLock` the vmap arena is built on, with
`IrqControl` implemented over each architecture's interrupt mask. Being
reached early is not the same as their stage being done: the arena uses the
interval tree as an allocator of kernel ranges, not as a process's address
space. `libs/sync`'s stage has now come — stage 4's exit test is four
processors contending for one of its ticket locks.

What they buy is that the stage in question begins with its byte-handling
already fuzz-shaped, host-testable and argued about, rather than being written
at three in the morning against a machine that reboots on a mistake.

| Crate | Waiting for | Tests |
|---|---|---|
| `libs/acpi` | 3, 10 — RSDP, XSDT/RSDT, MADT, FADT fixed fields, GTDT, HPET, MCFG, GIC MSI frames, DMAR, IORT. No AML, and there will be none. | 72 |
| `libs/fdt` | Reached at 1 on ARMv7-A — the console, the GIC, the timer's interrupt and the PSCI conduit come from it there, and nothing else describes that machine. Reached at 10 for PCI host bridges `virtio,mmio` devices and `GICv2m` frames; stage 10 is still the rest of it. | 70 |
| `libs/sync` | Reached at 4 — `SpinLock` and `IrqSpinLock` guard every shared kernel structure and carry the contended counter; `RwSpinLock` is still waiting. Fair by construction, because an unfair lock on a starved core is a stage-14 latency bug nobody will find. | 19 |
| `libs/vma` | 6 — already backs the vmap arena. The VMA interval tree and the three calls that reshape it (`mmap MAP_FIXED`, `munmap`, `mprotect`). | 60 |
| `libs/linux-abi` | 7 — syscall numbers, `errno`, `repr(C)` layouts. Constants only; nothing executes. Three number tables, one of them 32-bit. | 59 |
| `libs/ustack` | 7 — the initial process stack `execve` hands a program: argv, envp and the auxiliary vector, at both pointer widths. Has its fuzz target and its Miri step already. | 22 |
| `libs/cpio` | 8 — the "newc" reader an initramfs is unpacked from. Borrows, copies nothing, allocates nothing. | 45 |
| `libs/vfs` | 8 — dentries, mounts, the path walk, open file descriptions, descriptor tables, tmpfs over a page store, initramfs unpacking. Written at the start of its stage rather than ahead of it. Has its fuzz target and its Miri step already. | 45 |
| `libs/procfs` | Reached at 8 — the text of `/proc`: the `maps` line padded to its name column at both pointer widths, `meminfo`, `status`, `stat` and `mounts`, pinned byte for byte against lines a real Linux printed, and the `maps` parser the kernel's boot check reads its own output back with. No fuzz target: it arranges the kernel's own numbers rather than parsing a stranger's bytes. | 14 |
| `libs/virtio` | 10 — the split virtqueue as logic over an abstract shared memory, and the PCI transport's status protocol, feature negotiation and queue activation. Reached at 10 by the boot check's virtio-rng driver. | 61 |
| `libs/pci` | 10 — configuration space: ECAM geometry, headers, BAR decoding and sizing, both capability lists, MSI-X, the bus walk, virtio's PCI transport, MSI-X messages and the pages of a BAR a driver must not be given. Has its fuzz target and its Miri step already. | 45 |
| `libs/native-abi` | Reached at 9 — native syscall numbers, handles, rights, signals, `errno` names, `repr(C)` layouts. Constants only, like `libs/linux-abi`, and tested against it. | 13 |
| `libs/objects` | Reached at 9 — the handle table and the channel message queue, generic over what a handle names; every process's table and every channel is one; and the reachability walk a send makes before it queues an endpoint. Has its fuzz target and its Miri step. | 22 |
| `libs/btrfs` | 11, 12 — superblock, chunk tree, B-tree nodes, item payloads. Parsing only: no device, no cache, no transactions. | 38 |

With the five crates the boot path was built on — `bootinfo`, `elf` (the
loader's), `frame`, `heap`, `paging` — that is **670 host unit tests, all
passing**, plus the doc-tests and the 41 of `xtask` itself.

**The gap this opens, stated rather than hidden.** The continuous rule below
asks for a fuzz target *and* a Miri run per crate, and `fuzz/` has six:
`elf_parse`, `frame_alloc`, `ustack_build`, `handle_table`, `vfs_ops` and `pci_walk`. Every crate in the table above
parses bytes that came from outside the system — a disk, a firmware table, an
archive a stranger built — which is precisely the population the rule was
written for. The fuzz targets are owed, and are owed *before* the consuming
stage starts, not when it ships.

`ustack_build` is what the rule looks like when it is followed rather than
recorded as debt: written before a line of stage 7 kernel code existed, and it
found a real gap within a minute. A string with a NUL byte inside it built a
perfectly well-formed image that read back as a *different, shorter* string,
because everything on that stack is recovered by scanning for a NUL. The
builder now refuses it. Nothing about that bug is visible from the kernel side
— it is a program receiving an argument nobody passed it — and it would have
been found, if at all, by whoever was debugging a shell that mangled its own
arguments.

Miri is further behind than fuzzing. CI runs it over `libs/elf`,
`libs/bootinfo`, `libs/ustack`, `libs/objects`, `libs/vfs` and `libs/pci`, although the CI file's own comment names the
page-table arithmetic and the allocators as the reason the job exists — so
`frame`, `heap` and `paging`, all running in the kernel today, are owed a Miri
step too, and ahead of every crate in the table.

---

## Continuously, from stage 1

* Every stage's exit criterion joins the CI boot test and stays there.
* The assembly allow-list is not added to without an argument in the diff.
* Anything expressible as a pure function of bytes goes to `libs/` and gets a
  fuzz target and a Miri run — before it is called from the kernel, not after.
