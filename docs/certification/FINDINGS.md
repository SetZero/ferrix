# Findings

The audit register for the item defined in [ITEM.md](ITEM.md). One entry per
finding, each naming what was measured, which objective it bears on, and what
would close it.

30 findings are open and 5 are closed. No finding here is closed by argument:
a finding closes when the thing it describes stops being true and something in
the build says so.

**Severity.** *Blocking* — a rating cannot be claimed while it stands.
*Major* — a named objective is unmet. *Moderate* — an objective is partially
met or met without evidence. *Minor* — a defect with no objective attached yet.

| | Blocking | Major | Moderate | Minor | Informational |
|---|---:|---:|---:|---:|---:|
| Open | 4 | 9 | 13 | 3 | 1 |

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
**Minor.** 1 reference from the core's resource-claim code into a load-ring
driver protocol.

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

### F-10 — statement coverage is 71.4%, not 100%
**Major.** Measured over `test-boot`, `test-threads`, `test-vfs` and `test-net`:
core 69.5%, item ring 74.9%, certified item 5,010 of 7,016 statements.

DO-178C table A-7 objective 5 at DAL C wants statement coverage complete, with
every gap either driven by a new requirements-based test or justified as
unreachable defensive code. 71.4% is a real measurement where there was none,
and it is not a pass.

*Closes when:* the remaining 2,006 statements are either covered or listed with
justifications.

### F-11 — coverage measures the debug profile, the item ships release
**Moderate.** The reference configuration in the manifest is `release`;
`coverage-x86_64.json` was produced from `target/x86_64-unknown-none/debug`.

Optimised builds inline, so the line table is approximate and the numbers would
move. DAL C requires the coverage analysis to address the configuration that
ships, or to argue the difference.

### F-12 — coverage is x86-64 only
**Moderate.** AArch64 and ARMv7-A are in the reference configuration and have
no coverage data. Both are supported by the tooling as written; nobody has run
them.

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

### F-19 — the build driver and gates are unclassified
**Moderate.** `xtask` (242 tests) and the eleven gate scripts decide what ships
and whether it passes. They need T1/T2/T3 classification and, for anything T3,
a qualification argument.

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
**Major.** `AVA_VAN.4` requires a methodical analysis against moderate attack
potential. None has been performed, and it is the largest single gap between
the Security Target and an evaluable one.

### F-21b — the TOE claims no audit and no authentication
**Moderate.** There is no FAU family at all, and FIA lives in the uncertified
load ring. Defensible for an isolation kernel and the reason no OS Protection
Profile can be claimed — but an evaluator would press on whether a TOE that
cannot record a security-relevant event can claim EAL5.

### F-22 — no safety case
**Blocking** for EN 50716 SIL 2. No EN 50129-shaped argument, no generic
application conditions.

### F-23 — dynamic memory allocation throughout, with no bounded-allocation argument
**Major.** Buddy allocator, slab, kernel heap, reclaim, demand paging, CoW and
an OOM killer, all inside the item.

EN 50716 Annex A discourages dynamic memory at SIL 2 and above; DO-178C needs
a defect-free-allocation argument covering fragmentation and exhaustion. An OOM
killer is a non-deterministic failure mode in the trusted base.

### F-24 — no worst-case execution time analysis
**Moderate.** `docs/ARCHITECTURE.md` §5 states plainly that no certified WCET
is promised for a kernel that also hosts LLVM. Correct, self-aware, and a gap
that must be declared in any safety case rather than discovered in one.

### F-25 — no complexity, unit-size or recursion limits
**Moderate.** Eleven gates enforce unsafe documentation, panic exemptions, the
assembly budget, device access, crate layering and now the item boundary —
none bounds cyclomatic complexity, function length or recursion, all of which
a SIL 2 coding standard must specify with metrics.

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
