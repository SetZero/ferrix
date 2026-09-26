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
| `core` | 43,523 | 8,213 | EAL6+, ASIL D, SIL 3/4, DAL B — *aspirational* |
| `item` | 8,002 | 248 | EAL5+, DAL C, Class C, SIL 2 — *the present claim* |
| `load` | 47,528 | 23,324 | nothing |

**The certified item is `core` + `item`: 51,525 lines of product code**, against
47,528 lines of uncertified load. The item is 52.0% of the kernel's product
code. (Measured 2026-09-26, after W-5 moved the Linux dispatcher's routing and
five of the personality's files out of the `item` ring: see below.)

### `core` — the minimal trusted base

Memory protection, scheduling, capability objects, the trap and syscall entry
paths, the IOMMU, SMP, the MMU and CPU control for three architectures, the
firmware tables that say where the CPUs and timers are, the device registry
that makes an MMIO claim exclusive, and the panic path.

This is the code that must be correct for isolation to mean anything. Nothing
here may depend on a filesystem, a network stack or a device driver, and
`check-item-boundary.py` asserts that rather than trusting it.

### `item` — the core, plus what brings it up and dispatches into it

Bring-up (`main.rs`, `init.rs`), the system call dispatcher's way in
(`syscall/mod.rs`: the native range, and a Linux number decoded with its
Spectre clamp), the native ABI (`native`), the program file init starts
(`program`), PCI enumeration, `devmgr`, the entropy source and power. The pid
table was the item's `syscall/registry.rs` until W-1 moved it into the core
(`object/process.rs`); what that file keeps is the Linux personality's typed
lookup, and it is in `load` with the rest of the personality.

**What left the item ring on 2026-09-26, and why (W-5).** Until then this list
also held `memory`, `thread`, `limits`, `system` and `futex`, as "syscalls
that belong to the item rather than to the Linux personality", and the item
ring held the Linux dispatcher's routing. The resolving gate showed each of
them reaching into the personality for its state (F-09), and asked whether
the state belonged in the core or the file in the personality. Each is the
personality's:

| File | What it is | Why it is not the item's |
|---|---|---|
| `syscall/linux.rs` (was the body of `syscall/mod.rs`) | the Linux dispatcher's routing: `exit`, `clone`, `execve`, then every table | it names 21 of the personality's modules; the item keeps the decode and hands the call on through a `Personality` trait it defines, which `main.rs` composes with it |
| `syscall/memory.rs` | `mmap`, `munmap`, `mprotect`, `mremap`, `msync`, `madvise`, `brk` | argument decoding, by its own account, onto the core's `AddressSpace`, which is where a mapping is refused or made |
| `syscall/futex.rs` | `futex(2)` | Linux's operations and timeouts; the native ABI waits on objects and ports, never a futex |
| `syscall/limits.rs` | `getrlimit`, `setrlimit`, `prlimit64`, `sched_*` | the POSIX process's limits and credentials; the quota the ST claims (FRU_RSA.1) is the job's, in the core |
| `syscall/system.rs` | `uname`, `sysinfo`, `sethostname`, `syslog`, `reboot` | checks over POSIX credentials, which the ST claims nothing about; `power`, which `reboot` reaches, stays |
| `syscall/thread.rs` | the POSIX thread | since W-1 the scheduler holds a `UserThread`; only the Linux dispatcher needed this |

Nothing the item's claims rest on moved: the Security Target names the Linux
personality a threat agent outside the TSF (§3.2), every call these files make
into the core is checked there, and the Spectre clamp stayed in the item, in
front of the table. What the move does change is honest scope: the item
ring's product code went from 10,578 lines to 8,002, and the coverage and
traceability evidence scoped to it has to be read against the new boundary.
Nothing in the core or the item names the five files, so the move needed no
code change and left no edge.

The native ABI is here rather than in `core` because it is the interface the
item *exports*, and an interface is evaluated with the thing that exports it.

Where the item has to act on the load -- power commits a filesystem before the
machine stops, init starts a program from one, `devmgr` reads its drivers
from one, device enumeration asks board support what it prepared, the native
ABI makes a ring's control channel or a native process, a Linux call is
answered -- the item defines the interface and the load registers into it
(`kernel/src/hooks.rs`, `syscall::native::serve`), or implements a trait the
item defines and `main.rs` composes the two (`syscall::Personality`). `main.rs` is the crate root: it declares every
module, and its `register_load` is the one place the load is told to
register, in bring-up order, with a boot check that it did.

**`main.rs` is in the item, and it is the composition root.** The manifest
puts it in the `item` ring as bring-up, and nothing about that is changed
here. But it is also where the load is put together with the item. Since
2026-09-26 the gate reads its calls into the load -- 37 modules -- and
records them under `composition_root` in the manifest rather than in the
debt register: ratcheted the same way, filed against no finding. Of the 37,
20 are the load's own boot self-checks (`fs::check`, `net::check`,
`syscall::check` and seventeen more verification files), and 17 are product
modules: registration (`syscall::launch`, `syscall::linux`,
`syscall::deliver`, `stm32mp1`, `fs`, and since W-5 `block_ring`,
`net_ring`, `render` and `input`), the load's subsystems brought up in order
(`fs::root_disk`, `fs::data_disk`, `net`, `syscall::time`, `display`), and
the pieces `main.rs`'s own boot checks drive a program through
(`syscall::image`, `syscall::exec`, `fs::cgroupfs`).

That is a judgement an assessor has to accept, and it is only as good as the
claim that those edges carry composition and no item logic. It is plausible
from the list; it is not checked, because the exemption covers the file, and
`main.rs` is 2,897 lines. Making it checkable means reducing the root to
composition, with bring-up logic in item-ring modules that name nothing above
them, and the boot checks that drive the load in verification files.

A `mod` declaration is not counted as an edge anywhere. A parent declaring
its child says where the child sits in the module tree, not that the parent's
code runs it: `main.rs` declares 10 load-ring modules and `syscall/mod.rs`
declares 36. What either file's *code* then does with them is resolved and
counted like any other reference.

### `load` — everything it runs and does not vouch for

The VFS, btrfs, procfs, sysfs, cgroupfs and tmpfs; the network stack; the
Linux personality's syscall surface; the display, render, input and ring
drivers' kernel halves; STM32MP1 board support.

This is not a list of code that matters less — it is most of what makes Ferrix
useful. It is excluded because a defect in it is bounded by the item's own
enforcement, and because a claim over 99,053 lines is one nobody can afford to
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
43,523 lines is in the size range where EAL6-grade work has actually been done
(INTEGRITY-178B, ~10k SLOC, is the benchmark and is still four times smaller).
It is not there yet. Saying which ring carries which target keeps that gap
visible instead of letting "Ferrix is certified" absorb it.

---

## 4. What the measurement found

The boundary above is a claim about dependencies, so the gate measures it.
Today the item contains **no upward references** -- no place where a ring
names something in a ring above it -- beside the composition root's 37 (§2).
When the audit began it had 94, in 28 files, by today's measure; each was
recorded in the manifest against a finding id and analysed in
[FINDINGS.md](FINDINGS.md), and each finding closed when the build said so.

**The counts this section gave before 2026-09-26 — 29, and 62 when the audit
began — were lower bounds.** The gate matched only the literal text
`crate::a::b`, so it missed nested `use` groups, paths through a name bound by
`use` or declared by `mod`, and every path in code its string pattern had taken
for a literal. It now resolves names as the compiler does
(`scripts/check-item-boundary.py`, whose docstring says how, and what it still
cannot see: an edge that is a type flowing through a value rather than a name
written in the file). Re-measured the same day, the tree had 56 where 29 were
reported, and the audit's starting tree 94 where 62 were.

They were not a reason to move the boundary. They were the reason the boundary
is worth having: each was a specific, addressable piece of coupling that was
invisible while the architecture was described in prose. They were paid down
by F-02, F-02a, F-03, F-04, F-05 and F-08; by F-01 and F-06, splitting the
process (W-1); and last by F-07, F-09 and F-33 (W-5):

* **F-07** — the native ABI named ten load-ring modules, and `devmgr` two. The
  six calls about a subsystem above the item are a table the subsystems
  register into, a native process is made through what the personality lends,
  and every boot checks the table is full.
* **F-09** — the Linux personality in the item ring. The trap entries reach the
  dispatcher through the core; the Linux dispatcher's routing is the load's,
  behind one registered pointer; and five personality files moved to the load
  ring, argued in §2. The item ring shrank by 2,576 lines for it.
* **F-33** — the x86-64 paranoid entry's boot check moved to a verification
  file.

`object/` and `sched/` name nothing above the core since W-1: a task holds a
`sched::UserThread` and the core holds a process whole only as an
`object::process::Host`, a trait the personality implements. `trap.rs` — the
most trusted file in the kernel — names nothing above the core, which it did in
three places when the audit began, and since W-5 neither do the architectures'
system call entries.

The gate's debt register is empty and stays in the manifest: a new upward
reference fails the build until somebody adds it against a finding, which is a
diff somebody has to argue for. What the gate cannot see is still what its
docstring says -- a load-ring value reaching the item through a type rather
than a name -- and the composition root, which is exempt by file and argued in
§2 rather than checked.

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
