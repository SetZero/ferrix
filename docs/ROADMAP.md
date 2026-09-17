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
bring-up, 9–14 are the parts this design chose to do properly, 15–16 are the
goal, and 17–19 are the goal after it: a Hyprland-shaped Wayland compositor,
written in Rust, running on Ferrix (decided 2026-09-13). Nobody should read
the table as a schedule; from that date sizes for new work are story points,
measured into time only after the fact.

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
Stages 17 and 18 have begun, and the compositor runs: `cargo xtask
test-compositor` boots it as init on Ferrix, and two Wayland clients tile on
the card, pixel for pixel as the renderer draws them, on x86-64 and AArch64.
Stage 17, display and input, has begun. Its display iteration is done:
`/dev/dri/card0` served by a ring-3 virtio-gpu driver, with `cargo xtask
test-display` requiring a compositor's colour pixel for pixel on x86-64 and
AArch64. Its input iteration is done too, to the same standard:
`/dev/input/eventN` served by a ring-3 virtio-input driver and a kernel input
core, with `cargo xtask test-input` sending a key and a touch in at QEMU's
far end over QMP and requiring them back out of the nodes on both
architectures, and a negative control that must fail. `epoll`, `eventfd` and
`ioctl(FIONBIO)` are in the boot test, so the kernel side of iteration 2's
prerequisites is done, and `card0` has the primary plane and `type` property
Smithay's legacy path needs (E4). The compositor reads those nodes now, so
stage 17 is met: `cargo xtask test-seat` types into a window on Ferrix from
QEMU's far end.
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
back; names came in the landing after. `SCM_RIGHTS` passes descriptors: a
send takes a reference to each named file and queues it with the first byte,
a receive installs as many as its control buffer has room for and closes the
rest, flagging `MSG_CTRUNC`, and a file is only ever dropped with the
descriptor table and the queue unlocked. Sockets passed over each other's
connections and then closed are collected at the last close, as Linux's
`unix_gc` collects them: a pass finds the sockets only queues refer to that no
readable queue holds, and empties their queues. A peek installs nothing where
Linux's installs duplicates.
The `unix` boot check drives a pair of each type through `dispatch`, and what
each call refuses stands beside what it answers -- the families and types
`AF_UNIX` is not, a call on the console, one on a closed descriptor, a peek
that took what it looked at, a record read that found the last record's tail:

      unix     a stream pair carried bytes across two writes and a peek left them; records kept their boundaries and MSG_TRUNC their lengths; shutdown ended one direction; a socket reported its type, buffers, credentials and unnamed address; a connection crossed an abstract name and a path, and 6 name calls were refused as specified; a descriptor travelled with a message, kept its file open in the queue, and was closed when a receive had no room for it; a cycle of sockets in flight was collected at its last close, and one a descriptor reached was kept

**Left, and why it did not block the exit:**

* **What signals do not do yet.** There is no vDSO, so a handler needs
  `SA_RESTORER`, which musl and glibc always set; only `ITIMER_REAL` arms, not
  `ITIMER_VIRTUAL` or `ITIMER_PROF`. `SA_RESTART`, `SIGCHLD` to a handler with
  `wait4` still reaping, job-control stop and continue, `alarm`/`ITIMER_REAL`,
  the alternate stack and the fault-to-signal path are now driven by the
  `sigpaths` boot check; the fault-to-signal catch and an `SA_RESTART`
  interrupted read are proven at the kernel's decision, not yet end-to-end by a
  hand-assembled faulting or interrupted-read user program.
* **Threads.** `clone(CLONE_THREAD)` makes a thread of the calling process:
  every program's task runs a `Thread`, whose id comes from the pid space and
  finds its process; signal state is split as Linux splits it, the thread
  taking its own signals before its process's; `exit` ends a thread and
  `exit_group` its process, which lets go of what it holds when its last
  thread has gone. Boot checks on all three architectures make threads in
  shared memory, clear `CLONE_CHILD_CLEARTID` as a thread ends, and end a
  process whose last two threads exit together; ferrousli's static pthread
  test runs to its end on x86-64. Signals work across threads: a signal sent
  to a process is judged across every live thread's mask and wakes one that
  can take it, `tkill`, `tgkill` and `SIGPIPE` reach one thread, a stop parks
  every thread and their blocked calls restart after `SIGCONT`, and `execve`
  from any thread ends the others and takes the pid -- each checked at boot on
  all three architectures. A futex wait reads its word under the table without
  faulting, and retries if another thread unmapped the page in between; `brk`
  and `fork` take a heap lock that may sleep, so a fork never copies a heap
  half shrunk; and an unmap on one processor waits for a copy on another to
  let go of its page. `/proc/<pid>/task` lists each thread with its `status`,
  `stat` and `comm`, and `Threads:` counts them. And the exit test runs:
  `cargo xtask test-threads` boots a static musl Rust program of five threads
  using `std::thread`, `Mutex` and `mpsc` as init on all three architectures,
  which counts its threads through `/proc/self`, with a build that expects
  one thread too many failing on that count. It is linked as rustc links a
  musl program by default, which on x86-64 is a static PIE: the loader places
  an `ET_DYN` image without an interpreter at two thirds of the user half,
  with its entry and `AT_PHDR` moved, and the program relocates itself; a
  boot check loads such an image on all three architectures. A dynamically
  linked program, which names an interpreter, is still refused.
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
are the file's and nothing here has a disk to flush.

**Done — a file mapped privately.** A private mapping of a file shows the
file's pages until it writes one, and that write copies the page into a shadow
object of the mapping's own. So neither the file nor any other mapping sees
it: not a write through the mapping, not a `read` into it, and not a write by
a `fork` child, which inherits the copies and copies again on its own write. A
write to the file still shows through the pages the mapping has not copied.
The region names both objects by one id, the file's VMO attached shared and
the shadow attached privately, so a file's VMO still has only shared mappers.
Every fault checks the file's end first, so a copied page past a cut is
`SIGBUS` too. A truncation takes the copies past the cut, so a file grown back
shows zeros through the mapping, as on Linux. `munmap` gives back the shadow's
pages and never the file's. A writable private mapping of a file opened
read-only maps, as Linux allows. No mappable object is filled from a page
source yet, and one that is must give the fault a way to fill a page before
it is copied. The boot check maps a file under `/tmp` from a process of its
own, shared and private side by side, twice. It also has a program in user
mode write a page of a private mapping it has only read:

```
  mmap     12339 bytes written through a shared file mapping and read back from the file, and the other way; refusals, msync and /proc maps answered; a truncation took 3 pages away from the mapping; 2 pages copied into a private mapping and kept from the file, a fork and a user-mode write included; 0 frames leaked
```

**Done — an anonymous file, and its seals.** `memfd_create` makes a regular
file that no directory names, on a tmpfs of its own that nothing mounts, named
`memfd:NAME`, so it reads, writes, truncates and maps as a tmpfs file does.
With `MFD_ALLOW_SEALING` it takes seals through `fcntl(F_ADD_SEALS)`, and
tmpfs enforces them under the file's lock in shmem's order. A shrink seal
refuses truncating downwards, a grow seal refuses extending by truncation,
write or `fallocate`, and a write seal refuses every write. A write seal is
refused with `EBUSY` while any shared mapping may write the file. The file's
VMO counts those mappings, as Linux's `i_mmap_writable`: a shared mapping of a
file open for writing, raised as its id enters a space's tables and lowered as
it leaves, a `fork` child's copy included. The seal is stored and the count
read under the file's lock, and `mmap` counts itself before it takes that lock
to read the seals, so a write seal and a writable shared mapping never both
stand. Once a write seal stands, a shared writable mapping is `EPERM` and
`mprotect` to writable is `EACCES`; a private mapping still maps and keeps its
writes to itself. The same accounting closes a gap: a read-only shared mapping
of a file opened read-only can no longer be made writable. `MFD_HUGETLB`,
`MFD_NOEXEC_SEAL` and `MFD_EXEC` are `EINVAL`, as on a kernel before 6.3's exec
seal. The boot check runs it all twice, from a process of its own:

```
  memfd    4 seals added and enforced, 14 calls refused as Linux refuses them, a write seal refused while a shared mapping could write, a fork's copy included; 0 frames leaked
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

`process_start(process, bootstrap)` works in four steps, so that a race
between two starts, or a start and a kill, is harmless, and a start that fails
has nothing to undo:
1. It claims the start, so a second start is refused before it has moved
   anything.
2. It makes the child's task without running it -- the processor it goes to,
   its stack, its thread counted live -- so everything that can fail fails with
   nothing moved.
3. It moves the bootstrap handle into the child's table under both tables'
   locks. A refusal here drops the prepared task, which frees its stack and
   gives the start back.
4. It enters the program with that handle's value in its first argument
   register, which cannot fail.

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

**Done — `devmgr`, the program.** `/sbin/devmgr` on the native runtime is
what `docs/DEVMGR.md` says: started by the kernel after the boot checks with
a bootstrap channel already holding DEVICES — a job, every device node
twice, and every driver image the initramfs lists in `/lib/drivers/MANIFEST`,
read by the kernel into anonymous VMOs, since a native program reads no
files — it asks `device_info` of each device, matches virtio-blk by a table
of its own, makes the ring, starts `blk` from the image in a job of its own
with START, and waits for the kernel's PUBLISHED before the next, so disks
register in PCI order and no two drivers race to be `vda`; then it REPORTs,
the kernel prints the line and `xtask` requires it, and from then on a
driver's death reaches `devmgr`'s port, which quiesces the device (retrying
a `TIMED_OUT`, never a `BAD_STATE`) and tells the kernel DIED. A channel
message carries 64 handles and ARMv7-A publishes 36 device nodes, so the
kernel sends DEVICES in as many messages as the handles need, each saying how
many devices are still to come. The boot check's own starter now runs only
when the image carries no `devmgr`; with it, the driver check reads through
the disks `devmgr`'s drivers serve. `libs/devmgr-proto` is the protocol's
crate, host-tested. What the stage still owes is one row: trusting
decoding-off BARs.

The run recorded when it landed: `devmgr   8 devices, 1 drivers, 2 started,
0 failed` on x86-64, 4 devices on AArch64, 36 on ARMv7-A at four processors
and at two, each followed by the driver check's sectors read back through
the disks `devmgr` started.

**Still to do, after the exit.** One row, beside the path to the goal.

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
ABI: `AF_UNIX` stream and datagram with descriptor passing (pulled forward to
stage 17's path on 2026-09-13, because Wayland is an `AF_UNIX` socket
carrying descriptors, and needing no net core), `AF_INET` and
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

**Written ahead so far.** `libs/netwire` has the headers — Ethernet, ARP,
IPv4 and IPv6 with its extension headers, ICMPv4, ICMPv6 and Neighbor
Discovery, UDP and TCP — with 54 host tests and the `netwire_parse` fuzz
target; see *Written ahead of their stage*. `libs/linux-abi` has the numbers
and layouts a program passes: `sockaddr_in` and `sockaddr_in6`, the
`IPPROTO_`, `IP_`, `IPV6_` and `TCP_` options, and the fixed headers of
netlink and its routing messages, each checked against a probe compiled from
the UAPI headers. `libs/nettcp` has the TCP state machine over those headers,
with 30 host tests and the `nettcp_state` fuzz target, and `libs/net` has the
net core over both — interfaces, routes, neighbours, reassembly, ICMP, UDP and
the socket table — with 45 host tests and the `net_input` fuzz target; see
*Written ahead of their stage*. `libs/netlink` has the byte-level half of
netlink over those headers — walking a buffer of messages and the attributes
after each one, and building replies into a caller's buffer — with 48 host
tests and the `netlink_walk` fuzz target. virtio-net's device protocol is the
one that is still to be written.
*Written ahead of their stage*. virtio-net is written too, in the two halves
virtio-blk is split into: `libs/virtio`'s `net` module for the device protocol
— the configuration block, the feature bits and the header — and
`libs/virtio-net` for the driver logic over two queues, with 22 host tests and
the `virtio_net` fuzz target. Netlink message encoding is still to be written,
and nothing in the kernel calls any of it yet.

**Done — the net core, and `AF_INET` and `AF_INET6` sockets.** `kernel/src/net`
is `libs/net` behind one lock and a task that drives it. Nothing sleeps inside
that lock: every call takes what it needs into a kernel buffer, drops it, and
only then touches the program's memory, which is `kernel/src/fs/socket.rs`'s
rule and the same reason. Sockets have no wait queue of their own -- they all
wait on one, woken whenever the stack moved, and each waiter re-checks its own
condition; that is a thundering herd in the textbook sense and the right trade
for a host with tens of sockets rather than thousands.

`socket`, `bind`, `listen`, `accept`, `accept4`, `connect`, `getsockname`,
`getpeername`, `send*`, `recv*`, `shutdown`, `getsockopt`, `setsockopt` and
the two queue ioctls answer for `AF_INET` and `AF_INET6` as they already did
for `AF_UNIX`, through one enumeration so the order of Linux's checks cannot
drift apart between the families. An `AF_INET6` socket carries IPv4 through
`::ffff:0:0/96` unless `IPV6_V6ONLY` says otherwise, and reports a v4 peer in
that spelling. `SOCK_RAW` was `EPERM` for everyone until raw sockets landed
(below); it still is for a process without `CAP_NET_RAW`.

The boot check uses the loopback and nothing else, so it passes on a machine
with no network device -- which every machine is until the driver lands. It
requires a datagram to arrive with its sender's address, a datagram to an empty
port to earn `ECONNREFUSED` from the unreachable this host sends itself, a
connection to be made, accepted, to carry bytes both ways and to end as a clean
close, and a connection to a port nobody listens on to be refused rather than
left to time out -- over IPv4 and again over IPv6. It reads:

```
  net      1 interface up, 318 bytes carried over the loopback in both families, 2 connections made and accepted, 3 calls refused as specified
```

**Done — raw IPv4 sockets, for `ping`.** busybox 1.37's `ping` opens
`socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)` and nothing else — it has no fallback
to the unprivileged echo socket — and read every reply with its IPv4 header in
front. `SOCK_RAW` now opens for root at any protocol from 1 to 255 (zero is
`EPROTONOSUPPORT` for everyone, as `inet_create`'s lookup makes it; past
`IPPROTO_MAX` is `EINVAL`), and is `EPERM` without the privilege. A raw socket
is handed a copy of every IPv4 packet of its protocol that reaches this host,
header and all, beside the stack's own handling: an echo request is both
answered and copied. It takes only its peer's packets once connected, only its
address's once bound, and only its device's once pinned; `IPPROTO_RAW` takes
nothing; `ICMP_FILTER` holds back the ICMP types it names. A send builds the
header, or with `IP_HDRINCL` — always set for `IPPROTO_RAW` — completes the
program's: the length, the checksum, and an identification and source left at
zero. `AF_INET6` raw sockets are not in yet, and `AF_PACKET`, which `udhcpc`
needs, is next. The boot check pings the loopback through a raw socket and
requires the request and the reply, each with its header; a second socket
filtering replies must read the request and not the reply; an `IPPROTO_RAW`
packet with zeroed fields must reach a UDP socket; and uid 1000 must be
refused, a control that panics the boot when the privilege check is removed.
The line now ends `, 3 packets read by raw sockets`, and `test-net`'s `ping`
passes with the busybox built against ferrousli.

**Done — `AF_PACKET` sockets, for `udhcpc`.** A DHCP client has to hear an
offer for an address its interface does not have yet, which IP drops, so it
listens below IP: busybox's `udhcpc` opens `socket(AF_PACKET, SOCK_DGRAM,
htons(ETH_P_IP))`, binds it to the interface with a `sockaddr_ll`, and reads
IPv4 packets with the link header taken off; it sends through another, naming
the broadcast hardware address. Packet sockets now open for root, in both
types: `SOCK_RAW` reads and writes whole frames, `SOCK_DGRAM` payloads with
the stack putting the header on. Each is handed a copy of every frame of its
protocol, or of every protocol with `ETH_P_ALL`, that an Ethernet interface
takes in, on the interface it is bound to or on all of them, with the
sender's hardware address and whether the frame was to this host, the
broadcast or a group. `packet_create` asks for the capability before the
type, and that order is kept: uid 1000 is `EPERM` even for a stream packet
socket. Not yet: frames this host sends are not copied back to `ETH_P_ALL`
sockets, a packet socket on the loopback carries nothing, `SOL_PACKET`
options are `ENOPROTOOPT` — `udhcpc`'s `PACKET_AUXDATA` among them, which it
takes quietly — and no BPF filter attaches, which busybox 1.37 compiles out.
The net ring's boot check binds a packet socket for ARP to its pretend
driver's interface: it must read the ARP request the driver delivers, with a
`sockaddr_ll` naming the sender, the interface and the broadcast, and a frame
it sends to the peer must be the next one the kernel submits, with the link
header it asked for. With the stack's frame tap removed, the boot panics with
"a packet socket bound for ARP did not read the ARP request". The line ends
`, 2 through a packet socket`. In a `run --net` guest on Windows, `udhcpc -i
eth0 -n -q` broadcast its discover and select, got the lease of 10.0.2.15
from the gateway, and the address, route and resolver it configured fetched
`http://example.com`. Since the landing after it, the image carries that
`default.script`, the interactive shell's `/etc/profile` runs `udhcpc` when
`eth0` has no address, and `test-net` gets its address the same way: its
second program is `udhcpc -i eth0 -n -q`, which must print the lease the
gateway gave.

**Done — raw IPv6 sockets, for `ping6`.** busybox's `ping6` opens
`socket(AF_INET6, SOCK_RAW, IPPROTO_ICMPV6)`, requires `ICMP6_FILTER` to be
accepted, asks for `IPV6_CHECKSUM` at `SOL_RAW`, and prints the hop limit from
a control message. `SOCK_RAW` now opens in `AF_INET6` too, on the same terms
as in `AF_INET`. An IPv6 raw socket reads what follows the header and its
extensions, not the header, as RFC 3542 says. It matches peer, address and
device as an IPv4 one does. A message whose checksum at the socket's
`IPV6_CHECKSUM` offset does not verify is dropped, and `ICMP6_FILTER`'s eight
words hold back the ICMPv6 types they block. On send the stack writes that
checksum over the pseudo-header, always at offset 2 for ICMPv6. The option is
`EINVAL` at `SOL_IPV6` on an ICMPv6 socket and for an odd offset, and a
negative offset turns it off. `recvmsg` now writes control messages as
`put_cmsg` does, with `MSG_CTRUNC` for one that does not fit. The first ones
are the hop limit: `IPV6_RECVHOPLIMIT` gives an `IPV6_HOPLIMIT` message and
`IPV6_2292HOPLIMIT`, which musl hands `ping6`, one of that older type, on any
IPv6 datagram socket. Every received datagram now carries its hop limit. The
stack's echo replies over IPv6 went out with hop limit 255, the Neighbor
Discovery value, and now carry the stack's default of 64, as Linux's do. Not
yet: `IPPROTO_RAW` in IPv6 opens but its sends are `EINVAL`, because the
program's own IPv6 header is not taken. The boot check pings `::1` through a
raw ICMPv6 socket with the checksum left zero. It requires the reply without
a header, with a checksum that verifies and with `IPV6_2292HOPLIMIT` 64. A
second socket that passes only requests must read the request and not the
reply. With the filter disabled, the boot panics with "ICMP6_FILTER let the
echo reply it blocks through". The line now ends `, 7 calls refused as
specified, 6 packets read by raw sockets`, and `test-net` runs `ping6 -c 2 ::1`,
which must print `ttl=64`: a stack that sent no control message would print
-1.

**Done — `AF_NETLINK` route sockets, which is how an interface is
configured.** Every way of configuring a network on Linux ends at the same
socket: `ip` uses nothing else, `ifconfig` and `route` use ioctls that are a
shim over it, and `udhcpc` and a C library's `getifaddrs` read it directly.
`socket(AF_NETLINK, SOCK_DGRAM | SOCK_RAW, NETLINK_ROUTE)` now opens one,
`bind` gives it a port identifier, and `sendmsg` and `recvmsg` carry requests
and replies.

`kernel/src/net/netlink` answers dumps of links, addresses, routes and
neighbours, and the changes that matter: `RTM_SETLINK` and the `RTM_NEWLINK`
that `ip link set dev eth0 up` actually sends, `RTM_NEWADDR` and `RTM_DELADDR`,
`RTM_NEWROUTE` and `RTM_DELROUTE`. An unknown type is `NLMSG_ERROR` with
`EOPNOTSUPP`, a message too short for the fixed header its type implies is
`EINVAL`, and a change asked for without `NLM_F_REQUEST` is `EINVAL` — a
notification is what the kernel sends, not what it takes.

A request is answered before `sendmsg` returns, as `NETLINK_ROUTE` is on
Linux, which is what lets `rtnl_talk` send and then read with no poll and no
timeout. Each reply is queued as a datagram of its own rather than packed with
its siblings, because a netlink datagram that does not fit the buffer offered
is truncated and the rest dropped — one dump in one datagram would be a reader
with a small buffer silently losing interfaces. Nothing is encoded inside the
net core's lock: the buffer is allocated before it is taken and the replies
copied out after it is dropped.

What is not there is multicast — nothing yet sends a notification when an
interface changes — and dump filters: a `RTM_GET*` answers with the whole table
whether or not `NLM_F_DUMP` was set, because the attributes that would narrow
it are read by nobody. Both wait for the first program that needs them.

The boot check is the path `ip` takes rather than the pieces it is made of: it
opens a socket, binds it, reads its port back, dumps the links and finds the
loopback with its flags, adds an address and a route and sees each in the next
dump, removes them and sees them gone, and requires `EOPNOTSUPP` for a type
nothing answers and `EINVAL` for a message too short for its header. It reads:

```
  netlink  1 links, 2 addresses and 2 routes dumped, an address and a route added and taken away again, 2 requests refused as specified
```

**The host side already exists, and it is ours.** `cargo xtask run --net`
attaches a virtio-net device whose backend is `xtask/src/gateway/`: a NAT
gateway in the build tool, on the guest network `10.0.2.0/24` with the gateway
at `10.0.2.2`, DNS at `10.0.2.3` and the guest at `10.0.2.15` — slirp's numbers,
so that every habit and every piece of QEMU documentation carries over. It
answers ARP and ICMP echo for its own addresses, offers the guest its address
over DHCP, relays UDP through one ephemeral host socket per flow with
`10.0.2.3:53` forwarded to the host's resolver, and terminates TCP, re-opening
each connection as an ordinary host `TcpStream`. Twelve host tests speak to it
over the socket pair QEMU would use, so the half of the path that is ours is
covered by `cargo test` with no QEMU and no network at all.

**Why it is written rather than QEMU's own.** `-netdev user` is slirp, and
slirp is an optional build-time dependency: the QEMU this was developed against
was built without it, and says so — *network backend 'user' is not compiled
into this binary*. The two ways round that both want privilege a build tool
should not ask for. `-netdev tap` needs `CAP_NET_ADMIN` or a setuid helper, and
the usual escape — a `tap` inside an unprivileged user namespace — is refused
outright on a host whose `AppArmor` policy blocks those namespaces, as Ubuntu's
now does. What is always available is QEMU's `dgram` backend, which hands every
Ethernet frame to a UNIX datagram socket; the other end of that pair is a
network backend anyone can write, and this is it. No raw socket, no tun device,
no capability, and the same behaviour on every developer's machine.

Two things it does not do, both for the same reason. ICMP echo is answered only
for `10.0.2.2` and `10.0.2.3`, never forwarded: originating ICMP needs a raw
socket or a permitted ping group, neither of which a build tool can rely on. And
there is no IPv6, because a half-answered IPv6 is worse than none — a guest that
receives a router advertisement will prefer the address in it.

**Done — the ring the driver will speak over.** `libs/netring` and
`docs/NET-RING.md` are the memory the kernel shares with a ring-3 network
driver. It is the block ring's discipline with its allocator taken out: a frame
is bounded by the interface's MTU, so the data VMO is `entries` slots of a fixed
size and a submission names its slot. That removes the whole region-allocation
half of the protocol and with it the class of bug where a region is reused
before its completion, which on an untranslated IOMMU domain is a device writing
into somebody else's packet. The index discipline is `libs/blkring`'s, written a
second time rather than shared, which `docs/BACKLOG.md` carries as a debt with
its reason.

**Done — the kernel's end of the ring.** `kernel/src/net_ring` is one task per
ring: it waits for the driver's HELLO, checks the rights every handle carries
exactly rather than at least, holds the two VMOs, adds the interface to the net
core, and answers READY with its completion port. Then it posts half the ring
for the driver to fill and keeps the other half for frames the net core wants
sent — posting *every* free slot is the mistake that leaves an interface
receiving for ever and never answering, and the first end-to-end check of this
path found it.

The check plays the driver, so the whole kernel side runs on a machine with no
network adapter: it makes the VMOs and the port a driver makes, sends HELLO,
and answers submissions by hand. An ARP request written into a posted slot
comes back as an ARP reply in a slot the kernel submits, which is a frame in
and a frame out through the whole stack. It reads:

```
  netring  1 HELLOs refused as specified, 4 slots posted for a driver to fill, 1 frames taken up the stack and 1 answered back down it
```

**Done — `/proc/net`.** `libs/procfs` gains `dev`, `route`, `tcp`, `tcp6`,
`udp`, `udp6` and `arp`, each pinned in its tests against a line copied from a
running Linux, because `route`, `netstat`, `arp` and `ifconfig` read these
files with `sscanf` and fixed columns and a field one column off is a program
that reads the wrong number confidently. Two details that look like mistakes
and are not: the addresses are the network-order bytes read as a host-order
number, so `10.0.0.0` prints as `0000000A`; and the lines are padded to a
fixed width, 127 for `route` and `udp` and 149 for `tcp`, by Linux's
`seq_pad`, which pads a short line and leaves a long one alone -- which is why
an IPv6 row overflows.

**Done — the driver, in ring 3.** `user/net` is the process that makes a
virtio-net function an interface. It holds handles and nothing else:
`libs/virtio-net` drives the device, `libs/netring` speaks the ring,
`libs/netserve` joins the two, and all three are tested on the host, so the
program is the eight steps of `docs/NET-RING.md` §7 with a `Step` per failure.
`devmgr` starts it from a second row in its table, and the net ring's `take_up`
sends PUBLISHED for the device's PCI location before READY goes out, because a
driver that has not published by the time `devmgr` reports is killed.

The first end-to-end run found the failure the ring's sleep handshake exists to
prevent: the driver waited on its port without first asking to be rung, so the
kernel — which rings only a driver that has said it is going to sleep — never
rang it. Frames the *device* delivered still woke it through the interrupt, so
the interface looked alive and transmitted nothing at all.

That was possible because `libs/netserve` left the handshake to its caller
while `libs/blkserve` owns it, which is why `user/blk` never had the bug and
`user/net` did. The handshake is now `netserve`'s too, and with it the rule
`blkserve` already had: while a frame waits for room in the device's transmit
queue the answer is always to sleep, whatever the ring holds. Without that
rule a full transmit queue is a spin rather than a wait — the loop takes no
submission while a frame waits, so the ring stays full and answers "do not
sleep" until the device interrupts. A test pins both.

**Done — the `ifreq` ioctls.** rtnetlink is how an interface is configured and
`kernel/src/net/netlink` answers it, but `if_nametoindex` — which POSIX.1-2024
specifies, which every program that names an interface goes through, and which
musl implements as `ioctl(SIOCGIFINDEX)` over an `AF_UNIX` socket — had nothing
to talk to, so `ip` could not find a device that was right there.
`kernel/src/net/ifreq` is the index, the flags, the address, the mask, the
broadcast and peer addresses, the MTU, the hardware address, the queue length
and `SIOCGIFCONF`. `sys_ioctl` sends what a socket's own family did not know to
it whatever the family, as Linux's `sock_ioctl` passes it to `dev_ioctl`, so
`ifconfig` and `getifaddrs` are answered as well as `ip`.

**Done — `cargo xtask test-net`, the exit criterion as a test.** The servers
the guest fetches from are threads of `xtask` on ports the host's kernel chose,
and `10.0.2.2` is the host's loopback as it is under slirp, so the run is
hermetic: it says the same thing on a machine with no network, and the name it
resolves is answered by a stub the gateway's forwarder is pointed at for the
run. The digest is POSIX `cksum`, written out in `xtask/src/net.rs` and checked
against what the host's own `cksum` prints, because it is the one digest this
busybox and this build tool can both compute with nothing added to either.

Thirteen programs, on x86-64, AArch64 and ARMv7-A: `ip` configures an address
and a route and reads them back; `route -n` and `netstat -rn` report through
`/proc/net/route`; `ping` reaches the gateway; `nslookup` resolves a name;
`wget` fetches by name through `/etc/resolv.conf` and fetches a quarter of a
megabyte whose `cksum` matches the server's; `nc -u` sends a datagram and reads
the answer; and `/proc/net/dev` and `arp -n` show what the traffic left behind.

**Done — `AF_UNIX` names.** `bind`, `listen`, `connect` and `accept` on a
local socket, over both namespaces Linux has. A pathname is a node in the
filesystem: `bind` creates an `S_IFSOCK` node exactly as `mknod` would, and
`kernel/src/fs/sockname` maps that node — its device and inode numbers, not
the path, because two paths can name one node — to the socket. `connect` walks
the path like any other, which is what makes the permissions on the
directories above it mean something. An abstract name, a `sun_path` starting
with a NUL, is a flat namespace of its own that goes when the socket does. The
tables hold weak references, so a socket is not kept alive by having a name.

The connection is complete when it is queued rather than when it is accepted,
as Linux's `unix_stream_connect` has it, so a client may write before the
server calls `accept`. A socket left behind by a program that died keeps its
node — Linux does not unlink one either, which is why `unlink` before `bind`
is the universal idiom — and a `connect` to it is `ECONNREFUSED` rather than
`ENOENT`: the two answers say different things, and a C library reads them.

Which is how this closed the musl busybox's `su`, open since the applet
landed. musl's `initgroups` tries an `AF_UNIX` connection to nscd before it
reads `/etc/group`; `EOPNOTSUPP` is an error it gives up on, and `ENOENT` is
one it falls back from.

**Done — curl, built against ferrousli.** `ferrousli/tools/ports/curl` builds
curl 8.22.0 over Mbed TLS 3.6.7 as a static x86-64 program against ferrousli,
from sources pinned by checksum, with curl.se's extract of Mozilla's CA
certificates. It linked with nothing missing from the library. `cargo xtask
ports` builds it. Every x86-64 image that carries a busybox carries it at
`/bin/curl`, with the bundle at `/etc/ssl/certs/ca-certificates.crt`. When
curl is installed, `test-net` adds two programs to its thirteen: curl fetches
the file by name through `/etc/resolv.conf`, and fetches the quarter megabyte
whose `cksum` must match the server's.

HTTPS first failed in the guest. A fetch from `https://1.1.1.1/` got through
the handshake to the certificate check and was refused with *"The certificate
validity starts in the future"*, because the kernel's clock started at 1970.
And `getrandom` was xorshift seeded from a counter, so a session key would have
been predictable.

**Done — the time of day, and random numbers worth a key.** The loader asks
firmware for both before it leaves boot services: `GetTime`, turned into Unix
nanoseconds by `ferrix_bootinfo::unix_nanos`, and 32 bytes from
`EFI_RNG_PROTOCOL`. `BootInfo` version 5 carries them, with a flag for each
that firmware provided. The kernel starts `CLOCK_REALTIME` at that time after
stage 3's timer check.

`libs/crng` is the generator: ChaCha20 with fast key erasure, its block
function checked against RFC 8439's vector and OpenSSL's keystream. Every
64-byte block replaces the key with its first half and hands out the second.
`kernel/src/random.rs` seeds it from firmware's bytes, credited 256 bits, and
from the CPU's `RDSEED` or `RDRAND` on x86-64, or `RNDR` on AArch64, at 32 bits
a word. It also mixes in timer jitter, credited nothing, and mixes the counter
into every read. `getrandom`, `/dev/random`, `/dev/urandom` and `AT_RANDOM` all
read it. A boot says what it had:

```
  firmware clock read, random number protocol read
  clock    1789590336 seconds since the epoch, from firmware's clock
  random   seeded with 512 bits: firmware, 8 words from the CPU, timer jitter
```

That was OVMF, with RDRAND turned on in `xtask`'s QEMU CPU. AAVMF, and U-Boot's
EFI on ARMv7-A, gave both the time and the random bytes too. A machine with no
firmware protocol and no CPU instruction boots `NOT SEEDED`, in capitals, and
`getrandom` answers anyway. Linux would block instead, but that wait never ends
on a machine with nothing to wait for. The boot check reads the generator twice
and panics as FX-0306 if the two reads match. Its negative control, not
committed, on x86-64: with the second read replaced by a copy of the first, the
boot printed `FERRIX-PANIC random generator check failed: two reads of the
random generator were the same` under FX-0306. The clock's: with the loader's
time flag cleared, the boot said `firmware has no clock: CLOCK_REALTIME starts
at the epoch`.

**Done — btop, and the C++ runtime under it.** `ferrousli/tools/ports/libcxx`
builds LLVM 23.1.1's libc++, libc++abi and libunwind against ferrousli with
the host's gcc. `ferrousli/tools/ports/btop` builds btop 1.4.7, a C++23
program, over them. What ferrousli lacked for that landed with them:
`dl_iterate_phdr` and `dladdr`, the message catalogues, the `strtod_l` family,
`pathconf`, `copy_file_range`, `getloadavg`, and thread cancellation, which
btop uses to stop a stalled collector thread. The kernel lacked two things.
`/proc/<pid>/mounts` did not exist, and it is where btop reads the mounts.
And a program larger than four mebibytes could not be started at all: `execve`
read the file into one heap allocation, the heap takes a large one from the
buddy allocator in a single block, and the largest block is `2^MAX_ORDER`
frames. btop is 4.6 MiB, and `timeout btop` failed with `ENOMEM`. A program
is now read into a `vmap::Buffer`, on single frames mapped into the kernel's
arena, so the limit is `READ_FILE_LIMIT`'s 64 MiB, which it was always
documented to be.

Every x86-64 image with a busybox carries `/bin/btop`. In a `test-net` guest,
`timeout 8 btop` drew the CPU, memory, network and process panels on the
serial console, with `eth0`, `lo` and the running `sh`, `busybox` and
`btop`, and redrew them every two seconds until `timeout` ended it. No gate
runs it yet; `docs/BACKLOG.md` has the row.

**Exit, and it is met:** under `xtask`'s gateway — which is where this
criterion's *"under QEMU's user-mode network"* now reads — busybox configures
`eth0` with `ip`, and `route` and `netstat` report through `/proc/net`. `wget`
fetches a file from a server on the host that byte-for-byte matches what it
served. All of it runs in a test of its own, `cargo xtask test-net`, for the
reason stage 7's exit is one, and it passes on all three architectures.

The criterion's `nc` clause is met by other programs, deliberately: this
busybox's `nc` has no `-U`, so it cannot open a local socket at all, and a
criterion written before that was known is not worth bending the code to. A
stream over `AF_UNIX` — bound to a path and to an abstract name, connected,
accepted, and carrying bytes each way — is proven by the stage 7 boot check on
every architecture, and by `su`, which reaches `/etc/group` only because a
`connect` to a name nobody bound answers the way a C library expects.

---

## Dynamic linking — PIE, `PT_INTERP`, a loader  ·  *39 points*

Placed after *Networking* without a number of its own, for the same reason:
nothing on the path to `rustc` needs it, since Rust's `std` targets static
musl. What needs it is the promise `docs/ARCHITECTURE.md` §2 makes — that
somebody else's Linux binary runs unchanged — which today holds only for a
static, fixed-address executable. `kernel/src/syscall/load.rs` refuses
`ET_DYN` by name (`NeedsRelocation`) and never reads `PT_INTERP`, and nearly
every binary a distribution ships is a position-independent executable that
asks for glibc's `ld-linux`. The question of 2026-09-16 that put this here was
whether Steam could run; the answer began with this section, before the
32-bit ABI, networking and the display stages it also waits on. The 32-bit
x86 ABI is not part of it and is not staged.

Three parts, in the order they can be tested:

* **The kernel half, 5 points.** `execve` loads an `ET_DYN` executable at a
  base of its own — Linux's `ELF_ET_DYN_BASE`, unrandomised until stage 13 —
  and applies its relative relocations, which `libs/elf` already reads
  because the UEFI loader relocates itself. A `PT_INTERP` names a second
  file: the interpreter is loaded at its own base, the entry point is the
  interpreter's, and the auxiliary vector says the rest — `AT_BASE` for the
  interpreter, `AT_PHDR`, `AT_PHNUM` and `AT_ENTRY` for the program, plus
  `AT_RANDOM`, `AT_EXECFN` and `AT_PLATFORM`, whose keys `libs/linux-abi`
  carries. The interpreter then maps libraries itself, through stage 8's
  file-backed `mmap` with `MAP_FIXED` and `PROT_EXEC`, and `mprotect`s its
  `PT_GNU_RELRO` — calls that exist and gain a test that uses them as
  `ld.so` does. `AT_SYSINFO_EHDR` stays absent: there is no vDSO, and glibc
  and musl both fall back to the real call.
* **ferrousli's loader, 21 points.** The fifth item of `ferrousli/README.md`:
  a dynamic loader in Rust, shipped as ferrousli's `ld.so` with
  `libferrousli.so` beside `libferrousli.a`. The `PT_DYNAMIC` section, symbol
  lookup through the GNU hash table, the relocation types of all three
  architectures (`GLOB_DAT`, `JUMP_SLOT`, the TLS forms, `IRELATIVE`),
  `PT_TLS` for loaded modules with `__tls_get_addr` and the dynamic thread
  vector, `DT_INIT_ARRAY` and `DT_FINI_ARRAY` in dependency order,
  `LD_LIBRARY_PATH` and `DT_RUNPATH`, and `dlfcn.h` — `dlopen`, `dlsym`,
  `dlclose`, `dlerror`, `dladdr` — which `docs/POSIX-2024.md` lists as
  ferrousli's dynamic-loading area and does not price. Lazy binding is not
  in it: everything is bound at load, as `LD_BIND_NOW` does, so there is no
  resolver trampoline to write per architecture.
* **glibc's names, 13 points.** The README's "then glibc's symbol versions":
  `GLIBC_2.2.5` and its successors as `libferrousli.so`'s version
  definitions, `libc.so.6`, `libm.so.6` and `libpthread.so.0` as its
  `SONAME`s, and the startup contract glibc's `crt1.o` and `ld-linux` make
  between them — `_dl_start_user`, `__libc_start_main` with `_dl_fini`,
  `_rtld_global` where a binary reaches for it. A binary linked against
  glibc then loads ferrousli in glibc's place, which is what the README
  calls the destination.

`cargo xtask test-shell` gains `--interpreter` and `--library`, which put
the named files onto the initramfs beside `--init` at the paths the binary
asks for, so the test binary is still one the repository does not carry.

**Exit,** in two halves, each a test of its own for the reason stage 7's is:

1. A distribution's dynamic busybox — Debian's, linked against glibc — with
   its own `ld-linux-x86-64.so.2` and `libc.so.6` on the image, runs stage
   7's `test-shell` script on x86-64 and AArch64, and the `armhf` pair does
   the same on ARMv7-A, printing the same lines and exiting 7. This proves
   the kernel half against a loader nobody here wrote.
2. The same binary with glibc's files removed and ferrousli's `ld.so` and
   `libferrousli.so` at their paths, running the same script on all three
   architectures. This proves the other two parts, and is the README's fifth
   item in its entirety.

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

## Stage 17 — Display and input  ·  *74 points*

The first stage of the goal after `rustc`: a Hyprland-shaped Wayland
compositor, written in Rust, running on Ferrix. The compositor is a user
program on the Linux ABI, so what it needs from the kernel is what a Linux
compositor needs, and this stage provides it the way stage 10 provides disks:
a kernel core with a ring-3 driver on stage 10's device objects, exposed
through the Linux ABI so that Rust's existing compositor crates run unchanged.

* **A display core and a virtio-gpu driver in ring 3.** The core owns
  connectors, modes and scanout buffers; the driver drives virtio-gpu's 2D
  commands (resource create, attach backing, set scanout, transfer, flush)
  through a translated domain, the way virtio-blk drives the disk. The Linux
  ABI is `/dev/dri/card0` with the DRM/KMS subset a software-rendered
  compositor uses: `GET_RESOURCES`, connectors and modes, dumb buffers
  (`MODE_CREATE_DUMB`, `MODE_MAP_DUMB` served by file-backed `mmap`),
  `MODE_ADDFB2`, the atomic commit with page flip, and the vblank event read
  from the descriptor. No GEM import, no PRIME, no render node yet: rendering
  is on the CPU into a dumb buffer.
* **An input core and a virtio-input driver in ring 3,** exposed as evdev:
  `/dev/input/event*` with `EVIOCGBIT`, `EVIOCGNAME`, `EVIOCGABS` and the
  `input_event` stream, keyboard, mouse and tablet as QEMU offers them.
* **The calls a Rust event loop needs:** `epoll_create1`/`epoll_ctl`/`epoll_pwait`,
  with an epoll descriptor another epoll can wait on, `eventfd2`, `FIONBIO`,
  `timerfd_create`/`timerfd_settime` (called by `polling`, which tolerates
  their absence), `memfd_create`
  with sealing, and `AF_UNIX` sockets with `SCM_RIGHTS`, pulled forward from
  the networking stage because Wayland is a Unix socket carrying descriptors
  and `wl_shm` is a sealed memfd mapped by both sides. The `AF_INET` half
  stays where it is.
* **Nothing is drawn by the kernel.** `docs/ARCHITECTURE.md` keeps its rule:
  the firmware framebuffer is a panic's, once. The display core hands scanout
  to whoever opened the card; a panic after that still writes text over it.

**Done — iteration 1, a colour on the screen (2026-09-16).** `docs/DISPLAY.md`
is the design. The display core (`kernel/src/display`) takes a ring-3
driver's HELLO on a control channel `DISPLAY_CONTROL_CREATE` (0x104C) makes,
checks it through `ferrix-displayctl`'s session, refuses it when the firmware
framebuffer is memory the allocator owns, and publishes `/dev/dri/card0`: a
256 MiB card VMO whose ranges are dumb buffers, mapped by the program through
the card's inode and pinned read-only by the driver. `user/gpu`, started by
devmgr for 0x1050, drives virtio-gpu's 2D commands through
`ferrix-virtio-gpu` behind VT-d or the SMMUv3. The card answers the legacy
DRM subset — resources, connector, encoder, CRTC, dumb buffers, `ADDFB`,
`ADDFB2`, `SETCRTC`, `PAGE_FLIP` with its event, `DIRTYFB` — to one open at a
time. `cargo xtask test-display` boots `compositor/blank` as init on x86-64
and AArch64 and requires every pixel of QEMU's screendump of the virtio-gpu
to be its colour, and its negative control to fail at exactly pixel (0, 0).

**Done — L1 of the input iteration, evdev's numbers (2026-09-16).**
`docs/INPUT.md` is the design, approved by os-f6 the same day. Its first
landing is `ferrix-linux-abi::input`: the evdev ioctls `/dev/input/eventN`
will answer, with the sized ones as functions and a request's parts taken
apart as the kernel's `_IOC_*` macros do, the event types and the codes the
test and QEMU's keyboard and tablet use, and `input_event`, `input_id` and
`input_absinfo`. A committed probe, `probe/input.c`, prints every number and
layout from linux-libc-dev 7.0.0-29.29's headers natively and under
`qemu-arm`, including the three views of `input_event` a 32-bit libc can
take, and the tests name each line the module disagrees with.

**Done — L2 of the input iteration, the virtio-input protocol (2026-09-16).**
`ferrix-virtio::input` asks a device virtio 1.2 §5.8's configuration queries
(name, serial, ids, property and event bitmaps, each axis's range) and reads
the 8-byte event. It refuses an answer over 128 bytes or running past the
configuration block, which QEMU shortens to its longest answer, a structure
answer shorter than its structure, an axis whose minimum lies above its
maximum, and a completion that wrote anything but one event. Its tests read
QEMU 9.2.4's keyboard, mouse, tablet and multi-touch devices whole, beside
devices that lie, and require the bits QEMU sets to be L1's `KEY_*`,
`BTN_*`, `REL_*`, `LED_*` and `ABS_MT_*` codes. It no longer writes evdev's
numbers down itself: `EV_*` and `SYN_REPORT` are L1's, re-exported, so the
driver and the evdev nodes read one copy the probe pins. Its fuzz target,
`virtio_input`, ran 50,283,653 inputs in ten minutes without a failure.

**Done — L3 of the input iteration, the input control protocol and evdev's
queues (2026-09-16).** `libs/inputctl` holds `docs/INPUT.md` §3.2's messages
between the kernel's input core and a ring-3 driver, the core's side of that
conversation, and §3.1's per-open queue, host-tested (23 tests) and fuzzed
(3,038,857 inputs in ten minutes without a failure). Where the design left a
rule to Linux it follows `drivers/input/evdev.c` and `input.c`, and
`docs/INPUT.md` §5 lists the four places where Linux answers differently from
the design's first text, for L6 to settle. No kernel code uses it yet.

**Done — L4 of the input iteration, the virtio-input driver (2026-09-17).**
`libs/virtio-input` is the driver a `user/input` process will run: the logic
over a [`Transport`], pinned pages and an event area the process hands it, as
`libs/virtio-gpu` is written. Bring-up negotiates features, reads the device's
description through L2's configuration queries and builds the event queue, and
then stops with `FEATURES_OK` set and no buffer posted, because QEMU discards
every event until `DRIVER_OK` and `docs/INPUT.md` §3.2 has the core judge the
device before it is set; READY sets it, posts a buffer in every descriptor and
rings the doorbell, and a device the core refuses never sees it. The event
queue is kept full before a single event is forwarded, since QEMU drops a
whole report without a word when a buffer is missing. What the core would not
publish is dropped here rather than breaking the session, and a report that
reaches one event short of the core's limit is cut with a `SYN_REPORT` of the
driver's own and counted: `docs/INPUT.md` §6 decisions 9 and 10. Memory the
device may still write into comes back `Teardown::Wedged` and is never
dropped. Its 28 tests run it against a device copying QEMU 9.2.4's keyboard,
mouse, tablet and multi-touch tables and against devices that lie; the
`virtio_input_driver` fuzz target plays the device and the glue against a real
`inputctl` session and requires the core never to refuse a message the batch
made, no event to be lost or reordered, and every message to fit and to end at
a report boundary unless it is full. It ran 10,523,605 inputs in ten minutes
without a failure. No kernel code uses it yet: L5 is the process.

**Done — L5, L6 and L7 of the input iteration: events, end to end
(2026-09-17).** The input core (`kernel/src/input`) takes a ring-3 driver's
HELLO on a control channel `INPUT_CONTROL_CREATE` (0x104D) makes, judges it
through `ferrix-inputctl`'s session, and publishes `/dev/input/eventN` with a
boot line naming the device and what it publishes -- `input    event0 QEMU
Virtio Keyboard: keys, LEDs, repeat`. `user/input`, started by devmgr for
0x1052, drives the device through `ferrix-virtio-input` behind VT-d or the
SMMUv3, as `user/gpu` drives the card. The nodes answer the evdev subset
`docs/INPUT.md` §2.4 reads out of the `evdev` crate -- `EVIOCGVERSION`,
`EVIOCGID`, `EVIOCGNAME`, `EVIOCGUNIQ`, `EVIOCGPROP`, `EVIOCGBIT` of each
type that has a bitmap, `EVIOCGKEY`, `EVIOCGLED`, `EVIOCGSW`, `EVIOCGABS`,
`EVIOCGREP`/`EVIOCSREP`, `EVIOCGRAB`, `EVIOCREVOKE`, `EVIOCSCLOCKID` -- with
a queue per open following `evdev.c`'s size, drop and `SYN_DROPPED` rules.
`compositor/evecho` is the consumer, and it runs on a Linux host's own
`/dev/input` as well as on Ferrix, which is where it found that `EVIOCGBIT`
of `EV_REP` is `EINVAL` on Linux. `cargo xtask test-input` boots it as init
on x86-64 and AArch64, sends a key press, a key release, an absolute position
and a button through QMP's `input-send-event`, and requires each one back out
of the right node as a finished report; its negative control reports every
key as `KEY_RESERVED` and must fail the same check. `--display` now brings
the keyboard and the tablet too, so the compositor `run --display` starts has
a seat to read when it grows one.

**Estimate, re-baselined by os-f6 on 2026-09-16:** 74 points, from 55. The
display's iteration 1 took 37, the input iteration is 23 (`docs/INPUT.md`
§5), and the prerequisites of iteration 2 are 14: nested `epoll` (E1),
`eventfd2` (E2) and `FIONBIO` (E3), which os-26 landed on 2026-09-16, and
`card0`'s planes and properties (E4, `docs/DISPLAY.md` §2.3), which the GUI
session holds.
Nested `epoll`, `FIONBIO` and the plane objects were not counted before
`docs/INPUT.md` §2 read Smithay's event loop.

**Still to do:** input L5–L7; `timerfd` (wanted,
not required, deferred); atomic commit; per-open windows onto the card VMO;
and the rest of the exit below. E1–E3 (os-26) and E4 are done, so iteration
2's prerequisites are all in. E1–E3's paragraphs below record
what they do not yet do as Linux does: `EPOLLRDHUP` and `EPOLLPRI` are never
reported, `poll` and epoll waits recheck every 5 ms instead of waking on the
event (a P2 row in `docs/BACKLOG.md`), and a zero-length write to an eventfd
returns 0 rather than `EINVAL`.

**Exit:** in the boot test on x86-64 and AArch64 (ARMv7-A's QEMU machine has
no virtio-gpu; the DK1's LTDC is a hardware row, P3), a user program opens
`/dev/dri/card0`, sets the mode, draws a known pattern into a dumb buffer and
page-flips it; `cargo xtask` reads QEMU's screendump and requires the pattern
pixel for pixel. A key and a pointer motion sent through QEMU's monitor arrive
as `input_event`s on `/dev/input/event0` and are echoed on the console. Two
processes exchange a sealed memfd over an `AF_UNIX` socket and both see the
other's writes through `MAP_SHARED`.

**Done — epoll, iteration 2's first kernel row.** `epoll_create1`,
`epoll_create`, `epoll_ctl`, `epoll_wait`, `epoll_pwait` and `epoll_pwait2`
answer on all three architectures, with `struct epoll_event` packed to 12
bytes on x86-64 and 16 elsewhere. A set is an anonymous file on
`anon_inodefs`, named `anon_inode:[eventpoll]`. Its registrations are keyed by
the open file and the number, as Linux keys them, and hold the file weakly, so
closing a number leaves the registration while a `dup` keeps the file open.
Level-triggered, edge-triggered and one-shot registrations report only the
events they asked for. A wait asks each file's readiness at the moment it
waits, and sleeps and asks again as `poll` does. For edge-triggered mode every
pollable object (pipe, terminal, `/dev/dri/card0`, `AF_UNIX`, `AF_INET`,
netlink and packet sockets) reports how often its wait queues were woken, and a registration is
due a report when that count moved or a readiness bit appeared. A set is
itself pollable, so a Wayland server's set can sit in its toolkit's. A set
added to itself is `EINVAL`, one that would contain itself `ELOOP`, and a chain
of sets deeper than `EP_MAX_NESTS` `ELOOP`. A regular file or a directory is
`EPERM`, and `EPOLLEXCLUSIVE` is refused where Linux refuses it. Not yet:
`EPOLLRDHUP` and `EPOLLPRI` are never reported, because a file's readiness
does not say either. A wait wakes within 5 ms of its file becoming ready, not
at once. The boot check makes sets by number and watches pipes. Level,
edge-after-drain, one-shot re-armed by `EPOLL_CTL_MOD`, reporting by turns
with room for one event, a registration outliving its number through a `dup`,
a set inside a set and a chain of six are each required. Twenty-two refusals
are checked in Linux's order, and the run is done twice with no frame kept.
The line reads `epoll    13 events delivered ..., 22 calls refused as Linux
refuses them; 0 frames leaked`. With edge-triggered mode blind to the wake
count, the boot panics with "an edge-triggered set did not report more data
written into a readable pipe". With the loop check looking for nothing, it
panics with "a set added to a set it holds was not ELOOP".

**Done — eventfd, iteration 2's second kernel row.** `eventfd2` on all three
architectures, and the older `eventfd` on x86-64 and ARMv7-A, make a 64-bit
counter on `anon_inodefs`, named `anon_inode:[eventfd]`. A write adds its
eight bytes and a read takes the counter, or one with `EFD_SEMAPHORE`. A read
of zero waits or is `EAGAIN`, and so is a write that would carry the counter to
`u64::MAX`, the value it never holds. `poll` answers writable exactly while one
more fits. A buffer shorter than eight bytes and a write of `u64::MAX` are
`EINVAL`, as in `fs/eventfd.c`, and `EFD_CLOEXEC` and `EFD_NONBLOCK` reach the
descriptor and the open file. Each read and write wakes the other side's
queue, which is also what epoll's edge-triggered mode counts. Not yet: a
zero-length write returns 0 before it reaches the eventfd, where Linux answers
`EINVAL`, because the write path answers every empty write itself. The boot
check covers the initial value, adding up, the semaphore, the ceiling and
`poll` at it. A blocking read must be ended by a write's wake, not the wait's
5 ms recheck, as the queue's count of wake-ended waits shows. An
edge-triggered registration must be reported after a second write although
the counter stayed readable, and eight refusals are checked. The run is done
twice with no frame kept. The line reads `eventfd  7 values read back, a
waiting reader woken by a write, 8 calls refused as Linux refuses them; 0
frames leaked`. With the write's wake removed, the boot panics with "a waiting
eventfd reader was ended by its recheck, not by the write's wake".

**Done — waits that wake on the event.** `poll`, `ppoll`, `select`,
`pselect6` and the epoll waits used to sleep 5 ms and look again, so a
compositor's frame loop and every idle client paid two hundred wake-ups a
second and up to 5 ms of latency. Now each pollable object names the wait
queues it wakes when its readiness changes, through `Inode::poll_queues`:
pipes, `AF_UNIX`, `AF_INET`, packet and netlink sockets, eventfds, the
terminal, `/dev/dri/card0` and epoll sets. An epoll set names its registered
files' queues and a queue of its own that `epoll_ctl` wakes. A wait sleeps on
all of them at once, as Linux's `poll_wait` does, and a wake of any ends it.
It still looks again by itself in case a wake is missing, every second when
every watched object vouches that each change wakes a queue, and every 5 ms
otherwise. The terminal vouches only when its input is interrupt-driven. A
readiness question to the net core no longer wakes the stack's waiters: a
socket wait that asked from inside its own wait used to wake itself, and two
such waits woke each other. The eventfd check now runs a `poll` and an
`epoll_wait` in tasks of their own, which a write must end by its wake. A
300 ms `poll` on a quiet eventfd must look at most 12 times, where looking
every 5 ms takes about 120. Three controls, each run and each failing the
boot. With waits that never trust their queues, it panics with "a poll on a
quiet eventfd kept looking instead of sleeping on its queues" after 119 looks.
With an eventfd naming no queue, it panics with the same. With an eventfd
naming only its writable queue, it panics with "a waiting poll was ended by
looking again, not by the write's wake".

**Done — `ioctl(FIONBIO)`, iteration 2's third kernel row.** `FIONBIO` is
answered for every file before any file-specific request, as `do_vfs_ioctl`
answers it. The `int` its argument points at sets `O_NONBLOCK` when not zero
and clears it when zero, and an unreadable argument is `EFAULT`. It was
`ENOTTY` for everything but the console before. The stage 8 pipe check makes a
blocking pipe non-blocking through it: an empty read is then `EAGAIN` and
`F_GETFL` reports `O_NONBLOCK`, and zero clears the flag again. It does the
same to an `AF_UNIX` socket. With the request left unanswered, the boot panics
with "FIONBIO on a pipe was refused". With E1 to E3 in, the kernel side of
iteration 2 is done.

**Done — `FIOCLEX` and `FIONCLEX`, E3's other two requests.** Rust's standard
library sets close-on-exec with `ioctl(FIOCLEX)` in its fallback paths, and E3
promised both. They are answered for every file beside `FIONBIO`, set and clear
the descriptor's close-on-exec flag as `F_SETFD` does, and read no argument.
The stage 8 pipe check sets and clears the flag through them and reads it back
with `F_GETFD`, passing an unreadable argument to show nothing is read. With
`FIOCLEX` left unanswered, the boot panics with "FIOCLEX on a pipe was
refused".

**Done — E4, `card0`'s primary plane (2026-09-16, reviewed by os-02).** Smithay's legacy path lists planes and reads each one's `type`
property, and has nothing to draw on without a primary plane. `card0` now
accepts `DRM_CLIENT_CAP_UNIVERSAL_PLANES`, without which Linux's
`drm_mode_getplane_res` lists only overlay planes; it implies nothing atomic,
and atomic stays refused. It lists one primary plane, id 4, whose `GETPLANE`
gives `XRGB8888`, `possible_crtcs` 1 and the CRTC and framebuffer last shown.
`OBJ_GETPROPERTIES` gives the plane its immutable `type` enum, property 5, at
`Primary`, and the CRTC and the connector no properties; `GETPROPERTY` names
the values `Overlay`, `Primary` and `Cursor`. Each follows Linux's
`drm_plane.c`, `drm_mode_object.c` and `drm_property.c`, read before the code
(`docs/DISPLAY.md` §2.3). Framebuffer ids now start at 32, so no id names two
objects. The numbers come from the extended `probe/drm.c`. `compositor/blank`
reads the planes after its modeset as Smithay does, and `cargo xtask
test-display` requires its marker line to end in `plane <id> Primary` on
x86-64 and AArch64. With the plane's `type` value set to `Overlay`, the line
ends in `plane 4 Overlay` and the test fails with "the program found no
primary plane on the card" on both.

---

## Stage 18 — The compositor  ·  *96 points*

A Wayland compositor in Rust, on `libs/`' side of the tree as its own
workspace the way ferrousli is, **written from scratch** rather than on the
Smithay crates: decided on 2026-09-17, the reasoning in `docs/BACKLOG.md`.
The wire protocol, the window management, the layouts, the configuration and
the IPC are all written new, in Rust, to Hyprland's shape; the only C library
allowed is `xkbcommon`.

* **Protocols:** `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`
  with keyboard and pointer (keymaps through `xkbcommon`, the one C library
  allowed at this stage, built on ferrousli; a Rust keymap compiler is a
  later row), `xdg_shell` with toplevels and popups, `xdg_decoration`,
  `wlr_layer_shell` for bars; `zwp_linux_dmabuf` withheld until stage 19.
* **Rendering** on the CPU into stage 17's dumb buffers: damage tracking,
  a pixman-shaped Rust rasteriser (`tiny-skia`), one page flip per frame,
  frame callbacks on vblank.
* **The wire protocol with no libwayland** (`compositor/wire`): the message
  header, every argument type, descriptor passing over `AF_UNIX`, and the
  object map, written from `/usr/share/wayland/wayland.xml` and fuzzed
  (8 points).
* **XKB data in the image:** a pinned subset of xkeyboard-config, the files
  xkbcommon's keymap compiler reads (2 points).
* **Hyprland's shape:** the dwindle and master layouts, workspaces, the
  keybind and dispatcher model (`movefocus`, `movewindow`, `workspace`,
  `killactive`, `togglefloating`, `fullscreen`), gaps and borders, window
  rules, and a configuration file that parses Hyprland's `hyprland.conf`
  syntax (sections, `$variables`, `bind`, `windowrule`, `exec-once`).
* **IPC:** a Unix socket with `hyprctl`'s request shape (`clients`,
  `workspaces`, `activewindow`, `dispatch`, `keyword`, `reload`) and the
  event socket, so a Rust `hyprctl` and a bar can be written against it.
* **Clients for the tests,** in Rust over `wl_shm`: a solid-colour client
  that draws a given pattern, and a small terminal emulator over the
  console's pty, both static, both under `cargo xtask` like busybox.

**Done — the configuration, before the Smithay decision needs making.**
`compositor/` is a workspace of its own, gated by `cargo xtask check`. Its
first crate, `compositor/config`, parses `hyprland.conf` as hyprlang does:
categories and the `category:key` shorthand, `$variables`, `##` escapes,
`source`, Hyprland's option types and defaults for the options the compositor
implements (integers that are also booleans and colours, floats, gradients,
CSS gaps), `bind` with all fourteen flag letters, `unbind` and submaps, and
the keywords later parts interpret, with Hyprland's diagnostics and the rest
of the file still applied past a bad line. `Config::keyword` is `hyprctl
keyword`. Host-tested and fuzzed (`hyprconf_parse`).

**Done — the layout and dispatcher core.** `compositor/layout` is Hyprland's
window management as rectangles and ids, checked against Hyprland's source:
monitors, workspaces made and dropped on demand, the dwindle tree (split
direction, `preserve_split`, `force_split`, split ratio) and the master layout
(`mfact`, orientations, `new_status`, `new_on_top`) dividing the work area
inside `gaps_out`, `gaps_in` and borders between windows, floating and
fullscreen windows, the focus history, and `movefocus` (edge search, angle
search for floating windows, wrap-around), `movewindow` (dwindle's take-out
and put-back at the focal point, across monitors too), `workspace` and
`movetoworkspace`(`silent`) with `N`, `+N` and `e+N`, `killactive`,
`togglefloating` and `fullscreen`, run from a `Bind`. Every call returns the
changes it caused. Host-tested; where it departs from Hyprland (no cursor, so
`force_split` 0 takes the second half; pseudotiling; floating `movewindow`)
its crate docs say so.

**Done — the renderer, the last of the pure crates.** `compositor/render`
draws a frame into the `XRGB8888` dumb buffer stage 17 gives it, on the CPU,
with no C: a `Canvas` over a `tiny-skia` pixmap (pinned at `=0.12.0`, default
features off, no build script) with `clear`, `fill`, `border` and `composite`
-- `ARGB8888` source-over, `XRGB8888` copied and made opaque -- each drawn
only inside a `Damage` of disjoint rectangles, so a translucent client blends
every pixel once. `present` writes into a target of any stride, `render` draws
one monitor from `compositor/layout`'s output with `compositor/config`'s
border colours, and `damage_between` two layouts is the region a frame has to
redraw. The everyday gate is pixel comparison on the host: the two pattern
clients stage 18's tests will run are drawn in code, and a run-length expected
image of them tiled dwindle-style at 1024x768 is committed and compared byte
for byte, with a negative control that alters one pixel and requires the check
to name exactly it. Twenty tests. It holds no Wayland object and no
descriptor, so it carries over whichever way the compositor-server decision
goes; its crate docs say how a Smithay `Renderer`/`Frame`/`ImportMem` or a
server written from scratch wraps it.

**Done — the wire protocol, with no libwayland (2026-09-17).**
`compositor/wire` is the bottom of the server: the message header, every
argument type, descriptors travelling beside the bytes rather than in them,
and the per-client object map that keeps a client's ids and the server's in
their own halves. It holds no socket and no descriptor -- an `fd` is an `i32`
here and nothing more -- so it is host-tested and fuzzed as `libs/netwire`
and `libs/inputctl` are, and the part that has to run on Ferrix to be tried
is only the socket above it.

It is written from the protocol and from `connection.c`, so by construction
nothing in it is checked against a real implementation. The check is a probe,
in the shape `libs/linux-abi/probe` set: `compositor/wire/probe/wire.c`
drives a real libwayland client and a real libwayland server over socket
pairs it owns and prints the bytes each wrote, `probe/wire.txt` is that
output committed with the libwayland version on its first line, and the tests
require this crate to write the same bytes and to read them back. Fifteen
message shapes, both directions, every argument type: `new_id`,
`wl_registry.bind`'s unnamed `new_id` with its interface string, a null
`object` where the protocol allows one, negative `int`s, a descriptor that is
not in the byte stream, `array`s of three lengths and `fixed` at 1.5 and
-2.25. Its negative control, not committed: with a string's length counting
its bytes without the NUL -- the likeliest way to get the format wrong -- six
tests fail, `every_message_libwayland_sent_is_written_the_same_way_here`
among them.

Reading libwayland taught it two things it would not have got right from the
protocol. It is stricter in one place: a message whose arguments do not use
every byte its header claimed is refused, where libwayland consumes the rest
without looking, because that check catches a wrong signature in this crate's
own tables rather than letting it misread the next message. And it must not
be stricter in another: the padding after a string or an array is never
looked at, because `serialize_closure` steps over it and `wl_closure_queue`,
the path a queued request takes, allocates its buffer with `malloc` -- so a
real client's padding is uninitialised heap, and a server requiring it to be
zero would drop clients at random. Thirty tests; the `wayland_wire` fuzz
target reads a stranger's bytes against a signature it picks and requires a
failed read to consume nothing, every string it accepts to be UTF-8 without a
NUL, no descriptor to be invented, and everything the writer builds to read
back the same. It ran 65,887,144 inputs in ten minutes without a failure.

**Done — the interface tables, generated from the protocol (2026-09-17).**
`compositor/protocol` is what tells `compositor/wire`'s reader the signature
of the message it is about to read: every interface's requests and events by
opcode, their argument types, their `since` versions, which of them are
destructors, and every enumeration value. It is generated by
`scripts/gen-wayland-protocol.py` from XML vendored under
`compositor/protocol/protocols/` -- `wayland.xml`, `xdg-shell.xml`,
`xdg-decoration-unstable-v1.xml` and `wlr-layer-shell-unstable-v1.xml`, each
carrying its own permissive licence, copied into the generated file. The XML
is vendored rather than read from the machine because a table built from
whatever `wayland-protocols` the builder happened to have installed would
change under the compositor without a commit. `cargo xtask check` runs the
generator with `--check`, so a hand edit fails the gate.

A generator can read XML wrong, and nothing in it would notice, so the tables
are checked against an implementation the way `compositor/wire` is:
`probe/interfaces.c` links against libwayland's own compiled
`wl_*_interface` structures -- libwayland's for the core protocol and
`wayland-scanner`'s output from the same vendored XML for the rest -- and
prints every interface's version and every message's opcode, name and
signature. `probe/interfaces.txt` is that output committed, and the tests
require the generated tables to agree: 31 interfaces and 194 messages, with
the count asserted so a table quietly dropping messages fails. The one place
the two spellings differ is `wl_registry.bind`'s unnamed `new_id`, which
libwayland expands into the three things it is on the wire and this tree
keeps as the protocol's single argument; the test folds them back and says
why.

Two negative controls, neither committed. With `allow-null` read as its own
opposite in the generator -- a nullable flag is the subtlest thing to get
wrong -- `wl_display.error`'s signature disagrees and the comparison fails.
With `wl_surface.commit`'s opcode changed by hand from 6 to 7, `--check`
reports `compositor/protocol/src/generated/core.rs is stale` and exits 1.

**Done — the connection, `wl_display` and `wl_registry` (2026-09-17).**
`compositor/server` is the protocol half of the compositor and holds no
socket, no descriptor and no pixel: a `Client` is handed the bytes that
arrived and the descriptors that came with them and gives back the bytes to
send, so object lifetimes, versions and every way a client can break the
rules are host-tested, and running it on Ferrix will test the socket rather
than the protocol. A connection starts as libwayland starts one, with
`wl_display` as object 1 and nothing else. `sync` makes a `wl_callback`,
fires it and takes the id back with `wl_display.delete_id`; `get_registry`
announces every global in order; `bind` checks the name, the version and the
interface the client named, because a client binding `wl_shm`'s name while
saying `wl_seat` would otherwise get a `wl_shm` answering seat requests. The
object map is `compositor/wire`'s, now carrying the server's own state for
each object, so there is one map of live objects rather than two that can
disagree about which exist.

A protocol error is the end of a connection -- Wayland has no way to refuse
one request and carry on -- so every refusal queues one `wl_display.error`
and stops reading, and nothing after it is answered. Writing that down found
a defect: `wl_display.error`'s object argument is not nullable, so a client
that sent a `new_id` of zero would have had its error event silently fail to
encode and would have seen the socket close with nothing said. The error now
names `wl_display` where it has no object to name, which is where libwayland
posts one it cannot attribute.

Twenty-four tests, and the last of them is a real client:
`probe/roundtrip.sh` runs `examples/transcript.rs` to get the server's answer
to the conversation every toolkit opens with, replays it to a real libwayland
client through `probe/roundtrip.c`, and records what the client made of it.
libwayland reported all six globals with their names and versions and
`wl_display_roundtrip` returned with no error. The recording is rebuilt by the
test and compared, so a server that changed its answer cannot leave the
check passing against a conversation that no longer happens. Its negative
control, not committed: with `wl_registry.global`'s name and version written
the other way round, libwayland reports `global 6 wl_compositor 1`.

**Done — surfaces and shared memory (2026-09-17).** `wl_compositor` with
its surfaces and regions, and `wl_shm` with its pools and buffers: everything
a client needs to put a picture somewhere, short of a shell to give it a
window. The double buffering the protocol is built on is written as two
states of the same shape rather than as dirty flags, since flags are where
that bug lives: every request but `destroy` and `frame` changes the pending
state and `commit` makes all of it current at once, taking the damage and the
frame callbacks and leaving everything a commit did not mention alone. A
buffer a commit replaced is the client's again and one committed twice is
not, because a client that committed the same buffer again never got it back.
An object made by another inherits its version, so a `wl_surface` from a
`wl_compositor` bound at 4 is never sent `preferred_buffer_scale`, which
arrived in 6.

**It is stricter than libwayland in one place, on purpose.**
`wayland-shm.c`'s bounds check reads `stride < width`, comparing bytes with
pixels: for a four-byte format a client may pass `stride == width`, a quarter
of the row it needs, and libwayland takes it. The pool then only has to hold
`stride * height` bytes while a compositor reading `width` pixels from each
row reads `width * 4` from the last row's start and runs off the end. Here
the stride must be at least `width * 4` and the arithmetic is checked rather
than guarded by the division libwayland uses to keep its multiply from
overflowing. Every real toolkit sends `width * 4` or more, so the rule costs
nothing and closes an out-of-bounds read. Only `ARGB8888` and `XRGB8888` are
offered, because a format announced and not drawn is a client rendering a
frame nobody can show.

Forty-two tests, and one of them is the whole point: `probe/roundtrip.c` now
drives a real libwayland client through everything it does to show a
window -- bind, create a surface and a region, make a pool and a buffer,
attach, damage, ask for a frame callback, set the scale and commit -- and
records the bytes it wrote. The test replays them into the server and
requires the surface, the region, the pool, the buffer's rectangle and the
commit to be what the client asked for. Not one byte of that test is written
by this tree.

**Done — `xdg_shell` and the socket, and a real client's whole handshake
(2026-09-17).** `xdg_shell` is how a surface becomes a window: the configure
conversation by which the compositor and the client agree on a size, the
serials that pin each one, and the toplevel state a tiling layout needs. Its
rules are the protocol's. A surface may be given one role and no second; a
client may ack a configure it is several behind on, which drops the older
ones with it; and a buffer may not be attached at all until a configure has
been acked, which is the rule that stops a client painting at a size the
compositor never agreed to. `set_max_size` and `set_min_size` are recorded
and not obeyed, and `move`, `resize` and `show_window_menu` are ignored, as
Hyprland ignores them for a tiled window.

`compositor/socket` is the first part of the compositor that has to be on
Ferrix to be tried: an `AF_UNIX` listener and the `sendmsg`/`recvmsg` control
messages that carry descriptors, which the standard library has no stable way
to do. It is the crate's only `unsafe`, split one operation to a block as the
rest of the tree is, and its tests send a descriptor through a socket pair and
read the file on the far side to show it is the same open file and not merely
the same number.

**The whole handshake now runs end to end against a real client.**
`probe/live.c` is a libwayland client that connects to `examples/serve.rs`
over a real socket and does what every application does when it starts: bind
`wl_compositor`, `wl_shm` and `xdg_wm_base`, take `wl_shm`'s formats, make a
surface, give it an `xdg_surface` and an `xdg_toplevel`, set a title and an
app id, commit with nothing attached, take the `xdg_toplevel.configure` and
the `xdg_surface.configure` that follows it, ack the serial, make a pool over
a `memfd` sent through `SCM_RIGHTS`, cut a buffer, attach, damage, ask for a
frame callback and commit. The client was configured at 640x480 with
`activated` and `tiled_left`, acked serial 1, and `wl_display_get_error`
returned zero; the server mapped the surface. Both sides' output is recorded
and the tests require each step. Its negative control, not committed: with the
`xdg_surface.configure` that carries the serial not sent -- the configure
conversation's last message -- the client never acks, the compositor refuses
its buffer, and libwayland prints `xdg_surface#7: error 3: a buffer was
attached before a configure was acked`.

**Done — the compositor runs, and two clients are tiled on it
(2026-09-17).** `compositor/hyprix` is the compositor itself: it reads a
`hyprland.conf`, binds a Wayland socket, starts what `exec-once` names, tiles
what connects to it with `compositor/layout`, draws with `compositor/render`
and puts the frame on a screen. Nothing in it parses a file, works out a
layout, draws a pixel or decodes a message; it is the loop that joins the
crates that do, and the two places the compositor touches the world -- a
client's shared memory, mapped read-only, and the screen.
`compositor/pattern` is the client it draws: a whole Wayland client in one
file, over `compositor/wire` and `compositor/socket` rather than a toolkit,
which means the tests exercise those crates from both ends.

**The headless half of this stage's exit passes.** Two pattern clients
connect over a real socket, are tiled dwindle-style with the configured gaps
and borders, draw into shared memory, and the frame the compositor composed
is compared pixel for pixel against the expected image `compositor/render`'s
own tests bless: 0 differing pixels of 786,432. The two pictures are built by
different paths -- one by calling the renderer with rectangles from the
layout, the other by two programs talking Wayland to a server that works the
same rectangles out from the requests they sent -- and nothing but the pixels
is shared between them. A second test runs one client instead of two and
requires the comparison to notice, so the check is known to fail when the
picture is wrong.

Writing it found two things. A window is reconfigured when *any* window
arrives or leaves, not only when it is made: a client that is not told is one
drawing at the size it had before, which the compositor then draws cropped,
and the first run showed exactly that. And the server refused the client's
own second pool for reusing an object id it had not destroyed, which was the
client's bug and the server being right.

What was left of this stage at that point: `wl_seat`, so a window can be
typed into; the `hyprctl` IPC; and the screen itself, which is
`compositor/blank`'s DRM path moved behind `hyprix`'s backend so the same
frame goes to `/dev/dri/card0` under QEMU. All three have landed since.

**Done — a real toolkit runs on it (2026-09-17).** `compositor/pattern` is
written against the same crates the server is, so a test with it shows the two
halves of this tree agree -- not that the protocol is right. `foot`, a
Wayland terminal built against libwayland and every other compositor, knows
nothing about this one, and running it found four gaps in an afternoon that
the pattern client could never have found:

* `wl_data_device_manager` was not offered at all, and the toolkit refused to
  start without it. The objects are made now and no selection is ever sent,
  which is exactly what a client sees when nobody has copied anything. A
  compositor that advertises it and then does not answer `get_data_device` is
  worse than one that does not advertise it, because the client only finds
  out at its first copy.
* `wl_subcompositor` was advertised and `get_subsurface` was not answered, so
  the toolkit's first window died on `wl_subsurface#15: error 0: object 15 is
  not live`. Every toolkit makes subsurfaces -- a title bar, a shadow, a
  cursor -- so this was every toolkit.
* `wl_output` described nothing, and the toolkit printed `(null):
  0x0+0x0@0Hz`: a client with no mode has no size to scale against. It now
  sends geometry, mode, scale, name, description and the `done` that says the
  description is whole, and only the ones the version bound can read.
* `wl_seat` announced nothing, because there was no input path until stage
  17's L5 to L7 landed. A client may only ask for a capability the seat
  announced, so asking was `missing_capability` rather than a keyboard that
  never sends a key. It announces what there are devices for now.

With those, `foot` gets a window, works out its cell size from the mode this
compositor gave it, draws its terminal over 690,820 of the screen's 786,432
pixels, and exits by choice with no protocol error.
`compositor/hyprix/probe/real-client.sh` records the run and the test requires
each step of it; the record summarises the busiest frame rather than
committing a picture of somebody else's font rendering.

**Done — `hyprctl`, driven by Hyprland's own client (2026-09-17).**
`compositor/ipc` is the request shape and the answers: the flags in front of
a request, `[[BATCH]]`, and the JSON and readable forms of `version`,
`monitors`, `workspaces`, `clients`, `activewindow` and `activeworkspace`,
with Hyprland 0.56.2's field names in its own order, read from
`src/debug/HyprCtl.cpp`. A bar reads those by name, so a missing one is a
crash in somebody else's program. `dispatch`, `keyword` and `reload` come
back for the compositor to run, because the crate holds no compositor and no
socket; `compositor/hyprix` binds the socket where Hyprland binds it, under
`$XDG_RUNTIME_DIR/hypr/<instance>/.socket.sock`, and a program looks there
and nowhere else.

The answers are checked twice. `compositor/ipc`'s tests parse them back with
a JSON parser written in the tests -- the only way to say "this is JSON"
without the compositor taking a dependency for it -- and require Hyprland's
field order, that a window title holding a quote, a backslash and a newline
comes back as it went in, and that `-j -r` and `-j` are the same document.
Then `compositor/hyprix/probe/hyprctl.sh` runs the real `hyprctl` against the
compositor and records what it printed: `version`, `monitors`, `workspaces`,
`clients` and `activewindow` in Hyprland's own shapes; `dispatch movefocus l`
moving the focus from the second window to the first; and `keyword
general:gaps_in 40` re-tiling both windows from 485 pixels wide to 450 while
the compositor runs. A compositor that took the keyword and did not re-tile
would still have said `ok`, so the test requires the sizes.

**Done — the compositor on a screen, on Ferrix (2026-09-17).**
`compositor/drm` is the card, lifted out of `compositor/blank`: the legacy
mode-setting calls and nothing a compositor does not need. `blank` drives the
screen through it still -- `cargo xtask test-display` passes unchanged,
negative control and all -- and `hyprix` drives it too, with two dumb buffers
drawn into in turn and shown with a page flip. One buffer would tear: the
card scans out of the same memory the compositor is writing.

`cargo xtask test-compositor` boots the compositor as init on Ferrix with a
virtio-gpu and requires the screen. It opened `/dev/dri/card0` through the
kernel's display core and the ring-3 virtio-gpu driver, set the card's
preferred mode, bound its Wayland socket, drew a frame with
`compositor/render` and flipped it, and every one of QEMU's 786,432 pixels is
the compositor's background.

Two things had to be true for it to run as init that are not true of a
program started from a shell. The kernel starts its first program the way it
starts a shell, so the compositor is handed `sh -i` or `sh -c <script>`: `-i`
alone now means "the defaults", and `-c` means the words after it are the
compositor's own arguments. And `XDG_RUNTIME_DIR` is a session manager's to
set, and a machine that has just booted has neither, so a bare display name
falls back to `/tmp` rather than the compositor refusing to start -- which
would be a compositor that only runs where something else ran first.

**And stage 18's exit criterion passes on the card.** The initramfs carries
`compositor/pattern` at `/bin/pattern` and the compositor's own `exec-once`
starts two of them, so what reaches the screen is two real Wayland clients,
tiled dwindle-style with the configured gaps and borders, drawn from the
shared memory they committed. `cargo xtask test-compositor` compares QEMU's
screendump against the same expected image `compositor/render`'s own tests
bless and `compositor/hyprix/tests/two_clients.rs` compares against on the
host: every one of 786,432 pixels, on x86-64 and on AArch64. Its negative
control, not committed: with one client started instead of two, 362,542
pixels differ and the test says the picture is not the one the renderer
blesses.

Carrying the clients meant one change outside the compositor. `xtask`'s
initramfs wrote the files a caller asked for only when a shell was going in
beside them, and the reason given was that the boot check's archive must not
change -- but the boot check asks for no files at all, so what kept its bytes
the same was the empty list and never the branch. The files go in either way
now, and the test that named the old rule says the new one.

**Done — the seat: a window that can be typed into (2026-09-17).** The input
iteration landed the nodes; this is the compositor reading them. `hyprix`
opens every `/dev/input/eventN` through `compositor/evecho`, grabs it, and
turns its events into `wl_keyboard` and `wl_pointer` ones: `enter` and
`leave` as the layout's focus moves and as the pointer crosses a window,
`key` with evdev's own code, `modifiers` with the masks the keymap declares,
`motion`, `button`, `axis` and the `frame` that groups them, and
`repeat_info` from `input:repeat_rate` and `input:repeat_delay`.

The keymap is a real one. `compositor/xkb` carries the text libxkbcommon
itself printed for the `evdev` rules with the `us` layout -- 34,205 bytes,
from a committed probe -- and hands it to each client in a sealed `memfd`, as
Smithay's `SealedFile` does. The same probe asks libxkbcommon's own state
machine what each key holds and what each lock leaves locked, so which key is
`Shift` and which is `Caps Lock` is a property of the keymap here as it is
there, and the bits in `wl_keyboard.modifiers` are the indices that keymap
gives rather than constants somebody wrote down.

Keybinds fire. A bind matches when the key matches and the held modifiers are
*exactly* the bind's, Caps Lock and Num Lock excepted -- which is what lets
`SUPER, Q` and `SUPER SHIFT, Q` be two different binds -- and it eats its
key, release included, because a client told a key came up that it was never
told went down has that key stuck down for ever. The `r`, `e`, `n` and `i`
flags, `code:NN`, `mouse:NNN` and the wheel directions all resolve, and one
dispatcher path serves both a bind and `hyprctl dispatch`.

`cargo xtask test-seat` is the proof, on x86-64 and AArch64: QEMU's
`input-send-event` puts a key in at the far end of a `virtio-keyboard-pci`,
and the test requires the client to report the keymap it was handed, the
focus it was given, the pointer's position in its own coordinates, the button,
and the evdev code of the key -- and then requires the screendump taken after
the key to differ from the one before, because the client redraws on a key.
Then it presses `SUPER Q` against a carried `hyprland.conf` holding
`bind = SUPER, Q, killactive`, and requires the window to close and the
screen to be the compositor's background and nothing else. 712,932 of 786,432
pixels changed on the key; every one of them was the background after the
bind.

Two things this found. A `wl_keyboard` is usually asked for in the same burst
of requests that maps the window, so an `enter` sent when the layout focuses
that window can reach no object at all; the server now says whether an
`enter` arrived and the compositor asks again until it does, because a focus
remembered but never delivered is a window that can never be typed into. And
a compositor may not open its devices blocking: the first version read the
keyboard once round a loop that also had the clients and the screen in it,
and stopped all three until somebody typed.

**Done — the exit criterion (2026-09-17).** `cargo
xtask test-compositor` now does what this stage's exit asks, on x86-64 and
AArch64. The compositor starts from a `hyprland.conf` carried in the
initramfs; its `exec-once` lines start the two clients; they tile
dwindle-style with the configured gaps and borders; a keybind pressed through
QMP's `input-send-event` moves the focus and another swaps the windows; and
each of the three states is required from QEMU's screendump, pixel for pixel,
against an image `compositor/render`'s own tests bless. Every one of 786,432
pixels, three times over, and the test also requires the three pictures to be
three pictures -- a compositor that ignored both keybinds would otherwise
pass every comparison if two expected images happened to be the same file.

The IPC half runs on the guest. Hyprland's `hyprctl` is not on Ferrix's
image, so `compositor/ctl` is the same program written here: it finds
`$XDG_RUNTIME_DIR/hypr/<instance>/.socket.sock` where Hyprland's looks,
writes one line and prints the answer. `probe/hyprctl.sh` now runs every
read-only command through both clients against one compositor in one session,
and a test requires the two answers to be identical -- which is the whole
claim it makes. On Ferrix, two more binds run it:

    bind = SUPER, C, exec, /bin/hyprctl clients
    bind = SUPER, W, exec, /bin/hyprctl activewindow

`exec` is a dispatcher the compositor answers rather than the layout, because
starting a program is the compositor's to do; it is also how a person opens a
terminal. After the swap, `hyprctl clients` names both windows with the
titles and the class they set, and `hyprctl activewindow` names the
checkerboard -- the same window the third picture draws the active border
round.

A window is reconfigured on a change of *state* as well as of size now. A
window that has just been focused is the same size and a different state, and
a client that is not told has a title bar that never lights up.

**Done — the event socket (2026-09-17).** `.socket2.sock` is the second of
Hyprland's two: a bar connects once and reads a line for every state change,
and never writes. The line is `CEventManager::formatEvent`'s and no more --
`"{event}>>{data}\n"`, the data cut to 1024 bytes, every newline inside it
turned into a space so that one event is always one line however a client
titled its window -- and each payload is the one Hyprland's own `postEvent`
call builds, with the file and line cited on the variant that carries it. The
`v2` forms are sent beside the old ones, because both have readers.

The events are worked out from the difference between two descriptions of
the compositor rather than posted from inside whatever changed. Hyprland
scatters `postEvent` calls through its source and a change made somewhere new
is a change nothing reports; a difference cannot be missed. The cost is that
two changes in one pass are reported together and in one fixed order, which
no reader can tell from two changes a millisecond apart.

`hyprctl subscribe` reads it. That is not one of Hyprland's commands -- its
own readers are `socat - .socket2.sock` -- but Ferrix has no socat, and a
socket nothing on the image can read is a socket nothing proves. `cargo xtask
test-compositor` now starts it as the configuration's first `exec-once`, so
the transcript holds the whole stream: the monitor, the workspace, both
windows arriving with their class and title, and the focus moving each time a
keybind is pressed. The sockets are bound before `exec-once` runs now, which
is what lets a bar started that way find them.

**Done — `zwlr_layer_shell_v1`: the surfaces that are not windows
(2026-09-17).** A bar, a wallpaper, a notification and a launcher are not
windows: they are not tiled, they are not in the focus order, and they sit at
a fixed place on a fixed layer. `wlr-layer-shell-unstable-v1` is how every
wlroots-shaped compositor -- Hyprland included -- lets a client say so, and it
is what `waybar`, `hyprpaper`, `mako` and `wofi` are written against. Without
it a Hyprland user's setup does not start at all, which made it the largest
thing missing.

The protocol is answered whole for the four layers and the placement rules:
the anchors, the size with the protocol's own `invalid_size` rule for an axis
with no size and no two anchors, the margins, the exclusive zone, the
keyboard interactivity, and the configure conversation with its serials.
Where a surface goes is `compositor/layout`'s `layers`, which follows
wlroots' `wlr_scene_layer_surface_v1_configure`: the usable area less the
margins, the size the client asked for on any axis it is not stretched
across, and each exclusive zone taken off the area the next surface is placed
in -- which is what makes two bars on one edge stack rather than overlap. The
zones become the monitor's reserved strips, so the windows tile in what is
left.

`compositor/pattern --bar 30` is a bar, asking for what `waybar` asks for in
the order it asks. On the host it is drawn beside two windows and the frame
is compared against an image `compositor/render`'s own tests bless; on
Ferrix, `cargo xtask test-compositor` boots a second time with a bar in the
`hyprland.conf` and requires the same picture from QEMU's screendump. Every
one of 786,432 pixels, on x86-64 and AArch64.

**Done — the terminal (2026-09-17).** The last thing this stage owed. It
needed pseudoterminals, which the kernel did not have and now does, and a
terminal emulator, which is `compositor/term`: stage 19's own entry has the
whole of it, since that is where the work landed. `cargo xtask test-pty`
proves the pair without a window and `cargo xtask test-compositor` boots a
terminal in the compositor and requires the picture it makes.

This stage's exit criterion is met in full.

**Still to do, in the order visible iterations need it.** Iteration 1, a
blank screen on Ferrix in QEMU, pulls a first cut of stage 17 forward (the
customer's order of 2026-09-16); then the protocol server, the seat, the
IPC, the clients.

**Exit:** in a test of its own on x86-64 and AArch64, the compositor starts
from a `hyprland.conf`, `exec-once` launches two pattern clients, they tile
dwindle-style with the configured gaps and borders, a keybind sent through
QEMU's monitor moves focus and another swaps them, and each state is
required from QEMU's screendump; `hyprctl clients` and `hyprctl
activewindow` over the IPC socket report the same. A person at the serial
console can run the terminal client in it.

---

## Stage 19 — Hyprland fidelity, and the GPU  ·  *144 points*

What makes it Hyprland rather than a tiling compositor: animations with its
bezier curves, rounded corners, blur and shadows, dimming and opacity rules,
special workspaces, groups, multi-monitor with per-monitor workspaces and
scaling, the plugin-shaped extension points, and the rest of `hyprctl`.

**Done — the window rules that were read and not obeyed (2026-09-17).**
`WindowRuleEffectContainer.cpp` has 55 effect strings. When the merged
0.56 grammar landed, 19 of them were carried out and the rest were kept by
name so that one unsupported word could not cost a person the matchers
beside it. Sixteen more are carried out now, and the ones that are not
each have a reason written down.

The ones that change how a window is *drawn* go through
`compositor/render`: `rounding_power` (a superellipse rather than a
circle, which is the "squircle" a person sets the option for),
`border_color`, `decorate false` for a window that draws its own frame,
`opaque` for a client that leaves rubbish in its alpha channel,
`nearest_neighbor` for pixel art, and `dim_around`.

`dim_around` was on the list of effects said to need a second render
pass, and it does not: Hyprland darkens what is *behind* the thing that
asked for it, this renderer draws in order, so "behind" is "already
drawn" and one fill of the canvas just before that thing is the whole
effect. Both halves work -- `windowrule` for a dialog and `layerrule` for
a launcher.

The ones that change where a window *is* go through `compositor/layout`:
`monitor`, `min_size`, `max_size`, `no_max_size`, `keep_aspect_ratio`,
`fullscreen_state` and `scrolling_width`. The size limits belong with the
window rather than with the rule that set them -- Hyprland clamps at
every point a size could change, and a rule fires once while a window is
resized many times -- so every floating rectangle in the layout goes
through one call that holds it down.

And two that are bookkeeping: `group set` makes a window a group of one
so the next window opened onto it joins it rather than splitting the
workspace, with all seven of Hyprland's group words read as
`applyDynamicRules` reads them; and `no_close_for` holds a window open,
with `killactive` saying why rather than doing nothing.

What is left is `xray` and `no_screen_share`, which do need a second pass
-- the first reads what is behind the frame being drawn and the second
means drawing the frame again without one surface in it -- the five that
belong to a GPU this compositor has not got (`immediate`, `no_vrr`,
`no_auto_hdr`, `tonemap`, `force_rgbx`), `persistent_size`, which needs
state on disk, and the input ones.

**Done — all four tiling layouts, and the options that shape them
(2026-09-17).** Hyprland 0.56 has four: `dwindle`, `master`, `monocle` and
`scrolling`. This compositor had two, and a person who wrote either of the
other names got dwindle with no diagnostic.

**Monocle** is the small one: every window fills the workspace and the
focused one is shown. What makes it a layout rather than a fullscreen
window is that the windows are still tiled -- `cyclenext` walks them,
closing one shows the next, and the gaps and the border are the
workspace's.

**Scrolling** is the largest of the four and the one no other tiling
compositor has: a *tape* of columns wider than the screen, with the screen
a window onto it. A column's width is its own, so a wide editor and a
narrow terminal sit side by side and a third column scrolls in beside them
without either of the first two changing shape.
`calculateCameraOffset` is the rule that makes it look right -- a tape
narrower than the screen is centred rather than pushed left, and a tape
wider than it never scrolls past its own start -- and eleven `layoutmsg`
words move windows between columns, resize them and scroll the tape.

The master layout grew the masters it was always meant to have: `addmaster`
and `removemaster` did nothing, and two masters sharing the master column
is the whole reason those messages exist. With them came
`master:orientation = center` (the masters in the middle with the stack in
two columns beside them, once there are `slave_count_for_center_master` of
them), `center_master_fallback`, `always_keep_position`, `new_on_active`,
`focus_master_on_close` and `allow_small_split`.

And twelve more options across the other categories: `dwindle:split_bias`,
`general:float_gaps`, `misc:background_color`,
`misc:close_special_on_empty`, the two `special_scale_factor`s that make a
scratchpad look like one, `binds:workspace_back_and_forth`,
`binds:hide_special_on_workspace_change`, `binds:allow_pin_fullscreen`,
`binds:movefocus_cycles_fullscreen`,
`binds:window_direction_monitor_fallback`, and `workspace previous`,
`next`, `empty` and `name:` as dispatcher arguments -- `workspace,
previous` being the commonest keybind in any Hyprland configuration after
the numbers themselves.

The whole `binds` category was unreachable before this: `bind` takes its
flags as letters glued to the keyword -- `bindl`, `bindrm`, `bindel` -- and
the parser reached for a bind before an option, so
`binds:workspace_back_and_forth` was read as `bind` with a flag `s` and
answered `invalid flag :`. A keyword never has a colon in it and an option
always does.

**Done — a person's own configuration, run (2026-09-17).** The test of a
clone is not a checklist, it is somebody's real file. This one is
`~/.config/hypr/hyprland.conf` on `nazuna`: 377 lines, 55 binds, two window
rules, a bar, a dock, a wallpaper daemon and a desktop-effects daemon. It
runs, with no diagnostic at all, and the screenshot has waybar across the
top, the wallpaper behind it and a terminal tiled under it with the
gradient border and the graded blur the file asks for.

Getting there found six things, each of which was a person's configuration
being read and quietly not obeyed.

* **The window and layer rules were 0.55's.** 0.56 merged `windowrulev2`
  into `windowrule` and gave `layerrule` the same grammar -- comma-separated
  fields, `match:` for what a thing must be, snake_case names -- and this
  read the older one. `layerrule = ignore_alpha 0.2, match:namespace waybar`
  was refused as a line. Both halves are `Rule.cpp`'s whole matcher list and
  `WindowRuleEffectContainer.cpp`'s whole effect table now, and an effect
  Hyprland has that this compositor does not carry out is kept by name
  rather than refused: one unsupported word must not cost a person the
  matchers written beside it.
* **`suppress_event maximize` had nothing to suppress.** A client asking
  `xdg_toplevel.set_maximized` reached the loop and nothing read it, so the
  rule -- the first line the file writes -- was a rule about a thing that
  never happened. A client asking for fullscreen or maximize gets it now,
  unless a rule says otherwise.
* **`wl_shm_pool.destroy` unmapped the memory.** The protocol says the
  mapping goes when the last *buffer* made from the pool does, and a client
  that makes its buffers and throws the pool away is not unusual -- it is
  what `grim` does between asking for a screenshot and taking it, and what
  most toolkits do. Nothing could take a screenshot of this compositor but
  its own client.
* **`wl_output.description` was a fixed sentence.** A bar told to be on one
  screen matches on the description, not the connector: waybar's `"output"`
  is `Lenovo Group Limited R27qe Gen2 UTP03KBB`, it matched nothing, and
  waybar correctly drew no bar and said nothing about it. A monitor's
  description comes out of its `EDID` now, read through
  `DRM_IOCTL_MODE_GETPROPBLOB`, and `monitor = desc:` matches the start of
  it as `CMonitor::matchesStaticSelector` does.
* **`input:kb_layout = de` was read and never looked at.** Every client was
  handed the `us` keymap and every bind resolved against it, so a German
  keyboard typed `y` where its key says `z`. The probe takes a layout now
  and the compositor ships one keymap per layout, `layout()` picks one, and
  a layout that is not shipped says so rather than quietly typing English.
* **The option table held 71 of Hyprland's 348.** A configuration naming any
  of the other 277 was told `config option does not exist`, which is the
  right answer for a typo and the wrong one for an option Hyprland has. Of
  the 71 that were there, 70 already held Hyprland's default exactly.

And the blur, which is the compositor's whole frame budget, ran over the
whole window whatever the damage said: a terminal's cursor blinking cost 92
milliseconds of a 1920x1080 screen where blurring what changed costs 1.8.
The pixels are the same either way, because a blurred pixel depends on
nothing further than the kernel's reach -- which turned out to be twice what
was being read, so the outermost ring of every blurred window was a blur of
the region's clamped edge rather than of the frame.

**Done — the rest of Hyprland's dispatcher table (2026-09-17).**
Twenty-seven names in Hyprland's `m_dispMap` had no answer here; every one
of them does now. The split is by what they touch. `compositor/layout`
answers the ones that move windows -- `layoutmsg` (both layouts' own
messages: `togglesplit`, `swapsplit`, `movetoroot`, `preselect` for dwindle,
and `swapwithmaster`, `focusmaster`, `mfact`, `orientation*`, `swapnext`,
`rollnext` and the rest for master), `moveintoorcreategroup`,
`movewindoworgroup`, `focusworkspaceoncurrentmonitor`, `movewindowpixel`,
`resizewindowpixel` -- and a new `hyprix::act` answers the ones that reach
past it: starting a program, signalling one, turning a screen off, moving
the pointer, dragging a window with the mouse, writing a line on the event
socket, and ending the session.

Four dispatchers name a window with one of Hyprland's *window expressions*
rather than a direction, and the layout holds neither a title nor a class
nor a process. `hyprix::select` is `CViewQuery::bySelector` written out:
`class:`, `initialclass:`, `title:`, `initialtitle:`, `tag:`, `address:`,
`stableid:`, `pid:`, `floating`, `tiled`, `active`, and a bare expression
read as a class -- each matched against the *whole* field, as RE2 matches
one. To answer `pid:` at all, the socket now asks `SO_PEERCRED` who
connected, which is also why `hyprctl clients` stopped printing `pid: 0`.

Two things this found. The dwindle layout could not exchange two windows, so
`swapwindow` did nothing in the default layout; it swaps the two leaves now,
the way `switchWindows` does, leaving every split's ratio alone. And
`misc:focus_on_activate` is *off* in Hyprland -- a program asking for
another's window makes it urgent rather than taking the focus -- which this
compositor had as always-on; it is the option now, with the urgency list
`focusurgentorlast` reads.

`toggleswallow` keeps its flag and says that swallowing a terminal is not
implemented, because it is not. A dispatcher that quietly did nothing would
be worse than one that says so.

**Done — the protocols a desktop session asks for (2026-09-17).** Eleven
more, each small and each bound by something a person runs. `xdg-output`
gives a bar the screen's *logical* position, size and name, which on a
scaled monitor is not what `wl_output.mode` says. `presentation-time` says
when a frame actually reached the screen, which is what a toolkit that
animates needs and what a frame callback does not say. `ext-idle-notify` and
`idle-inhibit` are the two halves of "is anyone there": a locker waits on
the first, a video player holds it off with the second, and the `forceidle`
dispatcher drives the clock so a person can test a locker without waiting
ten minutes. `single-pixel-buffer` is a `wl_buffer` that is one colour and
has no pool at all. `content-type` and `alpha-modifier` are a client saying
what it is showing and how much of it shows -- the second is drawn, so a
client can fade itself. `xdg-dialog` floats a modal dialog, which is
Hyprland's `windowrule = float, xdg_dialog` said by the protocol itself.
`xdg-system-bell` and `xdg-toplevel-tag` are the terminal bell and the name
a window keeps across restarts. `kde-server-decoration` is KDE's own
`xdg-decoration`, answered with the same `Server`.

**And the frame callbacks, which were never fired.** A client that asks for
`wl_surface.frame` and waits for it before drawing again -- which is every
toolkit -- drew one frame on this compositor and then stopped. The tree's
own clients draw once and never noticed. `foot` now draws five frames in the
same run where it drew one, and every surface on the screen is told: the
windows, the bars, the menus and the lock's own.

**Done — the pointer and keyboard protocols beyond `wl_seat` (2026-09-17).**
`wl_pointer` says where the pointer *is*, which is the wrong question for a
game, a 3D modeller or a remote-desktop viewer: they want how far it moved,
and they want it to stay inside their window while they have it.
`zwp_relative_pointer_v1` and `zwp_pointer_constraints_v1` are that pair, and
both are carried out rather than answered: a locked pointer does not move at
all and the client is told the distance instead, a confined one is clamped to
its window's rectangle, and a one-shot constraint is destroyed by the event
that ends it. A constraint is in force only while its own surface has the
pointer, which is the compositor's judgement and not the client's.

`zwp_keyboard_shortcuts_inhibit_manager_v1` is how a virtual machine or a
nested compositor gets `SUPER` instead of the compositor eating it: while the
inhibiting surface has the keyboard, no bind fires at all.

`zwp_virtual_keyboard_v1` and `zwlr_virtual_pointer_v1` are a client acting
as a device -- `wtype`, `ydotool`, an on-screen keyboard, a remote viewer.
What they report goes to the seat as a person's input would, keybinds and
all, which is what makes them worth having and what wlroots gates behind a
compositor's policy; this one offers it to every client, as Hyprland does.

`zwp_pointer_gestures_v1` is offered and never sent to, and says so: a
touchpad's swipe and pinch come from libinput's gesture recogniser and this
compositor reads evdev directly. A toolkit that binds it and hears nothing
behaves as it does on a machine with a mouse; one that finds no global warns
on every start.

That machinery is also what `pass`, `sendshortcut` and `sendkeystate` needed.
A `wl_keyboard` has one surface at a time, so sending a key to a window that
is not focused means handing it the keyboard for the length of the key and
handing it back -- which is what Hyprland does too. `pass` sends *the key
that fired the bind*, so the seat now carries the trigger through to the
dispatcher.

**And a bug the clipboard boot found while this landed.** A connection
ending compacts the slot list, and three things held a client by its *place*
in that list and were never moved with it: the clipboard's owner, the
session lock's client and the input method's. A window's `Source` and the
keyboard focus had already been fixed for exactly this; these three had not.
What it looked like was a paste answered by nobody while the program that
copied sat waiting to be asked -- once in about ten boots, whenever a
clipboard client happened to exit before another pasted. `Clipboard::renumber`
is host-tested, and the test fails without the fix.

**Done — what a taskbar, a clipboard manager, a night-light and a settings
panel ask (2026-09-17).** Seven more protocols, each bound by a program
people run rather than chosen from a list.

`ext-foreign-toplevel-list-v1` is the window list as the newer specification
has it -- the same job `zwlr_foreign_toplevel_v1` does with the acting-on-a-
window half taken out, and the one a taskbar written this year binds. Both
are published from the same list each pass.

`wlr-data-control` and `ext-data-control` are the clipboard as a *manager*
sees it. `wl_data_device` gives a client the selection only while it has the
keyboard, which is right for an application and wrong for `cliphist` or
`wl-paste --watch`: they have no window at all. Both selections are carried
to them whether or not anything is focused, and a manager can set either as
well as read it. They are the same protocol twice -- wlroots wrote the first
and the `ext` namespace standardised it -- so they are one module with a
table of interfaces, the way `compositor/clip` is one program with a flag.

`wlr-gamma-control` is `gammastep` and `hyprsunset`. The client hands over
three ramps on a descriptor and every pixel is looked up in its channel's
ramp on the way to the screen. On hardware the connector does that; here the
screen is memory, so the compositor does it once a frame over the pixels it
drew -- the same picture by a slower road.

`wlr-output-power-management` is `wlopm`, which is the `dpms` dispatcher
reached from a program instead of a keybind.

`wlr-output-management` is `kanshi` and `wlr-randr`: every screen with its
mode, its position and its scale, published with a serial, and a whole
arrangement taken back at once. A configuration made against a stale serial
is `cancelled` rather than applied, which is the one part of that protocol a
compositor must not skip. Moving a screen is carried out --
`State::move_monitor` keeps the workspaces where they are, because moving a
screen is not unplugging it.

`ext-workspace-v1` is the workspace numbers a bar draws, one group a
monitor, which until now every Hyprland bar read out of `hyprctl`.

**The eighteenth boot.** `zwp_virtual_keyboard_v1` had no proof that a
client's keys reach the seat, and that is the whole point of the protocol.
So: one key starts `/bin/vkbd`, `vkbd` types `SUPER Q` on the Wayland
socket, the bind fires, and `closewindow, title:^(one)$` closes the window
that expression names. The screen must be the picture `compositor/render`
blesses for the window that is left. Two new things in one picture -- a
client acting as a device, and a dispatcher picking a window out by title.

**Done — Hyprland's own protocols (2026-09-17).** Six, each written for
something Hyprland does that no other compositor had a protocol for.

`hyprland-global-shortcuts-v1` is how a screen recorder or a push-to-talk
program has a key without reading the keyboard: it registers a *name*, the
person binds a key to `dispatch global <app_id>:<id>`, and the program hears
`pressed`. That is also what finally makes the `global` dispatcher mean
something.

`hyprland-focus-grab-v1` is a launcher holding the focus on its own surfaces
until a click lands outside them. `hyprland-lock-notify-v1` tells a program
that is *not* the locker when the screen locks -- a recorder or a notifier
has no other way to know, because `ext-session-lock-v1` is the locker's own
protocol and says nothing to anybody else.
`hyprland-toplevel-mapping-v1` joins a toplevel handle from either window
list to the address every other protocol calls that window by.
`hyprland-surface-v1` is a surface asking to be drawn see-through, which is
the same field `wp_alpha_modifier_v1` sets.
`hyprland-toplevel-export-v1` is `zwlr_screencopy_v1` for one *window*,
which is what a recorder uses for "share this window": the same two halves,
answered out of the same pixels, with the window's rectangle in place of a
screen's.

Two are not offered, and the module says why. `hyprland-input-capture-v1`'s
whole conversation is a `libei` socket the compositor hands over, and there
is no `libei` on Ferrix; offering the global and never sending the
descriptor would leave a client waiting for ever.
`hyprland-ctm-control-v1`'s `blocked` event has a `<description>` with no
`summary`, which this `wayland-scanner` refuses -- so its table could not be
checked against libwayland's, and an unchecked table is the one thing the
generator exists to avoid. The colour work it does is
`wlr-gamma-control`'s as well, and that one is offered.

**Done — `layerrule` (2026-09-17).** The `zwlr_layer_shell_v1` half of
`windowrule`. A layer surface has no title and no application id -- it has a
*namespace*, which is what it passed to `get_layer_surface`, and that is what
a rule matches on, as a regular expression.

Three are drawn. `blur` blurs what is behind a translucent bar, which is what
makes one look like Hyprland's, and is a rule rather than the default because
blurring behind an opaque bar costs a pyramid of passes and changes not one
pixel. `abovelock` draws the surface *over* the session lock -- the whole
reason an on-screen keyboard can be used on a lock screen, and until now the
compositor drew nothing over a lock at all. `order` decides where a surface
goes among its own layer's, a higher number nearer the top, with the sort
stable so that surfaces with the same order keep the sequence their clients
made them in.

The rest are read, kept and not acted on, and the module says why each:
`noanim` has nothing to turn off, and `dimaround`, `xray`, `blurpopups` and
`noscreenshare` each need a second render pass -- they read what is *behind*
the frame being drawn, or need the frame drawn again without one surface in
it, and this renderer draws one pass over one canvas. They are parsed rather
than refused so a person's configuration is not a wall of diagnostics.

**Done — drag and drop (2026-09-17).** The most intricate conversation in
core Wayland, and the one every file manager, browser and editor uses.
`wl_data_device.start_drag` was read and dropped; there was no drag at all.

Three objects talk at once and the two clients cannot see each other, so
every step is the compositor's. It makes the *offer* for whichever client
the pointer is over, names every type the source put on it, says what the
source can do, and enters -- in that order, because a client reads the types
inside its `enter` handler and one told afterwards would have nothing to
read. It tells the source which type that client said it would take, settles
the action the two agreed on (the target's preference where both offered
it), and, when the last button comes up, tells one to drop and the other
that the drop happened. A drop over nothing, or over a client that would
take no type, cancels the source -- which is what stops a file manager
deleting the original after a move that went nowhere.

Two rules that are easy to miss and were written down here. While a drag is
on, the pointer enters and leaves nothing: a window told `wl_pointer.enter`
mid-drag would think the person had clicked it, so the pointer's own events
stop for the length of the drag. And the offer is destroyed by the `leave`,
so a client that walked the pointer across three windows does not end up
holding three offers.

The icon the source gave is drawn at the pointer and under it, because what
a drag *looks* like is a thing following the pointer.

**Done — the last of the protocols (2026-09-17).** Nine more, which brings
what this compositor offers to **60 globals** against Hyprland's 60.

`wp_pointer_warp_v1` is a client putting the pointer somewhere *inside its
own window*, which a game's settings panel and a drawing program both want;
the surface has to be one the client owns, and one it does not is a protocol
error. `ext-background-effect-v1` is a surface asking for what is behind it
to be blurred -- `layerrule = blur` said by the protocol instead of by the
person -- and it is drawn.

`tearing-control-v1`, `fifo-v1` and `commit-timing-v1` are a client saying
how it would like its frames scheduled. Each is read and recorded and acted
on by nothing, and the module says why: acting on any of them means choosing
*when* to put a frame on the screen, and this compositor draws when
something changed and presents at once, which is what a software renderer
with no vertical blank can do.

`wp_security_context_manager_v1` is a sandbox handing over a socket of its
own; the descriptors are taken and closed and the sandbox is named in the
log, because this compositor accepts on one listener and telling a flatpak's
clients apart would mean a second. `vicinae-hotkey-v1` is a launcher asking
for a key by keysym rather than by registering a name.

`ext-image-capture-source-v1` and `ext-image-copy-capture-v1` are
screenshots as the `ext` namespace has them: a *source* -- a screen, or a
window from either toplevel list -- and a session that copies frames out of
it one after another. That is what a recorder actually needs, and it is what
a `grim` or an `xdg-desktop-portal` written this year binds. It is answered
out of the same pixels `zwlr_screencopy_v1` is.

**What is left, and why.** Six of Hyprland's globals are not offered.
`wl_drm`, `wp_linux_drm_syncobj_manager_v1`, `wp_color_manager_v1` and
`xwayland_shell_v1` are the GPU path and XWayland, neither of which exists
on Ferrix yet. `hyprland-input-capture-v1`'s whole conversation is a `libei`
socket, and there is no `libei`. `hyprland-ctm-control-v1`'s XML has a
`<description>` with no `summary`, which this `wayland-scanner` refuses, so
its table could not be checked against libwayland's -- and an unchecked
table is the one thing the generator exists to avoid.

**Done — a terminal (2026-09-17).** Stage 18's exit asked for one, and it
needed pseudoterminals the kernel did not have. It has them now:
`/dev/ptmx` gives a master, `TIOCGPTN` says which pair it is, `TIOCSPTLCK`
unlocks it and `/dev/pts/<n>` is the slave. What the master writes goes
through the same line discipline the console's terminal has -- `ICANON`,
`ECHO`, `ISIG` and the rest, honoured exactly as they are there -- and the
echo goes back to the master, because on a pseudoterminal the *terminal* is
the program at that end. What the slave writes has `OPOST` applied and is
read by the master. Closing the master takes the pair away: the slave's
reads end and its writes fail, which is what a shell reads as "the terminal
has gone". The slave answers every terminal request, including the
session and process-group ones, with a pair's own session and foreground
group; the master answers those that act on the pair, as Linux's does.

`compositor/term` is the terminal: a character grid with the escape
sequences a shell and its programs actually send (the cursor, the erases,
the colours, the cursor's visibility), drawn with `libs/fbtext`'s Spleen
font -- the same one the kernel's panic screen uses, so the image carries
one typeface -- into a `wl_shm` buffer. It starts a program on a pair with
the slave for its session and its three descriptors, sends what is typed
back through the master, and tells the program when the window is resized.
`--headless` runs the program with no window at all, which is what
`cargo xtask test-pty` boots: a program's output, through the pair, into the
grid, printed a row at a time.

`cargo xtask test-compositor` boots a ninth time with `exec-once = /bin/term
/bin/hyprctl` and requires the picture the terminal makes, pixel for pixel,
on x86-64 and on AArch64.

**Done — the eight protocols a real toolkit asked for (2026-09-17).** Not
chosen from a list: `foot`, a Wayland terminal written against libwayland and
every other compositor, prints a warning line for each protocol it wanted and
did not find. It printed six, and the seventh and eighth are the two halves
of the one it named last. `hyprix/probe/real-client.txt` is that log, and it
now has no warning in it at all.

* **`wp_cursor_shape_v1`** -- a client *naming* the cursor it wants rather
  than drawing one, which is what a toolkit would rather do: it has no idea
  what the person's theme looks like and the compositor does. This one draws
  its own arrow for every shape and says which was asked for; there is one
  shape and no theme to pick another from.
* **`zwp_primary_selection_device_manager_v1`** -- the middle-click paste,
  which is the clipboard's older and simpler sibling and the same protocol
  under another name. `compositor/clip` grew `--primary` rather than a twin,
  and the compositor's clipboard carries both selections apart.
* **`xdg_activation_v1`** -- one program asking for another's window to be
  raised: a link opened from a chat window raising the browser. The token is
  a string the compositor makes and only it can make, and one it did not make
  is refused, which is the whole of what stops any program stealing the focus
  whenever it likes.
* **`wp_viewporter`** and **`wp_fractional_scale_v1`** -- a client saying its
  buffer is to be cropped or scaled into its surface, and the compositor
  telling it a scale that need not be a whole number. This compositor's
  monitor scales are whole, so the preferred scale is that number in the
  protocol's 120ths and is sent at once rather than left for the client to
  wait on.
* **`xdg_toplevel_icon_v1`** -- the icon a taskbar draws beside a window's
  name. The name is kept, which is what a taskbar looks up in an icon theme;
  the buffers are accepted and not kept, because this compositor draws no
  icon itself and holding a client's pixels for something nobody draws is
  memory nobody asked for.
* **`zwp_text_input_v3`** and **`zwp_input_method_v2`** -- the two halves of
  typing through an input method. The application says it wants text, the
  method says what was typed, and the compositor is what joins them: they are
  two connections and neither can see the other. One input method a seat; a
  second is told `unavailable`. With none running, a text field is told
  nothing, which is a session with no IME and is the truth rather than a
  pretence.

The eleventh boot of `cargo xtask test-compositor` now copies to both
selections and pastes each back, with different text in each: a compositor
that answered a primary paste from the clipboard would pass with one string
and fail with two.

**Done — the pointer (2026-09-17).** A compositor with a mouse and no arrow
on the screen is one a person cannot use, and there was none: `wl_pointer`
carried motion and buttons to the clients and nothing was ever drawn.

The arrow is in `compositor/render`, in code, as a shape rather than as a
file: Hyprland loads an XCursor or a `hyprcursor` theme and Ferrix has
neither the files nor a library to read them with, so the one this draws is
written out -- a 24x24 left-pointing arrow with a black outline and a white
fill, every pixel opaque or clear, its tip at the pointer. A client replaces
it with `wl_pointer.set_cursor`, which is how a text field shows an I-beam
and a link a hand; a client that asks for a null surface gets no pointer at
all, which is what a video player playing full screen does.

It is drawn over everything -- windows, bars and menus -- because a pointer
that goes under a menu is one nobody can follow, and it is not drawn while
the session is locked, because a lock screen draws its own.

**It is also not drawn until it has moved.** The seat starts the pointer in
the middle of the screen, which is a guess: nothing has said where the mouse
is until a device does. An arrow drawn at a guess is worse than none, and a
machine with a mouse plugged in and never touched should look like a machine
with no mouse.

`cargo xtask test-compositor` boots a seventeenth time and takes two
pictures: the tiled pair with nothing on it, and then -- after QMP moves the
mouse -- the same pair with the arrow's tip where it was put, which must be
the picture `compositor/render` blesses for exactly that.

**Done — menus (2026-09-17).** `xdg_popup`, which is what every right-click
menu, dropdown, tooltip and combo box in every toolkit is. The objects were
being made and nothing else: a positioner was a bag of numbers nobody read,
`get_popup` handed back an id and never a `configure`, and a client that
asked for a menu waited for ever. On a compositor like that a person right-
clicks and nothing happens.

Where a popup goes is `xdg_positioner`'s arithmetic and nothing else -- a
rectangle on the parent to hang off, a point of it to anchor to, a direction
to grow in, an offset, and what to do when the result falls off the screen
-- so it is written in `compositor/layout` with the rest of the geometry,
where it is tested against the rules rather than against a screenshot. The
order is the protocol's: anchor, offset, gravity, then `flip`, `slide` and
`resize`, each on the axis that is off the screen and only if the client
asked for it. A flip that would not help either is not made, which is what
stops a menu jumping to the other side for no gain; a slide takes the far
edge first, so a popup wider than the screen ends flush with the near one;
a resize is the last resort and the only one that gives the client
something other than the size it asked for.

The compositor places each popup against its parent's rectangle -- a
window's, or another popup's, since a submenu is a popup on a popup --
clips it to the monitor that parent is on, and draws it over the windows
with no border and no gaps, which is what a menu is. `grab`, `reposition`
and `popup_done` are all answered; `set_parent_size` and
`set_parent_configure` are read and dropped, because this compositor places
a popup against the parent's geometry as it is.

`cargo xtask test-compositor` boots a sixteenth time with `exec-once =
/bin/pattern checkerboard one --menu 200`: the window asks for a menu the
moment it has drawn, as a toolkit would, and the screen must be the picture
`compositor/render` blesses -- which it builds by calling the same placement
rules -- with the client having been told where it was put. A host test
compares a screenshot of the same thing.

**Done — the screen lock (2026-09-17).** `ext-session-lock-v1`, which is
what `hyprlock` speaks and `swaylock` speaks, and the one protocol whose
whole point is that the compositor stops drawing everything else.

A program binds the manager and asks for the lock; from that moment the
compositor draws no window, no bar and nothing of what was on the screen a
second ago -- *before* the program has drawn anything, which is the part the
protocol is most particular about. It makes an `ext_session_lock_surface_v1`
for each screen, is configured at that screen's exact size, and when every
screen is covered it is told `locked`, which is the compositor's judgement
and nobody else's. `unlock_and_destroy` gives the screen back.

The keyboard goes with it. While the session is locked the only binds that
fire are the ones written `bindl` -- which is what that flag has always been
for, and what keeps the volume keys working on a locked screen -- and every
other key goes to the lock's own surface and to no window. `hyprctl locked`
says so.

A lock whose program *dies* leaves the screen locked with nothing drawn on
it. That is the one thing the protocol insists on, and the one thing a
compositor gets wrong by doing nothing: an unlocked session is not what a
crash is allowed to produce. A second program asking to lock while one is
held is sent `finished` and refused.

`compositor/lock` is `hyprlock` with the password taken out: it locks, draws
a checkerboard over every screen, holds it, and unlocks. It asks for no
password because Ferrix has no notion of one yet; the part that can be
tested is the part that matters to the compositor.

`cargo xtask test-compositor` boots a fifteenth time and takes three
pictures: the windows, the lock over them, and the windows again. The middle
one must be the picture `compositor/render` blesses for a locked screen and
not one pixel of either window, and a keybind pressed while the screen was
locked must have done nothing -- the window it would have closed is still
there in the third. On x86-64 and on AArch64. A host test compares a
*screenshot* of the locked screen against the same image, which is the
compositor's own answer to a program rather than what QEMU read off the
scanout.

**Done — who draws the title bar (2026-09-17).**
`zxdg_decoration_manager_v1` was vendored, generated and checked against
libwayland, and never offered: the compositor's list of globals did not have
it. That is a gap a screenshot does not show and a person notices at once,
because a toolkit that does not find the manager assumes the job is its own
and draws a title bar, a shadow and a resize border *inside* the rectangle
the tiling gave it. GTK and Qt both do.

It is offered now, and the answer is always `server_side`: a tiling
compositor draws the border and the client draws nothing. The decoration is
configured the moment it is made, which the protocol allows and which saves
a round trip before the first frame, and a client that asks for
`client_side` is told `server_side` just the same -- the answer does not
depend on what was asked. A mode that is neither is the interface's own
`invalid_mode`.

**Done — the rest of what `hyprctl` reads (2026-09-17).** Seven more
commands, which between them are what a bar and a script ask for that is not
a window: `binds`, `devices`, `layers`, `cursorpos`, `locked`,
`workspacerules` and `globalshortcuts`. Each is answered in Hyprland's own
shape, readable and JSON, with its field names -- a script reads them by
name, and a close-enough name is a script that prints nothing.

`binds` lists what the *configuration* parsed rather than what the seat
resolved, as Hyprland's does, so a bind naming a key this keymap does not
have is still listed -- which is what makes the list worth reading when a
bind is not firing. `devices` puts each `/dev/input/eventN` in the group its
capabilities put it in, which is libinput's rule and the one the seat
already uses. `layers` groups by monitor and then by
`zwlr_layer_shell_v1`'s four levels, which is how a bar finds its own
surface. The last two have nothing to list -- there is no `workspacerule`
keyword yet and no global-shortcuts protocol -- and are answered with an
empty list rather than `unknown request`, because a bar asking for them
should get an empty answer and carry on.

**And the rest of them (2026-09-17).** `getoption` answers in Hyprland's own
shape -- the value under the key its *type* names, with a `set` flag saying
whether the configuration said anything or it is the table's default, which
is the one field a script reads to know whether a line took effect.
`descriptions` lists every option with its type and value; Hyprland prints a
sentence about each, written beside its default in its own table, and this
compositor's table has no such sentences, so what is printed is the truth
rather than a row of empty strings. `animations` prints the whole tree with
the `overridden` flag and then the beziers, which is how a person finds out
that their `animation =` line named a bezier that does not exist.
`configerrors` is what could not be read, `rollinglog` the last five hundred
lines the compositor said -- everything it says now goes to whoever is
watching *and* into a rolling buffer -- and `systeminfo` and `status` what it
is and how long it has been up. `globalshortcuts` is no longer empty:
`hyprland-global-shortcuts-v1` fills it.

`notify`, `dismissnotify` and `seterror` come back for the compositor, which
says the message and puts it on the event socket. Hyprland draws a rectangle
over everything; this compositor draws none, and a line a notification daemon
or a bar can pick up is more use than an overlay only this compositor can
draw. `decorations` lists what is drawn around a window, which here is a
border and nothing else.

`getprop` answers one property of one window -- the value bare in the
readable form and under its own key in JSON, which is Hyprland's shape --
and a property nothing set is answered with the compositor's own, since that
is what a person asking "what is this window drawn with" wants to know.

Four say plainly that they act on something this compositor does not have,
rather than pretending or refusing: `switchxkblayout` (this keymap has one
layout), `output` (the screens are the card's), `setcursor` (the cursor is
drawn in code and there is no theme) and `kill` (click-to-kill needs a
pointer grab).

That leaves **two** of Hyprland's thirty-eight unanswered: `eval` and
`repl`, which are its plugin console. They need a scripting runtime, and
this compositor's plugins are programs it starts and talks to over the
control socket.

The second boot of `cargo xtask test-compositor` has the bar, so it is the
one that asks: a keybind runs `hyprctl --batch binds ; devices ; layers ;
cursorpos ; locked`, and the transcript must name the bind's dispatcher and
key, the keyboard QEMU published, the bar's own layer at level 2 under the
namespace it asked for, and the pointer in the middle of the screen -- with
the picture unchanged, because asking a compositor about itself must move
nothing.

**Done — screenshots (2026-09-17).** `zwlr_screencopy_v1`, which is what
`grim` speaks, what `hyprshot` wraps, and what every screen recorder and
screen-sharing portal on wlroots goes through. The compositor says what
buffer to make -- `XRGB8888` at the screen's size, since its canvas is
opaque and an alpha channel that is always `0xFF` is a larger file saying
the same thing -- the client makes one in `wl_shm` and hands it over, and
the compositor writes the screen into it row by row and answers `ready`.
`capture_output_region` is the same with the rows clipped. A frame may be
copied into once: a second `copy` is `already_used`, which is a protocol
error, because a client that sent one has lost track of an object it owns.

This is the one place the compositor *writes* into a client's memory, and it
is a mapping of its own: `compositor/hyprix`'s pool mapping is read-only and
says why, so a screenshot maps the same pool a second time, writable, for
exactly as long as the copy takes. The rule that the compositor never writes
into a window's buffer still holds everywhere else.

`compositor/shot` is `grim` without the file format: it binds the manager
and a `wl_output`, makes the buffer it is told to make, and reads back what
was written into it. It prints the size and an FNV digest of every pixel,
which is how a whole screen is compared through a serial port.

And that is the strongest picture proof in this tree. Every other boot
compares QEMU's *screendump*, which reads the virtio-gpu's scanout; this
compares what the compositor handed a program **through the Wayland
protocol**, against the image `compositor/render` builds on the host by
calling the renderer with rectangles. A compositor that drew the right thing
and answered screencopy with rubbish is caught here and nowhere else.

`cargo xtask test-compositor` boots a fourteenth time: a keybind runs
`/bin/shot`, and the digest it prints must be the digest of the expected
image, on x86-64 and on AArch64. A host test in `compositor/hyprix` compares
the screenshot against the same image pixel by pixel.

**And a second real bug came out of it.** The seat turned a whole batch of
input events into actions *before* any dispatcher ran, so a key pressed
after `submap` in the same batch was judged against the map that was in
force before it. On a machine fast enough to see each key on its own the two
are the same; under emulation a whole sequence arrives in one read, which is
where it was found. Each input is now carried out before the next is read,
which is what Hyprland does and what anything a dispatcher changes about the
meaning of the *next* key requires.

**Done — the window list a bar draws (2026-09-17).**
`zwlr_foreign_toplevel_management_v1`, which is the other half of a bar's
job: `zwlr_layer_shell_v1` puts the bar on the screen, and this tells it what
to draw on it. Waybar's `wlr/taskbar`, eww's window list and every panel that
shows what is open read this protocol and nothing else, so a compositor that
does not offer it is one whose bar shows a clock and an empty strip.

The compositor makes a `zwlr_foreign_toplevel_handle_v1` for each window --
a server-side object, as it must be, out of the server's half of the id space
-- and sends the title, the application id and the four states, followed by
`done`, which is what makes the three one atomic change. A client is told
about the windows that already exist *inside the pass its bind arrived in*,
before the `wl_display.sync` every such client sends after binding is
answered: a list that arrives after that callback is a list the client has
already stopped waiting for. Nothing is written to a client that has already
been told the same thing, because a bar woken by `done` on every frame of an
animation is a bar that burns a core.

The requests come back the other way: `activate` is `focuswindow`, `close` is
`killactive`, and the two state requests focus the window and then run the
dispatcher a keybind would. `set_rectangle` is accepted and dropped -- it
says where the window's icon is on the bar so a minimise can fly to it, and
there is no such animation here -- and `maximized` and `minimized` are
reported false and refused rather than faked, because this layout has one
fullscreen state and no minimised one, and a wrong tick in a taskbar's menu
is worse than none.

`compositor/lswt` is `lswt`, Leon Henrik Plickat's "list wayland toplevels",
which is a taskbar with the drawing taken out: `lswt` prints a line a window,
`lswt activate <title>` focuses one and `lswt close <title>` asks one to
close. It is how the protocol is tested with no screen and no panel.

**It found two real bugs, which is why the protocol was worth writing.**
Both are about a client *leaving*, which nothing in the tree had tested:
every picture until now was made by clients that all stayed.

The compositor holds a client by its *place* in the list of connections --
a window's `Source`, the keyboard focus -- and a connection that ended was
taken out of that list with `retain`, which moves every connection after it
down one. Every such place then named the wrong client: a window that
outlived an earlier client was drawn from somebody else's buffer and typed
into by somebody else's keyboard. The places are renumbered in step with the
list now.

And the window left behind was never told it had grown. The loop
reconfigures the workspace when something changed, and a connection ending
was handled *after* that, so the one case where the layout changes without
anyone asking -- a neighbour going away -- set the flag a moment too late.
The window kept drawing at the size it had and the compositor drew that
buffer into a rectangle twice as wide. The connection that ended is handled
before the reconfigure now, with everything else that changes the layout.

`cargo xtask test-compositor` boots a thirteenth time: a keybind runs
`/bin/lswt`, which must name both windows with the focused one marked, and a
second keybind runs `/bin/lswt close one`, after which the screen must be the
picture `compositor/render` blesses for one window left alone -- on x86-64
and on AArch64. A host test in `compositor/hyprix` runs the same program
against the compositor in one process and requires the window that went to be
the one it named.

**Done — submaps (2026-09-17).** Hyprland's modal keybinding: `submap =
resize` puts every bind after it in a map of its own, `submap = reset` goes
back to the global one, and the `submap` dispatcher moves between them while
the compositor runs. Only one map is in force at a time -- while a submap is
entered the global binds do not fire and the submap's do -- which is what
makes it a mode a person is *in* rather than a prefix they hold.

The configuration already parsed the keyword and the `u` flag, and every
bind written in a submap was being dropped on the floor, so a `hyprland.conf`
with a resize mode in it started a compositor where those keys did nothing
and nothing said why. They are kept now and gated at match time.

The details are `setSubmap`'s. A name nothing was bound in is refused with
Hyprland's own sentence -- `Cannot set submap <name>, submap doesn't exist
(wasn't registered!)` -- rather than entered, because entering one leaves a
keyboard on which nothing works and no bind written to get out of it. The
`u` flag fires whichever map is in force, which is how the bind that leaves
a submap is written once. Reading the configuration again leaves the map, as
it must: a submap the new file does not have is one nothing could leave.
`hyprctl submap` prints the name or `default`, and in JSON a bare string, as
`submapRequest` does; the event socket says `submap>>resize` on entering and
`submap>>` on leaving. What is not done is the per-submap `reset` target
0.56 added, which leaves a map automatically after any bind in it fires.

`cargo xtask test-compositor` boots a twelfth time with `L` bound in the
submap and nowhere else: it does nothing before `SUPER R` is pressed, swaps
the windows after it, and does nothing again once `Escape` has left the map,
with `C` bound in both maps to `hyprctl submap` so that one key names the
map it was pressed in -- `default`, then `resize` -- and the event socket
carrying both changes. On x86-64 and on AArch64.

**Done — the clipboard (2026-09-17).** Copy and paste between two programs,
which is `wl_data_device_manager`'s selection and the thing a person notices
is missing before anything else on this list.

Wayland's clipboard is a promise and not a buffer, and the implementation is
shaped by that: the program that copies keeps the data and says which types
it can give it in; the compositor remembers *who* that is and tells every
client with a `wl_data_device` what is on offer; a program that pastes asks
for a type and hands over a pipe, and the compositor passes that pipe to
whoever copied, who writes to it and closes it. No byte of what is copied
ever passes through the compositor, which is the point -- a selection can be
a gigabyte of video and the compositor's memory does not move.

`wl_data_device_manager`, `wl_data_device`, `wl_data_source` and
`wl_data_offer` are all four implemented for the selection. The offer is a
server-side object, as it must be: the compositor makes it, names it in
`wl_data_device.data_offer`, sends an `offer` event per type and then
`selection`, which is the order libwayland's own clients rely on. A client
that copies while another holds the selection is given it and the previous
owner is sent `cancelled`; a client that goes away while holding it clears
it. Drag-and-drop is the other half of the same four interfaces and is not
done: nothing here is dragged.

`compositor/clip` is `wl-copy` and `wl-paste`, neither of which is on Ferrix:
`clip copy <text>` offers the text as `text/plain;charset=utf-8` and stays
alive to answer, because it must; `clip paste` waits to be told what the
selection holds, asks for it on a pipe it makes, and prints what comes back.

Getting it right was a question of who owns a descriptor. One that arrives
over a socket is owned by `compositor/socket`'s `Connection` until a message
claims it, and a claim is what `Connection::consume`'s second argument says:
a program that reads an event carrying a descriptor and then forgets to say
so has the same descriptor owned twice, closed twice, and -- since Rust 1.86
checks -- aborts the process. Two places had it wrong and both are fixed:
`compositor/clip` now claims what `Reader::descriptors_taken` counted, and
the compositor no longer claims for the two clipboard events that carry no
descriptor at all.

`cargo xtask test-compositor` boots an eleventh time with `exec-once =
/bin/clip copy ...` and `exec-once = /bin/clip paste` beside the two windows,
and requires that the compositor say it took the selection and passed the
pipe on, that the copying program say it was asked for its data exactly once,
that the pasting program print the text the other one copied, and that the
windows still be drawn pixel for pixel while all of that happens -- on x86-64
and on AArch64. A host test in `compositor/hyprix` runs the same two programs
against the compositor in one process.

**Begun — plugins (2026-09-17).** A plugin is a program the compositor
starts and talks to, not a shared object it loads into itself. Hyprland's
plugins are C++ objects `dlopen`ed into the compositor, which hook its own
functions; Ferrix's programs are statically linked and there is no dynamic
loader to `dlopen` with, so a plugin that is a shared object is a plugin that
cannot be loaded on the operating system this compositor is for. The other
half of the reason is that a `dlopen`ed plugin takes the compositor down with
it when it dereferences a bad pointer, and one at the end of a socket cannot.

The keyword is Hyprland's: `plugin = /bin/plug` starts the program, after the
sockets are bound and before `exec-once`. The plugin connects to the same
`.socket.sock` every `hyprctl` connects to and keeps the connection:

* `[[PLUGIN]]name,author,version,description` -- what `PLUGIN_INIT` returns
  in Hyprland, in one line;
* `handle <dispatcher>` -- Hyprland's `addDispatcher`: from then on
  `dispatch <name>`, from a keybind or from `hyprctl`, is written to the
  plugin as `dispatch>><name>,<argument>` rather than refused as unknown;
* `subscribe` -- `registerCallbackDynamic`: the lines `.socket2.sock`
  carries, on this connection;
* anything else -- an ordinary request, so a plugin asks `clients` and runs
  `dispatch movewindow r` the way a bar does.

`hyprctl plugin list` prints them in Hyprland's own shape, with a
`Dispatchers:` line Hyprland has no need for. A plugin that goes takes its
dispatchers with it and the compositor carries on.

`compositor/plug` is the example: it adds `swapthem`, and answers it with the
two dispatchers that exchange the focused window with its neighbour.
`cargo xtask test-compositor` boots a seventh time with `plugin = /bin/plug`
and `bind = SUPER, P, swapthem` -- a dispatcher nothing in the compositor
knows -- and requires the picture a swap makes, pixel for pixel, on x86-64
and on AArch64.

**Begun — scaled monitors (2026-09-17).** `monitor = name, resolution,
position, scale` is read: the name (empty for every monitor no other rule
names), `disable`, the resolution as `preferred` or `WxH[@R]`, the position
as `auto` or `XxY`, and the scale as `auto` or a number. What is not done --
`transform`, `mirror`, `auto-left` and the rest -- says so rather than being
read as if it were not there.

A scaled monitor is laid out in logical pixels and drawn in the screen's
own: a 1024x768 screen at `scale = 2` tiles its windows in 512x384 and draws
each of those pixels as two, with the border, the rounding, the shadow and
the blur scaled with them, as Hyprland scales its decorations by the
monitor's scale. Everything above the renderer -- the layouts, the
dispatchers, `hyprctl`, the layer surfaces -- works in logical pixels and
never learns the difference.

The clients are told: each `wl_output` carries its own scale, and
`compositor/pattern` now reads it, sends a buffer that many times the size
and says so with `wl_surface.set_buffer_scale`, which is what a client on a
scaled monitor does. `hyprctl monitors` prints the scale it is at.

`cargo xtask test-compositor` boots a sixth time with `monitor = ,
preferred, auto, 2` and requires the picture `compositor/render`'s own tests
bless for a scaled monitor, pixel for pixel, on x86-64 and on AArch64.

**Begun — more than one monitor (2026-09-17).** A screen a connected
connector, across every card: the compositor opens every `/dev/dri/cardN`,
takes each connected connector with a mode and a CRTC of its own, and drives
one canvas and one pair of dumb buffers for each. The monitors are laid out
side by side from the left in the order the kernel lists them, which is
Hyprland's `auto`, and each is named after its connector -- `Virtual-1`,
`Virtual-2` -- with each connector type numbered from one across the whole
machine, as wlroots numbers outputs.

The card grew heads to match: `/dev/dri/cardN` publishes one connector, one
encoder, one CRTC and one primary plane per scanout the driver reported,
each in a block of four ids of its own, and `SETCRTC` and `PAGE_FLIP` carry
the head's scanout number down to the driver. `docs/DISPLAY.md` §2.3 has the
table.

Every monitor is a `wl_output` global of its own, so a client is told there
are two screens and which is which, and a layer surface is placed on the
screen its `wl_output` names -- a bar on one monitor reserves a strip of that
monitor and moves no window on the next. `hyprctl monitors` lists them all
with their names, positions and active workspaces, and the event socket
announces each.

The dispatchers that name a monitor are Hyprland's, with
`CMonitorQueryCore::fromConfigString`'s argument forms -- `current`, a
direction, `+N`/`-N` along the list, an id counting from zero, or a name:
`focusmonitor`, `movewindow mon:<monitor>` with `silent`,
`movecurrentworkspacetomonitor`, `moveworkspacetomonitor` and
`swapactiveworkspaces`.

The proof is a fifth boot of `cargo xtask test-compositor`: two virtio-gpu
devices, so two cards and two monitors in the guest, two windows tiled on the
first, and then a keybind moving one to the second -- with each screen
required, pixel for pixel, to be the picture `compositor/render`'s own tests
bless for it, on x86-64 and on AArch64. QEMU enables a second *output* of one
virtio-gpu only when a host window manager resizes its window, which a
headless test cannot do; two devices are two consoles, and a screendump names
each.

**Begun — window rules (2026-09-17).** `windowrule = <effect> [value],
match:<prop> <value>, ...`, which is Hyprland 0.56's own form: comma-separated
fields, each a name and a value, with `match:` in front of the ones the
window must be. `windowrulev2` is refused with Hyprland's own sentence,
because 0.56 merged the two syntaxes and took the old one away.

Matching is by regular expression for the four names a window has -- `class`,
`title`, `initial_class`, `initial_title` -- and a yes-or-no for `float`,
`fullscreen` and `focus`. The expressions are `compositor/regex`'s, which is
RE2's syntax as far as a window rule uses it: literals and escapes, `.`,
classes with ranges and negation, `*`, `+`, `?`, groups with alternatives,
and the anchors every rule carries and a full match makes redundant. What is
not done -- counted repetition, `\d`, lookaround, non-greedy -- is refused
with a sentence rather than matched wrongly, because a rule that silently
matched everything would float every window a person owns. The matcher
counts its steps and gives up rather than hanging the compositor on a
pattern that backtracks for ever.

A rule is applied where Hyprland applies one: when the window maps, which is
where the client has finished saying what it is called. `float`, `tile`,
`size`, `move`, `center`, `workspace` (with `silent`), `fullscreen`,
`maximize` and `no_focus` go to the layout, through the same calls a
dispatcher makes; `opacity`, `rounding`, `border_size`, `no_blur`,
`no_shadow` and `no_dim` go to the renderer, which now draws each window
with what a rule gave it and every other window with the configuration's
style.

`cargo xtask test-compositor` boots a tenth time with six rules -- one
window floated at a size and a place and drawn at `opacity 0.6`, the other
with its corners cut and no shadow -- and requires the picture they make.
`compositor/render` blesses it by calling `State::float_window` and handing
the renderer the same per-window styles, which is what the rules do, so the
two pictures are made by one piece of code.

**Begun — window groups (2026-09-17).** Hyprland's tabs: windows that share
one slot in the tiling, of which one is drawn. Only the head is in the
dwindle tree, and the slot draws whichever member is active, so cycling a
group changes the picture and not the layout -- a group that moved the
windows each time would be a workspace switch with extra steps.

`togglegroup` makes a group of the focused window and dissolves the one it is
in, putting every member back beside the head; `moveintogroup <direction>`
takes the neighbour in that direction and adds the focused window to its
group; `moveoutofgroup` puts one back in the tiling, and a group of one is no
group, which is what Hyprland leaves behind; `changegroupactive
[f|b|<index>]` cycles, wrapping both ways, with the index one-based as
Hyprland's is; `lockgroups lock|unlock|toggle` is read and reported. Every
direction search maps a grouped window through its slot, so `movefocus` and
`movewindow` work from inside a group and move the whole of it.

`hyprctl clients` grew the `grouped` field and the `hidden` one that goes
with it: the members a group does not draw are listed as hidden windows with
the group's box rather than left out, which is what a bar drawing the tabs
reads. The event socket says `togglegroup>>1,<head>` when one is made,
`moveintogroup>><window>` and `moveoutofgroup>><window>` as it fills and
empties, and `togglegroup>>0,<head>` when it goes. `hyprctl --batch` landed
with them, because one keybind running three dispatchers over the control
socket is how the Ferrix proof presses them.

`cargo xtask test-compositor` boots a fourth time: two windows tiled, one
keybind, and then both of them in one slot with the one that was moved in
drawn -- pixel for pixel the image `compositor/render`'s own tests bless, on
x86-64 and on AArch64, with `hyprctl clients` naming the group from inside
the guest and the event socket carrying both events.

**Begun — shadows, dimming and blur (2026-09-17).** The three decorations
that needed no GPU, each ported from the shader that is the only description
of it there is.

`decoration:shadow:*` is `shadow.glsl`'s `getShadow` and
`pixAlphaRoundedDistance`: a box the window's rectangle grown by
`shadow:range`, with a falloff of `((radius − d) / range)^power` in the
corners and `(smallest / range)^power` along the edges, `radius` being the
range plus the window's own rounding. Drawn under the border and the window,
as Hyprland draws it. This is the one place in the renderer that blends a
pixel by hand -- every pixel has an alpha of its own and tiny-skia's shaders
take one colour for a rectangle -- so the arithmetic is written out to be the
same source-over its `f32` pipeline does.

`decoration:dim_inactive` and `dim_strength` lay black over a window that is
not focused, over its surface and inside its rounding.

`decoration:blur:*` is the dual-Kawase pair, `blur1.glsl`'s five taps down
and `blur2.glsl`'s eight up, `blur:passes` times each way at `blur:size`.
It reads the frame so far from under a translucent window and writes it back
before the window is drawn, which is what Hyprland does and why the pass has
to sit between the two. The two passes do *not* use the same offsets --
`blur1`'s `halfpixel` is four times `blur2`'s -- and using one for both turns
a bright block into a dark hole with a bright halo, which is what this looked
like before the shaders were read again. The colour grading (`noise`,
`contrast`, `brightness`, `vibrancy`) is not done: it changes the blur's
colour and not its shape.

The compositor now reports its slowest frame in microseconds, which is the
stated bound this stage asks each software effect to have: a number measured
on the machine that ran it rather than one somebody hoped for.

**Begun — special workspaces (2026-09-17).** Hyprland's scratchpad: a
workspace shown *over* the monitor's own rather than instead of it, with a
negative id and a `special:` name. `togglespecialworkspace [name]` shows and
hides it, `workspace special:name` and `movetoworkspace special:name` reach
it, and `hyprctl monitors` says which one a monitor has over it -- in the
readable form and in the JSON, both in Hyprland's own shape, because a bar
reads that field to know whether the scratchpad is up.

The ids are Hyprland's: `special:special` is `SPECIAL_WORKSPACE_START`, −99,
and every other name counts up from there towards −2. Three things had to
change for a workspace that is shown beside another rather than instead of
it: focusing a window on one shows it rather than switching to it, a monitor
showing one has two workspaces the focus can be on, and an empty one is not
pruned while it is being shown -- an empty scratchpad is a scratchpad you can
put something in.

**Begun — animations with Hyprland's curves (2026-09-17).** `compositor/anim`
is the curves, the tree and the values they move, and it holds no window and
no clock: a value is asked what it is at a time the caller gives it, so every
curve and every inheritance rule is host-tested.

The curve is `hyprutils`' `CBezierCurve` to the point: 255 points baked at
`t = (i + 1) / 255`, the binary search over their `x`s, and the linear
interpolation between two of them. `default` is `DEFAULTBEZIERPOINTS`,
`(0, 0.75)` and `(0.15, 1.0)`, which puts a quarter of the time at 0.843 of
the distance -- a number worked out from the control points by hand and
pinned by a test, because a curve that is nearly Hyprland's is an animation
that looks nearly right and cannot be compared against anything.

The tree is `AnimationTree.cpp`'s names and parents, so `animation = windows,
1, 3, myCurve` reaches `windowsMove` and leaves `fade` alone, and `global` is
on at speed 8 with the default curve. Speed is in deciseconds, which nothing
in Hyprland's configuration says and only `getPercent` does:
`clamp((ms / 100) / speed, 0, 1)`. Every refusal is Hyprland's
`handleAnimation` word for word -- `no such animation`, `invalid animation
on/off state`, `invalid speed`, `no such bezier` -- and a bad line is
reported with the rest of the file still applied.

A window the layout moves slides there along `windowsMove`'s curve. The
client is configured at the goal and draws once, as Hyprland's is, and the
renderer scales its surface into the rectangle while it moves -- the one
place in this renderer where a pixel is not a pixel, and the only place it
can be. A window that is not moving goes through the exact path, which is why
every expected image in this tree still holds to the byte.

The proof is the frames themselves. The compositor writes a PPM a frame, and
a test swaps two windows through `hyprctl dispatch movewindow l` and follows
the moving window's left edge across them: it must be at more than three
places, it must not go backwards, and by the middle frame it must be more
than half way -- which a straight line is not. With `animations:enabled = 0`
the same swap puts it at two places and no more.

**Begun — rounded corners and opacity (2026-09-17).** `decoration:rounding`
cuts a window's corners and the border follows them, at the window's rounding
plus the border's width as Hyprland draws it; `decoration:active_opacity`,
`inactive_opacity` and `fullscreen_opacity` multiply the surface's alpha as
it is drawn. Anti-aliasing stays off, as everywhere in this renderer: a row's
inset is the circle's at that row's centre rounded to the nearest pixel, so
coverage is all or nothing and every frame is exact. `cargo xtask
test-compositor` boots a third time with both set and requires the picture
`compositor/render`'s own tests bless, and a test pins what a rounded corner
must show: the compositor's background where the corner was cut, and the
border along the same edge away from it.

Those are shaders. This stage brings the GPU: virtio-gpu's 3D commands
through a render node (`/dev/dri/renderD128`), `zwp_linux_dmabuf`, GBM-shaped
buffer allocation, and either Mesa's virgl and Venus drivers built on
ferrousli for OpenGL ES and Vulkan, or a Rust path over Vulkan (`wgpu`) once a
Vulkan driver exists — the choice is the customer's, recorded in
`docs/BACKLOG.md` when it is made. Until it is made, every effect has a
software fallback with a stated frame-time bound, so the compositor is never
GPU-only.

**What this stage still owes.** The GPU, which is the largest thing left
in this tree: virtio-gpu's 3D commands through a render node,
`zwp_linux_dmabuf`, GBM-shaped allocation, and a driver stack -- Mesa's virgl
and Venus on ferrousli, or a Rust path over Vulkan -- that does not exist on
Ferrix yet and is a stage's work in itself.

Of the protocols and keywords a Hyprland setup uses, what is left is:
`layerrule`, `zwp_virtual_keyboard`, `zwp_pointer_constraints` and
`relative-pointer` (a game that grabs the pointer), `presentation-time`,
drag-and-drop -- the other half of the four interfaces the clipboard already
uses -- and `hyprctl getoption`. Each is a protocol or a table rather than a
subsystem, and each is written the way the four above were: the XML
vendored, the tables checked against libwayland's own, a program in
`compositor/` that speaks it with no screen, a host test against the image
the renderer blesses, and a boot of `cargo xtask test-compositor` that does
it on Ferrix.

**Exit:** the stage 18 test with animations on, requiring a sequence of
screendumps to show a window moving along the configured curve with rounded
corners and blur behind a translucent client, at the stated frame rate under
the GPU path and inside the stated bound under the fallback; two monitors on
QEMU with independent workspaces; a plugin-shaped extension loaded from the
configuration.

**Where the exit stands (2026-09-17).** Every part of it but the GPU path is
met, and by `cargo xtask test-compositor` on x86-64 and on AArch64:

* the sliding window with its decorations on, as a sequence of screendumps,
  with the guest's own frame times reported and the renderer's software
  bound stated and checked in release by `compositor/render`;
* two monitors, each with a workspace of its own and each required to be the
  picture blessed for it;
* a plugin loaded from `plugin = /bin/plug`, adding a dispatcher a keybind
  presses.

What is left is the GPU path, and with it the frame rate the exit asks for
under one: virtio-gpu's 3D commands, `zwp_linux_dmabuf`, and a driver stack
that does not exist on Ferrix.

---

## Stage 20 — Self-hosting

Build Ferrix — and its compositor — on Ferrix. At that point the acceptance
test writes itself: the image produced by the Ferrix-hosted compiler boots
and passes every test above. Moved from 17 on 2026-09-13, when the compositor
became the goal after `rustc`; it is not on the compositor's path, and the
compositor is cross-compiled until it is.

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
| `libs/acpi` | 3, 10 — RSDP, XSDT/RSDT, MADT, FADT fixed fields, GTDT, HPET, MCFG, GIC MSI frames, DMAR, IORT, and the HPET block's capability register with the arithmetic a 32-bit counter needs. No AML, and there will be none. Has its fuzz target. | 77 |
| `libs/fdt` | Reached at 1 on ARMv7-A — the console, the GIC, the timer's interrupt and the PSCI conduit come from it there, and nothing else describes that machine. Reached at 10 for PCI host bridges `virtio,mmio` devices and `GICv2m` frames; stage 10 is still the rest of it. Has its fuzz target. | 70 |
| `libs/sync` | Reached at 4 — `SpinLock` and `IrqSpinLock` guard every shared kernel structure and carry the contended counter; `RwSpinLock` is still waiting. Fair by construction, because an unfair lock on a starved core is a stage-14 latency bug nobody will find. | 19 |
| `libs/vma` | 6 — already backs the vmap arena. The VMA interval tree and the three calls that reshape it (`mmap MAP_FIXED`, `munmap`, `mprotect`). | 60 |
| `libs/linux-abi` | 7 — syscall numbers, `errno`, `repr(C)` layouts, and which identification register fields grant each Arm `AT_HWCAP` bit. Constants and pure functions of them. Three number tables, one of them 32-bit. The socket numbers and address layouts `AF_UNIX`, IPv4, IPv6 and netlink use, and the fixed headers of the routing messages, from a probe compiled against the UAPI headers. Reached early for stage 17: the DRM/KMS ioctls, capabilities and structure layouts `/dev/dri/card0` answers, checked line by line against `probe/drm.c`'s output at both widths; and the evdev ioctls, codes and layouts `/dev/input/eventN` will answer, against `probe/input.c`'s. | 119 |
| `libs/ustack` | 7 — the initial process stack `execve` hands a program: argv, envp and the auxiliary vector, at both pointer widths. Has its fuzz target and its Miri step already. | 22 |
| `libs/cpio` | 8 — the "newc" reader an initramfs is unpacked from. Borrows, copies nothing, allocates nothing. Has its fuzz target. | 45 |
| `libs/vfs` | 8 — dentries, mounts, the path walk, open file descriptions, descriptor tables, tmpfs over a page store, initramfs unpacking. Written at the start of its stage rather than ahead of it. Has its fuzz target and its Miri step already. | 59 |
| `libs/procfs` | Reached at 8 — the text of `/proc`: the `maps` line padded to its name column at both pointer widths, `meminfo`, `status`, `stat` and `mounts`, pinned byte for byte against lines a real Linux printed, and the `maps` parser the kernel's boot check reads its own output back with. No fuzz target: it arranges the kernel's own numbers rather than parsing a stranger's bytes. | 14 |
| `libs/virtio` | 10 — the split virtqueue as logic over an abstract shared memory, the PCI transport's status protocol, feature negotiation and queue activation, and each device class's own protocol: virtio-blk's in `blk`, virtio-net's in `net`, and, reached early for stage 17, virtio-gpu's 2D control commands and responses in `gpu`, checked against QEMU 9.2.4's header, and virtio-input's configuration queries and events in `input`, checked against Linux 6.8's header with QEMU 9.2.4's still to compare; both fuzzed. Reached at 10 by the boot check's virtio-rng driver. | 135 |
| `libs/pci` | 10 — configuration space: ECAM geometry, headers, BAR decoding and sizing, both capability lists, MSI-X, the bus walk, virtio's PCI transport, MSI-X messages and the pages of a BAR a driver must not be given. Has its fuzz target and its Miri step already. | 52 |
| `libs/native-abi` | Reached at 9 — native syscall numbers, handles, rights, signals, `errno` names, `repr(C)` layouts. Constants only, like `libs/linux-abi`, and tested against it. | 13 |
| `libs/objects` | Reached at 9 — the handle table and the channel message queue, generic over what a handle names; every process's table and every channel is one; and the reachability walk a send makes before it queues an endpoint. Has its fuzz target and its Miri step. | 24 |
| `libs/btrfs` | 11, 12 — superblock, chunk tree, B-tree nodes, item payloads. Parsing only: no device, no cache, no transactions. | 38 |
| `libs/netwire` | Networking — the headers: Ethernet with one 802.1Q tag, ARP, IPv4 with its options, IPv6 with the extension-header walk, ICMPv4, ICMPv6 and Neighbor Discovery, UDP, and TCP with the options a connection negotiates. Parsed without allocation and emitted into the caller's buffer, with each format's checksum verified where it carries one. Has its fuzz target, which requires every header that parses to emit and parse back unchanged. | 54 |
| `libs/nettcp` | Networking — the TCP state machine over `libs/netwire`'s headers: the eleven states of RFC 9293 in the standard's order, including simultaneous open and simultaneous close; reassembly of what arrives out of order; window scaling and the maximum segment size; selective acknowledgment blocks for what is missing; Nagle, delayed acknowledgments, silly-window avoidance and the zero-window probe; retransmission timing by RFC 6298 with Karn's algorithm and Linux's bounds; and NewReno slow start, congestion avoidance, fast retransmit and fast recovery. It holds no clock, no socket and no address, so its tests drive two connections against each other across a wire the test loses and delays segments on, at a clock it advances by hand. Has its fuzz target. | 30 |
| `libs/displayctl` | 17 — the control protocol between the kernel's display core and a ring-3 display driver (`docs/DISPLAY.md` §2.2): twelve fixed little-endian messages decoded strictly, reserved bytes included, and the core's side of the conversation as a state machine of fixed capacity that accepts only the reply it is waiting for — an ATTACHED it asked for, the oldest FLIPPED, a DETACHED it asked for — and stays broken once a driver lies. No ring: frames do not move, the card VMO's ranges are the device's backing. Has its fuzz target. | 10 |
| `libs/inputctl` | 17 — the control protocol between the kernel's input core and a ring-3 input driver (`docs/INPUT.md` §3.2), and evdev's per-open queues (§3.1): six fixed little-endian messages decoded strictly, a HELLO checked field by field in the order it is read, and the core's side of the conversation, which publishes only the event types the input iteration supports, refuses a whole EVENTS holding one event the driver did not declare or a report over 256 events, keeps the key, LED, switch, axis and repeat state as Linux's `input_get_disposition` does, and hands each whole report to the grabbing open or to every open. The queue is Linux evdev's ring as `drivers/input/evdev.c` has it: `SYN_DROPPED` and the newest event when it fills, nothing readable past the last `SYN_REPORT`, the flush a state read makes and the drop a clock change makes, and `read`'s errors in evdev's order, writing `input_event` at either width in the open's clock. Has its fuzz target. | 23 |
| `libs/virtio-gpu` | 17 — the virtio-gpu 2D driver logic over the same shape of traits `libs/virtio-blk` uses, so it holds no handle: bring-up with only the control queue, one command at a time with its request at the start of a command area and its response at the end, every response checked; and the pipeline from the display core's ATTACH, SCANOUT, FLUSH and DETACH to the device commands each takes, undoing a failed step as far as is safe and never unpinning pages the device may still hold. Its tests drive a frame through to a fake device's screen; its fuzz target checks one reply per request and no unpin without a pin. | 14 |
| `libs/virtio-net` | 10, networking — the virtio-net driver logic over the same traits `libs/virtio-blk` uses, so it holds no handle and does no I/O of its own: bring-up in the order the status protocol fixes, both queues sized and activated before `DRIVER_OK`, a receive queue filled at bring-up and refilled as frames are taken — an empty one drops every frame in silence — a transmit queue whose buffers stay the caller's until the device says it has read them, and a drain that acknowledges the interrupt first so a completion landing during it raises another rather than being lost. It negotiates no checksum, segmentation or merge-buffer feature, which is what makes a received frame one buffer and every header the twelve bytes `VIRTIO_F_VERSION_1` makes it. Has its fuzz target. | 22 |
| `libs/net` | Networking — the net core over the two above: interfaces and their addresses, one routing table for both families with longest-prefix and metric order, a neighbour cache that answers ARP's question and Neighbor Discovery's the same way and holds the packets waiting for either, IPv4 fragmentation and reassembly bounded so a stranger cannot fill this host's memory, ICMP echo both ways including the unprivileged socket `ping` uses and the unreachable a closed port earns, UDP with Linux's socket-matching order, and TCP connections and listeners. A packet routed to the loopback goes back into the input path instead of out of a driver, so a host talks to itself with no device at all. Has its fuzz target. | 45 |
| `libs/netring` | Networking — the net ring, `docs/NET-RING.md` in code: the memory the kernel shares with a ring-3 network driver. The block ring's discipline with its allocator removed, because a frame is bounded by the MTU: the data VMO is `entries` slots of a fixed size and a submission names its slot, which takes away the class of bug where a region is reused before its completion — on an untranslated domain, a device writing into somebody else's packet. Private indices, checked reads of the peer's, the want-bell handshake, and every entry checked when it is read; corruption is terminal for the side that sees it. | 32 |
| `libs/netlink` | Networking — reached already, by the `AF_NETLINK` sockets above: walking a buffer of netlink messages and the attributes after each fixed header, and building replies into a caller's buffer with every length and pad computed rather than taken. The walks refuse a length below the header they introduce, one past the end, and the zero that walks the same message for ever, and every step forward is at least a header wide, so a walk over any bytes ends. Its `netlink_walk` fuzz target requires that, requires what a walk borrows to lie inside the input, and requires anything the builder writes to walk back to what was built. | 48 |
| `libs/netserve` | Networking — a ring-3 network driver's serve loop, between the net ring and a virtio-net device. The two directions are not symmetrical and that is the design: sending is a copy and a submission, while a frame arrives into a buffer the *device* chose and takes the oldest receive slot the kernel posted, or is dropped if none is waiting. A submission is never taken that cannot be answered, a device buffer goes back the moment its bytes are copied, and frames the device refuses wait in the order the kernel asked for them — a queue and not a single frame, because the ring's head advances for a whole batch and keeping one would drop the rest. | 10 |

With the five crates the boot path was built on — `bootinfo`, `elf` (the
loader's), `frame`, `heap`, `paging` — that is **1101 host unit tests, all
passing**, plus the doc-tests and the 41 of `xtask` itself.

**The gap this opens, stated rather than hidden.** The continuous rule below
asks for a fuzz target *and* a Miri run per crate, and `fuzz/` has seventeen:
`elf_parse`, `frame_alloc`, `ustack_build`, `handle_table`, `vfs_ops`,
`pci_walk`, `btrfs_read`, `block_queue`, `cpio_parse`, `fdt_parse`,
`acpi_tables`, `blkring`, `virtio_blk`, `netwire_parse`, `nettcp_state`,
`net_input` and `netlink_walk`. Every crate in the table above parses bytes
that came from outside the system — a disk, a firmware table, an archive a stranger built —
`acpi_tables`, `blkring`, `virtio_blk`, `virtio_net`, `netwire_parse`,
`nettcp_state` and `net_input`. Every crate in the table above parses bytes that came from
outside the system — a disk, a firmware table, an archive a stranger built —
which is precisely the population the rule was written for. The fuzz targets
still owed — `virtio` and `linux-abi` — are owed *before* the consuming stage
starts, not when it ships.

`cpio_parse` asserts more than the absence of a panic: that every name and
data slice lies inside the archive exactly where the format puts it, that the
summary agrees with the walk and `find` with the first entry of a name, that a
prefix of an archive never reads a different entry, and that any archive
walked to its trailer, written out again from what the reader reported, reads
back identical. Its seeds include the archive every boot image carries, byte
for byte, and two that GNU cpio wrote.

`netwire_parse` runs every header parser in `libs/netwire` on the same bytes,
with the transport checksums' addresses taken from the input so the fuzzer can
steer them. Each header that parses must lie inside the input, and must emit
and parse back to exactly the same header and payload — TCP's options in
canonical form, Neighbor Discovery by its message body — while the IPv6
extension walk stays inside the payload and a checksum summed in two pieces
equals the checksum of the whole.

`virtio_net` drives the network driver from a device whose every register,
used entry and header byte the fuzzer chose. Beyond the absence of a panic it
requires that a `written` the device invented, a header shorter than the
negotiated length and a descriptor id the driver never handed out each come
back as a `DeviceError` rather than as a read past the end of a buffer, that
every frame the driver reports as received lies inside the region it was given,
and that every frame the caller was allowed to send is answered exactly once —
by a completion, or by the abandoned list after the reset.

`nettcp_state` drives one connection from a stranger's segments: every field
of every segment, interleaved with writes, reads, closes and a clock the
fuzzer moves. Beyond the absence of a panic it requires that every header the
state machine answers with can be written by `libs/netwire` and parsed back,
that neither buffer grows past the capacity it was built with however many
out-of-order segments arrive, and that a connection which reached `CLOSED`
stays there and sends nothing more. It has already earned its place: it found
a connection closed during its handshake that kept its retransmission timer,
which fired afterwards and rewound the sequence numbers of a connection that
no longer existed.

`net_input` drives a whole host — an interface, an address, a route and four
sockets — from a stranger's frames, with the clock moved by the fuzzer between
them. Every frame the host answers with is parsed back as Ethernet and as the
IP packet inside it, so a header the stack builds that nothing can read is a
crash rather than a packet on a wire. The reassembly ceiling is asserted after
every frame, and a host that has been sent nothing but rubbish is required to
stop talking rather than to keep producing frames for ever.

`netlink_walk` walks the fuzzer's bytes as a buffer of netlink messages and
each message's payload as attributes, from every fixed-body offset a routing
message uses. Beyond the absence of a panic it requires the walk to end — a
buffer of *n* bytes can hold no more than *n*/16 messages, and a walk that
yields more is walking the same bytes twice, which is the hang the target
exists to catch — that everything a walk borrows lies inside the input, that an
error is the last thing a walk yields, and that a message built from a header,
a body and attributes the fuzzer chose walks back to exactly those.

`fdt_parse` holds the device tree reader to a second walk of the token stream
written from the specification: a tree the reader accepts must be well formed
by that walk, `nodes()` must yield exactly its nodes with the cell counts
their parents declared and exactly the properties after each name, and
`find_node` must return the first node the specification's path matching
selects. Its seeds are `dtc`-compiled trees holding every binding the crate
decodes, and token-built shapes `dtc` will not write: NOPs, a property after a
subnode, nesting at and past the depth limit.

`acpi_tables` reads its input as physical memory and walks it the way the
kernel does, from an RSDP at address zero through the root table to every
table listed, and it also reads every table whose signature appears anywhere
in the input. Each table must be exactly its declared length inside that
memory; each MADT entry, MCFG allocation, DMAR structure and IORT node must
decode to the little-endian bytes at the specification's offsets, and be
called malformed exactly when it is too short for its type; `check_entries`
must accept a MADT exactly when its entries tile it; and `Acpi::find` must
return the first listed table of a signature. Its seeds are hand-built with
QEMU's values: a q35 machine with intel-iommu, an AArch64 `virt` machine with
an SMMUv3, an ACPI 1.0 machine, each table alone, and the faults firmware
ships.

`ustack_build` is what the rule looks like when it is followed rather than
recorded as debt: written before a line of stage 7 kernel code existed, and it
found a real gap within a minute. A string with a NUL byte inside it built a
perfectly well-formed image that read back as a *different, shorter* string,
because everything on that stack is recovered by scanning for a NUL. The
builder now refuses it. Nothing about that bug is visible from the kernel side
— it is a program receiving an argument nobody passed it — and it would have
been found, if at all, by whoever was debugging a shell that mangled its own
arguments.

Miri was further behind than fuzzing, and the crates the CI file's own comment
names as the reason the job exists had no step. They have one now: CI
interprets `libs/elf`, `libs/bootinfo`, `libs/ustack`, `libs/objects`,
`libs/vfs`, `libs/pci`, `libs/block`, and the three the kernel runs on every
allocation and every mapping, `libs/frame`, `libs/heap` and `libs/paging`. None
of the three had undefined behaviour to report. Their local run times were 541
seconds for `frame`, 102 for `heap` and 142 for `paging`; the frame
allocator's long random workload was 97% of the first, and runs 2,000 of its
20,000 steps under Miri. `cargo xtask check --miri` runs the same list, and a
test fails when it and the workflow disagree.

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
`CLONE_THREAD`. Its own status is in [ferrousli/README.md](../ferrousli/README.md),
and its distance from POSIX.1-2024, interface by interface, in
[POSIX-2024.md](POSIX-2024.md).

---

## Continuously, from stage 1

* Every stage's exit criterion joins the CI boot test and stays there.
* The assembly allow-list is not added to without an argument in the diff.
* Anything expressible as a pure function of bytes goes to `libs/` and gets a
  fuzz target and a Miri run — before it is called from the kernel, not after.
