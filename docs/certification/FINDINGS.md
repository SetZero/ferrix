# Findings

The audit register for the item defined in [ITEM.md](ITEM.md). One entry per
finding, each naming what was measured, which objective it bears on, and what
would close it.

17 findings are open and 21 are closed, of 38. F-10 advanced from 71.4% to 81.9%, F-31's side-channel half was built, and F-07, F-09 and F-33 closed, which leaves the boundary with no upward reference (2026-09-26). No finding here is closed by argument:
a finding closes when the thing it describes stops being true and something in
the build says so.

**Severity.** *Blocking* — a rating cannot be claimed while it stands.
*Major* — a named objective is unmet. *Moderate* — an objective is partially
met or met without evidence. *Minor* — a defect with no objective attached yet.

| | Blocking | Major | Moderate | Minor | Informational |
|---|---:|---:|---:|---:|---:|
| Open | 2 | 6 | 8 | 0 | 1 |

Blocking: F-27 and F-28 — independent assessment and a quality management
system. Both need an organisation; neither is a defect in the code.

F-20 and F-22 were on this list until 2026-09-25, when the element was
documented as a *safety element out of context*
([SAFETY-MANUAL.md](SAFETY-MANUAL.md)) and their element-level halves were
written. The system-level halves are exported to the integrator as assumptions
of use, which is how every general-purpose certified kernel handles them.

---

## A. Boundary integrity

Measured by `scripts/check-item-boundary.py`; **no upward references**, from
94 in 28 files when the audit began. The debt register in
`scripts/certification-item.json` is empty since W-5 closed F-07, F-09 and F-33
(2026-09-26), and stays, empty, so that a new upward reference has to be argued
into it against a finding. `main.rs`'s 37 edges into the load are recorded
beside it, not in it: they are the composition root's ([ITEM.md](ITEM.md) §2).

**Every count this section gave before 2026-09-26 was a lower bound** -- the
29, and the 62 the audit started from. The gate matched the text
`crate::a::b` and nothing else, in source whose strings it found with a
pattern that mis-paired quotes after a `\`-newline continuation. Three kinds of
edge were invisible to it:

* **a nested `use` group.** `use crate::syscall::{exec, process}` was read as
  `crate::syscall`, the item's own dispatcher; `devmgr.rs` has exactly that;
* **a path through a name.** `syscall/mod.rs` declares `mod exec;` and calls
  `exec::sys_execveat`, and a file that writes `use crate::syscall::fd;`
  and then `fd::arg(..)` names `fd` either way. The old gate saw the second
  kind only at the `use`, and the first not at all: `syscall/mod.rs` was
  measured as naming one load module, and it names 21;
* **code read as a string.** After the first continuation in a file, the
  string pattern took code for literal text. All five of
  `arch/x86_64/paranoid.rs`'s references were in such a stretch.

The gate now resolves names the way the compiler does, short of type
checking: every `use` tree however nested, `crate`, `self` and `super`, names
bound by `use` or declared by `mod`, and `pub use` re-exports followed to the
module that defines the item. Re-measured on the same tree the register went
from 29 to 56 -- 20 more under F-09, 2 under F-07 and 5 under a new F-33 --
and none of them is new coupling. The audit's own starting tree, re-measured
the same way, has 94, not 62; the 38 paid down since are real, and F-01 to
F-08's closures stand, since each removed edges the old gate could see and the
new one confirms are gone.

### F-01 — the `Process` type is a core concept living in the Linux personality
**Closed 2026-09-26** by W-1, as a split rather than a move.

The finding was that the core named `syscall::process` -- 2,229 lines of
`fork`, `wait4` and signal bookkeeping -- because a process *is* the container
the core isolates, and the type lived in the personality. It was 10
references: 2 from the core and 8 from the item ring.

The core half is now `kernel/src/object/process.rs`: the address space, the
pid, the start time, the handle table, the job, and how the process ended,
with `ProcessRef`, `Control` and the pid table. The POSIX process contains it
and adds the descriptor table, the filesystem context, the signal state,
`brk`, the credentials, the family and the threads, none of which the core
type can reach. Where the core must hold a process whole it holds a `Host`, a
trait of five methods the personality implements; the scheduler holds a
thread as a one-method `UserThread`. IMPLEMENTATION.md W-1 has the design.

Of the 10 references, 4 are gone: `object/job.rs`, `object/mod.rs` and
`syscall/futex.rs` no longer name the personality, and `syscall/registry.rs`,
whose table moved into the core, went to `load` with the typed lookup it kept.
**The other 6 were not made to point into the core, and this entry does not
claim they were.** They are item-ring files naming the POSIX process for POSIX
state -- `brk`, the fd table, credentials, the dispatcher's `current()`, the
POSIX thread's signals, the Linux loader -- which is not the core-concept
defect this finding described. They are refiled where the register already
describes them: five under F-09 and `syscall/native.rs`'s under F-07.

*Verified by:* no `F-01` entry in `scripts/certification-item.json`, and
nothing under `object/` or `sched/` naming `syscall::` in `check-item-
boundary.py --report`; the full boot gate row, `test-threads` and `test-jobs`.

### F-02 — the trap return path calls signal delivery directly
**Closed 2026-09-25.** Six of the seven references are gone. The frame types
`arch/*/signal.rs` needs moved to `kernel/src/signal_frame.rs` in the core, and
the three functions the trap return called are now reached through
`crate::trap::ReturnPath` — a struct of three function pointers the personality
registers at boot, held in an `AtomicPtr` rather than a lock because it is read
on every return to user mode.

A kernel whose personality registers nothing now returns to user mode directly,
which is the property that makes the core independently analysable.

Verified by booting all three architectures, since signal frames are
architecture-specific and the change touched every one.

### F-02a — a fault becomes a signal by an upcall from the core
**Closed 2026-09-25**, and it did not need F-01 first after all.

`ReturnPath` gained a fourth entry, `fault_signal`, and a core-owned answer
type `FaultOutcome` with three cases — delivered, ended with a pid, or no
process. `Posted` and `Origin` stay on the personality's side of the interface,
which is the whole point: a trap path that had to name them would be back where
F-02 started.

The fault *resolver* needed no interface at all, only a better question. It was
asking the Linux personality for the current process in order to reach its
address space; the scheduler already knows which address space is running, and
sets it from that same process when the task is made. `sched::current()
.address_space()` is both correct and more honest about what the fault path
means.

**`trap.rs` now names nothing above the core.** All three of its references —
`syscall::deliver`, `syscall::signal`, `syscall::process` — are gone, and the
most trusted file in the kernel is clean. Verified by booting all three
architectures with the fault path exercised: the `signal pid N ended by signal
11` lines still print, with the right pids.

### F-03 — architecture modules name the personality's `StatLayout`
**Closed 2026-09-25.** The `StatLayout` enum moved to `kernel/src/arch/mod.rs`,
beside the other ABI facts the facade carries; its `impl` stayed in
`syscall/stat.rs`, which is legal within a crate. Data in the core, behaviour
in the personality, and the dependency now points downward. 62 upward
references became 59.

### F-04 — the core device registry names STM32MP1 board support
**Closed 2026-09-26.** All six of the manifest's F-04 entries are gone: the
three from `device.rs` into `stm32mp1`, `stm32mp1_gpu` and `stm32mp1_usb`, and
the three from the item that reached the same board for its boot mode
(`power.rs`, `syscall/system.rs`) and its pixel clock (`syscall/native.rs`).

The registry now keeps a list of `BoardBinding`s: a binding number, a
`prepare` function that answers with registers, an interrupt, a DMA shape and
a line for the log, and the one clock a driver may set, if there is one. The
list is a `Hooks` (`kernel/src/hooks.rs`, core), a handful of `Once` cells
read without a lock, because board support waits on the timer while it
prepares a device. `stm32mp1::install` registers the display, the USB host and
the GPU, in the order their nodes were published before, and hands power the
function that writes U-Boot's boot mode. The registry mints the apertures and
the vector from what `prepare` says under the rules it applies to every node,
so board support still cannot hand a driver memory the kernel owns.
`device_clock` asks `device::board_clock` for the node's binding's clock.

Registration is one explicit call in `main.rs`'s `register_load`, before
enumeration, not a link-time table. The boot prints what was registered and
stops with FX-0006 if anything is missing.

*Not verified on the board.* QEMU's machines carry no STM32MP15 device tree,
so under every gate the three bindings find nothing. The DK1's display, USB
host and GPU were not enumerated again on hardware for this change. The path
is the old one reshaped -- same order, same minting, same log lines -- and the
next board session should confirm that the `display`, `usb` and `gpu` lines
are unchanged.

### F-05 — `claim.rs` names `block_ring`
**Closed 2026-09-25.** Only the `StillServed` enum was wanted, and it belongs
to the claim rather than to the ring: a quiesce asks whether anything still
serves a node, and the answer must not depend on which uncertified subsystem
happens to be serving it. Moved to `kernel/src/claim.rs`; `block_ring`,
`render`, `display` and `native` now answer with the core's type.

### F-06 — core names two item-ring modules
**Closed 2026-09-26** by W-1. The three references were `object/job.rs` to
`syscall::registry`, to find a job's members, and `sched/mod.rs` and `sched/
task.rs` to `syscall::thread`, because a task held the POSIX thread. The pid
table is the core's now, and a task holds a `sched::UserThread` -- the
scheduler's view of a thread, which is the process it runs in and nothing
POSIX. Inner-ring only, so no present rating moved; it is the ratchet's path
to an EAL6+/ASIL D `core`, and `object/` and `sched/` now name nothing above it.

### F-07 — the native ABI dispatcher fans out across the load ring
**Closed 2026-09-26** by W-5. All 12 references are gone: 10 from
`syscall/native.rs` into `block_ring`, `net_ring`, `fs::cgroupfs`, `display`,
`render`, `input` and the personality's `exec`, `load`, `fd` and `process`,
and 2 from `devmgr.rs` into `syscall::exec` and `syscall::process`.

The item defines three interfaces and the load registers into them from
`main.rs`'s `register_load`, as F-04 and F-08 did:

* **A table of handlers** for the six calls that are about a subsystem above
  the item -- `block_ring_create`, `net_ring_create`,
  `display_control_create`, `render_control_create`,
  `input_control_create` and `job_for_cgroup` -- keyed by call and
  registered by the subsystem (`native::serve`). The device handle, its
  `MANAGE` right and the driver's handle stay in the item
  (`native::control_channel`); only the channel is the subsystem's to make.
* **`native::Processes`**, which the Linux personality lends from
  `syscall/launch.rs` beside init's `Launcher`: load an image into a new
  process, and claim, prepare and start it with an argument taken between
  the prepare and the run. `process_create`, `process_start` and `devmgr`'s
  own start all use it; the job, the rights and the handle move stay in the
  item.
* **`native::Server`s**, which a quiesce waits out and releases in the order
  registered: the block ring, the display and the renderer.

The dispatcher finds its caller through the scheduler's `UserThread` and hands
handlers the core's `Process`, and the table's the caller as a `Host`.

*The guarantee the `match` gave.* It was exhaustive, so an unanswered call did
not compile. It still is, for every call the item answers. The six it leaves
to the table are checked at boot instead, on every boot: `main.rs` stops with
FX-0006 if any has no handler, and says what it found (*"6 native calls
answered above the item, 3 subsystems a quiesce waits out"*). The table is
searched by the decoded call, never indexed by a program's number, so F-31's
clamp in `decode` is still the only bound a misprediction could cross.

*Verified by:* no `F-07` entry in the manifest, and nothing from `native.rs`
or `devmgr.rs` above the item in `--report`; the full boot gate row, in which
devmgr starts its drivers through `Processes` and the block ring's check
drives the table, and `test-net`, which makes a net ring through it.

### F-08 — bring-up and power name the filesystem
**Closed 2026-09-26.** All six entries are gone, from `init.rs`, `power.rs`
and `devmgr.rs` into `fs`, `fs::root_disk`, `fs::data_disk`, `block_ring` and
`syscall::load`. Each consumer in the item now defines what it needs, and the
load ring registers into it from `main.rs` before the first use:

* **Power** keeps a list of `Flush`es -- a mount point and a commit -- and
  commits every one, in the order registered, before the machine stops.
  `fs::install` registers `/` and then `/data`, so a power-off still commits
  both before power goes, in the old order. `test-shell`'s `/data/k7`
  surviving `poweroff -f -n`, and `test-powerfail`, both pass.
* **Init** decides which program runs and says how it ended; how a program is
  opened and started is a `Launcher` that `syscall/launch.rs`, in the load
  ring, registers. That also removed `init.rs`'s use of `syscall::exec`,
  which the gate never reported (below).
* **`devmgr`** reads its program and drivers through a `ReadFile` the
  filesystem registers. `location_of` -- the PCI location devmgr's messages
  and every ring's HELLO name a device by -- moved from `block_ring` into
  `devmgr.rs`, and `block_ring` re-exports it.

`main.rs` checks, on every boot and before anything uses them, that a flush,
the launcher and the reader are registered (FX-0006).

*What the gate did not see.* When this closed, two kinds of edge from the item
into the load ring were invisible to `check-item-boundary.py`, and closing
this finding did not claim them. Both are measured since 2026-09-26. The
`use crate::syscall::{exec, process}` in `devmgr.rs` is F-07's, where its two
edges are now filed. And `main.rs`, the crate root, calls the load ring by
bare module paths (`fs::install`, `syscall::launch::install`,
`stm32mp1::install`, `fs::init`, `fs::root_disk::init`): those are the
composition root's 32 edges, listed in the manifest apart from the debt
register ([ITEM.md](ITEM.md) §2). None of it was F-08's: `init.rs` and
`power.rs` name nothing in the load by any route.

### F-09 — item-ring syscalls reach personality modules
**Closed 2026-09-26** by W-5. All 39 references are gone: 3 from the `arch`
trap entries into `syscall`, 21 from the Linux dispatcher `syscall/mod.rs`,
and 15 from `syscall/{futex,limits,memory,system,thread}.rs` into the
personality's state. Each part took a different answer, because each was a
different question.

* **The trap entries** now call `crate::trap::system_call`, which answers
  through a `SyscallEntry` the core holds in a `Once` and `main.rs` registers
  (`syscall::dispatch`), beside the `ReturnPath` the personality registers
  for the way back. `SyscallArgs` and `Outcome`, the trap path's own contract,
  moved into `trap.rs`. With nothing registered a call is `ENOSYS`.
* **The Linux dispatcher** is split where the item's job ends. `syscall/mod.rs`
  keeps the way in, the native range, and the Linux number decoded by
  `arch::decode_syscall` with F-31's clamp in front of the table; it hands the
  decoded call to a `Personality`, a trait the item defines, which
  `syscall/linux.rs` -- the routing through the personality's modules, in the
  load ring -- implements. `main.rs` composes the two at compile time,
  registering `dispatch_with::<Linux>` as the core's entry.
* **The five files** were each asked this entry's question -- does the state it
  wants belong in the core, or is the file the personality's -- and each is
  the personality's: `futex(2)`, the rlimits and `sched_*` calls, `mmap`'s
  argument decoding onto the core's `AddressSpace`, `uname`/`sethostname`/
  `reboot(2)`'s checks over POSIX credentials, and the POSIX thread. Nothing
  in the core or the item calls them once the Linux dispatcher is above the
  item, so they moved to `load` in the manifest with no code change and no new
  edge. [ITEM.md](ITEM.md) §2 argues the move file by file.

*What the item gave up.* The item ring's product code went from 10,578 lines
to 8,002; the five files were 2,330 of it, and the Linux dispatcher's routing
most of the rest. None is
code the Security Target's claims rest on: it names the personality a threat
agent outside the TSF, and its quota is the job's, in the core. The credential
checks in `limits.rs` and `system.rs` are the personality's policy over
identities the ST claims nothing about. What that does mean, and did before, is
that load code runs in ring 0 and shares the kernel heap: `futex.rs`'s waiter
allocations are among the program-driveable sites F-23 lists, wherever the file
sits.

*Cost.* Each system call now pays one indirect call, through the core's
`Once`, where it paid none. The first shape held the personality in a second
pointer, and that showed: two million `read`/`write` calls under KVM (busybox
`dd bs=1`, 54 runs each, alternating boots) took a median 1.244 s against
1.185 s, +5.0%. Composed at compile time instead, 1.257 s against 1.242 s,
+1.2% (minimum +1.8%), which is inside this host's noise; the commit
"Compose the Linux personality with the dispatcher at compile time" has the
table. No lock and no allocation were added to the path.

*Verified by:* no `F-09` entry in the manifest, and nothing from `arch/` or
the item above its ring in `--report`; the full boot gate row, `test-threads`,
`test-jobs`, `test-net`, and `test-boot --mitigations off`.

### F-33 — a core self-check loads a Linux program
**Closed 2026-09-26** by W-5. All 5 references are gone: `arch/x86_64/
paranoid.rs` no longer names `syscall::exec`, `syscall::image`, `syscall::load`,
`syscall::process` or `fs`.

The x86-64 NMI and `#DB` entry's boot check -- which builds an ELF, loads it
with the Linux loader and starts it, to prove a breakpoint in the `SYSCALL`
trampoline fires and returns with the kernel's `GS` -- moved whole into
`arch/x86_64/paranoid/check.rs`. That is the first of the two ways this entry
said it could close: the file matches the manifest's `check.rs` test pattern,
so it is counted as the verification it is, and reaching the load ring for a
fixture is what a check may do. It is a child of the entry's module, so it
reads the entry's counters without the entry exporting them. The entry keeps
only `debug_hook`, which its own handler runs. The boot's lines are unchanged.

---

## B. Verification

### F-10 — statement coverage is 81.9%, not 100%
**Major**, advanced 2026-09-25 from 71.4%. Adding `test-jobs` to the union
takes the certified item to 5,795 of 7,073 statements: core 80.4%, item ring
85.2%.

The residual is now enumerated *and sorted*:
`coverage-residual-x86_64.json` lists all 1,278 unreached statements by file
and line, and [COVERAGE-RESIDUAL.md](COVERAGE-RESIDUAL.md) puts each into the
category table A-7 asks about.

The sorted answer is less comfortable than the percentage. **103 statements are
argued** — 65 unreachable on the measured architecture, 38 reached only when
the kernel is stopping — 121 are a statement about which machine was measured
rather than an argument, and **1,054 simply need a test**. 82% of the residual
is real work, not justification.

Two things learned in the attempt. Four further gates -- `test-btrfs`,
`test-shell`, `test-sysfs`, `test-restart` -- pass under the plugin and write
an *empty* trace, because the plugin flushes when QEMU exits and those gates
end by killing it; their coverage is unobtainable until they power the guest
down instead. And part of the residual is unreachable by construction rather
than untested: `iommu/smmuv3.rs` is 65 statements of AArch64 IOMMU that no
x86-64 run can reach, so the justification has to be made per configuration.

*Closes when:* the 1,054 in the *needs-a-test* category are covered or
individually justified. The other 224 have their argument written.

### F-11 — coverage measures the debug profile, the item ships release
**Closed 2026-09-25.** The release profile is now measured:
`coverage-x86_64-release.json`, 47.6% of the item against the debug profile's
46.6% on the same gate.

The finding's premise was right and its expected consequence was wrong. The
percentage barely moves; the *denominator* moves by a third, 7,065 statements
to 4,798, because optimisation leaves fewer distinct `is_stmt` rows to reach.
So the number survives a change of profile and the population being counted
does not, which is the thing a submission has to state. VERIFICATION.md §3.2.

### F-12 — coverage is x86-64 only
**Closed 2026-09-25.** AArch64 at 46.1% and ARMv7-A at 70.8% of the item, one
`test-boot` each: `coverage-aarch64.json`, `coverage-armv7a.json`. Every
architecture in the reference configuration can now be measured, which is what
this finding asked.

Raising them to the four-gate suite x86-64 has is part of F-10, not this. The
gap between the two Arm numbers is itself informative and recorded in
VERIFICATION.md §3.2: x86-64 carries more arch-specific code that a plain boot
never reaches, so the same gate covers a smaller share of it.

### F-13 — no decision or MC/DC coverage
**Informational.** Not required at DAL C. Required at DAL B and DAL A, and the
present method (basic-block granularity) cannot produce MC/DC without
instrumenting conditions.

### F-14 — tests are not traced to requirements
**Major.** The boot gates assert rich properties — 2,387 mappings swept for
W^X, 16 of 16 interrupt deliveries waking their waiter — but nothing links an
assertion to a requirement id. `docs/sysml/` has 33 requirements and 32
`verify`/`objective` links, at system granularity.

Requirements-based testing is the spine of DO-178C, 62304 §5.6-5.7 and
EN 50716; without the trace, the tests are evidence of *something* rather than
evidence *for* something.

---

## C. Requirements

### F-15 — no low-level requirements
**Major.** 33 requirements exist, all at system level (`<'G.1'>` kernel
threads, `<'G.2'>` address-space scale). DO-178C needs high- and low-level
requirements with the design between them; 62304 §5.4 needs detailed design
down to the software *unit*; EN 50716 needs a Software Requirements
Specification traced to components.

49,431 lines of item product code trace to 33 requirements.

### F-16 — requirements are narrative, not verifiable
**Major.** They are prose doc comments (*"Forces: 1:1 kernel threads, a real
futex, per-thread TLS registers"*) explaining why the system is shaped as it
is. Excellent design rationale; not requirements with pass/fail criteria that a
test can be written against and an assessor can check.

---

## D. Tools

### F-17 — the compiler is unqualified
**Major.** `rustc 1.97.1`, pinned exactly, no unstable features in `kernel/` or
`boot/` — good practice, and not qualification evidence.

Ferrocene is the concrete route: a qualified Rust toolchain with evidence
packages for IEC 62304 Class C, IEC 61508 SIL 4 and ISO 26262 ASIL D. Adopting
it means pinning a Ferrocene-released rustc and checking the qualified target
list; `armv7a-none-eabi` and the three UEFI targets are the ones expected to
fall outside it.

### F-18 — six code generators produce product code and are unqualified
**Moderate.** `gen-wayland-protocol.py`, `gen-xkb-tables.py`, `gen-font.py`,
`gen-term-font.py`, `gen-panic-catalog.py` and `gen-btrfs-fixtures.py` emit
committed source. Under EN 50716 §6.7 each is class T3; under DO-330 each needs
qualification or output verification.

Mitigating: each has a `--check` mode that fails the build when its output and
its input disagree, which is the beginning of the argument.

Only `gen-panic-catalog.py` and `gen-font.py` touch the item; the rest generate
load-ring or compositor code and are out of scope at the present boundary.

**Tool operational requirements written 2026-09-25**: [TOOLS.md](TOOLS.md) §6
carries TOR-1 and TOR-2 for those two — what each shall and shall not do, its
failure mode, how it is verified, and the residual that generator and
`--check` share code so the verification is not independent. The documentation
half is done; the finding stands because a shared-code check is not
qualification.

### F-19 — the build driver and gates are unclassified
**Moderate, classified 2026-09-25.** [TOOLS.md](TOOLS.md) §3 gives every gate
and `xtask` a T1/T2/T3 class, and §6's TOR-3 covers `coverage-report.py`, the
one whose failure would be least visible — coverage is offered directly as
evidence against DO-178C table A-7 rather than used to find defects, so a tool
that over-reports produces a number nobody can distinguish from a correct one.

TOR-3 records that it has **no independent verification** and does not pretend
otherwise. Its mitigation is that both biases are declared, the residual is
enumerable, and cross-checking it against raw `objdump` is what found three
measurement defects. Qualification would need a second implementation.

The finding stands on that residual.

---

## E. Safety and security analysis

### F-20 — no hazard analysis and no risk management file
**Closed at the element level 2026-09-25** by
[SAFETY-MANUAL.md](SAFETY-MANUAL.md) §5: nine failure modes of the element,
each with its effect at the element boundary, its detection, its mitigation and
its residual. FM-9 — kernel stack overflow with no guard page and no depth
bound — is named as the least-defended.

The earlier text on this finding was wrong in an instructive way. It said the
analysis needed a device and that a generic hazard list "would be a document,
not evidence". That is not how general-purpose kernels are certified: ISO 26262
Part 10's *safety element out of context*, EN 50716's *generic software* and
DO-178C's *reusable software component* all exist precisely so a component with
no application of its own can be analysed against **assumed** safety
requirements, with the system-level analysis exported to the integrator as an
assumption of use. QNX, PikeOS and VxWorks 653 all ship exactly this.

So the element's half is done and the system's half is exported as AoU-1 rather
than missing. What remains open is the *integrator's* risk file, which by
construction is not ours to write.

### F-21 — no Security Target
**Closed 2026-09-25** by [SECURITY-TARGET.md](SECURITY-TARGET.md): TOE
description and scope, assets, threats, assumptions, security objectives, SFRs
drawn from CC Part 2, a TOE summary specification mapping each objective to the
code and the evidence, and rationale. EAL5+ (ALC_FLR.2) claimed.

Superseded by F-21a and F-21b, which are what the ST itself records as the
reasons it would not survive evaluation.

### F-21a — no vulnerability analysis
**Closed 2026-09-25** by
[VULNERABILITY-ANALYSIS.md](VULNERABILITY-ANALYSIS.md): all seven ST threats,
attack paths enumerated per threat with the resisting mechanism, the evidence
and a verdict. Five residual vulnerabilities V-01 to V-05, superseded by F-32.

It also corrected an error in the Security Target it was written against, which
is the most useful thing it did. See F-32.

### F-32 — no SMAP, SMEP or PAN; one software check guards kernel memory
**Closed 2026-09-25**, with one honest caveat about the emulated CPU.
`CR4.SMEP` and `CR4.SMAP` are set in `init_traps` when CPUID reports them, and
secondary processors inherit them through the `CR4` snapshot
`smp::secondary_start` already copied. The boot says so:
*"cpu   ring 0 kept out of user pages: SMEP on, SMAP on"*.

**Turning it on found three real violations, and all three are in test code.**
`user/check.rs` installs an address space and reaches a user linear address on
purpose — to prove the processor walks an installed space, and to prove a
task's own space is the one installed when it runs. SMAP refused each, loudly:
a page fault at 0x50000000, then 0x30000000. They are bracketed with
`arch::permit_user_access` / `forbid_user_access`, `EFLAGS.AC` via `stac` and
`clac`, with the window kept tight around the access in the case that yields,
since `AC` is part of the context a switch carries.

**No product-code path needed one.** That is the result worth having: the claim
in `uaccess`'s header — that every legitimate access to a program's memory goes
through the direct map and never through a user linear address — is now
enforced by hardware rather than asserted, and it survived `test-boot`,
`test-threads` and `test-vfs`.

**AArch64 has PAN too**, implemented the same way: `PSTATE.PAN` set, and
`SCTLR_EL1.SPAN` *cleared* so an exception entry from user mode does not undo
it — the part that is easy to miss, since leaving SPAN set turns the protection
off for exactly the code that handles system calls. It needed no access windows
beyond the three SMAP already required, which confirms the same invariant holds
there.

Two things about it are worth recording rather than glossing.

The instruction is emitted as a word. `msr pan, #1` needs the ARMv8.1 `pan`
extension the target does not enable; `.arch_extension pan` inside an `asm!`
changes assembler state for the whole translation unit and broke section
emission, failing the link on anonymous constants; and a `const` operand to
`.inst` did the same. `0xd500419f` and `0xd500409f` are written literally, with
the derivation in a comment — the same two words Linux emits.

And the reference configuration's CPU does not have the feature. `cortex-a72`
is ARMv8.0; PAN is 8.1. The boot correctly reports *"PAN unavailable"* and
carries on. Demonstrated on a CPU that has it via `FERRIX_ARM_CPU=max`, which
prints *"PAN on"* and reaches `FERRIX-BOOT-OK`. Whether to move the Arm
reference CPU is a project decision about what every Arm test runs on, not a
certification fix, and it is left open deliberately.

**ARMv7-A cannot have it at all**: the Cortex-A7 is ARMv7-A and PAN is an
ARMv8.1 feature. There the software bound check remains the only barrier, and
V-01 stands. That is a hardware limit, not a gap that work closes.

Original text follows.

**Was:** **Major.** `uaccess.rs` says so in its own header and the code confirms it: no
`CR4.SMAP` or `CR4.SMEP` bit is set on x86-64, no `PAN` on AArch64. The bound
check in `uaccess` is the only thing between a user pointer and a read or write
of kernel memory at kernel privilege (V-01), and it bears on three of the seven
threats.

The mitigation is sound — one chokepoint, checked first, before any arithmetic
that could wrap — and it has no defence in depth. One syscall that ever
dereferences a user pointer without going through `uaccess` is an immediate
compromise; SMAP and PAN exist to make that a fault instead.

Worth recording how it was missed: an early sweep of this tree counted 56
matches for "smap" and concluded the feature was wired up. They are
`smap_base`, `smap_len` and `smap_phys` — the **s**ystem **map**. The Security
Target asserted SMAP/PAN enforcement on that basis until the vulnerability
analysis checked the registers.

*Closes when:* SMEP and SMAP are enabled on x86-64 with `stac`/`clac` around
the copy, and PAN on AArch64.

### F-31 — no side-channel or layout-randomisation defences
**Moderate, advanced 2026-09-26** by [SPECULATION.md](SPECULATION.md). Not
closed: the layout-randomisation half is untouched.

*Was:* no Spectre, Meltdown or cache-timing analysis, and no mitigation — no
retpolines, no KPTI, no IBT or shadow stacks, no ASLR or KASLR.

*Now:* the speculative-execution half is analysed per architecture and built,
behind the kernel's one build switch. `cargo xtask --mitigations on`, the
default and the reference configuration, gives every program-chosen index at
the system call boundary a clamp a misprediction cannot see past (syscall
numbers, handles, descriptors, user addresses), and applies what each
processor needs and offers: on x86-64 enhanced or automatic IBRS, STIBP, SSBD,
`IBPB` and a return stack refill at each switch of address space, `VERW` on an
MDS-exposed part, a `swapgs` fence and cleared registers on entry; on AArch64
the Spectre-BHB loop, `SSBS` or firmware's workaround 2, and firmware's
workaround 1 at a switch, each decided by every core for itself so that a
machine of mixed cores (a Pixel 7's A55s, A78s and X1s) gets what each kind
needs; on ARMv7-A `BPIALL`/`ICIALLU` for the cores Arm lists
as affected, of which the reference Cortex-A7 is not one. Every processor reads
back what it wrote and the boot check fails otherwise (FX-0307); the boot log
names what is covered and what is not, on AArch64 for each kind of core. `--mitigations off` compiles all of it
out, and `cargo xtask check` builds both settings. Measured cost under KVM: +1.0%
on two million system calls, +2.8% on a thousand fork-exec-waits.

What a processor needs and the build cannot give it — a Meltdown-affected part,
or one with no IBRS form, no `IBPB`, no `SSBD` — is excluded by the new AoU-11
rather than mitigated. Retpolines were evaluated and rejected: the pinned
compiler has them only through a deprecated target feature scheduled to become
an error, and not in the precompiled `core` and `alloc`.

*Closes when:* KASLR exists (SPECULATION.md §6 lists the four steps, loaders
first); IBT and shadow stacks are either built or argued out; and the
residuals SPECULATION.md §9 lists — cache timing between processes, KPTI for
an affected CPU if one enters the reference configuration, the libraries'
clamps without `csdb`, the tables deeper than the system call boundary — are
each built or argued. At `AVA_VAN.4`'s moderate attack potential the half that
is done was the half that mattered more: without it, isolation between
processes held only against programs that did not time their loads.

### F-21b — the TOE claims no audit and no authentication
**Moderate.** There is no FAU family at all, and FIA lives in the uncertified
load ring. Defensible for an isolation kernel and the reason no OS Protection
Profile can be claimed — but an evaluator would press on whether a TOE that
cannot record a security-relevant event can claim EAL5.

### F-22 — no safety case
**Closed at the element level 2026-09-25** by
[SAFETY-MANUAL.md](SAFETY-MANUAL.md): the argument is §2 (assumed safety
requirements, with the evidence for each), §3 (the safe state, and the
obligation it creates), §4 (eleven assumptions of use) and §5 (the failure
analysis).

The generic application conditions EN 50716 asks for are AoU-1 to AoU-11, and
several of them exist *because* a finding is open — no WCET (F-24), fatal
allocation failure (F-23), reduced claims on ARMv7-A (F-32, V-03), no audit
(F-21b), processors the side-channel defences do not cover (F-31). Those stop being embarrassments and become stated conditions the
integrator designs around, which is what an application condition is for.

What remains is assessment by somebody independent, which is F-27 and not
this.

### F-23 — dynamic memory allocation throughout, with no bounded-allocation argument
**Major, analysed 2026-09-25** in
[MEMORY-AND-TIMING.md](MEMORY-AND-TIMING.md) §1. Not closed: the analysis
concludes the property does not hold, and recording that as a closure would be
the failure this register exists to avoid.

Now measured rather than impressionistic. **225 allocation sites across 40
files** in the item's product code, over four allocators. And the part that is
worse than "unbounded": `KernelAllocator::alloc` returns null on failure and
**there is no `#[alloc_error_handler]` in the tree**, so a failing `Box::new`
reaches Rust's default handler and aborts. Allocation failure in the certified
item is fatal, not recoverable, at all 225 sites — even though `libs/heap`
itself reports `OutOfMemory` properly and the `GlobalAlloc` adapter above it
throws that distinction away.

And the obvious fix is unavailable. `#[alloc_error_handler]` is an unstable
library feature (rust-lang #51540), verified against the pinned 1.97.1, and
`kernel/` uses no unstable features by policy. `Box::try_new` and
`Arc::try_new` are unstable for the same reason; only `Vec::try_reserve` is
stable, and it covers growth rather than the `Box` and `Arc` allocations that
dominate the 225 sites. Making failure recoverable therefore means hand-rolled
fallible construction site by site — and a Ferrocene toolchain (F-17) would not
change that.

### F-24 — no worst-case execution time analysis
**Moderate, scoped 2026-09-25** in
[MEMORY-AND-TIMING.md](MEMORY-AND-TIMING.md) §2. Not closed, and will not be:
no WCET is claimed.

What the analysis adds is consequences. It lists what the item *does* promise
instead — EDF admission control, partitioned scheduling, bounded RT critical
sections, a preemptible kernel, interrupts that cannot steal unaccounted time —
and what each standard therefore does and does not get. It also notes that DAL
C does not require WCET as such, so this is not what blocks that rating; the
absence of any stated timing requirement to verify is, and that is F-15.

The boundary helps here more than anywhere: a WCET argument over 93,646 lines
including btrfs and a TCP stack is not a project; over the 38,989-line `core`
ring, with no recursion anywhere, it is at least conceivable.

### F-25 — no complexity, unit-size or recursion limits
**Closed 2026-09-25** by `scripts/check-complexity.py`, a ratchet over
`scripts/complexity-baseline.json` in the shape the item-boundary gate uses:
47 of the item's 1,887 functions sit above a floor, and the gate fails when one
gets worse, when a new one appears, or when a stale entry is left behind.

The measurement that matters: **no function in the certified item is directly
recursive.** For a kernel with no guard page under its stack that is worth
having as an enforced property rather than a belief.

Getting there needed three corrections, each a real defect in the measurement
rather than in the tree. Matching a bare name called 124 architecture-facade
shims recursive, because `fn flush_tlb` forwarding to `aarch64::flush_tlb`
names itself. `drop(x)` inside a `Drop::drop` body is `core::mem::drop`. And
taking the next `{` after a signature gave every `extern "C"` declaration the
*following* item's body, which is how the assembly symbol `ferrix_switch` came
out recursive with borrowed complexity and length scores.

A fourth was found on 2026-09-26, and it was the largest. The pattern that
stripped string literals could not cross a `\`-newline continuation, so after
the first one in a file it paired every quote with the wrong partner and read
code as string from there on. **It measured 1,559 functions of 1,887: 328
of the item's functions, 17%, were not measured at all** -- 73 of `sched/mod.rs`'s 89
functions and 71 of `main.rs`'s 76 among them. `main.rs::say_booted` scored
102 lines because it had swallowed everything up to the next string that
happened to pair; it has seven. The gate now reads source through
`scripts/rustlex.py`, a lexer shared with the item-boundary gate that knows
nested comments, raw, byte and C strings, continuations, and a char literal
from a lifetime, and every run starts with its self-test. Re-measured, the
baseline went from 34 entries to 47: twelve functions that were always over a
floor and were never seen, two that were scored just under one, three whose
scores were understated (`devmgr.rs::start` by six lines), and `say_booted`
gone. No function in the item became more complex; the
measurement became less wrong, which is the one reason the baseline may rise.
The no-recursion result holds over all 1,887.

Complexity is an approximation — branch tokens, not a control-flow graph — and
the script's docstring says so, along with the two kinds of recursion it cannot
see: mutual, and through a function pointer or trait object.

### F-26 — `unsafe` is documented but not traced
**Moderate.** 662 blocks, every one with a `SAFETY:` comment, one operation
each, counted per crate by `check-unsafe-audit.py`. Best-in-class as hygiene.
For an assurance argument each block in the item also needs to trace to the
requirement or hazard that justifies it.

---

## F. Organisational

These cannot be closed by engineering. They are recorded because an audit that
omits them is not an audit.

### F-27 — no independence
**Blocking** for formal certification at any level. No independent verifier,
validator or assessor. EN 50716 at SIL 2 is permissive — roles may be combined
with justification — but DO-178C DAL C still requires independence for 5 of its
62 objectives, and a CC evaluation requires an accredited laboratory by
definition.

### F-28 — no quality management system
**Blocking** for IEC 62304, which presumes ISO 13485. No documented
configuration management procedure, problem-resolution process (§9) or
maintenance plan (§6) in standard terms. `docs/CONVENTIONS.md` is 135 lines
about commit authorship and agent coordination.

### F-29 — development security is not demonstrable
**Moderate** now; **Blocking** at EAL6+ (`ALC_DVS.2`). Development happens in
ephemeral cloud containers with AI agents as authors. The one-author-per-commit
gate is real provenance control and is not a controlled site with personnel
vetting and need-to-know.

Also genuinely novel: no scheme has settled how to treat AI-authored code in a
certified item. It should be raised with a certification body early rather than
discovered at assessment.

### F-30 — no field history
**Moderate.** Every rating here is argued from construction and verification.
The proven-in-use credit that IEC 61508 route 2s and EN 50716's prior-use
provisions offer Linux is unavailable to a kernel this young.

---

## Closed

### F-00 — the kernel had no structural coverage measurement
**Closed 2026-09-25** by `scripts/coverage-report.py` and the
`FERRIX_QEMU_PLUGIN` hook. Superseded by F-10 to F-13, which are about the
*level* of coverage rather than its absence.

### F-0A — the certified item's SOUP was unenumerated
**Closed 2026-09-25** by `scripts/gen-soup.py`, which measured it as empty and
now fails the build if that stops being true.
