# Findings

The audit register for the item defined in [ITEM.md](ITEM.md). One entry per
finding, each naming what was measured, which objective it bears on, and what
would close it.

27 findings are open and 11 are closed. F-32 is closed on x86-64 and open on the two Arm architectures. F-10 advanced from 71.4% to 81.9%. No finding here is closed by argument:
a finding closes when the thing it describes stops being true and something in
the build says so.

**Severity.** *Blocking* — a rating cannot be claimed while it stands.
*Major* — a named objective is unmet. *Moderate* — an objective is partially
met or met without evidence. *Minor* — a defect with no objective attached yet.

| | Blocking | Major | Moderate | Minor | Informational |
|---|---:|---:|---:|---:|---:|
| Open | 4 | 9 | 12 | 2 | 1 |

Blocking: F-20, F-22, F-27, F-28 — a hazard analysis, a safety case,
independent assessment and a quality management system. Two are documents that
need an application context the repository does not have; two need an
organisation. None is a defect in the code.

---

## A. Boundary integrity

Measured by `scripts/check-item-boundary.py`; 62 upward references in 27 files.
These are recorded in `scripts/certification-item.json` as a debt register that
may shrink freely and may not grow.

### F-01 — the `Process` type is a core concept living in the Linux personality
**Major.** 12 references from `core` into `syscall::process`, for `Process`,
`ProcessRef` and `current()`.

A process *is* the address-space and capability container the core enforces
isolation between, so the core is right to need it. But it is defined in
`syscall/process.rs`, 2,229 lines of Linux-personality code (`fork`, `wait4`,
`getpid`), which means the trusted base structurally depends on a file that is
almost entirely untrusted. EAL5 `ADV_INT.2` asks for well-structured internals
and this is the clearest counter-example in the item.

*Closes when:* `Process`, `ProcessRef` and `current()` move to
`kernel/src/object/process.rs` and the 12 references point into `core`.

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
**Moderate.** 1 reference. `trap.rs`'s `user_fault` names `syscall::deliver`,
`syscall::signal` and `syscall::process` to turn a page fault into a `SIGSEGV`.

A different upcall from the one F-02 inverted: that was the return path, this
is fault delivery. Split out rather than folded into F-02, because closing the
return path does not close this and the register should not imply otherwise.
The fix is the same shape — a core-owned interface the personality registers
into — and it wants F-01 done first, since two of its three references are the
`Process` type.

### F-03 — architecture modules name the personality's `StatLayout`
**Closed 2026-09-25.** The `StatLayout` enum moved to `kernel/src/arch/mod.rs`,
beside the other ABI facts the facade carries; its `impl` stayed in
`syscall/stat.rs`, which is legal within a crate. Data in the core, behaviour
in the personality, and the dependency now points downward. 62 upward
references became 59.

### F-04 — the core device registry names STM32MP1 board support
**Minor.** 6 references from `device.rs` into `stm32mp1`, `stm32mp1_gpu` and
`stm32mp1_usb`.

*Closes when:* board support registers itself with the registry rather than
being named by it.

### F-05 — `claim.rs` names `block_ring`
**Closed 2026-09-25.** Only the `StillServed` enum was wanted, and it belongs
to the claim rather than to the ring: a quiesce asks whether anything still
serves a node, and the answer must not depend on which uncertified subsystem
happens to be serving it. Moved to `kernel/src/claim.rs`; `block_ring`,
`render`, `display` and `native` now answer with the core's type.

### F-06 — core names two item-ring modules
**Minor.** 4 references from `object/job.rs` and `sched/` into
`syscall::registry` and `syscall::thread`. Inner-ring only: it does not affect
the present ratings, and it is on the ratchet's path to an EAL6+/ASIL D `core`.

### F-07 — the native ABI dispatcher fans out across the load ring
**Major.** 9 references from `syscall/native.rs` into `block_ring`, `net_ring`,
`fs::cgroupfs`, `display`, `render`, `input`, `stm32mp1` and three personality
syscall modules.

Expected of a dispatcher and still a dependency: the item's exported interface
cannot be analysed without the whole of the load ring it dispatches into.

*Closes when:* subsystems register handlers in a table the dispatcher walks,
rather than the dispatcher naming each subsystem.

### F-08 — bring-up and power name the filesystem
**Moderate.** 6 references from `init.rs`, `power.rs` and `devmgr.rs` into
`fs`, `fs::root_disk`, `fs::data_disk`, `block_ring` and `stm32mp1`.

Legitimate in intent — init has to start a filesystem, and power has to flush
one — and the fix is the same interface inversion as F-07.

### F-09 — item-ring syscalls reach personality modules
**Moderate.** 14 references from `syscall/{futex,limits,memory,mod,registry,
system,thread}.rs` into `syscall::{process,time,poll,fd,attributes,credentials,
signal}` and `render::node`.

The consequence of F-01 mostly: these modules want the process object, and the
process object is in the wrong place.

---

## B. Verification

### F-10 — statement coverage is 81.9%, not 100%
**Major**, advanced 2026-09-25 from 71.4%. Adding `test-jobs` to the union
takes the certified item to 5,795 of 7,073 statements: core 80.4%, item ring
85.2%.

The residual is now *enumerated* rather than implied:
`coverage-residual-x86_64.json` lists all 1,278 unreached statements by file
and line, which is the list the remaining work starts from.

Two things learned in the attempt. Four further gates -- `test-btrfs`,
`test-shell`, `test-sysfs`, `test-restart` -- pass under the plugin and write
an *empty* trace, because the plugin flushes when QEMU exits and those gates
end by killing it; their coverage is unobtainable until they power the guest
down instead. And part of the residual is unreachable by construction rather
than untested: `iommu/smmuv3.rs` is 65 statements of AArch64 IOMMU that no
x86-64 run can reach, so the justification has to be made per configuration.

*Closes when:* every one of the 1,278 is either covered by a test naming a
requirement or carries a written justification.

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

48,887 lines of item product code trace to 33 requirements.

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
**Blocking** for IEC 62304 Class C and EN 50716 SIL 2.

62304 does not stand alone: it presumes ISO 14971 risk management, and §7.1
requires every hazard to be traced to the software items that could contribute
to it. EN 50716 sits under EN 50126 RAMS with SIL apportionment from a system
hazard analysis. Neither exists.

This one is genuinely application-dependent — hazards belong to a device or a
train, not to a kernel — so closing it needs an operational context the
repository does not have. A generic hazard list would be a document, not
evidence.

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
**Moderate.** No Spectre, Meltdown or cache-timing analysis has been performed
and no mitigation exists: no retpolines, no KPTI, no IBT or shadow stacks. No
ASLR or KASLR either, so an attacker who achieves V-01 faces a fixed layout.

At `AVA_VAN.4`'s moderate attack potential this is arguably in scope for a TOE
whose entire claim is isolation between mutually distrusting processes.

### F-21b — the TOE claims no audit and no authentication
**Moderate.** There is no FAU family at all, and FIA lives in the uncertified
load ring. Defensible for an isolation kernel and the reason no OS Protection
Profile can be claimed — but an evaluator would press on whether a TOE that
cannot record a security-relevant event can claim EAL5.

### F-22 — no safety case
**Blocking** for EN 50716 SIL 2. No EN 50129-shaped argument, no generic
application conditions.

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

The analysis lists four closure routes by cost, of which the cheapest is worth
doing on its own: an `#[alloc_error_handler]` with a catalogued explanation, so
the present behaviour is deliberate rather than inherited.

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

The boundary helps here more than anywhere: a WCET argument over 93,714 lines
including btrfs and a TCP stack is not a project; over the 38,719-line `core`
ring, with no recursion anywhere, it is at least conceivable.

### F-25 — no complexity, unit-size or recursion limits
**Closed 2026-09-25** by `scripts/check-complexity.py`, a ratchet over
`scripts/complexity-baseline.json` in the shape the item-boundary gate uses:
35 functions in the item sit above a floor, and the gate fails when one gets
worse, when a new one appears, or when a stale entry is left behind.

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
