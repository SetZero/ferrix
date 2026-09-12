# Stage 6 handover — user mode, partially built

Written for whoever picks stage 6 up next. **Delete this file when stage 6
meets its exit criterion**; `docs/ROADMAP.md` is the durable record and this is
scaffolding.

State at the time of writing: `main` = `fa9ca46`, all three architectures boot
to `FERRIX-BOOT-OK stages 1-5`.

---

## 1. What is built

Roughly the memory substrate of stage 6 — a third of it. Four commits:

| Commit | What |
|---|---|
| `011fa92` | `libs/frame` gains `share`/`release`; `deallocate` refuses a frame with references left |
| `f89135b` | `Vmo` — sparse anonymous page list, commit on demand |
| `987fe1d` | `Backing::Anonymous` gains an `id` and `offset` |
| `2fe57dc` | `AddressSpace` — region map, page tables, demand paging |

New code lives in `kernel/src/user/`:

* `vmo.rs` — `Vmo::new_anonymous`, `commit`, `page`, `replace`,
  `decommit_range`. A `BTreeMap<page index, Frame>`, sparse: an uncommitted
  page is absent, which is what makes a large `mmap` cheap. `Drop` releases
  every committed page through `mm::release_frame`.
* `space.rs` — `AddressSpace::new`, `root_table`, `map_anonymous`, `fault`,
  `unmap`, plus `SpaceError` and `Access`. A root frame, a
  `ferrix_vma::AddressSpace`, a `BTreeMap<u64, Arc<Vmo>>`, and a `SpinLock`.
* `check.rs` — the boot self-checks. Reports
  `objects 2048 pages reserved, 7 committed, 4 faulted in, 0 frames leaked`.

Supporting changes elsewhere:

* `kernel/src/mm.rs` — `share_frame`, `release_frame`, `frame_references`,
  `translate_in(root, virt)`.
* `arch::prepare_user_root(root)` — one new facade entry. x86-64 shares the
  kernel's top-level slots into the new root; AArch64 and ARMv7-A do nothing,
  because the kernel is reached through `TTBR1` and a user root only ever goes
  in `TTBR0`.

### Verify it still works

```
cargo xtask check
cargo xtask test-boot --arch all
```

Both must pass before and after anything you do. The boot test is the contract.

---

## 2. What is NOT built

In dependency order. Nothing below exists, not even stubbed.

1. **Installing a user root on a processor.** `AddressSpace::root_table()`
   returns the value; nothing writes it to `CR3`/`TTBR0`. See landmine 5.3.
2. **`arch::set_kernel_stack(top)`** — the stack a syscall from user mode lands
   on: `TSS.rsp0` on x86-64, `SP_EL1` on AArch64, the SVC stack on ARMv7-A.
   Agreed with the stage 5 owner as one facade entry, not three shapes.
3. **`Task` carrying an address space.** `Option<Arc<AddressSpace>>`, `None`
   meaning a kernel thread. See §4.
4. **fork and copy-on-write.** `libs/vma::AddressSpace::clone_for_fork` already
   marks both sides; `Vma.cow` is *read* by `space.rs::fault` and set by
   nothing. The frame refcount primitives exist for exactly this.
5. **The ELF loader for user binaries.** `libs/elf` parses; nothing maps a
   `PT_LOAD` into an `AddressSpace`.
6. **The ring-3 / EL0 / USR transition**, and the syscall vectors.
7. **`docs/ROADMAP.md` is not updated.** It still reads "Stage 6 is next" and
   stages 0–5 done. Someone should correct it — probably whoever finishes the
   stage, in the style stages 2–4 use ("Done — ...", "Still to do — ...").

---

## 3. Design decisions already made

Do not silently reverse these; they were argued with the other sessions.

* **No ASIDs / PCID in stage 6, on any architecture.** A full flush on
  address-space switch, as a correct baseline. Eliding it is a later stage's
  optimisation that makes the switch faster rather than unpicking it, matching
  how stage 4 deferred per-CPU frame caches. The stage 5 owner confirmed this
  does not disturb EEVDF: it charges real elapsed time, so a costlier switch
  widens the printed fairness bound rather than breaking the check.
* **Anonymous memory names a VMO.** `Backing::Anonymous { id, offset }`, with
  `id == 0` meaning private memory whose `offset` is by convention the
  mapping's own start address — Linux's `vm_pgoff` trick, which keeps two
  adjacent private regions contiguous so they still merge. Needed for
  `MAP_SHARED|MAP_ANONYMOUS` (musl uses it), futexes resolving to one wait
  queue, `/proc/self/maps`, and reclaim's owner field.
* **One lock per address space**, not a global one, so two processes faulting
  at once contend for nothing.

---

## 4. Interfaces agreed with the stage 5 owner

They deliberately left these unbuilt rather than guess at `AddressSpace`'s
shape. All three are yours.

1. **`Task` gains `Option<Arc<AddressSpace>>`.** `NewTask`
   (`kernel/src/sched/task.rs:93`) is a descriptor precisely so adding a field
   is a one-line change at each of two construction sites
   (`kernel/src/sched/mod.rs:246` and `:349`).
2. **The root swap goes in `sched::choose_next`**, under the run queue lock,
   before the switch — *not* inside `arch::switch_to`, which takes two stack
   pointers and whose job is register operations. Compare by pointer identity
   and skip when equal; two threads of one process share a root.
3. **`arch::set_kernel_stack(top)`**, called from the switch path.

---

## 5. Landmines

Each of these cost someone real time. They are in rough order of how much.

### 5.1 Never use `arch::flush_tlb` in the switch path

It is a *global* flush. An address-space switch is a plain root-register write;
global entries — kernel text, the direct map, device windows — are exactly what
must survive it. Stage 4 has a writeup of the converse bug (`CR3` reload not
invalidating global entries, which passed every test under `tcg` and failed
instantly under a hardware accelerator).

### 5.2 Copy-on-write's branch must go ABOVE the present-page check

`space.rs::fault` returns `Ok(())` when the page already translates, because
faults legitimately arrive on present pages — another processor resolved it
first, or the faulting one walked a stale TLB entry. That early return is safe
*only* while every present page carries its region's own permissions. A write
to a deliberately read-only COW page is precisely the fault that must copy, and
an early return would send the instruction back to fault forever. There is an
imperative comment at the site.

### 5.3 ARMv7-A: setting `TTBR0` is not enough

`drop_identity_map` sets `TTBCR.EPD0` and zeroes `TTBR0`
(`kernel/src/arch/armv7a/cpu.rs`, `disable_ttbr0` at :198, `TTBCR_EPD0` at
:25). `EPD0` means translations through `TTBR0` **fault instead of walking**.
So installing a user root requires clearing `EPD0` as well, in the same
sequence, then invalidating — otherwise every user access faults and it looks
like the page tables are wrong when they are fine.

The zeroing is deliberate: its comment says it is so that "a later change that
clears EPD0 cannot resurrect the loader's tables". That later change is yours.
Set `TTBR0` and clear `EPD0` together, then invalidate, or you may briefly have
`EPD0` clear with the old root still live.

Related: `kernel/src/arch/armv7a/smp.rs` installs its own identity map for
secondary entry and takes it down in `CpuStarter::finish` — different mapping,
same register. Worth a glance before you touch `TTBR0` handling.

### 5.4 `mm::map_in` takes no lock, by contract

Its doc says so, because the tree it was written for — a processor's identity
map during bring-up — is one nobody has installed and therefore nobody can
walk. A user address space is the opposite. `AddressSpace` holds its own lock
across map-and-reshape for this reason; anything new that maps into a live root
must do the same.

### 5.5 GICv2 private interrupt enables are banked per core

If stage 6 adds a per-core interrupt source on Arm, go through the driver's
recorded bitmask (`kernel/src/arch/gicv2.rs`) rather than writing the
distributor directly — the driver records and replays private lines so
secondaries get them. x86-64 has no equivalent replay; check the local APIC if
you add one there.

---

## 6. Repo conventions that will bite you

* **The commit-msg hook refuses `Co-authored-by` trailers.** Ferrix commits name
  one author. Do not work around it.
* **Never `git filter-branch` with a `main..HEAD` range.** It does not limit the
  rewrite: it rewrites every ancestor and repoints every ref naming one,
  including `main` and `origin/main`. This happened; trees were identical so
  nothing was lost, but every SHA changed.
* **`cargo test --workspace` fails** on the freestanding crates. CI uses
  `--exclude ferrix-kernel --exclude ferrix-boot`.
* **Fuzzing needs nightly**: `cargo +nightly fuzz run <target>` from `fuzz/`.
  `cargo fuzz cmin` **replaces** the corpus directory and will delete committed
  regression inputs — `git checkout -- fuzz/corpus/<target>` afterwards.
* **The continuous rule**: anything expressible as a pure function of bytes goes
  to `libs/` with a fuzz target and a Miri run *before* the kernel calls it.
  Miri currently covers only `libs/elf` and `libs/bootinfo`; `frame`, `heap` and
  `paging` run in the kernel and are owed one.
* Commit messages here are long and argue *why*. Match the surrounding style.

---

## 7. Other sessions

Work is spread across sessions sharing one object store, each in a worktree
under `.claude/worktrees/`. Coordinate before touching:

* `kernel/src/sched/`, `libs/sched` — the stage 5 owner. Also landing a per-task
  affinity `CpuSet` (replacing `pinned: bool`), task placement on spawn/wake,
  PELT load tracking and load balancing. None of it touches the facade.
* `kernel/src/arch/armv7a/`, `xtask/src/flash.rs`, STM32MP157 board bring-up —
  the ARMv7-A owner. They have explicitly released `trap.rs` and the SVC vector
  to stage 6, and `cpu.rs` is free. `cargo xtask deploy --arch armv7a` flashes a
  real board and watches for the boot marker; `docs/stm32mp157-dk.md` has the
  procedure.
* `docs/sysml/` — a SysML v2 model of the design, kept current with the code.
  `04-memory.sysml` and `06-objects.sysml` cover stage 6. Report divergence in
  both directions; the model is ahead of the code for everything in §2.

One more thing worth knowing: a worktree nested inside the repo merges the
parent checkout's `.cargo/config.toml` on top of its own. If the parent is on an
old commit whose config still passes `-Tkernel/linker/kernel.ld`, the link
script is evaluated twice and the loader rejects the kernel with
`FERRIX-PANIC loader: the kernel has a malformed segment`. Fixed at the source
(`kernel/build.rs` emits it once) but it recurs whenever a checkout sits on a
commit older than `89b680f`.
