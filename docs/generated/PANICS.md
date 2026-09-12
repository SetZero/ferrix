# Ferrix — panic codes

> Generated from kernel/src/panic/catalog.rs by scripts/gen-panic-catalog.py. Do not edit: change the catalog and regenerate with `python3 scripts/gen-panic-catalog.py`.

When the kernel stops on a fatal condition it prints a report beginning with a
`FERRIX-PANIC` line that carries the failing site's own message. A site that
names a catalog entry adds, below the trace, the entry's code and title on a
`code` line, then `means`, `causes` and `see` lines with the text below. The
message says what happened this time; the entry says what the check was for and
where to look.

A code is `FX-SSNN`. `SS` is the stage of `docs/ROADMAP.md` whose check or
bring-up failed: `00` for anything before stage 1 or outside any stage, `90` for
the trap path's reports. `NN` numbers the entries within it. Codes are never
reused, so a code in an old log still means what is written here.

Causes are listed most likely first.

| Code | What failed |
| --- | --- |
| [FX-0001](#fx-0001) | a processor never flushed its TLB for a shootdown |
| [FX-0002](#fx-0002) | a processor never left a read-side section |
| [FX-0101](#fx-0101) | the loader's hand-off is not what the kernel needs |
| [FX-0201](#fx-0201) | the frame allocator could not be built |
| [FX-0202](#fx-0202) | the kernel address arena could not be created |
| [FX-0203](#fx-0203) | the memory allocators failed their self-check |
| [FX-0204](#fx-0204) | the identity map, the W^X sweep or the reclaim failed |
| [FX-0301](#fx-0301) | a deliberate trap did not come back correctly |
| [FX-0302](#fx-0302) | the timer interrupt did not arrive as programmed |
| [FX-0303](#fx-0303) | the interrupt controller or the clocks could not be brought up |
| [FX-0304](#fx-0304) | the timer's interrupt could not be registered |
| [FX-0401](#fx-0401) | the processor list could not be read |
| [FX-0402](#fx-0402) | a secondary processor could not be started |
| [FX-0403](#fx-0403) | a secondary processor could not build its GDT |
| [FX-0404](#fx-0404) | a secondary processor arrived with no record of its own |
| [FX-0405](#fx-0405) | a secondary processor's record does not describe it |
| [FX-0406](#fx-0406) | not every processor firmware described came online |
| [FX-0407](#fx-0407) | the processors failed to work together |
| [FX-0501](#fx-0501) | the scheduler could not be started |
| [FX-0502](#fx-0502) | the scheduler failed its self-check |
| [FX-0601](#fx-0601) | the memory a process is built from failed its self-check |
| [FX-0701](#fx-0701) | the system call dispatch path failed its self-check |
| [FX-9001](#fx-9001) | a page fault the kernel cannot resolve |
| [FX-9002](#fx-9002) | a system call the trap path cannot carry out |
| [FX-9003](#fx-9003) | the processor refused to execute an instruction |
| [FX-9004](#fx-9004) | a processor exception the kernel has no handler for |

<a id="fx-0001"></a>

## FX-0001 — a processor never flushed its TLB for a shootdown

When a kernel mapping is removed or made less permissive on x86-64,
`flush_tlb_everywhere` flushes this processor's TLB, interrupts every other
online processor, and waits up to a second for each to flush its own. A
processor that has not answered may still translate through the old entry, so
the memory behind it cannot safely be freed or the narrowed permission relied
on, and the kernel stops instead. AArch64 and ARMv7-A invalidate every
processor's TLB in hardware and never wait here.

1. The named processor was spinning with interrupts masked on a lock this one
   held when it asked for the shootdown, so it could not take the interrupt;
   `flush_tlb_everywhere` must not be called holding such a lock.
2. The named processor is halted or stuck with interrupts masked somewhere other
   than the shootdown wait, which is the only place that answers without the
   interrupt.
3. Inter-processor interrupts sent through the local APIC are not reaching the
   named processor; the sends here ignore the APIC's own refusal.

See: kernel/src/smp.rs flush_tlb_everywhere; docs/ROADMAP.md stage 4.

<a id="fx-0002"></a>

## FX-0002 — a processor never left a read-side section

`synchronize` waits for a grace period by interrupting every other online
processor and waiting up to five seconds for each to take the interrupt, which
none can do inside a read-side section, because a section masks interrupts. A
writer frees what it unpublished only after that wait, so a processor that never
answers may still be reading it, and the kernel stops rather than free memory
that is in use.

1. A read-side section on the named processor ran for more than five seconds, or
   waited for something, which a section must never do.
2. The named processor was spinning with interrupts masked on a lock this one
   held when it called `synchronize`.
3. The named processor is halted or stuck with interrupts masked, so it takes no
   interrupt at all.

See: kernel/src/smp.rs synchronize; kernel/src/smp.rs read_section;
docs/ROADMAP.md stage 4.

<a id="fx-0101"></a>

## FX-0101 — the loader's hand-off is not what the kernel needs

Stage 1 checks what the UEFI loader handed over before anything depends on it.
The memory map must be non-empty, sorted, free of overlaps, report usable RAM,
and describe the loader's own allocations: the kernel image, the page tables and
the boot information. The kernel's first bytes must read the same through the
direct map as through the image mapping, walking the page tables must find the
image where the loader said it put it, and a framebuffer, where there is one,
must be mappable. The frame allocator hands out frames from this map and
everything after it reads physical memory through these mappings, so a wrong
hand-off would surface much later as corruption with no visible source.

1. The loader did not mark its own allocations in the memory map with their own
   kinds, so the kernel would hand out the frames holding its own page tables.
2. The loader mapped the direct map at a different physical origin from the one
   it recorded, so one physical address reads different bytes through the two
   mappings.
3. The loader passed on a memory map whose regions are out of order or overlap.
4. Firmware reported a framebuffer at an address the early mapper could not map.

See: kernel/src/main.rs self_check; kernel/src/early.rs; docs/ROADMAP.md stage
1.

<a id="fx-0201"></a>

## FX-0201 — the frame allocator could not be built

`mm::init` builds the buddy allocator that every physical frame is handed out
by. It needs a per-frame array covering RAM from the lowest frame to the highest
the direct map reaches, and carves that array, which has to be contiguous, from
the front of the largest usable region. Without the allocator there are no page
tables, no stacks and no heap, so nothing after this point can run.

1. The memory map describes no usable region, or no RAM below the limit of the
   direct map.
2. No single usable region is large enough for the per-frame array, which spans
   from the lowest RAM frame to the highest and so grows with the gaps between
   banks of RAM; the message gives the bytes it needed.

See: kernel/src/mm.rs init; libs/frame; docs/ROADMAP.md stage 2.

<a id="fx-0202"></a>

## FX-0202 — the kernel address arena could not be created

`vmap::init` creates the arena that hands out kernel virtual ranges, from which
every device window and every guard-paged kernel stack is later taken. It fails
only if `ferrix_vma` rejects the arena's bounds as misaligned or empty. Nothing
that needs a device register or a new stack can run without it.

1. `ARENA_BASE` and `ARENA_END`, or the `KERNEL_VMAP_BASE`,
   `KERNEL_VMAP_RESERVED` and `KERNEL_VMAP_SIZE` constants they are built from,
   are not page aligned or leave no room; this is a bug in those constants, not
   something a machine can cause.

See: kernel/src/vmap.rs init; libs/bootinfo; libs/vma.

<a id="fx-0203"></a>

## FX-0203 — the memory allocators failed their self-check

`memory_check` exercises what every later subsystem assumes about memory without
checking. The frame allocator must hand out distinct, aligned blocks and get
back exactly the frames it gave; the heap must hold what a `Box`, a `Vec` and a
`BTreeMap` put in it, give everything back, and keep at most one slab page per
size class. The vmap arena must hand out distinct, zeroed, guard-paged ranges,
change their permissions in the page tables, map device windows at the right
offset within the page, and free all of it without leaking a frame, and a kernel
stack must be aligned, writable at both ends and guarded at both ends. A broken
property here would otherwise show up as corruption in whichever subsystem first
relied on it.

1. The frame allocator was given too little memory for the check: `no frame
   available`, `no sixteen-frame block available` and `handed out nothing` mean
   it ran out, not that it is wrong.
2. A change to `libs/frame` or `libs/heap` broke their bookkeeping, so the free
   frame count or the heap's balance does not return to where it started.
3. A change to the page table code left a mapping behind on unmap, or made a
   permission change the descriptors do not show.

See: kernel/src/main.rs memory_check; libs/frame; libs/heap; libs/paging;
kernel/src/vmap.rs; docs/ROADMAP.md stage 2.

<a id="fx-0204"></a>

## FX-0204 — the identity map, the W^X sweep or the reclaim failed

`finish_memory` runs last in boot. It first requires the W^X sweep to see the
loader's identity map as a violation, which proves the sweep can find one; then
it drops the identity map and requires address zero to translate to nothing,
sweeps every live page table root for a mapping that is both writable and
executable, and gives the loader's memory and the ACPI tables back to the frame
allocator, requiring something to come back. A kernel still running with a
writable and executable mapping, or still depending on the identity map, is not
the kernel the boot marker describes.

1. A mapping was installed writable and executable; the `w^x` line printed
   before the panic gives its address and length.
2. The architecture's `drop_identity_map` did not remove the lower half, so
   address zero still translates.
3. On AArch64 and ARMv7-A the identity map is a root of its own, and
   `arch::identity_root` did not return it, so the sweep could not see the
   violation it is required to see.
4. The memory map marks no loader or ACPI-reclaimable region inside the range
   the per-frame array covers, so nothing was reclaimed.

See: kernel/src/main.rs finish_memory; kernel/src/mm.rs check_w_xor_x;
kernel/src/mm.rs reclaim_boot_memory; docs/ROADMAP.md stage 2.

<a id="fx-0301"></a>

## FX-0301 — a deliberate trap did not come back correctly

`trap_check` raises two breakpoints and requires both to reach the handler and
return to the next instruction with a canary register intact, which proves the
entry path restores what it saved. It then writes to three unmapped pages in the
on-demand window, requiring the fault handler to map each faulting page, the
write to retry and read back, the rest of each page to be zero, and the frames
consumed to be one per page plus at most one per table level. Every fault the
kernel resolves rather than reports goes through this path, which is how demand
paging works from stage 6.

1. The architecture's trap entry or exit does not save or restore a register the
   frame carries, or `arch::advance_past_breakpoint` returns to the wrong
   instruction.
2. Something mapped an address inside `mm::DEMAND_WINDOW` before the check ran,
   so the window was already mapped.
3. Mapping a page in a region that already had its tables cost more than one
   frame, which points at the page table code allocating a table it already had.

See: kernel/src/main.rs trap_check; kernel/src/trap.rs handle_page_fault;
docs/ROADMAP.md stage 3.

<a id="fx-0302"></a>

## FX-0302 — the timer interrupt did not arrive as programmed

`timer_check` proves time works before anything is built on it. A one-shot timer
must fire exactly once; then a periodic timer must deliver a thousand ticks,
every interrupt must reach a registered handler, and the rate measured against
the counter must be within 25 percent of the 1000 Hz asked for. A scheduler
slice, a sleep and every later timeout is this rate multiplied by something, so
a timer that is silent, repeats, or runs at the wrong rate makes all of them
wrong.

1. The timer's interrupt is not reaching the processor: firmware named the wrong
   interrupt for the virtual timer (the GTDT on AArch64, the device tree on
   ARMv7-A), or the controller was not left delivering it.
2. On AArch64 and ARMv7-A the timer interrupt is level triggered, and a handler
   that acknowledges it without disarming the timer is re-entered at once; this
   check is there to catch that, though it can equally show as a hang with
   nothing printed.
3. On x86-64 the local APIC timer was calibrated against a counter that was
   itself wrong, so it was programmed from a wrong frequency; the line printed
   before the panic gives the measured rate, and a rate that is too high points
   here.
4. The host running an emulator was too loaded to deliver a thousand interrupts
   in a second, so the measured rate is low.
5. An interrupt arrived on a line nothing registered for: a device firmware left
   enabled, or a controller programmed to deliver somewhere unexpected.

See: kernel/src/main.rs timer_check; kernel/src/timer.rs; docs/ROADMAP.md stage
3.

<a id="fx-0303"></a>

## FX-0303 — the interrupt controller or the clocks could not be brought up

`arch::init_interrupts` brings up, on the boot processor, the interrupt
controller, the counter and the timer that firmware describes. On x86-64 that is
the local APIC from the MADT, a counter from the HPET or from the TSC calibrated
against the PIT, the local APIC timer calibrated against that counter, and every
I/O APIC masked; on AArch64 a GICv2 from the MADT and the generic timer; on
ARMv7-A the same two from the device tree. Without them nothing arrives that the
kernel did not cause itself, so there is no timer, no preemption and no second
processor.

1. The interrupt controller is a GICv3, which the kernel refuses rather than
   drive with GICv2 register layouts; QEMU's `virt` machine gives a GICv2 unless
   asked for another.
2. Firmware does not describe what the kernel needs: no readable ACPI tables or
   MADT on x86-64 and AArch64, or no interrupt controller, distributor or CPU
   interface in the device tree on ARMv7-A.
3. On x86-64 there is no HPET and the PIT did not answer, so there is no
   counter; or the HPET is described but its counter does not advance, or
   reports a period outside the specification.
4. Firmware left the generic timer's frequency register, `CNTFRQ_EL0` or
   `CNTFRQ`, at zero.
5. The kernel address arena refused a device window for the controller, the HPET
   or an I/O APIC, for want of address space or of frames for page tables.

See: kernel/src/arch/x86_64/mod.rs init_interrupts;
kernel/src/arch/x86_64/apic.rs; kernel/src/arch/x86_64/clock.rs;
kernel/src/arch/aarch64/gic.rs; kernel/src/arch/armv7a/mod.rs init_interrupts;
docs/ROADMAP.md stage 3.

<a id="fx-0304"></a>

## FX-0304 — the timer's interrupt could not be registered

`timer::init` attaches the tick handler to the interrupt number
`arch::timer_irq` reports. Registration fails only if that number is past the
end of the handler table or already has a handler. Without the handler every
tick is counted as unclaimed, and the timer self-check that follows cannot pass.

1. Code added to `kmain` before this point registered a handler on the same
   number, or `timer::init` was called twice; nothing else registers a handler
   this early.

See: kernel/src/timer.rs init; kernel/src/irq.rs register.

<a id="fx-0401"></a>

## FX-0401 — the processor list could not be read

`smp::discover` finds which processors firmware says can be started, and which
one is running this code, before any other is started. The list comes from the
MADT on x86-64 and AArch64 and from the device tree's `/cpus` on ARMv7-A, and
must be non-empty, have no two processors with one identifier, and include the
boot processor. The boot processor's per-CPU record is then installed and must
lead back to itself and name the hardware reading it. Every later step addresses
processors by these identifiers, and a wrong one sends a start request or an
interrupt to the wrong processor.

1. The boot processor's identifier was read in a form the table does not store,
   so it is not in the list; on AArch64 and ARMv7-A only the affinity fields of
   `MPIDR` count.
2. The machine has no readable ACPI tables or MADT (x86-64, AArch64), or no
   device tree (ARMv7-A).
3. Firmware marks every processor disabled, or lists two with the same
   identifier.
4. The per-CPU register (`GS` base on x86-64, `TPIDR_EL1` on AArch64, `TPIDRPRW`
   on ARMv7-A) did not keep the address written to it.

See: kernel/src/smp.rs discover; kernel/src/arch/x86_64/smp.rs describe_cpus;
kernel/src/arch/aarch64/smp.rs describe_cpus; kernel/src/arch/armv7a/smp.rs
describe_cpus; docs/ROADMAP.md stage 4.

<a id="fx-0402"></a>

## FX-0402 — a secondary processor could not be started

`smp::start_secondaries` starts every processor but the boot one, one at a time,
and waits up to a second for each to mark itself online before starting the
next. x86-64 starts one with INIT and two start-up IPIs into a real-mode
trampoline below 1 MiB; AArch64 and ARMv7-A ask PSCI's `CPU_ON` to start it at
an identity-mapped entry sequence. A processor that was started and never
reported in may still be about to read the shared start block, so the kernel
stops rather than go on without it or free the block under it.

1. A processor was started and never reported in: it faulted or hung in its
   entry sequence before reaching `smp::secondary_main`. On a new board this is
   the usual first failure, and ARMv7-A's `nosmp` boot argument keeps to one
   core to rule everything else out.
2. PSCI refused `CPU_ON`: firmware does not provide PSCI (the FADT on AArch64,
   the device tree on ARMv7-A), the core is already on, or the entry address was
   rejected; the message says which.
3. On x86-64 there is no free frame below 1 MiB for the trampoline or its root
   table, or a processor's APIC ID is above 255, which needs x2APIC mode the
   kernel does not have.
4. On x86-64 the local APIC never accepted the INIT or start-up IPI.
5. On ARMv7-A the entry sequence, a stack, a per-CPU record or the start block
   lies above 4 GiB and cannot be passed in a 32-bit register.

See: kernel/src/smp.rs start_secondaries; kernel/src/arch/x86_64/smp.rs
CpuStarter; kernel/src/arch/aarch64/smp.rs CpuStarter;
kernel/src/arch/armv7a/smp.rs CpuStarter; docs/ROADMAP.md stage 4.

<a id="fx-0403"></a>

## FX-0403 — a secondary processor could not build its GDT

On x86-64 every processor needs a GDT and TSS of its own, because loading a TSS
marks its descriptor busy and so one cannot be shared, and the TSS carries the
stack a double fault runs on. `gdt::init_secondary` runs on the new processor
before it enables interrupts and takes that stack from the vmap arena. Without
it a double fault on this processor would have no stack to run on, which is a
triple fault and a silent reset.

1. The vmap arena had no address space or frames left for another guard-paged
   stack; every processor started also takes a kernel stack from the same arena.

See: kernel/src/arch/x86_64/gdt.rs init_secondary; kernel/src/arch/x86_64/smp.rs
secondary_start; docs/ROADMAP.md stage 4.

<a id="fx-0404"></a>

## FX-0404 — a secondary processor arrived with no record of its own

Each secondary processor is handed the address of its per-CPU record through the
architecture's start sequence, and `secondary_main` looks that address up among
the records rather than trusting it. With no record the processor has no
identity: it cannot install its per-CPU register or say it is online. This is
raised on the secondary processor itself.

1. The block the start sequence loads its argument from does not match what the
   assembly reads: `Header` on x86-64, `StartBlock` on AArch64 and ARMv7-A.
2. On AArch64 or ARMv7-A the start block written by the boot core had not
   reached memory when the new core, whose caches are off, read it;
   `clean_to_poc` after the write is what prevents that.

See: kernel/src/smp.rs secondary_main; kernel/src/arch/x86_64/smp.rs;
kernel/src/arch/aarch64/smp.rs; kernel/src/arch/armv7a/smp.rs.

<a id="fx-0405"></a>

## FX-0405 — a secondary processor's record does not describe it

Having found its record, a secondary processor installs it in its per-CPU
register and checks it: the register must lead back to that record, the record
must hold its own address and sit at its logical number's index, and the
hardware identifier in it must be the one this processor's hardware reports. A
register pointing at another record makes two processors share per-CPU state,
and a record naming other hardware sends this processor's interrupts elsewhere.

1. The identifier firmware listed for this processor is not the one the
   processor reads from its own hardware, so the start request reached a
   different core from the one the record describes.
2. The per-CPU register (`GS` base on x86-64, `TPIDR_EL1` on AArch64, `TPIDRPRW`
   on ARMv7-A) did not keep the address written to it.

See: kernel/src/smp.rs check_this_cpu; kernel/src/smp.rs secondary_main.

<a id="fx-0406"></a>

## FX-0406 — not every processor firmware described came online

Once `smp::start_secondaries` returns, every processor in the list must have
marked itself online. That function waits for each processor it starts and fails
if one does not arrive, and nothing ever marks a processor offline, so this
checks that promise rather than a separate step. Stage 4's checks hand work to
every online processor and expect all of them.

1. `start_secondaries` returned success without waiting for every processor
   after the boot one, which its loop is written to prevent.

See: kernel/src/main.rs bring_up_processors; kernel/src/smp.rs
start_secondaries.

<a id="fx-0407"></a>

## FX-0407 — the processors failed to work together

`smp::check::run` is stage 4's exit criterion, run on every processor at once.
Every processor must run a hundred rounds of work, each secondary woken for it
by an inter-processor interrupt; a page moved to another frame twenty times must
be read at its new frame by every processor; a hundred grace periods must never
end while a reader still holds what they retire; and a counter incremented
25,000 times by each processor under one lock must come out exact, with the
processors' shares overlapping in time. The scheduler and everything after it
rely on each of these whenever more than one processor runs.

1. A TLB flush does not drop every entry it should on every processor, so one
   reads a moved page through a stale translation; an emulated MMU keeps no TLB,
   so this shows under a hardware accelerator and not under tcg.
2. A secondary processor takes no inter-processor interrupts, because its own
   interrupt controller interface was not brought up or its copy of the
   interrupt is not enabled.
3. `synchronize` returned before every processor had taken its interrupt, so a
   grace period ended early and a reader found the object it poisoned.
4. A processor did not finish its share of the work within thirty seconds,
   because it missed the interrupt meant to wake it or sat with interrupts
   masked.
5. The spin lock let two processors in at once, or the processors never ran
   their increments at the same time, so the lock was never contended.

See: kernel/src/smp/check.rs run; kernel/src/smp.rs run_everywhere;
docs/ROADMAP.md stage 4.

<a id="fx-0501"></a>

## FX-0501 — the scheduler could not be started

`sched::init` builds one Throughput scheduling domain over every processor,
gives each processor a run queue, makes the boot context the boot processor's
first task with an idle task beside it, and wakes the secondaries so each joins
the scheduler from its idle loop. It waits up to five seconds for all of them,
re-sending the wake-up interrupt every millisecond. Nothing can be spawned,
slept or preempted until every processor is running tasks.

1. A secondary processor never reached `sched::enter_idle`: it missed the
   wake-up interrupt, or was interrupted so often it never ran the instructions
   between waking and joining.
2. The machine has more processors than a scheduling domain's `CpuSet` can hold.
3. The vmap arena had no stack left for the boot processor's idle task.
4. `ferrix_sched` rejected the scheduler's slice constant when a run queue was
   built.

See: kernel/src/sched/mod.rs init; kernel/src/sched/mod.rs wait_for_processors;
docs/ROADMAP.md stage 5.

<a id="fx-0502"></a>

## FX-0502 — the scheduler failed its self-check

`sched::run_checks` is stage 5's exit criterion and the checks added after it. A
task must run, be switched to and be reaped; a sleep must end neither early nor
twenty times late; a thousand tasks spawned on one processor must finish, run on
more than one processor and give every stack back; spinners of different weights
must each get their weighted share within EEVDF's bound; and placement,
affinity, load tracking, balancing and slice scaling must each do what they
claim, before every run queue's own bookkeeping is checked. Every later task, a
user process included, is scheduled by the code this measures.

1. A task became runnable without its processor being woken or its timer
   re-armed, so it never ran and a wait gave up after twenty seconds (the `never
   started`, `never finished` and `never stopped` messages).
2. An exited task's stack was never given back to the vmap arena; the arena's
   count and the expected count are printed before the panic.
3. The emulator's host descheduled virtual processors for long stretches,
   delaying a sleep or distorting measured service; the fairness bound grows
   with the overrun the scheduler saw, but a sleep twenty times late fails
   regardless.
4. Stealing, placement or balancing moved nothing between processors, or a task
   ran on a processor its affinity excluded.

See: kernel/src/sched/check.rs run; kernel/src/sched/mod.rs; docs/ROADMAP.md
stage 5.

<a id="fx-0601"></a>

## FX-0601 — the memory a process is built from failed its self-check

`user::check::run` tests the objects stage 6 builds processes from. A VMO must
cost nothing until a page is committed, commit each page once and zeroed, and
give every frame back when dropped; an address space must refuse mappings
outside the user half, resolve faults inside its regions and refuse the rest;
the processor, with the space installed, must write through a user address into
the frame the tables name; fork must share pages and a write must copy exactly
one; and two tasks in two address spaces must each read their own page at one
address. Across all of it the free frame count must end where it started.

1. A frame reference taken by a VMO's page list or by fork's sharing was not
   released, so the free frame count did not come back (the `leaked` messages).
2. `arch::install_user_root` wrote the root without re-enabling the lower-half
   translation regime, which on AArch64 and ARMv7-A passes the first
   installation and fails the second.
3. The scheduler did not swap address spaces when it switched tasks, so a task
   read another task's page.
4. The copy-on-write path let a write through to a frame still shared with the
   other address space.

See: kernel/src/user/check.rs run; kernel/src/user/space.rs;
kernel/src/user/vmo.rs; docs/ROADMAP.md stage 6.

<a id="fx-0701"></a>

## FX-0701 — the system call dispatch path failed its self-check

`syscall::check::run` proves the kernel decodes system calls with its own
architecture's table before any program can make one. Exactly one of the three
architectures' `getpid` numbers must decode to getpid on this build and answer;
the credential calls must report root, and getpid and gettid must agree; an
unknown number must be refused with ENOSYS, encoded as -38; and every number
from 0 to 600 must return a value. A build using another architecture's table
would pass every host test and answer a program's `write` with a different call.

1. The kernel was built with another architecture's number table behind
   `arch::decode_syscall`.
2. The number tables or the errno encoding in `libs/linux-abi` changed, so a
   credential call has no number on this architecture or ENOSYS no longer
   encodes as -38.
3. A handler added to the dispatch table returns `Outcome::Enter` for an
   ordinary call or answers a credential call with something other than root.

See: kernel/src/syscall/check.rs run; kernel/src/syscall/mod.rs dispatch;
libs/linux-abi; docs/ROADMAP.md stage 7.

<a id="fx-9001"></a>

## FX-9001 — a page fault the kernel cannot resolve

The trap path resolves one kind of page fault: a kernel access to an unmapped
page inside the on-demand window, which it maps with a fresh zeroed page before
letting the instruction retry. Every other fault, whether an unmapped address
elsewhere, a write to a read-only page, a fetch from a non-executable one, or
any fault from user mode, is a bug and stops the processor. The report's `page
fault at` line gives the address, the kind of access and whether a mapping
existed, and the architecture's saved registers follow.

1. Kernel code followed a null or otherwise invalid pointer; once the identity
   map is dropped, address zero translates to nothing on purpose.
2. A kernel stack overflowed into the unmapped guard page below it; on x86-64
   the fault cannot be pushed onto that stack and arrives as a double fault
   instead (FX-9004).
3. Code used a vmap allocation after freeing it, or a device window after
   unmapping it.
4. Kernel code touched a user address in an installed address space before
   faulting the page in with `AddressSpace::fault`; the trap path consults no
   address space, only the on-demand window.
5. A fault in the on-demand window found no free frame to map.
6. On AArch64 every data or instruction abort that is not a permission fault is
   reported as not mapped, an external abort from a device address included; the
   fault status in `esr` says which it was.

See: kernel/src/trap.rs handle_page_fault; kernel/src/mm.rs map_demand_page;
kernel/src/vmap.rs; kernel/src/arch/aarch64/trap.rs abort.

<a id="fx-9002"></a>

## FX-9002 — a system call the trap path cannot carry out

On AArch64 and ARMv7-A a system call is `svc`, an exception like any other, and
the trap path hands it to the architecture's `system_call`, which reads the
arguments from the saved registers, dispatches it and writes the answer back.
That fails in two ways, and either one ends here: the call came from the kernel
rather than from a program, or it was an `execve`, which that path cannot yet
carry out. The headline is the reason `system_call` gave. On x86-64 a system
call never comes this way, because `SYSCALL` has an entry of its own.

1. Kernel code executed `svc` (the headline says `from EL1` or `from SVC mode`);
   nothing in the kernel is meant to.
2. A user program called `execve` on an Arm architecture, and `execve` through
   the trap path is not wired yet (the headline says so).

See: kernel/src/trap.rs dispatch; kernel/src/arch/aarch64/trap.rs system_call;
kernel/src/arch/armv7a/trap.rs system_call; docs/ROADMAP.md stage 7.

<a id="fx-9003"></a>

## FX-9003 — the processor refused to execute an instruction

The processor raised its undefined-instruction exception: invalid opcode on
x86-64, an exception of unknown reason on AArch64, undefined instruction on
ARMv7-A. The kernel emulates no instruction, so it stops. The saved program
counter in the report is the instruction that was refused.

1. Execution reached bytes that are not an instruction, through a corrupted
   return address or function pointer; check whether the saved program counter
   lies inside the kernel image.

See: kernel/src/trap.rs dispatch; kernel/src/arch/x86_64/trap.rs classify;
kernel/src/arch/aarch64/trap.rs classify; kernel/src/arch/armv7a/trap.rs
classify.

<a id="fx-9004"></a>

## FX-9004 — a processor exception the kernel has no handler for

The trap path handles breakpoints, page faults and interrupts; every other
exception stops the kernel, with the architecture's own name for it on the
report's first line and the saved registers below. On x86-64 that is a vector
such as the double fault or the general protection fault; on AArch64 an
exception class such as an SError or a trapped system register access; on
ARMv7-A an alignment fault, an FIQ, or a data or prefetch abort that is not a
translation, access-flag or permission fault.

1. On x86-64, a double fault: a fault while delivering another, usually because
   the kernel stack was unusable, as after an overflow into its guard page. It
   runs on a stack of its own so that it can be reported at all.
2. On ARMv7-A, a data abort with an external abort status, from an access to an
   address with no device behind it; the `fsr` in the report gives the status.

See: kernel/src/trap.rs dispatch; kernel/src/arch/x86_64/trap.rs vector_name;
kernel/src/arch/aarch64/trap.rs class_name; kernel/src/arch/armv7a/trap.rs
abort.
