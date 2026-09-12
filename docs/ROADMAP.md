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

**Where it stands:** stages 0–5 are done and in the boot test on all three
architectures, and the boot marker reads `FERRIX-BOOT-OK stages 1-5`.
ARMv7-A joined after stage 3 — see *ARMv7-A* after stage 4. Stage 6 is next.
Nothing of stages 6 onwards exists in `kernel/` yet — no address space, no
user mode — though several of their byte-level crates do (see *Written ahead
of their stage*).

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

**Deferred:** load balancing beyond work stealing on an idle processor; a task
that is not a kernel thread, which is stage 6; and the other two domain modes,
which are stage 14.

---

## Stage 6 — User mode  ·  *week*

`AddressSpace`, VMOs, the VMA interval tree, demand paging, copy-on-write, the
ELF loader, and the ring-3/EL0 transition. The first user process is a
hand-written static binary that makes one syscall.

**Exit:** a boot test that runs a user binary which writes to fd 1 and exits,
with a page fault serviced along the way.

---

## Stage 7 — The Linux syscall ABI  ·  *month*

The syscall entry path on every architecture, the dispatch table, and the core
of the surface: memory (`mmap`, `mprotect`, `brk`), files, process
(`clone`, `execve`, `wait4`, `exit_group`), threads and `futex`, signals with
`sigaltstack` and `rt_sigreturn`, time, and identity.

**Exit:** a static musl `busybox sh` starts, runs a script, and exits — the
first time somebody else's binary runs on Ferrix.

---

## Stage 8 — VFS, initramfs, the pseudo-filesystems  ·  *month*

Inode and dentry caches, the mount table, file descriptors and their sharing
rules, tmpfs, devfs, procfs (`self/maps`, `self/exe`, `self/fd`, `cpuinfo`,
`meminfo`), and cpio initramfs unpacking.

**Exit:** `busybox ls -R /proc`, `cat /proc/self/maps` and a shell script that
manipulates files under tmpfs, all under the boot test.

---

## Stage 9 — The native ABI: handles, channels, ports, VMOs  ·  *week*

Handle tables, `Channel` with handle passing, `Port` event queues, `Interrupt`
objects, `IoMapping`, and `Job`. The syscalls in the `0x1000` range. This is
what stage 10 is written against.

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
| `libs/acpi` | 3, 10 — RSDP, XSDT/RSDT, MADT, FADT fixed fields, GTDT, HPET. No AML, and there will be none. | 58 |
| `libs/fdt` | Reached at 1 on ARMv7-A — the console, the GIC, the timer's interrupt and the PSCI conduit come from it there, and nothing else describes that machine. Stage 10 is still the rest of it. | 61 |
| `libs/sync` | Reached at 4 — `SpinLock` and `IrqSpinLock` guard every shared kernel structure and carry the contended counter; `RwSpinLock` is still waiting. Fair by construction, because an unfair lock on a starved core is a stage-14 latency bug nobody will find. | 19 |
| `libs/vma` | 6 — already backs the vmap arena. The VMA interval tree and the three calls that reshape it (`mmap MAP_FIXED`, `munmap`, `mprotect`). | 60 |
| `libs/linux-abi` | 7 — syscall numbers, `errno`, `repr(C)` layouts. Constants only; nothing executes. | 42 |
| `libs/cpio` | 8 — the "newc" reader an initramfs is unpacked from. Borrows, copies nothing, allocates nothing. | 45 |
| `libs/virtio` | 10 — the split virtqueue as logic over an abstract shared memory. | 50 |
| `libs/btrfs` | 11, 12 — superblock, chunk tree, B-tree nodes, item payloads. Parsing only: no device, no cache, no transactions. | 38 |

With the five crates the boot path was built on — `bootinfo`, `elf` (the
loader's), `frame`, `heap`, `paging` — that is **490 host unit tests, all
passing**, plus the doc-tests and the 41 of `xtask` itself.

**The gap this opens, stated rather than hidden.** The continuous rule below
asks for a fuzz target *and* a Miri run per crate, and `fuzz/` currently has two
targets: `elf_parse` and `frame_alloc`. Every crate in the table above parses
bytes that came from outside the system — a disk, a firmware table, an archive a
stranger built — which is precisely the population the rule was written for. The
fuzz targets are owed, and are owed *before* the consuming stage starts, not
when it ships.

Miri is further behind than fuzzing. CI runs it over `libs/elf` and
`libs/bootinfo` only, although the CI file's own comment names the page-table
arithmetic and the allocators as the reason the job exists — so `frame`,
`heap` and `paging`, all running in the kernel today, are owed a Miri step
too, and ahead of every crate in the table.

---

## Continuously, from stage 1

* Every stage's exit criterion joins the CI boot test and stays there.
* The assembly allow-list is not added to without an argument in the diff.
* Anything expressible as a pure function of bytes goes to `libs/` and gets a
  fuzz target and a Miri run — before it is called from the kernel, not after.
