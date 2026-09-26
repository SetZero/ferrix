# The certified item

Four assurance ratings are being argued for in this directory. Every one of
them is a claim about a *scope*, and naming that scope precisely is the first
and most consequential decision in the whole exercise — because each artifact
here is written against it. The Security Target's TOE, the hazard analysis's
safety item, the traceability matrix and the coverage obligation all mean the
same thing, and that thing is defined in
[`scripts/certification-item.json`](../../scripts/certification-item.json) and
enforced by `scripts/check-item-boundary.py`.

A boundary that lives only in a document is a boundary that has already moved.

---

## 1. Why the item is not Ferrix

Ferrix's acceptance test is that it hosts `rustc`, and since 2026-09-23 that it
builds itself. That goal is why the system is worth anything, and it is also
exactly why the whole of it cannot carry an assurance rating. A certified item
must be a frozen, analysable configuration with every line traced to a
requirement; a self-hosting general-purpose OS with a Wayland compositor, a
browser and a package of shell themes is the opposite of that, on purpose.

`docs/ARCHITECTURE.md` §1 argues for a monolithic core with capability seams
because that is what a compiler workload needs. That decision and a
certificate over the whole kernel are mutually exclusive. The way out is not to
abandon either one: it is to notice that the architecture already contains the
seam a certificate needs, and to say where it is.

**The item is therefore a subset of the kernel, and the rest of Ferrix is
uncertified load running on top of it.** This is the same shape as every
certified separation kernel, and the same shape 62304 §4.3(c) has in mind when
it permits software items of different classes in one system.

---

## 2. Three rings

The manifest puts every one of the kernel's source files into exactly one of
three nested rings. A file in no ring fails the build.

| Ring | Product lines | In-kernel test lines | Carries |
|---|---:|---:|---|
| `core` | 40,704 | 7,808 | EAL6+, ASIL D, SIL 3/4, DAL B — *aspirational* |
| `item` | 10,455 | 248 | EAL5+, DAL C, Class C, SIL 2 — *the present claim* |
| `load` | 44,272 | 23,080 | nothing |

**The certified item is `core` + `item`: 51,159 lines of product code**, against
44,272 lines of uncertified load. The item is 53.6% of the kernel's product
code.

### `core` — the minimal trusted base

Memory protection, scheduling, capability objects, the trap and syscall entry
paths, the IOMMU, SMP, the MMU and CPU control for three architectures, the
firmware tables that say where the CPUs and timers are, the device registry
that makes an MMIO claim exclusive, and the panic path.

This is the code that must be correct for isolation to mean anything. Nothing
here may depend on a filesystem, a network stack or a device driver, and
`check-item-boundary.py` asserts that rather than trusting it.

### `item` — the core, plus what brings it up and dispatches into it

Bring-up (`main.rs`, `init.rs`), the native ABI dispatcher and the handful of
syscalls that belong to the item rather than to the Linux personality
(`native`, `uaccess`, `memory`, `thread`, `program`, `limits`, `system`,
`futex`), PCI enumeration, `devmgr`, the entropy source and power. The pid
table was the item's `syscall/registry.rs` until W-1 moved it into the core
(`object/process.rs`); what that file keeps is the Linux personality's typed
lookup, and it is in `load` with the rest of the personality.

The native ABI is here rather than in `core` because it is the interface the
item *exports*, and an interface is evaluated with the thing that exports it.

Where the item has to act on the load -- power commits a filesystem before the
machine stops, init starts a program from one, `devmgr` reads its drivers
from one, device enumeration asks board support what it prepared -- the item
defines the interface and the load registers into it
(`kernel/src/hooks.rs`). `main.rs` is the crate root: it declares every
module, and its `register_load` is the one place the load is told to
register, in bring-up order, with a boot check that it did.

**`main.rs` is in the item, and it is the composition root.** The manifest
puts it in the `item` ring as bring-up, and nothing about that is changed
here. But it is also where the load is put together with the item. Since
2026-09-26 the gate reads its calls into the load -- 32 modules -- and
records them under `composition_root` in the manifest rather than in the
debt register: ratcheted the same way, filed against no finding. Of the 32,
20 are the load's own boot self-checks (`fs::check`, `net::check`,
`syscall::check` and seventeen more verification files), and 12 are product
modules: registration (`syscall::launch`, `stm32mp1`, `fs`), the load's
subsystems brought up in order (`fs::root_disk`, `fs::data_disk`, `net`,
`syscall::deliver`, `syscall::time`, `display`), and the pieces `main.rs`'s
own boot checks drive a program through (`syscall::image`, `syscall::exec`,
`fs::cgroupfs`).

That is a judgement an assessor has to accept, and it is only as good as the
claim that those edges carry composition and no item logic. It is plausible
from the list; it is not checked, because the exemption covers the file, and
`main.rs` is 2,897 lines. Making it checkable means reducing the root to
composition, with bring-up logic in item-ring modules that name nothing above
them, and the boot checks that drive the load in verification files.

A `mod` declaration is not counted as an edge anywhere. A parent declaring
its child says where the child sits in the module tree, not that the parent's
code runs it: `main.rs` declares 10 load-ring modules and `syscall/mod.rs`
declares 30. What either file's *code* then does with them is resolved and
counted like any other reference.

### `load` — everything it runs and does not vouch for

The VFS, btrfs, procfs, sysfs, cgroupfs and tmpfs; the network stack; the
Linux personality's syscall surface; the display, render, input and ring
drivers' kernel halves; STM32MP1 board support.

This is not a list of code that matters less — it is most of what makes Ferrix
useful. It is excluded because a defect in it is bounded by the item's own
enforcement, and because a claim over 95,431 lines is one nobody can afford to
substantiate.

---

## 3. Why the rings nest

The obvious alternative was to draw one boundary for the four present ratings
and a second, tighter one later if EAL6+ or ASIL D were ever wanted. That is
the expensive mistake. Every artifact in this directory is scoped to the item;
re-scoping later means rewriting the Security Target, the hazard analysis, the
trace matrix and the coverage evidence, because none of them mean anything
detached from a boundary.

Nesting makes the boundary a **ratchet**. Raising the target becomes a matter
of moving modules from `item` to `core` and paying down the findings in §4 —
not of starting the paperwork again. `core` is named now, while naming it is
free, precisely because the cost of discovering the right boundary later is
every document written against the wrong one.

The same nesting is what makes the ratings honestly *ordered*. `core` at
40,704 lines is in the size range where EAL6-grade work has actually been done
(INTEGRITY-178B, ~10k SLOC, is the benchmark and is still four times smaller).
It is not there yet. Saying which ring carries which target keeps that gap
visible instead of letting "Ferrix is certified" absorb it.

---

## 4. What the measurement found

The boundary above is a claim about dependencies, so the gate measures it.
Today the item contains **56 upward references in 12 files** — places where a
ring names something in a ring above it — plus the composition root's 32
(§2). They are recorded in the manifest against finding ids and analysed in
[FINDINGS.md](FINDINGS.md).

**The counts this section gave before 2026-09-26 — 29, and 62 when the audit
began — were lower bounds.** The gate matched only the literal text
`crate::a::b`, so it missed nested `use` groups, paths through a name bound by
`use` or declared by `mod`, and every path in code its string pattern had taken
for a literal. It now resolves names as the compiler does
(`scripts/check-item-boundary.py`, whose docstring says how, and what it still
cannot see: an edge that is a type flowing through a value rather than a name
written in the file). Re-measured, today's tree has 56 where 29 were
reported, and the audit's starting tree has 94 where 62 were.

They are not a reason to move the boundary. They are the reason the boundary is
worth having: each one is a specific, addressable piece of coupling that was
invisible while the architecture was described in prose. Thirty-eight have
been paid down since the audit began, by today's measure — F-02, F-02a, F-03,
F-04, F-05 and F-08, and F-01 and F-06 by splitting the process (W-1) — and
of the three that remain, two are structural rather than incidental:

* **F-07** — `syscall/native.rs`, the native ABI dispatcher, names ten
  modules in the load ring, and `devmgr.rs` two more for the process it
  starts. Expected of a dispatcher, and still a dependency.
* **F-09** — Linux-personality syscalls sitting in the item ring: `brk`,
  rlimits, the Linux dispatcher, the POSIX thread. The Linux dispatcher,
  `syscall/mod.rs`, accounts for 21 of its 39 references. Since W-1 split the
  core process out of the POSIX one (`kernel/src/object/process.rs`), what
  these files want from the personality is its state, not the process
  concept, and the open question for each is whether the item should hold it
  at all.
* **F-33** — the x86-64 paranoid entry's boot check, in the core, loads and
  starts a Linux program. Incidental: it is verification code in a product
  file.

`object/` and `sched/` name nothing above the core since W-1: a task holds a
`sched::UserThread` and the core holds a process whole only as an
`object::process::Host`, a trait the personality implements.

`trap.rs` — the most trusted file in the kernel — now names nothing above the
core, which it did in three places when the audit began.

The gate's debt register may shrink without ceremony and may not grow without a
diff somebody argues for. Stale entries fail too, so a fixed breach cannot
leave a permanent exemption behind.

---

## 5. The reference configuration

A certificate attaches to a configuration, not to a repository.

| | |
|---|---|
| Architectures | x86-64, AArch64, ARMv7-A |
| Profile | release |
| Toolchain | rustc 1.97.1, pinned exactly in `rust-toolchain.toml` |
| Unstable features | none in `kernel/` or `boot/` |
| Cargo features | 7 in the workspace, **0** in `kernel/` or `boot/` |
| Build settings | **one**, `cargo xtask --mitigations on\|off`; the reference is `on`, the default |
| External crates | 21, listed in [SOUP.md](SOUP.md) |
| Assembly | 492 lines across 22 allow-listed sites outside the Pixel 7 loader, ~99.76% Rust (`check-asm-budget.py`) |

The feature count is the line worth pausing on. A certified item must be one
configuration with all dead and deactivated code justified; Linux's ~18,000
Kconfig symbols are why that objective is unmeetable there at any budget. Here
the configuration space is three architectures and one switch with two
settings, which is most of why this item is analysable at all.

The switch is the side-channel defences ([SPECULATION.md](SPECULATION.md)).
`off` builds with `--cfg ferrix_mitigations_off`, set only by `xtask`, and
compiles every defence out; it exists to measure what they cost and for owners
who have decided they need none. It is not a Cargo feature, and the claims
here are made of `on` alone (SAFETY-MANUAL AoU-8). `cargo xtask check` builds
the kernel in both settings on all three architectures so that `off` cannot
stop compiling unnoticed, and the running kernel says which it is.

---

## 6. What this item deliberately does not claim

* **It is not a separation kernel.** It does not yet offer time or space
  partitioning as a service; stage 13's namespaces and stage 14's real-time
  domains are unbuilt. `docs/ARCHITECTURE.md` §5 is explicit that no certified
  worst-case execution time is promised for a kernel that also hosts LLVM.
* **It carries no field history.** Every rating here is argued from
  construction and verification evidence. The proven-in-use route that IEC
  61508 route 2s and EN 50716's prior-use provisions open to Linux is closed to
  a kernel this young, and nothing in this directory pretends otherwise.
* **It has not been assessed by anyone independent.** See
  [FINDINGS.md](FINDINGS.md) §Organisational, where that is recorded as the
  finding it is rather than omitted.
