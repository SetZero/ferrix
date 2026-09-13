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

**Where it stands:** stages 0–11 are done and in the boot test on all three
architectures, and the boot marker reads `FERRIX-BOOT-OK stages 1-11`.
ARMv7-A joined after stage 3 — see *ARMv7-A* after stage 4 — and has run on
hardware: an STM32MP157D-DK1 at two cores reached `FERRIX-BOOT-OK stages
1-9` and ran stage 7's script at `fd4442e`. Stage 7's exit is
somebody else's static musl busybox running a script on every architecture,
checked by `cargo xtask test-shell` rather than the boot test because it needs
a binary the repository does not carry. Since the exit, a program can
`fork`, `execve` and `wait4`; its section lists what the Linux surface still
owes. Stage 8's self-checks are in the boot test — the root filesystem
unpacked from an initramfs, every process's descriptor table, the calls that
take a path, pipes, `/dev` and `/proc` — and its exit, `cargo xtask test-vfs`
running `ls -R /proc`, `cat /proc/self/maps` and one shell script whose applets
are forked programs, passes on all three architectures; it is a test of its
own for the reason stage 7's is. Stage 9's exit runs in the boot test itself:
two programs in user mode exchange messages and a handle over a channel, and a
job kill takes down a process tree, with ports, interrupts delivered to them
and device memory a driver can map built on the same objects.
Stage 10's exit runs in the boot test itself: a virtio-blk driver in ring 3,
started from the boot check with the START it will get from `devmgr`, reads
sectors through the block ring with VT-d on x86-64 and the `SMMUv3` on AArch64
translating, and a deliberate out-of-domain write faulted on both; ARMv7-A runs
it in degraded trusted mode, as decided. What the stage still owes — `devmgr`
the program, trusting decoding-off BARs — is after the exit in its section.
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
kernel on each architecture, and the kernel verifies five things before it
reports success:

* the hand-off structure's magic, version and layout constants agree with what
  the kernel was built against;
* the memory map is sorted, non-overlapping, has usable RAM, and describes the
  loader's *own* allocations — each one looked up by its address and required
  to lie inside a region of its own kind: the kernel image, the root tables,
  the boot info and its array, the boot stack, the device tree copy and the
  initramfs. Without that the frame allocator would hand out the frames
  holding its own page tables, or the stack the kernel is running on, and a
  check that only asked whether a region of each kind existed somewhere would
  still pass;
* the kernel faults when it writes through a read-only mapping, which on
  x86-64 is `CR0.WP`: the loader sets it, because firmware need not have, and
  the W^X sweep reads entries rather than trying a write;
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

* `kernel/src/vmap.rs` — an arena over `KERNEL_VMAP_BASE` that hands out
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
  stack is unusable. So do the NMI, the debug exception and the machine check,
  on three more, because they arrive whether or not the kernel is on a stack of
  its own, and the `SYSCALL` trampoline's first and last instructions run in
  ring 0 on the program's stack: IST 1 the double fault, 2 the NMI, 3 `#DB`, 4
  `#MC`, sixteen kibibytes each, static on the boot processor and from the
  vmap arena, guard pages and all, on every other.
* x86-64 — those four take a *paranoid* entry (`arch/x86_64/paranoid.rs`),
  because the ordinary stub's rule, swap `GS` when the saved `CS` says ring 3,
  is wrong on the trampoline's ring-0 instructions that hold the program's
  `GS`. The entry reads `GS_BASE` instead, as Linux's `paranoid_entry` does:
  a kernel base is an upper-half record and no program can load one, so it
  swaps only for anything else and remembers to swap back. That rule needed a
  fix of its own: `syscall::init` had parked the kernel's record in the shadow
  MSR, so every program ran with the record as its `GS` base, both halves of
  `swapgs` held one address, and a wrong swap was invisible — the first boot
  of an entry that decided by `CS` passed the breakpoint check. A program now
  starts with `GS` base zero, as on Linux, and the check requires it. It clears `DR7` for
  the handler's run, which is what keeps `#DB` from nesting on its own stack
  (Linux's scheme since it retired the IST shift), counts each stack's
  occupants so a nested entry is reported as FX-9006 instead of returned
  from, and moves a ring-3 `#DB` onto the task's stack, where a signal may
  block or end the task. An NMI is counted and returned from; its handler
  takes no exception that could `iretq` early and let a second NMI in. A
  kernel `#DB` from a hardware breakpoint returns with `RF`; `#MC` stops the
  machine as FX-9005, on its own stack and the kernel's `GS`. A boot check
  after the trap flag check shows it (`paranoid::check`): an NMI sent to the
  processor itself with interrupts masked comes back; instruction breakpoints
  on the trampoline's `swapgs` and its `sysretq`, while a pinned program makes
  its calls, each find the kernel's `GS`, and the program's own `GS` base must
  not be a per-CPU record; and a breakpoint on code the `#DB` handler runs
  does not fire inside it. Each part stops the boot with its fix taken out,
  under tcg and kvm alike — tcg implements the debug registers and delivers
  the NMI.
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
  clock a PC is guaranteed to have. An HPET whose main counter is 32 bits
  wide — capability bit 13, and common on AMD chipsets — wraps every five
  minutes, so it is not handed out as the counter: the TSC is calibrated
  against it, subtracting in 32 bits, and used instead. QEMU's HPET is
  64-bit, so the boot test does not reach that path; `libs/acpi`'s `hpet`
  module holds its arithmetic and its tests. Every I/O APIC firmware described is
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

**Exit criterion met, and in the boot test on all three architectures.** The
timer interrupts are counted — 250 of them, a quarter of a second at the
kilohertz asked for; it was a thousand, and a second per boot per
architecture bought nothing the quarter does not — and the time they took is
measured with the *counter* rather than by multiplying the tick count by the
rate they were programmed at, which would be arithmetic that cannot fail
rather than a measurement. Against a requested 1000 Hz, all three report 998
to 999. It is a measurement, so it moves.

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
nothing in the log after the line before it. On x86-64 the count cannot move
at all, because the local APIC's one-shot never refires. The mistake that would
make it refire, a timer programmed periodic, was tried there: it hangs the boot
before the check reports, and the boot test's timeout catches it instead.

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

Once the scheduler runs, the shootdown is checked again against what only
preemption makes possible: a task that moves to another processor while it
waits for its turn must answer for the processor it is on. The wait first read
its per-CPU record once, on entry, and so flushed one processor and recorded
the flush for the one it had left; put back, x86-64 fails on exactly that.

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
which the board has and QEMU cannot place; using RAM beyond the direct map
(a machine with it boots, since 2026-09-13, and reports the excess unused); the
board's own UART; Thumb-2; VFP. The UART has since landed and carried the run
below.

**On the board, stages 1–9.** On 2026-09-13, at `fd4442e`, an STM32MP157D-DK1
— two Cortex-A7s and 512 MiB, under mainline TF-A, OP-TEE and U-Boot — booted
the kernel `cargo xtask test-shell` builds, copied to the card through U-Boot's
`ums` and started with `bootefi`. One boot on each, the same kernel:

| | QEMU `virt`, `--smp 2` | STM32MP157D-DK1 |
|---|---|---|
| Boot marker | `FERRIX-BOOT-OK stages 1-9` | `FERRIX-BOOT-OK stages 1-9` |
| `ACTLR.SMP` | clear on 2 of 2 | set on 2 of 2 |
| Stage 3 | 251 ticks at 998 Hz | 251 ticks at 999 Hz |
| Stage 4 | 50000 of 50000 | 50000 of 50000 |
| Stage 5 | 2462 switches, 44 steals | 2743 switches, 5 steals |
| Stages 6–9 | every check line | the same lines, except that the card carried no initramfs and the board has no PCI or IOMMU to find |
| W^X sweep | 961 mappings, 392 executable | 944 mappings, 392 executable |
| Stage 7's script | seven lines, then `the shell exited with 7` | the same seven lines; the exit line cut off |

The exit line is lost because the STM32 USART driver returns as soon as the
transmit register has room, and the PSCI `SYSTEM_OFF` after the last line
powers the board off while that line is still being sent. The drain is a
backlog row; the rerun that follows it, with the initramfs on the card, expects
the line byte for byte. The serial log is kept outside the repository, at
`~/.local/share/ferrix/board-boot-fd4442e-2026-09-13.log`.

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
  that. Folding in a long stretch costs no more than a short one: past 512
  periods, whatever came before is gone, and the average is set to the level
  held rather than walked there one period at a time. The walk had been done
  under the run queue lock with interrupts masked, three and a half million
  steps for a processor idle for an hour.
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

**Found by the review of stages 1–7 on 2026-09-13**, each fixed with a boot
check shown to fail without the fix:

* **The check that a dead task has left its queue could not fail.** It looked
  at a run queue's `previous` under the queue lock, but `finish_switch` empties
  `previous` before it releases that lock. `finish_switch` now asks the
  question itself, at the one moment a dead task leaves its processor for
  good, and records the answer for the invariant check. A kernel broken on
  purpose to leave dead tasks marked queued now ends stage 5 with "a dead task
  is still queued".
* **A dead task could be put back on the run queue.** A task woken after
  marking itself blocked, but before it reached `block`, kept its sleep
  deadline, because only a switch away took it. When the task later exited,
  that switch filed the dead task as a sleeper, and the timer made it runnable
  on a stack the reaper was freeing. `wake` and `exit` now clear the deadline,
  `choose_next` never files a dead task, and `wake_sleepers` wakes only tasks
  still blocked. The check wakes a task in exactly that window and requires
  that it not be filed as a sleeper after it exits.
* **A timer interrupt part-way into a wait lost the task.** `wait_until_deadline`
  marked the task blocked before it set its deadline or joined the waiter
  list, with interrupts on. A switch in between took the task off its run
  queue with no deadline to file it under and no waker able to find it, and
  the machine hung silently. The comment that excused it said every holder of
  the waiter lock masks interrupts; none does. The deadline and the waiter
  entry now come first and `BLOCKED` last, and the way out is the reverse.
  No boot check can hit the window on demand, so it was shown with a
  two-millisecond spin added inside it, locally: the old order hangs stage 5,
  and the new order boots clean with the same spin.
* **A task spawned or woken onto the caller's own processor waited for an
  unrelated interrupt.** The same class as the timer re-arm above, recorded
  as fixed for a spawn or wake onto *another* processor. Onto the caller's
  own, it only set the reschedule flag, which is read on the way out of an
  interrupt, and a system call or kernel thread returns through none. On a
  processor whose timer was stopped, the new task waited until the caller
  blocked or something else happened to interrupt it. The flag now comes
  with the timer armed for the shortest interval, and from inside an
  interrupt the exit re-arms it for the real decision first. The check
  spawns, then wakes, a task onto its own processor while it spins, and
  requires the task to run.
* **A task pulled by periodic balancing was not scheduled.** A pull added a
  task behind a processor's lone running task and asked nothing of it. Its
  timer was stopped, and later wake-ups saw two tasks and did not kick, so
  the pulled task waited for an unrelated interrupt. This was the
  intermittent "a balancing task never started": an anchor-only processor
  took a placement's broadcast, pulled a movable spinner, and never ran it.
  A pull now asks the puller to decide again. The check pulls a task from
  behind a spinner running with interrupts masked, then spins, and requires
  the pulled task to run.

**Four intermittent failures, found a day later and each a real bug.** Stage
5's checks had been failing one boot in three to six on a loaded host, and
had been counted as noise for a day. Probes printed from the worker tasks,
not the checker, and a per-pick trace found them:

* *"a task's stack was never given back"* — the idle loop reaped the whole
  zombie list at once and could be switched out mid-batch when the checker
  was woken onto its processor; the checker then yielded forever waiting for
  stacks the idle task held. The idle task now frees one stack at a time and
  is not switched out while holding one.
* *The thousand-task check taking 20–50 s* — the wait queue's lock was a
  plain ticket lock taken with interrupts on. A worker preempted inside the
  few instructions it is held started a convoy: every finishing worker spun
  its slice away holding a ticket, and each hand-off cost a full round of the
  queue. Two hundred thousand switches to run a thousand tasks. A holder with
  interrupts masked cannot be preempted, so that lock now masks them. Any
  plain spin lock taken from a task with interrupts on is exposed to the same
  thing once contended.
* *"every thread ran on one processor"* and *"a balancing task never
  started"* — a task spawned onto the spawner's own tickless processor, or
  pulled there by the balancer, waited for an interrupt that never came.
* *"a task's service strayed further from its share than EEVDF allows"* —
  not the scheduler. Lag carried *into* the window: a host stall while the
  first spinner ran alone was charged to it as service, EEVDF repaid its
  siblings inside the window, and the check read the repayment as a
  violation. The window now levels every lag when it opens, and its bound is
  a slice plus the sum of the overruns that processor served inside it.

**Preemption is disabled under every task-context spin lock.** The convoy
above was one lock; the kernel had forty more plain ticket locks taken from
tasks with interrupts on, each exposed to the same thing once contended.
Masking interrupts for all of them is the wrong tool, so the scheduler keeps
a per-processor preemption count, raised and lowered by the kernel's
`sync::SpinLock` (a `ferrix_sync::PreemptSpinLock`) for as long as it is
held and while it spins for its ticket, and read on the way out of every
interrupt: a pending reschedule waits until the count is zero, then is made.
A holder must not block, and the scheduler enforces it -- a switch with the
count raised stops the machine (FX-0503) -- so a holder that sleeps is found
by the first boot rather than by a convoy on a loaded host. The run queues'
own locks stay plain, being taken with interrupts masked and handed across a
switch.

**And no shootdown is asked for under one.** A shootdown waits for every
other processor to answer an interrupt, and a holder that asks for one keeps
its lock, contended, with preemption off, for the round trip; a holder with
interrupts masked cannot be answered at all. An audit of every lock in the
kernel on 2026-09-13 found no holder that blocks, and three that shot down:
a shrinking `brk` under the process's state lock, the alarm clock's spawn
under its running flag, and the migration check's spawns under the turn
itself, whose failure path would have waited for the turn it held. Each now
lets go first. The rule enforces itself: the scheduler counts, beside the
preemption count, how much of it *locks* raised, and both flushes assert
that count zero before they take the turn, naming the lock's site when it is
not, and interrupts on wherever they are about to wait for another
processor. A flush that waits for nobody else is exempt on purpose: stage 6's
checks fault with interrupts masked to keep a space installed, and a
copy-on-write fault there retires a page through a shootdown whose set names
only that processor; the first row of this landing found exactly that. The
same row found a real one: a secondary processor enters the idle loop with
the interrupts its hand-over masked, and its first reap frees a stack, a
global flush that waits for every processor; the idle loop now enables
interrupts once at entry. The reaper's own by-hand raise around freeing a
stack, which is a shootdown by design, is not a lock and passes.

**A queue insert charges the running task first.** `CpuQueue::insert`
placed a newcomer before charging the task already running, so a task alone
on a tickless processor -- charged only at its next decision, with an
`exec_start` a hundred milliseconds old -- had all of that billed after the
newcomer was counted, and the newcomer came out owed half of it, past the
placement clamp. Stage 7's first spinner arrived owed 59 ms and ran its
whole loop before the checker could start the second. `insert` and `release`
now charge first, as Linux's `enqueue_entity` calls `update_curr` before it
places; found by stage 7's session with a trace ring, 598 of 600 looped
iterations under KVM, and 2 of 12 whole boots under KVM on the tree before
the charge against 0 of 12 after it. And the check that caught it measures preemption now
-- each program switched out still runnable at the exit of an interrupt that
arrived in its user code, twice -- rather than being switched to twice,
which a program never preempted shows too, or switched away at all, which a
lock released inside its one write with a reschedule pending also does; it
gets three attempts, since a host stall charged to one program as service
lets the other run its whole loop, and prints what each attempt saw.

**Stage 4's contended count judges its overlap over five rounds.** The count
requires two processors' increments to overlap, and an emulator whose host
deschedules whole virtual processors can run the shares one after another
for a round. Each round is judged for the lock's correctness, the first
round that overlaps ends the check, a round that did not is printed with its
shares, and only five rounds without overlap fail it -- the same shape as
stage 7's three-attempt pair check.

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
* **Frame counts wait for the reaper, as a condition.** Every count in the
  self-check is taken by `sched::wait_until_reaper_quiet`: no task exited and
  not yet dropped by its reaper, nothing on the zombie list, no free in flight.
  The time-based settle before it let a task that had exited, but was not yet
  reaped, move the count ("running tasks in address spaces leaked frames").
  What every window counts is `mm::FrameWindow`'s: free frames and the heap's
  slab pages together. A slab page a size class takes or gives back inside a
  window moves one frame between the two and changes nothing. Before that, the
  first touch of a class failed stage 6's region check about one boot in five
  on a branch that grew the address space, and a slab drained elsewhere failed
  stage 8's path check under a slow boot, each by one frame that was not a
  leak. Large heap allocations still count. A small-object leak does not, by
  construction, so teardown of one is proven with a `Weak` that must fail to
  upgrade. A window that fails prints what it held at open and now, the heap's
  live bytes, and the frames through each route (allocated, freed, released,
  heap taken, heap returned, user and kernel tables), so a failure names its
  way in. A boot check holds a committed page and a large buffer across a
  window, and requires the window to count both.
* **An object knows who maps it, and a shootdown reaches only who may cache
  it.** Every address space that names a VMO is in that object's mapper list.
  So a page the object takes away is taken out of every space that maps it
  before its frame goes back. That covers a decommit, a replace, a move, and
  the copy a write makes of a page `fork` left shared. It happens in three
  phases: out of the page list under the object's lock; translations down in
  each space, with no object lock held, and one shootdown to the union of
  their processors; only then the frame. A shootdown names its pages and goes
  only to the processors in the space's set. A processor more than one
  generation behind, or a request with more pages than its slots, flushes
  everything. An object backing a private region has exactly one mapper,
  checked at every attach and before `mremap` moves its pages (FX-0005), which
  is what lets that move tell nobody else. `mprotect` shoots down what it takes
  down. The check poisons a page two processes on two processors have both
  touched, takes it away, and requires that neither reads the poison from user
  mode. It holds a page for a device across a decommit and a replace. Then it
  has a protect end the child's next write with `SIGSEGV`:

  ```
    rmap     2 frames taken from two processes on processors 0 and 1 and poisoned, never reached from user mode; a held page kept its mappings; scoped shootdowns 3 sent to 3 processors and 5 needing no interrupt, against 2 global; 0 frames leaked
  ```

  On the Arm pair a scoped shootdown needs no interrupt: `tlbi` with the `is`
  suffix reaches every processor. So the line counts none sent there.

**Decisions worth not reversing silently.** No `ASID`s or `PCID`s: a full
invalidation of user entries on switch is the correct baseline, and eliding it
later makes the switch faster rather than unpicking anything. On the Arm pair it
is still an invalidation *by* `ASID` — every space is `ASID` zero — because that
is the only form that drops user entries and keeps the kernel's global ones. A
mapping change is flushed by page, on the processors in the space's set, and a
switch never broadcasts. A frame is released only after every space its object
names has let go of it and the shootdown has returned. Anonymous memory names a
VMO, so shared anonymous mappings, futex keys and `/proc/self/maps` have an
object to name. One lock per address space, never a global one.

**Left for later, none of it on stage 6's path:**

* No check types at the console. Every architecture receives now — the Arm UART
  drivers were write-only, so a program reading fd 0 there waited forever, until
  the PL011 and the STM32 USART gained receive, by interrupt into a ring, as
  x86-64's 16550 now does too — but
  `test-shell` types nothing, so input is exercised only by hand. The ring
  itself is checked at boot.

**What the boot test could not see, now seen from user mode.** A
copy-on-write fault replaces a live read-only translation with a writable one,
and leaving the stale entry makes the retrying instruction fault forever — a
hang with no message; `protect` takes translations down and, until the reverse
map's scoped shootdown, left a processor's cached copy more permissive than the
map. No kernel-side check could see either. One writes from the kernel, through
the direct map or with the space installed, and a stale entry is only certain to
matter for a write made from user mode, on the processor that cached it. Stage
7's check now runs three hand-assembled programs per architecture, each the
witness this paragraph used to say nothing ran. A program writes a page, narrows
it to `PROT_READ`, writes again and must die of `SIGSEGV`; it is pinned to
another processor, so no switch between the two writes drops the entry by
accident, and without the shootdown it wrote through and exited with 1 on every
architecture, under `tcg` and under KVM alike; the reverse map's own check walks
the same narrowing from the kernel side, as its fourth step. A forked parent and
child, ordered
by a pipe, each write a page the other still shares copy-on-write, and neither
sees the other's write (61). A child's writes to `MAP_SHARED` anonymous pages,
one of them first touched by the child, reach its parent, and its `MAP_PRIVATE`
write does not (62). The last two have no failure shown without what they check:
a deliberate break in the fault path is caught by stage 6's kernel-side check
before stage 7 runs.
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
  as a different, shorter string. `execve` puts `AT_HWCAP` and `AT_HWCAP2` in it,
  from the core's own identification registers on both Arm architectures:
  musl's ARMv7-A `setjmp` saves `d8`-`d15` only when told there is a VFP. The
  bit values are checked against Linux's uapi headers and the fields are read
  as Linux reads them, signed where Linux reads them signed; `DCPOP` is not
  reported, because `DC CVAP` traps from EL0 without `SCTLR_EL1.UCI`.
* **Dispatch.** `kernel/src/syscall/`: `SyscallArgs`, `Outcome` and `dispatch`,
  reached through `arch::decode_syscall`, and total. The boot test puts every
  number in `0..=600` except `exit`, `exit_group`, `pause` and `alarm` through
  `dispatch`, with poisoned argument registers, from a task of a check process,
  so each call reaches its handler rather than a missing-process `ESRCH`. It
  fails if a call asks to enter user mode, ends or blocks that process, or
  leaves a frame behind.
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
* `uname` reported `sysname` as `Linux`, because the programs that ask are
  choosing a code path. On 2026-09-13 the project's owner chose `Ferrix`
  instead: `Ferrix ferrix 6.1.0-ferrix`, with the release still a Linux
  version. A build that maps `uname -s` to a target is told which to use.
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
      exits    4 programs that forked and exited gave every frame back once reaped, in window 1
      execve   a program became another and exited with 42; with the file gone it got errno 2

A child resumes from a copy of its parent's saved registers (`arch::UserRegs`,
entered by `arch::resume_user`) in a copy-on-write copy of its memory.
`execve` refuses everything it can before its point of no return, then empties
the process's own address space and loads into it, so the process keeps its
identity; a failure after that ends it with `SIGSEGV`'s status, as on Linux.
`vfork` copies rather than lends memory, and the parent still sleeps until the
child execs or ends. A new thread is still `ENOSYS`.

Every general register is cleared on entry to user mode, with one exception a
native process needs: `Startup.argument` arrives in the first argument
register -- RDI, x0 or r0 -- which is how stage 9's `process_start` hands a
driver its bootstrap handle. A Linux program's is zero. A starter claims the
start before it puts anything into the process (`process::claim_start`), so two
starts cannot both move a handle in or overwrite each other's argument; a
dropped claim gives the start back, and a process that has already ended cannot
be claimed. `exec::load_native` loads an image with nothing on its stack for
such a start. The boot test starts a two-instruction program that exits with
that register, through a claim:

      argument a program started with an argument found it on entry and exited with 57

Until 0510a8a every program leaked its task, its process and its address
space: `task_start` held the task's own reference across an entry that never
returns, and none of the checks that run programs counted frames, because an
unreaped task looks like a leak. Under busybox it showed as `free` rising by
about a megabyte for every process that exited, and a long session ending in
`Out of memory`. The `exits` line is the check that would have caught it: the
forking program runs four times to warm up, then four times in each of up to
four windows, and in one of them the free frame count must come back exactly
once the reaper has settled. One window is not enough, because what lives
across programs grows with how the processors happened to interleave; a leak
keeps frames in every window.

The program init starts is pid 1, as the first user process is on Linux:
ordinary numbering starts at 2, and each program of a command list takes 1 in
turn once the one before has let it go, so a shell as init reports `$$` as 1.
A process's children outlive it with a parent, handed on as Linux's
`forget_original_parent` hands them: to the nearest ancestor still running that
set `PR_SET_CHILD_SUBREAPER`, or else to init, each sent the signal it asked
for with `PR_SET_PDEATHSIG`. An orphan that had already ended is a zombie its
new parent's `wait4` takes, and one still running tells its new parent when it
ends. The boot check plays init with pid 1 and requires an ended and a running
orphan to reach it with their statuses, a reaping ancestor to take them first
and to pass them on once it has ended itself, and, as its control, the same
orphan to be left with no parent when there is no init.

**`futex` and `clone3`.** `futex` waits, wakes and requeues, plain and with a
bitset, keyed by address space and user address, with the word compared under
the table's lock so no wake is lost; the priority-inheritance operations and
`FUTEX_WAKE_OP` are `ENOSYS`. `clone3` reads its argument structure by size and
takes the same path as `clone`, which is what glibc tries first and falls back
from only on `ENOSYS`. A process that ends clears its `clear_child_tid` word and
wakes whoever waits on it. The boot test catches a wake that rouses nobody:

      futex    a changed word got EAGAIN and a timed wait ETIMEDOUT; a wake and a requeue roused 2 waiters, and a wake that roused nobody was caught

**What the checks cost.** Stage 7 prints a `cost` line, guest milliseconds
per group of checks, as stage 5 does. It found the boot's single most
expensive check the day it was added: the futex negative control let its
forgotten waiter sleep out a two-second timeout on every boot, proving
nothing a 200 ms one does not. The whole stage now costs about 450 ms under
`tcg`, of which the handler checks and the two spinning programs are most.

A process's descriptors close when it ends, as Linux's exit closes them, not
when its parent reaps it: otherwise a pipe's write end outlives the program
that wrote, and `ls | wc -l` waits for an end of file that only reaping brings.

**The calls busybox makes around the edges.** Measured across both busyboxes'
applets and answered as a single-user system without networking answers them:
`prctl` (name, death signal, dumpable, no-new-privileges, subreaper, bounding
set), the robust-list head, resource limits (`RLIMIT_NOFILE` is the descriptor
table's own), priorities and I/O priorities, `personality`, the scheduler's
affinity and policy queries, `nanosleep`
and `clock_nanosleep`, `times` and `getrusage`, setting the real-time clock,
`adjtimex` queries, host and domain names, `sysinfo`, `getcpu`, `syslog` over
an empty log, `reboot` powering off, and every socket call for a family that is
not `AF_UNIX` refused as Linux without that address family refuses it. Their checks run in the handler group
with every structure's buffer poisoned beyond its end. Still `ENOSYS`, each
said so at its arm: swap, modules, System V IPC (shared memory, message
queues and semaphores, each named in every architecture's table), `acct`,
`vhangup` and `rseq`.
**Credentials and file locks.** A process has real, effective, saved and
filesystem user and group ids and a supplementary group list. Fork copies
them; exec keeps them and makes the saved and filesystem ids the effective
ones, as `cap_bprm_creds_from_file` does, with `AT_SECURE` set when the
effective id is not the real one. The `set*id` calls, `setgroups` and `capget`
follow `kernel/sys.c` and `kernel/groups.c`, an effective uid of 0 standing in
for the capabilities, so busybox's `su` reaches a user. Nothing checks a
file's permissions against the ids yet; that is the VFS's. `flock` locks
belong to the open file description, so a forked command keeps its parent's.
`fcntl` record locks come in both kinds: classic ones, owned by the
descriptor table and released by any close of the file, and open file
description locks, which end with the description; ranges split and merge,
`F_GETLK` names the holder, and `F_SETLKW` waits until a signal, with no
deadlock detection. Every path that ends a descriptor goes through
`fd::closed`, which is how a close releases them, and busybox's `adduser` and
`passwd` lock `/etc/passwd` rather than warning. `readahead` checks what
Linux checks and answers 0, having no page cache to fill.

**`mremap`, `execveat`, and what `/proc/self/exe` says.** `mremap` shrinks in
place, grows in place when the pages after are free, and otherwise moves --
a private mapping's frames moved into a new object of the new length rather
than copied, keeping protection and copy-on-write -- and refuses as Linux does,
checked with a string across a page boundary surviving the move. A fixed
destination below 64 KiB is `EPERM`, as from `mmap`, but only after the
overlap (`EINVAL`) and unmapped-source (`EFAULT`) refusals Linux reaches
first. `execveat`
shares `execve`'s path, `AT_EMPTY_PATH` included. `unshare` answers what a
process without namespaces can honestly answer, and `setns` refuses. A program
is recorded as the absolute path of the file actually loaded, symlinks
resolved and a script's interpreter rather than the script, which is what
glibc's static start-up reads back through `/proc/self/exe`; `AT_EXECFN` is the
name `execve` was given. The shell init starts from its built-in
image, which has no file of its own, is named `/bin/busybox`, so the host's
static glibc busybox starts as init too.

**Signals are delivered.** `kill`, `tkill` and `tgkill` send; a child's end
sends its parent `SIGCHLD`; a write to a pipe with no reader raises `SIGPIPE`
and still returns `EPIPE`; `alarm` and `ITIMER_REAL` raise `SIGALRM`. Delivery
happens on every return to user mode, from a system call or a trap, into a
handler on Linux's own frame for each architecture -- floating-point state
included, `SA_ONSTACK`, `SA_NODEFER` and `SA_RESETHAND` honoured -- and
`rt_sigreturn` restores it with flags and processor mode sanitised, x86-64
returning through `IRETQ` because `SYSRET` cannot restore `rcx` and `r11`.
An interrupted blocking call is restarted when a handler with `SA_RESTART`
runs or when no handler runs, and is `EINTR` otherwise, exactly as Linux's
`arch_do_signal_or_restart` decides: reads, writes, `wait4`, pipe waits and
futex waits restart, `poll`, `select` and `pselect6` never do, and `nanosleep`
and `clock_nanosleep` resume through `restart_syscall` with the time left. A
default stop parks the process until `SIGCONT`, and a user-mode fault the fault
path cannot resolve becomes `SIGSEGV`, `SIGILL`, `SIGBUS`, `SIGFPE` or
`SIGTRAP` with Linux's codes rather than a kernel panic. The boot test runs a
handler that changes a register through its frame on each architecture, and a
`sigpaths` check drives the delivery decisions around it, each against a
negative control:

      signals  a program's handler ran on its own frame, changed a saved register, returned through sigreturn, and the program exited with 77
      sigpaths SIGCHLD reached a handler and wait4 still reaped; a stop and continue were reported; an alarm raised SIGALRM; SA_ONSTACK chose the alternate stack; a blocked fault was forced; SA_RESTART restarts, poll and a flagless handler do not

**The console is a terminal.** `fs/terminal.rs` holds the console's
`struct termios` and a line discipline that reads it for every byte --
canonical editing, echo, input and output mapping, `VMIN` and `VTIME` -- and
`syscall/tty.rs` answers the terminal requests, job control's included, so
busybox's `sh -i` gets a controlling terminal, turns job control on and does
its own line editing without a doubled echo. `select`, `pselect6` and
`pselect6_time64` answer from `poll`'s readiness and wait under the signal mask
they are given, as `ppoll` does. Ctrl-C, Ctrl-\ and Ctrl-Z raise their signals
on the foreground process group, and since nothing reads the console while a
shell waits for a foreground program, the first read starts a `console` thread
that drains the console every twenty milliseconds: the 4 KiB ring the PL011's,
the STM32 USART's and — through an I/O APIC input found from the MADT, since
2026-09-13 — the 16550's receive interrupts fill. A console read returns `EINTR`
when a signal is waiting. In the shell, `sleep 30` and `cat` are each ended by
Ctrl-C with status 130 and the prompt back at once.

**Unix-domain sockets, connected.** `fs/socket.rs` is a socket inode on a
sockfs of its own, built the way pipes are: each direction is
`libs/vfs`'s `SocketBuffer` -- one queue as values, telling a stream from
records -- behind a lock, with a wait queue each way, and no wait ever happens
with the buffer locked. `socket` and `socketpair` make stream,
sequenced-packet and datagram sockets; `read`, `write`, `send`, `recv`,
`sendmsg` and `recvmsg` carry bytes and whole records between a pair, with
`MSG_DONTWAIT`, `MSG_PEEK`, `MSG_TRUNC`, `MSG_WAITALL` and `MSG_NOSIGNAL`,
one copy in and one out however many buffers a message names. `shutdown` ends
one direction at a time -- what was queued is still read, and the peer's sends
break -- `poll` answers from the two queues, and `FIONREAD` and `SIOCOUTQ`
report what a read would find and what a peer has not taken. `SO_TYPE`,
`SO_DOMAIN`, `SO_PROTOCOL`, `SO_ERROR`, `SO_ACCEPTCONN`, the buffer sizes
(kept doubled, as Linux keeps them), `SO_PEERCRED` and the two timeouts read
back; `getsockname` and `getpeername` answer "unnamed", because a name is the
next landing and `SCM_RIGHTS` the one after, each `EOPNOTSUPP` until then.
The `unix` boot check drives a pair of each type through `dispatch`, and what
each call refuses stands beside what it answers -- the families and types
`AF_UNIX` is not, a call on the console, one on a closed descriptor, a peek
that took what it looked at, a record read that found the last record's tail:

      unix     a stream pair carried bytes across two writes and a peek left them; records kept their boundaries and MSG_TRUNC their lengths; shutdown ended one direction; a socket reported its type, buffers, credentials and unnamed address

**Left, and why it did not block the exit:**

* **What signals do not do yet.** There is no vDSO, so a handler needs
  `SA_RESTORER`, which musl and glibc always set; only `ITIMER_REAL` arms, not
  `ITIMER_VIRTUAL` or `ITIMER_PROF`. `SA_RESTART`, `SIGCHLD` to a handler with
  `wait4` still reaping, job-control stop and continue, `alarm`/`ITIMER_REAL`,
  the alternate stack and the fault-to-signal path are now driven by the
  `sigpaths` boot check; the fault-to-signal catch and an `SA_RESTART`
  interrupted read are proven at the kernel's decision, not yet end-to-end by a
  hand-assembled faulting or interrupted-read user program.
* **Threads**, which nothing single-threaded calls: `CLONE_VM` without
  `CLONE_VFORK`, and `CLONE_THREAD`, are `ENOSYS`. The ground is laid: every
  program's task runs a `Thread` of its process, and signal state is split as
  Linux splits it -- the dispositions, the signals sent to the process and
  `ITIMER_REAL` on the process; the blocked mask, the alternate stack, the
  signals sent to one thread and its restart state on each thread, which takes
  its own signals before its process's. Next: `exit` apart from `exit_group`,
  then `clone(CLONE_THREAD)`.
* **Three stand-ins, each written down where it lives.** The console is the one
  terminal, its line discipline fed by a thread that looks every twenty
  milliseconds rather than waiting on the receive interrupt, until stage 15
  brings ttys; the real-time clocks
  start at 1970 until something reads a clock chip, though `clock_settime`
  moves them; `getrandom` is xorshift seeded
  from a counter and says so.

---

## Stage 8 — VFS, initramfs, the pseudo-filesystems ✅

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
  close-on-exec belongs to the number. No spin lock of a description is held
  across a call into its inode, because stage 11's btrfs waits on the disk
  there; the offset is held across the read, write or listing it positions,
  as Linux's `f_pos_lock` is since 3.14, because it is a lock that sleeps
  (`ferrix_sync::SleepLock`, lent a wait queue by the kernel through the
  mount the file was opened on), so two reads racing on one description get
  consecutive bytes. A stream, `pread` and `pwrite` never take it.
* **tmpfs**, whose file contents are not a byte vector but a page store the
  kernel supplies — a VMO there — so that a shared `mmap` of a tmpfs file maps
  the file's own pages. The same store is the page cache of a filesystem on a
  disk: made over a `PageSource`, it fills runs of at most 32 missing pages
  with no lock held, keeps only those still missing, and forgets the source's
  bytes past a truncation. The host tests pin that, and the kernel's own VMO
  store fills the same way: frames allocated and zeroed before the source is
  called, a fill that fails or claims too much keeping nothing, and each page
  read under the VMO's lock so that a disk filesystem, which holds no lock
  across a read, cannot race a truncation into a freed frame. Directory
  cursors are never reused, so `rm -rf` reading a directory it is emptying
  sees every entry exactly once.
* **initramfs unpacking** through the same calls a program makes, hard links
  and device nodes included, and **the `getdents64` packer**, whose names start
  at byte 19 rather than at the structure's size of 24.

59 host tests, the `vfs_ops` fuzz target — which asserts that every name a
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
and removes it. Then it reads 41 pages of a store over a page source that
fills over nothing, and requires two fills, of 32 and 9. A second read must
not ask again, and a write into the middle of a page must fill the page's
other bytes first. A source that answers three pages at a time must still
fill every page asked for, and a source that fails or claims too much must
be `EIO` and keep nothing. Last, a cut keeps the bytes before it, reads
zeros past it, and never asks the source for what it cut:

```
  initrd   2 KiB unpacked: 6 directories, 2 files, 1 hard links, 1 symbolic links, 0 refused, verified true
  tmpfs    4 pages written through a VMO and read back, 52 filled from a page source in runs and cut, 0 frames leaked
```

**Done — the calls that take a path.** `kernel/src/syscall/path.rs` and
`stat.rs`: `mkdirat`, `mknodat` (regular files, pipes, socket names, and
character and block device nodes), `unlinkat`, `renameat2` with
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
  paths    170 path calls under /tmp, 42 names listed in 12 getdents64 calls, 4 device nodes opened by number, 0 frames leaked, dentry cache +0
```

**Done — descriptors, and the console as a file.** Every process has a
descriptor table and a root and working directory, each behind an `Arc` so
that `clone` can share them where `CLONE_FILES` and `CLONE_FS` ask and copy
them where they do not. A new process's descriptors 0, 1 and 2 are one open
description of `/dev/console`. `openat`, `close`, `read`, `write`, `readv`,
`writev`, `pread64`, `pwrite64`, `lseek` and `_llseek`, `dup`, `dup2`, `dup3`,
`fcntl` and `ftruncate` answer through it. `fcntl(F_GETFL)` reports `O_RDWR` on
the console, which is the one thing busybox's `printf` needed before it would
print, and stage 7's shell test has its `printf` line back. The `O_*` bits
x86-64 and the Arm architectures number differently are tables in
`libs/linux-abi`, chosen through the architecture facade. `ioctl` on the
console goes to `syscall/tty.rs`. It refused `TCGETS` while the console edited
and echoed every line itself, because `sh -i` would then switch to raw mode
and echo as well, doubling every character. Since stage 7 made the console a
terminal that honours `ICANON` and `ECHO` (bc8c64b, 6de8907), `TCGETS`,
`TCSETS`, `TIOCGWINSZ` and the job-control requests are answered. So are
`TCGETS2` and `TCSETS2`, which a newer glibc's `tcgetattr` asks instead:
refusing them made Ubuntu's static busybox decide it had no terminal, so its
`stty -a`, `tty`, `login` and `less` failed where Alpine's worked.

**Done — `/dev` and `/proc`.** devfs holds `null`, `zero`, `full`, `random`,
`urandom`, `tty` and `console`, numbered as Linux numbers them. It calls itself
`devtmpfs` in `/proc/mounts` and `/proc/filesystems`, the name init scripts and
service managers look for; Linux has had no `devfs` since 2.6.18. A device node
made anywhere else — by `mknod` on tmpfs, or unpacked from the initramfs —
opens as the devfs device with its number, and keeps its own inode for `stat`,
as `/dev/tty` does; a number devfs lacks is `ENXIO`. Disks are registered, not
built in: a driver's kernel side hands `devfs::register_block` a name, a number
and a `fs::block::BlockDevice`, and while the returned registration lives the
disk is a block node in `/dev` and a row in `/proc/partitions`, and
`devfs::block_device` finds it by number for a mount. Dropping the
registration takes all three away, and a device still held after that answers
every read with `EIO`. Opening a block node is `ENXIO` until reads through a
descriptor come, and its `stat` reports no size, as Linux's does. The block
check registers an in-memory disk, lists it in pieces while a second one
arrives, reads its sectors back by number, and checks each refusal and the
drop. The path check makes four such nodes by syscall number,
writes through the null one, reads zeros from the zero one, and checks the
refusals. procfs renders
every file at open, so a program reading `maps` in small pieces sees one
snapshot, and its directories opt out of the dentry cache, so a pid looked up
before its process existed is not remembered as missing. `/proc/self` links to
the caller's pid; each `/proc/<pid>` has `fd`, `status`, `comm`, `cmdline`,
`stat`, `maps`, `exe`, `cwd` and `root`; and `/proc` has `cpuinfo`, `meminfo`,
`mounts`, `stat`, `partitions`, `filesystems`, `uptime`, `version` and `sys`. The text is `libs/procfs`, pinned
byte for byte against lines taken from a real Linux `/proc`. `/proc/stat`'s
processor lines are each run queue's busy and idle time, read without charging
anything, so a line never goes backwards between reads; all busy time is
`user`, because the kernel keeps no split between a task's user and kernel
time. `/proc/uptime`'s idle field is the same count. With it, `top`, `mpstat`,
`iostat -c` and `nmeter` run. `/proc/sys` is a tree of the values the kernel
already keeps — `kernel.ostype`, `osrelease`, `version`, `hostname`,
`domainname` and `pid_max`, `fs.file-max` and `fs.nr_open` — in which the
host and domain names write through to what `uname` reports, and every other
value is refused at open with `EACCES`, as Linux refuses it. `/proc/partitions`
is empty, as Linux prints it with no block devices. With them, `pwdx`,
`sysctl` and `fdisk -l` run. Behind it, every
process now has a pid from a registry that finds a live process by it.

```
  devfs    7 nodes numbered as Linux numbers them; zero, null, full and urandom do what they are for; a disk registered as 254:250 listed, stat'ed, refused open and found by number, 2 sectors read, gone from /dev and /proc/partitions with its registration; 0 frames leaked
  procfs   36 names listed and walked back to, 4 maps lines parsed, 2 of them named; cwd and root read as getcwd; 8 /proc/sys values read, a host name written there reached uname; partitions empty with no block devices
  procstat /proc/stat read twice 50 ms apart: a cpu line for each of 4 processors, 21 ticks advanced, no counter went backwards
```

**Done — pipes, FIFOs, and the calls about filesystems.** `pipe` and `pipe2`
over `libs/vfs`'s pipe buffer, with a wait queue for each direction and each
end counted by its inode, so that a reader sees end of file and a writer
`EPIPE` exactly when the last descriptor that could feed or drain the pipe
closes. `O_NONBLOCK` reaches a stream with every read and write, because
`fcntl` can change it between them. A FIFO opens as an end of the one pipe its
node stands for, from a table keyed by device and inode number that `openat`
consults for a FIFO node. `statfs` and `fstatfs` answer in the word size's
layout, and ARMv7-A's packed `statfs64` takes musl's 88 as well as the kernel's
84. Then `sync`, `syncfs`, `fsync` and `fdatasync`; `truncate`, `truncate64`
and `fallocate`; `chroot`; `mount` of `tmpfs`, `proc` and `devtmpfs` — each a fresh instance, stacked over whatever the target showed — and `umount2`; the
extended-attribute calls, which report none; and `sendfile`, which busybox's
`cat` tries before it falls back to `read`. The boot check drives the handlers
from a process of its own, twice:

```
  pipes    18020 bytes through a pipe, a FIFO and sendfile; statfs, truncate and fallocate answered; proc and devtmpfs mounted, read and unmounted; 0 frames leaked
```

**Done — a file mapped shared.** `mmap` of a file maps the file's own VMO
pages, the ones `read` copies out of. So a write through the mapping is what
the next `read` returns, and a write to the file is what the mapping shows.
An inode offers what can be mapped through `Inode::mapping`: tmpfs its page
store's VMO, btrfs the same. `mmap` refuses in Linux's order: `EBADF` for a
descriptor that names nothing or only a path; `EACCES` for a file not open for
reading, or a writable shared mapping of one not open for writing; `ENODEV` for
an inode with nothing to map. `MAP_FIXED` clears its range only once every
check has passed. The mapping keeps its open file, so the file lives as long as
the mapping, as Linux's `vm_file`, and `/proc/<pid>/maps` names the region by
its path, offset, device and inode. A page wholly past the file's end is never
committed. The filesystem tells the store the file's length, after an extend
and before a cut, and a fault reads that bound under the VMO's lock. A touch
past the end is `SIGBUS` from user mode and `EFAULT` from a system call.
`msync` checks what Linux checks and writes nothing back, because the pages
are the file's and nothing here has a disk to flush. A private file mapping is
still `ENODEV`, until it can copy on write into an object of its own. The boot
check maps a file under `/tmp` from a process of its own, twice:

```
  mmap     12339 bytes written through a shared file mapping and read back from the file, and the other way; refusals, msync and /proc maps answered; a truncation took 3 pages away from the mapping; 0 frames leaked
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

After the exit programs, the same boot runs a group of applets, reported on a
line of its own and not part of the criterion: `pwdx`, `sysctl` reads with a
refused and an accepted write, `/proc/partitions` and `fdisk -l`, `top`,
`mpstat` and `iostat -c`, and `mknod` of a null, a zero and an unknown device.
They are the applets a sweep of busybox found failing, kept from failing again,
each judged on its output as well as its status.

**What a review found before the stage was called done.** Another session
read the VFS and its self-checks and found five bugs and one gap, three of the
bugs reproduced on the host, and the stage waited on all of it. A lookup that
lost a race with a create handed back a second, uncached dentry for a
directory, whose parent a later rename did not update, so the ancestry check
read a stale chain and a directory could be moved inside itself; a directory
now has one dentry, and tmpfs refuses the move under its own locks as well
(628d546). A racing `open(O_CREAT)` without `O_EXCL` failed with `EEXIST`
(e626316); `..` in a listing carried the directory's own inode number
(ed9f02b); a rename over an empty directory kept it alive (7ab4e3c); and
`openat` at the descriptor limit created the file before failing with
`EMFILE` (16ed9b7). Each fix came with a host test that failed before it. The
gap was that every self-check measured the frames it leaked and printed the
count without failing on it; each now fails on a non-zero count, and each
assertion was shown to fire by leaking a frame on purpose. `vfs_ops` gained
the property the worst bug broke: after every input, the namespace is still a
tree. After the stage was called done, the path check once failed under load
with frames it had not leaked: the programs the syscall check had just run
were reaped inside its measured window. It now waits for the reaper before its
first count, and fails on a count that rises (e8c98da). The pipe and
filesystem call check later failed the same way (FX-0860), so every frame
count in stages 6 to 9 now waits at both edges of its window until no exited
task is left unreaped, a condition rather than a delay, and prints the signed
difference when it fails.

**Left for later stages.**

* `/proc/loadavg`, which `top`'s load average line reads, and
  `/proc/diskstats`, which `iostat` needs past its processor report and which
  has nothing to count before stage 11's block core.

**Exit:** `busybox ls -R /proc`, `cat /proc/self/maps` and a shell script that
manipulates files under tmpfs, all under the boot test.

---

## Stage 9 — The native ABI: handles, channels, ports, VMOs ✅

Handle tables, `Channel` with handle passing, `Port` event queues, `Interrupt`
objects, `IoMapping`, and `Job`. The syscalls in the `0x1000` range. This is
what stage 10 is written against.

**Done — the ABI written down, and the table under it.** Host-tested, fuzzed
and under Miri, and reached from the kernel by everything below.

* `libs/native-abi` — the numbers, handle values, rights, signals, error names
  and `repr(C)` layouts. One number table on every architecture, in
  `0x1000..=0x1FFF`, held clear of all three Linux tables by a test rather than
  by a comment. No argument is wider than a register — anything that must be
  64 bits on ARMv7-A goes through a pointer — so no native call exists twice
  the way sixteen Linux calls do there. Failures are `errno`, each native
  failure a distinct one, so a musl program making a native call reads an `errno`
  it can name. Rights live on handles and only shrink, decided in one function.
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

* **Ports.** `port_create`, `port_queue` and `port_wait`, and
  `object_wait_async`, which asks for one packet on a port the next time a
  channel end becomes readable or its peer closes, or a job is killed — at
  once if that is already true. A registration is held by the object it
  watches, under the lock that object takes to change the state it reports,
  so a change cannot land between looking and registering; it fires once and
  is gone. User packets stop at 1024 and are told to wait; a signal packet is
  always queued, since refusing it would lose the event it was waiting for,
  and registrations are capped at 64 per object instead. `WRITABLE` cannot be
  waited for asynchronously: it depends on the peer's queue, whose lock an end
  may not take while holding its own. The queue sits under an interrupt-safe
  lock, so binding an interrupt to a port can queue from a handler. The check
  round-trips a user packet, fires a registration by a message, once and only
  once, fires one at once on a state already true and one on a closing peer,
  fires one on a job kill, refuses a full port, a `WRITABLE` registration, a
  port watched through a port and a registration without `WRITE`, and wakes a
  two-minute `port_wait` by a message a kernel thread writes twenty
  milliseconds later.

* **Interrupts reach ports.** `interrupt_bind` delivers an `Interrupt` to a
  port as packets carrying a key and the time it fired, one each time it goes
  from quiet to pending, so a device that fires twice before its driver
  acknowledges produces one packet. The handler queues it itself, under the
  port's interrupt-safe lock and without allocating, and takes the binding's
  lock before marking the interrupt pending, which serialises it against a
  bind racing on another processor: exactly one packet between them. The
  check, where a machine has a device vector, binds one, refuses a second
  binding, requires two deliveries before acknowledgement to queue one packet
  with its key, kind and time, and one more after acknowledging.
  A line is free the moment its holder's last handle closes, which is what a
  restarted driver needs. It was free only when the last reference to the
  object went, and under load that was later twice over: a delivery preempted
  on another processor still held the object, and a close queued it behind
  another processor's drain of disposed objects, so the check's immediate
  re-claim failed about once in three busy boots (`FX-0901`). A delivery now
  holds only the line's pending mark, binding and waiters, never the claim;
  finding a line and masking it, and giving one up and masking it, are single
  steps under the table's lock, so a stale delivery cannot mask a line its new
  holder unmasked; and a close drops the objects that contain no others where
  it stands. The check re-claims a line with a delivery for its old holder in
  flight, requires that delivery not to reach the new holder, and re-claims it
  again as though another processor were draining.

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
program spinning alone on its processor; `301aec4` fixed that.

**Done — an interrupt wakes its waiter at once.** The handler marks the
interrupt pending and queues its port's packet under interrupt-safe locks,
lets go of them, and wakes whoever waits on the interrupt and on the port.
Since `0cda8de` made a wait queue's lock interrupt-safe, `WaitQueue::wake_all`
may be called from a handler as it is: every lock it takes is taken with
interrupts masked, it allocates nothing, and it leaves the reschedule to the
way out of the interrupt or to an IPI. The device check makes sixteen
deliveries from another thread, eight waited on through the interrupt and
eight through its port, staggered across the five-millisecond recheck period,
and requires a majority of each kind to have ended their wait by waking it:
the wait queue counts a wait whose task a wake took off its list, which a task
the recheck timer woke is still on, however long its processor took to run
it. With the wakes taken out of the delivery, none does. It used to time the
rounds against 2 ms instead, and failed under a busy host or KVM, where a
halted processor can take that long to run again (`FX-0901`); the slowest
time is still printed.

**Done — a port hears that a process has ended.** A handle to a process
names its `Exit`: its status, the signal that ended it, the queue woken when it
ends, and the port registrations waiting for it. It never names the process,
so a handle kept past the end does not keep the address space or anything else
the process owned. `object_wait_async` on the handle queues a `TERMINATED`
packet once the process has ended and closed its handles and descriptors, at
once for one that already has, and the handle's own `TERMINATED` signal asserts
at the same point. So `devmgr` hears of a driver's death only after the
driver's pins have been given back or kept. The object check watches a process through
a port and directly, sees a channel the process held close before the packet
arrives, and sees the process freed while the handle is still open. A program
that dies of its own fault is heard the same way: the kill a user-mode fault
forces ran inside the trap with interrupts masked, where ending a process may
not run, until the reverse map's first boot check to end a child by `SIGSEGV`
stopped the kernel on it. The trap now opens interrupts while it forces the
signal, as the way back to user mode already did, and the object check runs a
program that writes through a null pointer on every architecture and requires
`128 + SIGSEGV`, the `TERMINATED` packet, and the process freed.

**Done — `vmo_map`, and what a DMA pin needs from a VMO.** A VMO maps into
the calling process a whole number of pages at a time, and shared: a write
through the mapping is a write to the VMO, a fork reaches the same pages
rather than copies, and the mapping keeps the object alive once the handle it
was made with is closed. A mapping always reads, writes only when asked and
when the handle carries `WRITE`, and never executes. The region is attached to
the object on the reverse map before it is recorded, so a VMO that takes a page
away forgets it in the mapping space before its frame goes back. `mremap` and
`mprotect` refuse a region `vmo_map` made -- the first would grow the VMO
through the Linux path, and the second, answering `EACCES`, would give a
mapping a right its handle did not carry -- and an object mapped twice counts
once in the resident set. The object check maps a VMO, reads and writes it
both ways, forks the space, closes the handle, and is refused twelve ways.

`Vmo::hold` keeps a page on the frame a device was given for as long as a pin
holds it. Decommitting skips the page, a copy-on-write replace is refused, and
a fork copies it instead of sharing it; a stage 6 self-check walks a held page
through each of those and through nested holds.

**Done — a process makes and starts another.** `process_create(job, image,
name, name_len)` reads an ELF image out of a VMO into kernel memory and loads
it with nothing on its stack. The child's code is therefore ordinary memory of
its own, and no VMO is ever mapped executable. The child is placed, not yet
running, in the job, and the caller gets a handle to it.

`process_start(process, bootstrap)` works in three steps, so that a race
between two starts, or a start and a kill, is harmless:
1. It claims the start, so a second start is refused before it has moved
   anything.
2. It moves the bootstrap handle into the child's table under both tables'
   locks.
3. It enters the program with that handle's value in its first argument
   register.

A process that has already started or ended is `BAD_STATE`, and the bootstrap
then stays with the caller.

The handle holds the process weakly. The one strong reference to a child
nobody started lives in what its handles share, and the start hands it over to
the task. Closing the last handle to an unstarted child kills it, so it ends
with a status and its watchers hear. That drop runs only inside
`object::dispose`, whose callers are all tasks with interrupts on, and it
asserts so.

The object check drives both calls through the native dispatch from a check
process. The child is a real program on every architecture that exits with its
first argument register, so its status is the bootstrap's value as the child
saw it. The check also covers the refusals; a child killed before its start; an
unstarted child's handle dropped with the channel message carrying it; and one
whose only handle is closed, which must be heard and freed.

**Left for later stages**, none of it on the exit criterion's path:

* **No native handle to a Linux mapping's object or a file's VMO until
  `mremap` makes other mappers forget moved pages under its own lock.** The
  VMO reverse map (stage 6, *An object knows who maps it*) tells a moved
  object's other mappers to forget its pages only after `mremap`'s lock is
  released. That is safe only while a private region's object has exactly one
  mapper, which an assertion at `Vmo::attach` and in `mremap`'s adopt path
  checks. `vmo_map` cannot break it: a VMO handle comes only from
  `vmo_create`, and native regions refuse `mremap`.
* **An `EXECUTE` right on a VMO handle**, with the native loader that is its
  first consumer. `vmo_map` refuses an executable mapping for now, by the
  product owner's decision of 2026-09-13; `process_create` reads a program's
  image out of its VMO into anonymous memory, so nothing needs one yet.
* Sub-page apertures, which need each access trapped. (An `Interrupt` on
  x86-64, masked in the device's own MSI-X table, came with stage 10's PCI
  vectors.)
* The calls that act on a process beyond making and starting it.
  `0x1032..=0x1037` is held for them.

**Exit:** two user processes exchange messages and a handle over a channel, and
a `Job` kill takes down a process tree.

---

## Stage 10 — Userspace drivers ✅

ACPI and device-tree enumeration in the kernel; IOMMU domains (VT-d, AMD-Vi,
SMMUv3); `devmgr`; the shared-ring block protocol; and the first driver —
virtio-blk — as a user process.

**Exit:** a boot test that reads a sector from a virtio disk through a driver
running in ring 3, with the IOMMU on and a deliberate out-of-domain DMA
attempt faulting — on x86-64, through VT-d, and on AArch64, through the
`SMMUv3`. The check itself clears VT-d's single fault record before it probes,
and requires every Arm domain to map the `GICv2m` doorbell, since QEMU sends
a device's MSI writes through the SMMU.

**ARMv7-A runs its drivers in degraded trusted mode.** U-Boot 2025.10 resets
when a virtio device offers `VIRTIO_F_ACCESS_PLATFORM`, so the 32-bit
machine's devices bypass its `SMMUv3`: a ring-3 driver there can DMA anywhere
in physical memory, the kernel prints `degraded trusted mode` on the console
at the first pin into an untranslated domain (`docs/ARCHITECTURE.md` §7), and
the exit's out-of-domain fault is checked on x86-64 and AArch64 only. A driver
reading sectors through an untranslated domain may land before then, so stage
11 can go on, but the stage is not done until both 64-bit machines translate
and fault.

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

**Done — what a review found.** A read-only review of stage 10 found ten
defects, and all ten are fixed. Two mattered most. A common Intel chipset would
have stopped the boot, because every function's vendor capabilities were read
as virtio's. And an aperture could be minted over RAM, which stopped being
theoretical the day `IoMapping` began mapping apertures for drivers.

* **Apertures are screened against everything the kernel owns:** the memory
  map (less firmware's own MMIO descriptions), every ECAM window, every
  controller window the kernel mapped for itself — `vmap` now records their
  physical addresses — the device tree's console, the framebuffer, and every
  other node's apertures. A function with memory decoding off contributes
  none. The publish check sorts every aperture and compares neighbours, which
  replaces a check that repeated the minting condition and so could not fail.
* **Device tree vectors** are shared peripheral interrupts only, never a line
  the kernel registered, never one another node holds.
* **virtio**: only a virtio device's vendor capabilities are read as virtio's;
  its blocks must lie in memory BARs, and every transport is verified. Freeing
  a virtqueue chain refuses a descriptor already free, since the device can
  move a link between validation and the free.
* **Hosts** whose segment and buses overlap one already accepted are refused;
  a device tree host without `linux,pci-domain` gets a segment of its own.
* **Mapping** refuses a physical range wider than the encoding's descriptors
  hold, which would otherwise have been masked onto low memory.
* **The entropy check** halts the boot only on a completion that cannot be
  right. A device that refuses, stalls or will not reset is reported and
  skipped — libvirt adds a virtio-rng to every guest, and a rate-limited
  backend is not the kernel's fault — and one that will not reset keeps bus
  mastering off and its pages out of the allocator. `xtask test-boot` fails a
  boot that reads no entropy, so on the machines it configures the regression
  is still caught.

One consequence, recorded rather than hidden: EDK2 on AArch64 leaves memory
decoding off for a function no firmware driver binds, so virtio-rng there now
publishes no aperture. A driver for such a device waits on the item below.

**Done — interrupts by message, on every architecture.** The boot check's
virtio-rng request now completes by MSI-X rather than by polling: table entry
0 is programmed with a vector the architecture allocated, the queue is told
to use it, and the used ring is read only once the vector has been delivered
to a kernel handler. That proves the whole message path — table, controller,
vector, dispatch — on each machine, and `xtask test-boot` fails a boot where
no completion arrived by interrupt.

* `arch::msi_allocate` hands out an interrupt number with the address and
  data a device writes to raise it: a local APIC vector from 0x40 to 0x7F on
  x86-64, below the legacy system call gate and the kernel's own vectors, and
  an SPI from the `GICv2m` frame on the Arm machines, found through the MADT
  or the device tree and sized from its `MSI_TYPER`. A `GICv2m` write is a
  pulse, so its SPIs are made edge-triggered; a level-sensitive line loses it.
* **One bug, and only the U-Boot machine could show it.** A GICv2 delivers a
  shared interrupt only to the CPU interfaces its `GICD_ITARGETSR` byte names,
  and QEMU resets that byte to zero on a multiprocessor GIC. The driver had
  never set one, because every interrupt the kernel used before was
  per-core. EDK2 routes what it touches, so AArch64 worked; U-Boot does not,
  so on ARMv7-A the device completed and the interrupt went nowhere. A shared
  interrupt with no target is now given the enabling core's own bit, read from
  the banked target bytes as Linux does.
* The check's vector and its handler are kept for the life of the machine:
  `irq` cannot take a handler back out.

The run recorded when it landed: one completion by MSI-X, 64 bytes, on x86-64
and AArch64 at four processors and on ARMv7-A at four and at two.

**Done — PCI vectors a driver can be given.** A PCI device node's vectors are
its MSI-X table entries. `DeviceNode::vector(i)` mints entry *i*'s the first
time it is asked — from `arch::msi_allocate`, programmed into the entry with
the entry masked — and hands out the same vector every time after. Minting
waits for the ask because a vector is spent for good and a machine has 64 to
give devices. The first mint on a function masks every entry and turns MSI-X
on; it does not turn bus mastering on, which belongs to whatever gives the
device DMA.

* **Masking says where.** A `Vector` carries whether it is a controller line or
  an MSI-X entry, and `Vector::mask` and `unmask` write the entry's own mask
  bit for the second — which the interrupt controller cannot reach — without a
  lock, since the interrupt handler calls them. Stage 9's `Interrupt` now
  masks and unmasks through them, agreed with that stage; its handler finds
  the object before masking, because only the object's vector knows where.
* **The check** mints one function's first entry after publishing — minting
  needs the node's place in the published list — and requires the same vector
  on a second ask, no handler already on its number, an entry that reads back
  masked and then unmasked and masked as told, and nothing past the table.
  Stage 9's interrupt check then claims that vector through `interrupt_create`,
  which on x86-64 it could not do before: the machine had no device vector.

The run recorded when it landed: one MSI-X table and one vector minted on
x86-64 and on ARMv7-A at four processors and at two, and "1 interrupt held
from delivery to acknowledgement" on x86-64 for the first time. AArch64 mints
none: its virtio-rng has memory decoding off, which the next item is for.

**Done — where each device's IOMMU is, and virtio's DMA sent through it.**
`kernel/src/iommu.rs` finds every IOMMU firmware describes and places every
PCI function behind one, before any unit is programmed:

* on x86-64, by the DMAR's endpoint scopes;
* on AArch64, by following the IORT's root complex one mapping to its `SMMUv3`;
* on ARMv7-A, by the `arm,smmu-v3` node a host's `iommu-map` names by phandle.

What it cannot follow — a scope through a bridge, a mapping to a node that is
not there — is counted as unresolved, never as bypassing, because a bypassing
function is one no domain will ever be built for. Every unit's register block
is withheld from apertures.

* `libs/fdt` reads SMMU nodes, phandles, and `iommu-map` with its mask. It
  masks the requester ID, then takes the first entry that matches, as Linux's
  `of_map_id` does. `libs/pci` gives a function's requester ID.
* `libs/paging` gains the two tables a domain is made of: VT-d's second level
  and an `SMMUv3`'s stage 2. Both are three levels over 39 bits of I/O
  address, with bits checked against QEMU's walkers, and they run through the
  same walk tests as the processors' tables. `Mapper::pages_only` holds a
  table to 4 KiB pages for a VT-d unit without superpages.
* **QEMU lets virtio bypass the IOMMU** unless the device is made with
  `iommu_platform=on`. So the test machines' virtio-rng now is, with
  `disable-legacy=on`. The entropy check accepts `VIRTIO_F_ACCESS_PLATFORM`,
  which such a device will not run without. Until a domain is switched on,
  the translation is the identity, and the check reads its 64 bytes as
  before.
* **Not on ARMv7-A.** U-Boot 2025.10's virtio-pci driver fails a heap
  assertion and resets when a device offers `VIRTIO_F_ACCESS_PLATFORM`, with
  or without an SMMU, while the loader is still on boot services. So the
  32-bit machine's virtio-rng bypasses its SMMU, and an out-of-domain fault
  there needs another way in.
* `xtask test-boot` fails a boot that places no function behind an IOMMU, or
  leaves one unresolved.

The run recorded when it landed:

* x86-64 places its 6 functions behind the VT-d unit at `0xfed90000`.
* AArch64 and ARMv7-A place their 2 behind the `SMMUv3` at `0x9050000`.
* Every function arrives as its requester ID.
* With the legacy interface off, AArch64's virtio-rng comes up with memory
  decoding on, so that node now publishes an aperture and mints a vector.

**Done — the domain a device's DMA goes through.** `iommu::Domain` is what a
driver pins its DMA pages into and takes device addresses back from.
`Domain::pin(frames, flags)` gives each page its device address — not
necessarily contiguous — and `Domain::unpin` takes them back, refusing a pin
another domain took. Every device node has one, made the first time it is
asked for. No unit is programmed yet, so every domain is untranslated: the
device address is the physical address, the first pin announces the degraded
trusted mode `docs/ARCHITECTURE.md` §7 requires, and an unpinned frame may be
freed only once its device is known to be quiet. The VT-d and `SMMUv3` domains
go in behind the same two calls.

* **The entropy check gives its device the addresses its domain returns**
  rather than physical ones, so it is already the harness the translated
  domains will be proven on.
* **The boot check** pins two frames through a device node's domain, and
  requires one domain per node, each frame's address, a count that returns to
  where it started, and a refusal of an empty pin and of a pin unpinned by the
  wrong domain.
* **Agreed with stage 9 and stage 11:** `VMO_PIN` (0x1025) pins a range of a
  VMO — held by stage 9's `Vmo::hold` — into a device's domain, owned by a
  `Pin` handle that unpins before it lets the frames go, and 0x1026 writes the
  pages' device addresses. The block ring, drafted by stage 11 and reviewed
  here, copies data through one pinned data VMO and rings doorbells as port
  packets.

The run recorded when it landed: 2 pages pinned and unpinned and 2 refusals, and
the entropy check's 64 bytes through its domain, on x86-64, AArch64 and ARMv7-A.

**Done — VT-d translating, and a function behind it given a translated
domain.** On x86-64 the kernel programs the VT-d unit the DMAR describes before
PCI enumeration, so from the first DMA on, a function behind it reaches only
what its domain maps and a function with no domain reaches nothing.
`kernel/src/iommu/vtd.rs` drives the unit in legacy mode, checked against
QEMU's `intel_iommu.c`: a root table per unit, a context table per bus, a
context entry per function naming a domain identifier and its second-level
tables, and register-based invalidation of the context cache and the IOTLB
after every change the unit may have cached. A unit firmware left translating,
or one that needs write-buffer flushing, is left alone and says why.

* **`iommu::domain_for(function)`** attaches a function to the unit its DMAR
  endpoint scope names, and gives an untranslated domain where no unit
  translates. `DeviceNode::domain` and the entropy check both use it, so the
  entropy device's rings and buffer are now reached through VT-d.
* **A translated domain maps each pinned page at its own physical address.**
  Nothing but what was pinned is mapped, which needs no I/O address allocator
  and refuses frames above 39 bits. A failed pin unmaps what it mapped; a
  domain dropped with nothing pinned detaches and gives back its tables.
* **`mm::map_io`, `unmap_io` and `translate_io`** build IOMMU tables beside
  the kernel's, generic over `libs/paging`'s encodings, a page at a time, so a
  unit is never asked to walk a block.
* **The domain check** now also requires a translated domain to resolve each
  pinned page to its frame and to fault it again once unpinned.

The run recorded when it landed: on x86-64, 1 VT-d unit translating, the
entropy check's 64 bytes and its MSI-X completion through a translated domain,
and 2 pages pinned and unpinned through a translated domain with 2 refusals.
AArch64 and ARMv7-A still say degraded trusted mode.

**Done — pinning VMO pages for a device.** `VMO_PIN` (0x1025) pins whole
pages of a VMO into a device's IOMMU domain, and `VMO_PIN_ADDRESSES` (0x1026)
says at which addresses the device reaches them: the DMA grant
`docs/ARCHITECTURE.md` §7 gives a driver. Stage 9's `Vmo::hold` keeps the pages
where they are, and the pin handle — which carries `READ` and nothing else, so
it stays with the driver that made it — owns the hold and the domain's record
of the pages together.

* **Given back in order.** Closing a pin unpins it from the domain and makes
  the unit forget the pages before the holds are released. On an untranslated
  domain the device can still reach the frames, and nothing resets a device
  yet, so they stay held and the console says so the first time.
* **The rules.** The device handle needs `MANAGE`; the VMO needs `READ`, and
  `WRITE` unless the pin is `PIN_READ_ONLY`. A range off a page boundary,
  empty, past the VMO's end or with an unknown option is `INVALID_ARGS`; a
  page already pinned into a translated domain is `ALREADY_BOUND`.
* **The check,** among stage 9's device objects, pins two pages for a PCI
  function, and requires every refusal, the second page's device address to
  lead to the frame holding what was written through the VMO, and — on a
  translated domain — both pages unreachable once the pin is closed.

The run recorded when it landed: 2 VMO pages pinned and found at their device
addresses on every machine, through a translated domain on x86-64.

**Done — the `SMMUv3` translating under ACPI, and a function behind it given a
translated domain.** On AArch64 the kernel programs every `SMMUv3` the IORT
describes before PCI enumeration, so a function an IORT root complex sends to
one reaches only what its stage-2 domain maps, and one with no domain reaches
nothing. `kernel/src/iommu/smmuv3.rs` drives the unit, checked against QEMU's
`smmuv3.c` and `smmuv3-internal.h`: a linear stream table of 256 entries, every
entry valid and aborting until a domain is attached; stage 2 per attached
stream, with its own VMID, a 39-bit walk from level 1 over 4 KiB pages, 40
bits of output and faults recorded; the command queue for `CFGI_STE`,
`TLBI_S12_VMALL` and `SYNC`, polled; and the event queue on.

* **Every domain maps the MSI doorbell.** QEMU sends a device's MSI writes
  through its stream, where VT-d exempts them, so each `SMMUv3` domain maps
  the `GICv2m` frame's page at its own address, from `arch::msi_doorbell`.
  The entropy check's MSI-X completion now arrives through it.
* **One enum for both units.** A translated domain's map, unmap, flush,
  resolve and detach go through `Translation`, so `Domain::pin`, `unpin` and
  the domain check are the same code for VT-d and the `SMMUv3`.
* **One bug, and QEMU found it.** A stream table entry is sixteen 32-bit
  words; the first version wrote the stage-2 walk as if they were 64-bit, and
  QEMU stopped the machine the moment it read an entry whose `S2AA64` was
  zero.
* **Not on ARMv7-A.** Its device-tree SMMU is left alone: U-Boot keeps the
  32-bit machine's virtio devices from offering the platform's translation,
  so they would bypass it anyway — the exit criterion's degraded trusted mode.

The run recorded when it landed: on AArch64, 1 `SMMUv3` translating, the
entropy check's 64 bytes and its MSI-X completion through a translated domain,
2 pages pinned and unpinned through a translated domain, and 17 refusals in the
pin check, the refusal of a second pin into a translated domain among them.
x86-64 and ARMv7-A are as before.

**Done — a deliberate out-of-domain write, faulted on both 64-bit machines.**
The boot check's entropy device, once its request has completed through a
translated domain, is asked to write into a page its domain does not map, and
its unit must record the fault: VT-d's fault recording register on x86-64,
cleared before the probe because QEMU keeps one record and drops a second fault
from the same device, and the `SMMUv3`'s event queue on AArch64. The record
must name the device's own stream, the probe's page and a write. The answer is
the unit's record, not the device's completion: QEMU's virtio device completes
the request anyway, through a bounce buffer whose write-back the unit refuses
a second time (the item below, where this was learned), so a completion stops
the boot only when no fault is recorded for the probe's page by the deadline
— DMA its domain should have prevented. `xtask test-boot` requires the fault
on x86-64 and AArch64 and does not ask ARMv7-A, as the exit criterion says.

The run recorded when it landed: 1 out-of-domain write faulted on x86-64 and on
AArch64; ARMv7-A's untranslated domain was not probed.

**Done — both units' waits made with interrupts on.** A VT-d invalidation and
an `SMMUv3` command are waited for under a gate, `kernel/src/iommu/gate.rs`,
instead of inside `IrqSpinLock`s that masked interrupts for up to 100 ms on the
path every unpin takes. A task waiting to enter a domain's pins and unpins, or
a unit's commands, sleeps on a wait queue; the one inside polls the unit with
interrupts on and gives up its processor between looks. Only the few writes
that queue an `SMMUv3` command still mask them. Before the scheduler runs, or
in a context holding a spin lock, the gate and the wait spin as the locks did,
within the same deadlines, and an unpin that runs out of patience keeps its
frames rather than freeing what the unit may still reach. The domain check
requires a translated domain's unpin to have waited with interrupts on.

The run recorded when it landed: 10 waits on a unit with interrupts on by the
end of the domain check on x86-64, under KVM, and on AArch64; none on ARMv7-A,
whose domain is untranslated.

**Done — the block ring's protocol, as a library, host-side.** `libs/blkring`
(`ferrix-blkring`) is the ring the kernel and a ring-3 block driver will share,
as `docs/BLOCK-RING.md` specifies it: the ring and data VMO layout, every index
and entry the other side writes checked before it is used, doorbells over stage
9 ports, and a HELLO carrying the disk's PCI location, its virtio serial and the
name `devmgr` chose, numbered as Linux numbers `vda`, `vdb`. When a driver ends,
every outstanding request fails at once, and the data VMO stays held until
`devmgr` confirms the device was reset (BLOCK-RING.md §6.3); no path in the
crate reports it releasable before that. It is pure logic, tested on the host,
under Miri, and by a fuzz target that plays one side of the ring against an
honest other. **No kernel crate or process uses it yet:** the ring's kernel side
and the driver process are still to do, below.

**Done — the probe's answer is the unit's record, never the completion.** Once
in a few AArch64 boots the probe halted the machine with FX-1001, "the device
wrote into a page its domain does not map". It had not. QEMU's DMA map of an
address the IOMMU refuses does not fail: `address_space_map` hands the device a
bounce buffer, the device fills it and completes the request with the length it
was given, and the write-back at unmap is refused a second time and dropped —
read in QEMU 9.2's `system/physmem.c`, not remembered. So the unit records the
fault first and the device pushes a completion an instant later, every time, on
VT-d and on the `SMMUv3` alike. The check read the event queue, then the used
ring, and lost whenever the push landed between the two reads. A completion is
now held until the deadline and fails the boot only if no fault for the probe
page has been recorded by then; after the fault the probe waits a moment for
the completion, and the boot log says whether the device completed the faulted
write, whether the completion was seen before the fault, and how many further
faults were recorded for it — the dropped write-back is one — so every boot
shows the mechanism rather than the one that lost the race.

The run recorded when it landed: nine AArch64 boots in a row, each faulting
the write and then completing it, 64 bytes never delivered, with 16 further
faults for the write-back refused in 4-byte pieces; three x86-64 boots and one
under KVM, the completion after the fault and no further fault, since VT-d
keeps one record; ARMv7-A unprobed at four processors and at two.

**Done — the block ring's kernel side, up to a published disk.**
`kernel/src/block_ring` is the glue `docs/BLOCK-RING.md` §8 leaves to the
kernel. A process holding a device with `MANAGE` asks for a ring with
`block_ring_create` (0x1048) and is answered the driver's end of the ring's
control channel; the kernel's end goes to a task of its own per ring, which
waits for HELLO, checks it in §6.2's order — the crate's checks, then that
`location` is the ring's own device, then the registry — and refuses or takes
the ring up: it holds the ring and data VMOs, attaches the crate's
`KernelSide` over the ring's pages, publishes the disk through stage 8's devfs
registry under HELLO's name with the virtio-blk major and `index × 16`, and
answers READY with its completion port. From then on it serves reads:
`libs/block`'s queue in front of the ring, one submission per dispatch into a
region of the data VMO the kernel allocates, the driver rung when it asked to
be, completions taken off the ring and copied out once, readers woken. A read
on a ring whose driver has gone answers `EIO` at once; the ring ends on
STOPPED, on the channel closing or on corruption, and its registration goes
with it. The kernel never serves a disk: it issues requests and copies
payloads, and whatever answers is the process at the other end of the ring.

* **One rule the first use found unwritable.** §2 said the HELLO handles carry
  exactly `READ | WRITE | MAP`, with no `TRANSFER`; but `channel_write` takes
  only a handle that carries `TRANSFER` and delivers it with its rights, so no
  HELLO could ever have passed. The rule now says what arrives: `TRANSFER` on
  every handle a process sends, `DUPLICATE` on none, and the completion port
  the kernel places itself with `WRITE` alone.
* **The check** drives the control plane from a process the way a driver
  will, every call through the native dispatcher: a ring refused without
  `MANAGE`, on a VMO and on a device that has one; a HELLO of another version,
  one with unreduced handles and one for another location each refused with
  its reason and the kernel's end closed; a HELLO as specified answered READY
  with a `WRITE`-only port, `vda` at 254:0 in the registry with the geometry
  HELLO gave, and gone once the driver says STOPPED, after which the next
  round's ring finds the device free; all of it twice, the second round
  giving every frame back; then once more with the driver closing the channel
  instead,
  which takes the disk but leaves the device bound, since nothing reset it.
  No request is put on the ring: a sector read through it is the ring-3
  driver's check, below.

The run recorded when it landed: 13 calls and HELLOs refused as specified, 2
disks published and unpublished, 0 frames leaked, in about 50 ms, on x86-64,
AArch64 and ARMv7-A at four processors and at two.

**Done — virtio-blk's protocol and driver logic, as libraries, host-side.**
`libs/virtio`'s `blk` module is the device protocol: features checked against
Linux's header, the configuration, request headers and statuses, and a request
split into descriptor chains one pinned page at a time. `libs/virtio-blk` is the
driver's logic: bring-up to `DRIVER_OK`, read, write and flush, each completion
counted exactly once even from a hostile device, and a teardown that hands
memory back only after the device's reset has finished. Device addresses reach
it only through `DevicePages`: the addresses the pin query (0x1026) returned,
page by page of the pinned range, with no relation to physical addresses
assumed. It is tested on the host, under Miri, and by the `virtio_blk` fuzz
target. **No kernel crate or process uses it yet:** the driver process on
`ferrix-rt` is still to do, below.

**Done — the kernel's half of `devmgr`: what a driver is started with, and
what ends it.** `docs/ARCHITECTURE.md` §7 has `devmgr` hand a driver its device
and its channel to the subsystem it serves; a ring-3 driver cannot walk
configuration space, so whoever starts it must say where its device's
registers are. Enumeration now keeps, on the device node, what it read and
threw away before: the PCI identity, the virtio transport's register blocks as
physical memory — the page-aligned pages holding each, the block's offset in
them, its length, the form `io_mapping_create` takes — the MSI-X table size
and the function's configuration space address. From that, `device_info`
(0x1049) writes a `DeviceInfo` for any device handle, `block_ring::start_for`
builds the START message `docs/BLOCK-RING.md` §6.4 now specifies — type 6, the
first message on a driver's bootstrap channel, carrying the blocks, the
location, the name, the device with `MANAGE` and the driver's end of the ring's
control channel — and `ferrix-blkring` encodes, decodes and checks it. Two more
things nothing did before:

* **Bus mastering goes on at a device's first `VMO_PIN`**, the moment a driver
  gives it memory, and memory decoding with it; until then a device nobody
  drives stays quiet. No kernel code had ever enabled it for a driver, so a
  ring-3 driver's DMA would have gone nowhere.
* **`device_quiesce` (0x104A)** is the reset before release §6.3 gives
  `devmgr`: with `MANAGE`, once the driver is gone, it turns bus mastering off
  and releases the device's ring claim, so the next driver may have it;
  `BAD_STATE` while a driver still serves the device through a ring. A ring
  now records its device as served before it answers READY, since the driver
  may act on READY, and `devmgr` ask after the device, the instant it is sent.

The ring check covers it: `device_info` compared field by field with the
kernel's own START for the node, every virtio block required to lie inside one
of the node's apertures; a quiesce refused without `MANAGE`, refused under a
serving driver, and, after a driver dies, freeing the device for a new ring.
The P1 row for reset on driver death closes here: §6.3 is the design and this
its kernel side. `devmgr` itself, the program, waits on the native runtime and
comes next.

The run recorded when it landed: 19 calls and HELLOs refused as specified, 2
disks published and unpublished, 0 frames leaked, on x86-64, AArch64 and
ARMv7-A at four processors and at two.

**Done — the exit: a sector read through a driver in ring 3, with the IOMMU
on.** `/sbin/blk`, stage 11's virtio-blk driver on the native runtime
(`user/blk`, over `libs/virtio-blk` and `libs/blkserve`), is started from the
boot check by a kernel-driven parent with the START `devmgr` will send
(`docs/BLOCK-RING.md` §6.4, from `block_ring::start_for`): the device with
`MANAGE`, and the driver's end of the ring's control channel. It maps the
transport's blocks through `IoMapping`s, claims its MSI-X entry through an
`Interrupt`, pins its data VMO into the device's domain — which is where bus
mastering goes on — brings the device to `DRIVER_OK`, sends HELLO, and serves;
the kernel reads sectors through the registered disk, `vda`, and compares them
with what `xtask` wrote into the test disk. On x86-64 that DMA goes through
VT-d translating and on AArch64 through the `SMMUv3`, and the deliberate
out-of-domain write faulted on both earlier in the same boot; on ARMv7-A the
driver runs in degraded trusted mode, as the exit criterion decided. The
disk is `virtio-blk-pci` on every machine, because under ACPI QEMU describes
its virtio-mmio devices only in AML. The marker moves to `FERRIX-BOOT-OK
stages 1-10` when it landed; what the stage still owes is below, after the exit, each with a
row in `docs/BACKLOG.md`.

The run recorded when it landed: `/sbin/blk serves vda (131072 sectors)
through the block ring; 21 sectors read back through the registry as xtask
wrote them`, on x86-64 through VT-d, on AArch64 through the `SMMUv3`, and on
ARMv7-A in degraded trusted mode, at four processors and at two.

**Still to do, after the exit.** Stage 11 has its driver; what is left of
stage 10 is what makes that a system rather than a check.

* **`devmgr`, the program** (`docs/DEVMGR.md`), on the native runtime: given
  every device node and every driver image at boot, it finds the virtio-blk
  function, makes its ring, starts the driver with START through
  `process_create` and `process_start` in a job of its own, watches it through
  a process observer, and on its death quiesces the device before anything of
  it is reused. Until it lands, the boot check's parent starts the driver; its
  kernel half is above. Two things go with it:
  * *The quiesce after a death.* `TERMINATED` fires when a driver's handles
    close, which queues the closed channel for the ring's task but does not
    wait for it, so a quiesce the instant after can find the device still
    served and be refused. The fix — a quiesce on a served device whose
    driver's end has closed waits, bounded, for the ring to let go, and only a
    driver still holding its end is refused — is written and verified on a
    branch and lands with `devmgr`, its first caller.
  * *No driver faults on the disk it serves*, which under `docs/DEVMGR.md` §5
    holds by construction: a driver's image is an anonymous VMO the kernel
    filled from the initramfs, and a native program maps VMOs, never files.
    The pivot onto btrfs must keep it so.
* **Trusting a BAR firmware placed but did not enable**, so a device no
  firmware driver used — as virtio-rng on AArch64 was until its legacy
  interface was turned off — can still be given to a ring-3 driver. Worked
  out with the review that found the gap:
  * *Where the windows are.* The device tree's `ranges` on the Arm machines.
    Under ACPI they are in `_CRS`, which is AML — but before
    `ExitBootServices` the loader can ask each root bridge's
    `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL.Configuration()`, which returns the same
    windows as ACPI address-space descriptors, and carry them in `BootInfo`
    beside the MCFG. The descriptor layout is to be checked against the UEFI
    specification and EDK2's `PciHostBridgeDxe`, not remembered.
  * *Bus address, not CPU address.* A BAR holds a PCI bus address; match it
    in bus space and build the aperture from the translated CPU address.
    QEMU's `virt` translates by zero, which hides a missing translation.
  * *The window's kind.* A 32-bit memory BAR only in a 32-bit memory window,
    never a memory BAR in an I/O one; a prefetchable BAR may use a
    non-prefetchable window, not the reverse.
  * *The whole BAR*, `[base, base + size)`, inside one window, and inside the
    forwarding window of every bridge upstream of it, each of them decoding.
  * *Order.* Decoding-on BARs are admitted to the overlap set first, so a
    stale decoding-off assignment cannot block a live device.
  * *When decoding goes on:* at `IoMapping` creation, not at enumeration, so
    a device nobody drives stays quiet. The boot check's entropy read turns it
    on today, and until this lands it refuses a register block that overlaps
    memory the kernel owns rather than vetting it against the windows.
  * *Unassigned BARs* — zero, or a reset value — fall outside every window,
    and are reported as unassigned rather than as outside one.
  * Later, on the device tree machines: assign addresses to decoding-off
    functions inside the windows, as Linux does unless `linux,pci-probe-only`
    is set, rather than depend on firmware's choice.

---

## Stage 11 — Block core and btrfs, read ✅

Request queues, merging, the I/O scheduler. Then btrfs stage A: superblock,
chunk tree, root tree, fs trees, extents inline and regular, crc32c, and
zstd/zlib/lzo.

The item parsing is `libs/` code — pure functions over bytes, fuzzed against
images `mkfs.btrfs` produced.

**Exit:** Ferrix mounts an image made by real `mkfs.btrfs`, and reads a file
tree out of it that byte-for-byte matches what the host wrote.

**Done — the read path, host-side, against real images.** Started while
stages 8 to 10 are under way, because everything short of the kernel mount is
logic `cargo test`, Miri and a fuzzer can reach.

* `libs/btrfs` — the read path, allocating nothing and forbidding `unsafe`.
  `volume.rs` mounts: superblock, system chunk array, chunk tree, root tree, and
  the default subvolume's fs tree — the one the root tree's `default` entry
  names, as Linux's `get_default_subvol_objectid` finds it, or the top-level
  tree when there is no entry. It holds one node buffer rather than a path,
  re-descending from the root to reach the next leaf, and checks every node
  against the level, generation and fsid its parent promised. `fs.rs` answers
  what a VFS asks: stat data, lookup by name hash, `readdir` from a resumable
  `DIR_INDEX` cursor, and `read`, which zero-fills and copies extents over the
  top so every kind of hole reads the same way. `compress/` holds zlib, LZO and
  zstd decoders, each written for btrfs's framing of its format.
* **Real images.** `scripts/gen-btrfs-fixtures.py` builds four images with real
  `mkfs.btrfs` — uncompressed, zlib, LZO and zstd, with 4 KiB nodes so the fs
  tree is deeper than a leaf — packed to their non-zero blocks, beside a
  manifest of every path's size and CRC-32C. All four read back exactly. A
  fifth, small image is made with `mkfs.btrfs -u default:sub`, so its default
  subvolume is not the top-level tree; it mounts the subvolume, and its
  top-level file is not visible. Each decoder is also checked against an
  independent implementation: `miniz_oxide`, `lzokay-native` and `ruzstd`.
* **Data checksums.** Every data sector a read takes from disk is checked
  against the checksum tree before its bytes are used: an uncompressed extent
  in whole sectors, a compressed one on its on-disk bytes before the decoder
  sees them. A sector the tree has no checksum for fails, as on Linux, where
  `btrfs_lookup_bio_sums` expects zeros for a checksum hole and
  `btrfs_data_csum_ok` fails the read. `NODATASUM` files are read unchecked, and
  inline extents are covered by their node's checksum. Checksum items are held
  to `check_csum_item`. A damaged sector reads as an error, never as bytes.
* **`INODE_EXTREF`** records parse, held to `check_inode_extref`, and their key
  hash matches the offsets `mkfs.btrfs` filed real extrefs under. Nothing reads
  back-references yet: lookups and listings use directory entries.
* **A bounded cache of metadata reads.** Every lookup descends a tree from its
  root, and each descent used to read every node on the way from the device
  again. `ferrix-btrfs`'s `Device` now says what each read is for —
  `ReadKind::Metadata` for the superblock and tree nodes, `ReadKind::Data` for
  an extent's bytes — and `libs/btrfs-vfs` keeps metadata reads in a CLOCK cache
  of 1024 entries every handle of a mount shares: a hit hands out a shared
  reference and copies with no lock held, and a miss reads with no lock held. A
  cached node is trusted no more than a read one, since every node is still
  checked against its parent pointer and its checksum. File data is never kept
  there; it belongs in the page cache. A second walk to a file reads no metadata
  from the device, and a cache of two entries still reads every file back.
* `libs/btrfs-vfs` — the mount: stage 8's `FileSystem` and `Inode` over the
  read path, read-only, holding no lock across I/O. Tested through the trait,
  and through `Namespace` at `/mnt` on a tmpfs root.
* `libs/block` — the block core's queue: merging, flush and FUA barriers that
  no request crosses, and deadline scheduling, checked against a model by the
  tests and the `block_queue` fuzz target.
* The `btrfs_read` fuzz target starts each run from a real image and applies
  the input as edits, re-checksumming what it edited, so a hostile image that
  checksums correctly reaches the walker and the extent arithmetic.
* **What Linux's tree-checker refuses, refused here too.** A review against
  it found consistency checks the parsers lacked. The parsers now mirror
  `check_leaf` (payloads packed back to back), `check_dir_item` (names, types
  and the name hash), `check_extent_data_item` (extents inside their disk
  extent, none overlapping the last), `btrfs_check_chunk_valid`, and the size
  and root checks of `btrfs_validate_super`. Chunks that overlap are refused,
  and so is a directory name a VFS could not hand to a program — empty, `.`,
  `..`, or containing `/` or NUL. All four images and three more `mkfs.btrfs`
  images from the review still read back.
* **A lock a walk may sleep under.** The namespace's rename lock is held
  across a rename's two path walks, and a walk into btrfs waits for a disk;
  it was a spin lock, so the first such wait would have stalled every CPU
  queued for it. It is now `ferrix_sync::SleepLock`, a lock whose waiters
  sleep on a `Parking` the kernel lends — a `sched::WaitQueue` per lock,
  through `sync::SchedParker` — and which spins on the host. The open file's
  offset is the same kind of lock, held across the read, write or directory
  listing it positions, so two reads racing on one description get
  consecutive bytes, as under Linux's `f_pos_lock`; a stream, `pread` and
  `pwrite` never take it. An uncontended release never touches the wait
  queue, since the offset is taken on every read.

* **The kernel mount.** `mount -t btrfs /dev/vda /mnt` names a block node;
  the kernel takes its number to devfs's registry, wraps the `BlockDevice` it
  finds as the volume's `Device` (whole sectors on one side, byte offsets on
  the other, a bounce buffer only for a read that is not sector-aligned), and
  mounts read-only: a mount without `MS_RDONLY` is `EROFS`, so nothing is told
  it has a writable btrfs before stage 12. The mount keeps one inode object per
  inode, found by number, so two names for a file share one page cache; a
  regular file's data lives in the pages the kernel's VMO storage lends it,
  filled from the volume in runs by a `PageSource` that reads with no lock
  held, zero-fills past the end, and on a damaged sector keeps the pages before
  it and answers `EIO` for the bad one alone. `/proc/filesystems` lists
  `btrfs` as the one type needing a device. Tested through the trait and the
  namespace on the host with heap pages; the same code runs over the kernel's.

**Done — the exit, on all three architectures.** `xtask` unpacks the `none`
fixture — the image `scripts/gen-btrfs-fixtures.py` made with real
`mkfs.btrfs` — into a raw disk and attaches it as a second `virtio-blk-pci`
after the pattern disk. The boot check starts a driver for every virtio-blk
function, so `/sbin/blk` serves the fixture as `vdb`; stage 11's check then
mounts it read-only at `/mnt` through the kernel's own mount path, exactly as
`mount -t btrfs -o ro /dev/vdb /mnt` would, and walks the fixture's manifest:
every file read whole through the VFS and the inode's VMO pages, its size and
CRC-32C compared with what the host computed from the bytes it gave
`mkfs.btrfs`; every directory found to be one; the link's target read and
compared the same way. The mount stays, and `test-vfs` reads a file of it
through busybox. The run recorded when it landed, on x86-64, AArch64 and
ARMv7-A at four processors and at two: `vdb mounted read-only at /mnt: 101
files (1270061 bytes), 17 directories and 1 links read back as the host wrote
them`, through the ring-3 driver, the block ring, the registry, the volume
reader with its checksums and the page source. What the check compares is a
checksum per file rather than every byte on the wire, and the checksum is the
manifest's; a byte wrong anywhere in the stack is a CRC that differs.

**Still to do, after the exit.**

* A mapping of a file on the mount seeing the pages `read` fills, once file
  `mmap` lands: the shared-page claim is about btrfs files, and the check
  joins this stage's when the mapping does.
* Device numbers are passed through as btrfs stores them, not yet checked
  against how Linux reports them.
* A file's hole is tested only by construction: `mkfs.btrfs --rootdir` writes a
  sparse file's gap out as data.
* Entering subvolumes, and the `subvol=`/`subvolid=` mount options: btrfs stage
  C.

---

## Networking — sockets, a net core, virtio-net  ·  *month*

Placed after stage 11 without a number of its own, the way *ARMv7-A* sits
after stage 4. The net core was named in `docs/ARCHITECTURE.md` and left
unstaged, because nothing on the path to `rustc` needs it. Then a sweep of
busybox's applets over stage 8's root showed how much of a real userland does:
`ifconfig`, `route`, `netstat`, `ip`, `wget`, and everything that talks over a
local socket, fail today, because stage 7 refuses every socket call.

The net core sits beside the block core. It provides sockets in the Linux
ABI: `AF_UNIX` stream and datagram with descriptor passing, `AF_INET` and
`AF_INET6` TCP and UDP, and the `AF_NETLINK` route family that `ip` configures
interfaces through. Those run over interfaces, routes and a loopback device.
virtio-net is the first driver. It runs in user mode on stage 10's device
objects and speaks to the core over a channel, with its buffers in VMOs, as
virtio-blk speaks to the block core. `/proc/net` (`dev`, `route`, `tcp`, `udp`,
`unix`) comes with it, rendered in `libs/procfs` like the rest of `/proc`.

The byte-level halves are `libs/` code, host-tested and fuzzed before the
kernel calls them, for the reason the continuous rule gives: a packet is bytes
someone else chose. They are header parsing, the TCP state machine with its
retransmission and congestion arithmetic, and netlink message encoding.

**Exit:** under QEMU's user-mode network, busybox configures `eth0` with `ip`,
and `route` and `netstat` report through `/proc/net`. `wget` fetches a file
from a server on the host that byte-for-byte matches what it served, and `nc`
carries a stream over loopback and over an `AF_UNIX` socket. All of it runs in
a test of its own, for the reason stage 7's exit is one.

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
| `libs/acpi` | 3, 10 — RSDP, XSDT/RSDT, MADT, FADT fixed fields, GTDT, HPET, MCFG, GIC MSI frames, DMAR, IORT, and the HPET block's capability register with the arithmetic a 32-bit counter needs. No AML, and there will be none. | 77 |
| `libs/fdt` | Reached at 1 on ARMv7-A — the console, the GIC, the timer's interrupt and the PSCI conduit come from it there, and nothing else describes that machine. Reached at 10 for PCI host bridges `virtio,mmio` devices and `GICv2m` frames; stage 10 is still the rest of it. | 70 |
| `libs/sync` | Reached at 4 — `SpinLock` and `IrqSpinLock` guard every shared kernel structure and carry the contended counter; `RwSpinLock` is still waiting. Fair by construction, because an unfair lock on a starved core is a stage-14 latency bug nobody will find. | 19 |
| `libs/vma` | 6 — already backs the vmap arena. The VMA interval tree and the three calls that reshape it (`mmap MAP_FIXED`, `munmap`, `mprotect`). | 60 |
| `libs/linux-abi` | 7 — syscall numbers, `errno`, `repr(C)` layouts, and which identification register fields grant each Arm `AT_HWCAP` bit. Constants and pure functions of them. Three number tables, one of them 32-bit. | 78 |
| `libs/ustack` | 7 — the initial process stack `execve` hands a program: argv, envp and the auxiliary vector, at both pointer widths. Has its fuzz target and its Miri step already. | 22 |
| `libs/cpio` | 8 — the "newc" reader an initramfs is unpacked from. Borrows, copies nothing, allocates nothing. | 45 |
| `libs/vfs` | 8 — dentries, mounts, the path walk, open file descriptions, descriptor tables, tmpfs over a page store, initramfs unpacking. Written at the start of its stage rather than ahead of it. Has its fuzz target and its Miri step already. | 59 |
| `libs/procfs` | Reached at 8 — the text of `/proc`: the `maps` line padded to its name column at both pointer widths, `meminfo`, `status`, `stat` and `mounts`, pinned byte for byte against lines a real Linux printed, and the `maps` parser the kernel's boot check reads its own output back with. No fuzz target: it arranges the kernel's own numbers rather than parsing a stranger's bytes. | 14 |
| `libs/virtio` | 10 — the split virtqueue as logic over an abstract shared memory, and the PCI transport's status protocol, feature negotiation and queue activation. Reached at 10 by the boot check's virtio-rng driver. | 62 |
| `libs/pci` | 10 — configuration space: ECAM geometry, headers, BAR decoding and sizing, both capability lists, MSI-X, the bus walk, virtio's PCI transport, MSI-X messages and the pages of a BAR a driver must not be given. Has its fuzz target and its Miri step already. | 52 |
| `libs/native-abi` | Reached at 9 — native syscall numbers, handles, rights, signals, `errno` names, `repr(C)` layouts. Constants only, like `libs/linux-abi`, and tested against it. | 13 |
| `libs/objects` | Reached at 9 — the handle table and the channel message queue, generic over what a handle names; every process's table and every channel is one; and the reachability walk a send makes before it queues an endpoint. Has its fuzz target and its Miri step. | 24 |
| `libs/btrfs` | 11, 12 — superblock, chunk tree, B-tree nodes, item payloads. Parsing only: no device, no cache, no transactions. | 38 |

With the five crates the boot path was built on — `bootinfo`, `elf` (the
loader's), `frame`, `heap`, `paging` — that is **706 host unit tests, all
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

**`ferrousli/` is on the goal's path.** It is a C library for Linux written in
Rust, modelled on musl and aimed in time at glibc's binary interface. It is its
own cargo workspace, depends on no Ferrix crate, and reaches the kernel only
through Linux system calls. Until 2026-09-13 it sat beside this roadmap; by the
customer's decision that day it is on it. `cargo xtask check --ferrousli` runs
its formatting, lints and tests in both profiles, and `docs/BACKLOG.md` says
which landings must pass it. Busybox 1.37.0 built against it by `cargo xtask
busybox` links with no symbol undefined, and since 5e9b0b6 passes `test-shell`
and `test-vfs` on x86_64 without reaching a stub. It is the primary busybox,
the userland Ferrix is measured with: the gates run it first, with `--init
ferrousli`, and keep Alpine's musl build and the glibc one as compatibility
checks. Its test programs already boot as Ferrix's first process with `cargo
xtask test-shell --init`, `tests/c/thread/on_ferrix.c` among them for
`CLONE_THREAD`. Its own status is in [ferrousli/README.md](../ferrousli/README.md).

---

## Continuously, from stage 1

* Every stage's exit criterion joins the CI boot test and stays there.
* The assembly allow-list is not added to without an argument in the diff.
* Anything expressible as a pure function of bytes goes to `libs/` and gets a
  fuzz target and a Miri run — before it is called from the kernel, not after.
