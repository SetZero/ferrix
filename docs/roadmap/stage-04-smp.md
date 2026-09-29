# Stage 4 — SMP ✅

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
  host-tested in `src/lib/kernel/frame`. Each processor gets its own GDT, TSS and
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

Once the scheduler runs, the shootdown is checked again against what only
preemption makes possible: a task that moves to another processor while it
waits for its turn must answer for the processor it is on. The wait first read
its per-CPU record once, on entry, and so flushed one processor and recorded
the flush for the one it had left; put back, x86-64 fails on exactly that.

The boot marker read `FERRIX-BOOT-OK stages 1-5` when this landed.

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

