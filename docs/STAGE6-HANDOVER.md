# Stage 6 handover — user mode, partially built

Written for whoever picks stage 6 up next. **Delete this file when stage 6
meets its exit criterion**; `docs/ROADMAP.md` is the durable record and this is
scaffolding.

State at the time of writing: `main` = `9daec68`, all three architectures boot
to `FERRIX-BOOT-OK stages 1-5`.

---

## 1. What is built

The memory substrate of stage 6, the processor walking it, and `fork` — call
it three fifths. Six commits:

| Commit | What |
|---|---|
| `011fa92` | `libs/frame` gains `share`/`release`; `deallocate` refuses a frame with references left |
| `f89135b` | `Vmo` — sparse anonymous page list, commit on demand |
| `987fe1d` | `Backing::Anonymous` gains an `id` and `offset` |
| `2fe57dc` | `AddressSpace` — region map, page tables, demand paging |
| `f6da17b` | `arch::install_user_root` / `uninstall_user_root`, and the `MMU` walking a user space in the boot test |
| *this one* | `AddressSpace::fork`, `Vmo::fork`, `mm::copy_frame`, and the copy-on-write fault |

New code lives in `kernel/src/user/`:

* `vmo.rs` — `Vmo::new_anonymous`, `commit`, `page`, `replace`,
  `decommit_range`. A `BTreeMap<page index, Frame>`, sparse: an uncommitted
  page is absent, which is what makes a large `mmap` cheap. `Drop` releases
  every committed page through `mm::release_frame`.
* `space.rs` — `AddressSpace::new`, `root_table`, `map_anonymous`, `fault`,
  `unmap`, plus `SpaceError` and `Access`. A root frame, a
  `ferrix_vma::AddressSpace`, a `BTreeMap<u64, Arc<Vmo>>`, and a `SpinLock`.
* `check.rs` — the boot self-checks. Reports `objects 2048 pages reserved,
  7 committed, 4 faulted in, 2 walked by the MMU, 1 copied on write, 0 frames
  leaked`.

Supporting changes elsewhere:

* `kernel/src/mm.rs` — `share_frame`, `release_frame`, `frame_references`,
  `translate_in(root, virt)`.
* `arch::prepare_user_root(root)` — one facade entry. x86-64 shares the
  kernel's top-level slots into the new root; AArch64 and ARMv7-A do nothing,
  because the kernel is reached through `TTBR1` and a user root only ever goes
  in `TTBR0`.
* `kernel/src/mm.rs` — `copy_frame(destination, source)`, the copy in
  copy-on-write, next to `zero_frame` and for the same reason.
* `arch::install_user_root(root)` and `arch::uninstall_user_root()` — two more,
  with `AddressSpace::install()` and `user::space::uninstall()` over them.
  x86-64 is one `CR3` write each way, because that write drops the non-global
  entries by itself. The Arm pair write `TTBR0`, clear `EPD0` and then
  invalidate by `ASID`, for the reasons under §5.1, §5.3 and §5.4.

### Verify it still works

```
cargo xtask check
cargo xtask test-boot --arch all
```

Both must pass before and after anything you do. The boot test is the contract.

---

## 2. What is NOT built

In dependency order. Nothing below exists, not even stubbed.

1. **`arch::set_kernel_stack(top)`** — the stack a syscall from user mode lands
   on: `TSS.rsp0` on x86-64, `SP_EL1` on AArch64, the SVC stack on ARMv7-A.
   Agreed with the stage 5 owner as one facade entry, not three shapes.
2. **`Task` carrying an address space.** `Option<Arc<AddressSpace>>`, `None`
   meaning a kernel thread. See §4. The two calls the swap needs now exist, so
   this is the next thing to do and it is a small change.
3. **Scoping the user TLB shootdown.** The invalidation itself now exists —
   `space.rs::invalidate` on the three paths that take a translation down or
   make one less permissive (`unmap`, `fork`, and the copy-on-write branch of
   `fault`) — but it is the global broadcast flush, and it tells every
   processor. Two refinements, neither a correctness question: invalidate the
   one page that changed, and tell only the processors with this space
   installed, which wants a `CpuSet` on the `AddressSpace` maintained by
   `install` and `uninstall`. Worth doing with item 2, since item 2 is what
   first makes the difference measurable. **Read §5.6 and §5.7 before touching
   any of it** — the boot test cannot see this class of mistake.
4. **The ELF loader for user binaries.** `libs/elf` parses; nothing maps a
   `PT_LOAD` into an `AddressSpace`. **Handed to the stage 7 owner** — §7.
5. **The ring-3 / EL0 / USR transition**, and the syscall vectors. The seam
   with stage 7 is settled: see §7.
6. **`copy_from_user` / `copy_to_user`.** Also **handed to the stage 7 owner**,
   who needs it for `write(2)`. It must resolve through the `AddressSpace` and
   call `fault` rather than dereference, because a mapped page need not be
   present yet; and it must reject anything `is_user_address` refuses, before
   any length arithmetic, because no `SMAP`/`SMEP`/`PAN` is enabled anywhere in
   this tree and nothing in the hardware will catch a kernel-half pointer.
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

  How that came out in the Arm code is worth knowing, because "no ASIDs" does
  not mean "no ASID field". Every address space is `ASID` zero, and the switch
  invalidates by `ASID` — `TLBI ASIDE1` on AArch64, `TLBIASID` on ARMv7-A. That
  is not an optimisation sneaking in: it is the only invalidation that throws
  away the user entries *and keeps the kernel's global ones*, which §5.1 says is
  the whole requirement. `TLBIALL`/`vmalle1` would also be correct and would
  discard kernel translations on every switch for nothing. When ASIDs do land,
  the change is allocating the identifier and putting it in the root register;
  these two call sites already say by `ASID`.

  Both are the *local*, non-broadcast form, with a `dsb nsh` — deliberately. A
  processor running another thread of the same process must keep its entries,
  and one about to run this address space invalidates as it installs the root.
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

### 5.2 Copy-on-write's branch must go ABOVE the present-page check — HANDLED

`space.rs::fault` returns `Ok(())` when the page already translates, because
faults legitimately arrive on present pages — another processor resolved it
first, or the faulting one walked a stale TLB entry. That early return is safe
*only* while every present page carries its region's own permissions. A write
to a deliberately read-only COW page is precisely the fault that must copy, and
an early return would send the instruction back to fault forever.

The copy-on-write branch is above it now, and the boot check catches the wrong
order rather than trusting the comment: putting the present-page return back
above it makes the check report *a write to a copy-on-write page was let
through to the shared frame*. If you reorder that function, that message is
what you will see.

Two related things the same function now relies on, both worth not undoing:

* A copy-on-write region keeps `cow` set forever — there is no per-page flag —
  so correctness comes from the *frame refcount*, not from clearing a mark. A
  write fault with one holder is let through without copying, which is both the
  optimisation and the reason a second write to an already-copied page
  terminates.
* `mm::map_in` refuses to overwrite a live mapping, deliberately, so the COW
  path unmaps the read-only entry before installing the writable one. If you
  add a `protect_in`, that is the pair of calls it replaces.

### 5.3 Arm: setting `TTBR0` is not enough — HANDLED, but read this

Both Arm architectures disable the lower half rather than dismantling it:
`TTBCR.EPD0` on ARMv7-A, `TCR_EL1.EPD0` on AArch64, and `EPD0` means
translations through `TTBR0` **fault instead of walking**. Installing a user
root therefore has to clear `EPD0` as well and then invalidate, or every user
access faults and it looks exactly like page tables that are wrong when they
are fine.

`install_user_root` does this now, in the order that matters: root first,
`EPD0` second, invalidate third. The other order leaves a window with the
regime live and the register still holding the *previous* process's tables.

What made this nearly ship broken is in §5.4. Read that one.

Related: `kernel/src/arch/armv7a/smp.rs` installs its own identity map for
secondary entry and takes it down in `CpuStarter::finish` — different mapping,
same register. Worth a glance before you touch `TTBR0` handling.

### 5.4 A self-check that runs before `drop_identity_map` proves less than it looks

The stage 6 checks run from `check_user_memory`, which `kmain` calls *before*
`finish_memory` — and `finish_memory` is where `arch::drop_identity_map` runs.
So at check time the loader's identity map is still live in `TTBR0` and `EPD0`
is still **clear**. A first `install_user_root` on Arm therefore walks whether
or not it thought about `EPD0` at all: the regime it needed to enable was
already on.

This was caught by deleting the `EPD0` clear and watching the boot test pass
anyway. The check now installs the space, uninstalls it — which switches the
regime off — and installs it a second time, and it is that second round that
is the real one, because it is the state every switch after the first happens
in. Deleting the `EPD0` clear now panics on both Arm architectures, which is
the point.

Two things follow. If you add a check that exercises a processor feature the
boot sequence has not yet put in its steady state, say so and drive it to that
state yourself. And note the reordering this causes: on Arm the lower half ends
up switched off earlier than `drop_identity_map` would have done it. That is
safe — nothing has read through the lower half since the secondaries finished
starting, and the W^X sweep reaches the identity map's tables through the direct
map rather than through `TTBR0` — but it is deliberate, not accidental, and
`finish_memory` still begins by requiring the sweep to *fail*, which it does
because that check reads `view.raw().ttbr0_phys` and not the register.

### 5.5 `mm::map_in` takes no lock, by contract

Its doc says so, because the tree it was written for — a processor's identity
map during bring-up — is one nobody has installed and therefore nobody can
walk. A user address space is the opposite. `AddressSpace` holds its own lock
across map-and-reshape for this reason; anything new that maps into a live root
must do the same.

### 5.6 The boot test cannot see a missing TLB invalidation

This is the one place in stage 6 where the contract — "the boot test is the
contract" — does not hold, so it is written down rather than discovered.

The copy-on-write branch of `fault` takes down a read-only entry and installs
a writable one. If the stale read-only entry is left in a TLB, the retrying
instruction faults on it, arrives at the handler again, finds one holder and
nothing to copy, installs the same entry again, and retries into the same
stale entry. Forever. A hang with no message.

`space.rs::invalidate` prevents that and is architecturally required. **Deleting
it does not fail the boot test on any of the three architectures.** That was
measured, not assumed: the check was strengthened until it resolved the fault
with the space actually installed on the processor and wrote through the
faulting address as the retrying instruction would — and it still passed with
the call deleted, because QEMU's `tcg` does not keep a stale entry to trip
over. Stage 4 has the writeup of the converse case, a `CR3` reload not
invalidating global entries, which "passed every test under `tcg` and failed
instantly under a hardware accelerator".

Two things follow. Do not remove an invalidation because nothing fails;
`cargo xtask deploy --arch armv7a` on the STM32MP157 is the only arbiter this
tree has, and `docs/stm32mp157-dk.md` has the procedure. And when you add
per-page or `CpuSet`-scoped invalidation, the boot test will not tell you if
you get the scope wrong either.

An intermediate step worth knowing about: the first version of the check
installed the space only for the *final write*, which passed trivially —
installing a root is itself a flush on x86-64, so the stale entry was gone
before the write looked for it. A check that installs has to bracket the
*fault*, not just the access after it.

### 5.7 A check that reaches a page through the direct map tests no permission

Related to §5.6 and more general. The kernel reads and writes user pages
through the direct map, which is writable for all of RAM, so any check that
verifies content with a direct-map read or write has tested the region
bookkeeping and not one page-table bit. The stage 7 owner found this in their
own `mprotect` check: it asserted a read-only region could not be written and
claimed that proved the stale writable translation was gone, and it passed with
the `unmap_in` deleted — the copy had been refused by the region flags in
`fault`, which happens either way.

So a check that means to verify the *tables* must ask `mm::translate_in`, or
install the space and go through the address. The fork and copy-on-write checks
do the former throughout and the latter once; `check_the_processor_walks_an_installed_space`
is the pattern.

### 5.8 An exact frame count can report a leak that is not one

The stage 6 checks bracket their work with `mm::free_frames()` and require the
count to come back exactly. That catches a real leak, and it has a blind spot in
the other direction: `libs/heap` keeps the last page of a size class it has not
used before, on purpose, so the *first* code path to allocate a novel size
appears to lose a frame and never give it back. A single before-and-after
measurement cannot tell that apart from a leak.

The stage 7 owner lost several builds to exactly this before working out it was
warm-up. Their fix is the one to copy: run the whole group twice and measure the
second run, so the warm-up is excluded and a real leak still shows.

Nothing in stage 6 trips it today — the checks pass — but that is luck about
which size classes the rest of boot has already touched. If you add a check that
allocates something new and it reports *the checks did not give back every frame
they took*, suspect this before you go looking for the leak.

### 5.9 GICv2 private interrupt enables are banked per core

If stage 6 adds a per-core interrupt source on Arm, go through the driver's
recorded bitmask (`kernel/src/arch/gicv2.rs`) rather than writing the
distributor directly — the driver records and replays private lines so
secondaries get them. x86-64 has no equivalent replay; check the local APIC if
you add one there.

---

## 6. Repo conventions that will bite you

* **The commit-msg hook refuses `Co-authored-by` trailers.** Ferrix commits name
  one author. Do not work around it.
* **The main checkout goes stale under you, silently, and `git add <paths>` does
  not scope a commit.** The most expensive mistake of the day this file was
  written, and the two halves compound.

  Sessions work in worktrees under `.claude/worktrees/` and commit to `main`.
  The main checkout at the repo root *also* has `main` checked out. When a
  worktree session commits, the branch ref moves but this checkout's **index
  and working tree stay exactly where they were** — so a tree that was current
  a moment ago becomes one that reverts somebody's commit, and nothing says so.
  `git status` reports that a file *differs* from `HEAD` without reporting in
  which direction, so a stale checkout is indistinguishable from your own
  uncommitted work.

  Then: `git add <paths>` followed by `git commit` does **not** commit only
  those paths. It stages them into an index that already holds everything else
  and commits the whole index. Together these produced `44bf30e`, which carried
  a correct change and also reverted 1600 lines of the `sched` owner's merged
  work, `libs/sched/src/balance.rs` deleted. The tell was in the commit's own
  output — `15 files changed` against six added paths.

  So: **read `git diff --cached --stat` before every commit** and check
  `git log --oneline -3` to see whether `main` moved under you. Treat
  `git status` as "these differ", never "I changed these". If you do land a bad
  commit, fix it **forward** with a restoring commit rather than rewriting —
  by the time it was noticed another session had already rebased onto it, and a
  rewrite would have destroyed their work to tidy the history.

  The same shape has bitten twice more. `core.hooksPath` was set to an absolute
  path in `.git/config`, which every worktree shares and which
  `scripts/check-commit-authors.py` compares literally, so `cargo xtask check`
  failed its first gate for everyone until it was set back to the relative
  `.githooks`. And `build/<arch>/ferrix.img` is shared, so another session's
  QEMU makes `cargo xtask test-boot` fail with `Failed to get "write" lock` and
  then time out — which during a hang investigation impersonates the bug being
  hunted. **Run boot tests from a worktree outside the repo.**

  As this was written, four files under `kernel/src/sched/` and `libs/sched/`
  were sitting modified *in the main checkout* belonging to a session that had
  not claimed them. That is the loaded version of this gun: uncommitted work in
  the one checkout whose index every worktree commit silently shares.
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

**Stage 5 got slow, and under QEMU that reads as a hang.** Worth knowing before
you spend an afternoon on it, and worth reading to the end, because four
sessions chased this and almost every intermediate conclusion — including both
of mine — was wrong. It is the `sched` owner's, they have it, and the workaround
is `--timeout 600`.

### Under QEMU it is settled: nothing hangs

`cargo xtask test-boot` sometimes stops after the `stage 4` line with no verdict
at all, which looks exactly like a deadlock. It is not. Ten runs of the unfixed
code at `--timeout 600`, on the same build and machine where the default had been
producing failures:

```
63.95  63.91  63.92  64.20  83.92  64.24  63.92  104.44  84.28  64.24
```

**Ten of ten passed.** A typical run is 64 s, three exceeded 83 s, and one took
104.4 s against a default timeout of 120 s. Nothing was ever hung; `xtask` was
giving up on runs that were still making progress.

One thing that is **not** covered by that and is still open: the stage 7 owner
saw `stage 5 self-check failed: a task's stack was never given back` twice on
x86_64, which is the check running and reporting rather than the harness giving
up, and no timeout explains it. It may be the same defect one level down —
`reap_to`'s patience is five seconds of *guest* time, which is as marginal
against a stage that got 2.7× slower as the 120 s harness default is — but that
is a hypothesis, and the assertion is evidence. Keep it separate until somebody
raises that budget and watches.

The real defect underneath is a **boot-time regression**: the armv7a boot test
took 23.5 s before the scheduler merge and 64 s after it, plus run-to-run
variance of another 40 s. The `sched` owner controlled it properly — `c008979`,
the commit immediately before their work, passed 8 of 8, and it arrived with
`9daec68`. The fix is to make stage 5 cheap again, not to find a deadlock.

### The default timeout is the amplifier, and it manufactured two wrong verdicts

**Investigate at `--timeout 600` and conclude nothing from a 120 s failure.** The
reason matters more than the rule, because the rule will be forgotten and the
reason generalises: **a 120 s default against a 68 s typical run is a coin flip
dressed as a verdict.** The board owner condemned a placement change as
regressing aarch64 on the strength of one such failure, and had told their user
so; at 600 s it measured 67.64 s against the baseline's 67.78 s — *faster* than
the thing it supposedly regressed, by less than the run-to-run noise. Mine was
the first wrong conclusion of the day, from a six-run bisect; that was the
second. Any default sitting that close to the typical time keeps making verdicts
out of scheduling jitter until it moves.

### The board was a hardware watchdog, and Ferrix does not service it

The STM32MP157D-DK1 failures were not the same bug at all, and the cause is a
**kernel gap rather than a test artifact**, so it matters beyond this
investigation. The board has a 32-second independent watchdog. U-Boot starts the
`IWDG` and services it once a second at its prompt; the moment Ferrix takes over,
**nothing services it**, and it fires. Measured from Ferrix's loader banner to
the SoC restarting: **29.8 seconds**, with no panic and no output — the board
simply resets.

That explains both board symptoms as one thing: the watchdog cutting stage 5
short at whatever point it had reached. The silent stop inside `fairness()` is
silent because nothing failed. The *arena holds 8, expected 4* is a run that got
far enough for the five-second reap budget to expire before the reset landed.

And it **retracts** something that was recorded here as evidence. The two shapes
alternating on identical bits was offered, and written down, as a constraint any
explanation had to satisfy. It constrains nothing: what varies between runs is
only where stage 5 happens to be when a fixed timer fires, which is precisely
what a clock that knows nothing about the code would produce. The board owner
retracted it themselves once they had measured the watchdog.

Two consequences:

* **The `PATIENCE_NANOS` experiment cannot pass on the board as it stands.**
  Raising the guest budgets to 120 s is moot when a 32 s hardware reset lands
  first. It is on hold until the watchdog is cleared, which needs a power-on
  reset — the `IWDG` cannot be disabled in software once started and survives
  warm resets, so every reset on that board today has kept the same watchdog
  alive. An OP-TEE built without it is already on the card and takes effect when
  the USB-C is unplugged.
* **Ferrix should service or disable that watchdog**, which is board support
  work rather than stage 6's. Until it does, any run on that board is capped at
  32 seconds whatever else is true, and stage 5 alone now takes 64 s under
  emulation.

Once the watchdog is out of the way, whether the board completes stage 5 late or
genuinely fails is the one remaining open question in this whole investigation.

### The pattern worth more than any of the leads

Four times in one day, across four sessions, a **budget expiring was mistaken
for a hang** — and they were three different budgets at three different levels:

* the harness's 120 s `--timeout`, killing runs that took up to 104 s;
* the guest's own patience — `reap_to`'s five seconds, `PATIENCE_NANOS`'s
  twenty;
* the board's 32 s hardware watchdog, which no software in the picture knows
  about.

Every one produced a confident wrong conclusion from somebody, including two
from me. The common shape: **a deadline expiring looks exactly like a hang, and
silence tells you nothing about whether anything was progressing.** Before
reasoning about a mechanism, find every deadline between you and the symptom and
raise it. That is the transferable lesson here, and it is worth more than any of
the individual leads below.

### Dead leads — do not re-run these

* **A lost wakeup at `sleep_for(WINDOW_NANOS)`.** This was my lead and it is
  wrong. It is true that it is the only wait in `fairness` with no deadline of
  its own, so its silence carries no information — but `arm_timer` takes the
  *minimum* of the earliest sleeper and the slice end and only disarms when
  there is neither, so a queue with nothing runnable and one sleeper does arm
  for the sleeper. Read the function before believing the story.
* **Lock ordering in `balance()`** — `steal_from` always locks the
  lower-numbered queue first, and the snapshot loops hold one at a time.
* **A TLB shootdown deadlock** — `flush_tlb_everywhere` returns immediately on
  both Arm architectures, because `TLB_FLUSH_IS_BROADCAST` is true, so there is
  no cross-processor wait to deadlock on.
* **The two classic Arm lost-wakeup shapes** — the idle path is `wfi` then
  unmask, not the reverse, and `send_ipi_to_others` does `dsb ishst` before the
  distributor write.
* **Kernel size or layout.** Same x86_64 binary hung and then passed back to
  back; a kernel that does both is not failing because it grew.
* **`[CpuLoad; MAX_CPUS]` on the stack** — 6 KiB of a 16 KiB kernel stack, at
  *two* call sites (`choose_cpu` and `balance`, the latter reached from
  `preempt_on_irq_exit`). A real defect, and fixed by folding one processor at a
  time. Not this bug: the fixed build passed 10 of 10 against the unfixed
  reproducing once in 10, which cannot tell a fix from luck. The `sched` owner
  withdrew that themselves rather than let it stand.

### One live fragility, not claimed as the cause

`has_work()` is `!fair.is_empty()` and ignores sleepers entirely, so `idle_loop`
decides whether to sleep on a question that does not mention them. That is safe
only because `arm_timer` gets it right, which is a load-bearing coincidence
rather than a design.

### What this cost, and the two lessons worth keeping

Both of my contributions to the account above were wrong before they were right.
I concluded from a six-run bisect that the hang was a latent race later commits
perturbed — **six runs cannot tell a one-in-three failure from a clean
commit**, and the control is what settles it. And I reasoned from the silence
that nothing could be progressing, so it had to be a deadlock — which assumed
the run had *reached* the check rather than been killed short of it. Both
corrections came from somebody else measuring rather than anyone thinking
harder, and the measurements that mattered were ten runs with a generous
timeout and reading `arm_timer` instead of imagining it.

* `kernel/src/sched/`, `libs/sched` — **released to stage 6.** The stage 5 owner
  finished and merged as `9daec68`, and said both of §4's sched changes are
  yours to make. Three things changed under the handover's line numbers:
  `NewTask` lost `pinned: bool` and gained `affinity: CpuSet`, so the address
  space is a third field on the same descriptor, filled at `new_idle_task` and
  `spawn_on`; `choose_next` is unchanged in shape, so the root swap still goes
  there; and a new `balance()` can move a *queued* task between processors from
  a third processor holding both run queue locks — so **do not cache a root
  against a processor**, because a task can change processor while blocked.

  They also confirmed PELT does not disturb §3's no-ASID decision: EEVDF still
  charges real elapsed time, so a costlier switch widens the printed fairness
  bound rather than breaking the check. Two numbers to watch when the root swap
  lands: the invalidation is charged inside `choose_next` under the lock,
  between `account(now)` and the switch, so it lands in the *incoming* task's
  window — a small systematic bias, and if ASIDs ever make it matter the fix is
  to account once more after the swap rather than to change EEVDF. And the
  printed worst lag is about 1000 us against its bound; the bound is expected to
  widen, but the lag approaching it is a real regression rather than noise.
* `kernel/src/arch/armv7a/`, `xtask/src/flash.rs`, STM32MP157 board bring-up —
  the ARMv7-A owner. They have explicitly released `trap.rs` and the SVC vector
  to stage 6, and `cpu.rs` is free. `cargo xtask deploy --arch armv7a` flashes a
  real board and watches for the boot marker; `docs/stm32mp157-dk.md` has the
  procedure.
* `libs/linux-abi`, `kernel/src/syscall/` (new), the `copy_from_user` layer and
  the ELF loader — the stage 7 owner, in `.claude/worktrees/stage7-syscall-abi`.
  The seam agreed with them is one call your syscall trampoline makes once it
  has saved registers:

  ```rust
  // kernel/src/syscall/mod.rs — theirs
  pub fn dispatch(args: &SyscallArgs) -> isize;
  ```

  `SyscallArgs` is a plain struct with public fields and no fallible
  constructor, carrying the raw syscall number plus six arguments in the
  architecture's own order, so the trampoline fills it straight from a
  `TrapFrame`. The `isize` is already the Linux return-register value, `-errno`
  encoded, so your side writes one register and returns. Register shuffling is
  yours, the ABI is theirs.

  One thing of theirs marked for deletion, since it will not be obvious to
  whoever removes it: `read_iovec` in `kernel/src/syscall/file.rs` hand-rolls a
  32-bit `struct iovec` as two native words, because `linux-abi::types` is
  documented 64-bit-only and reading a 32-bit array through the 64-bit `Iovec`
  would walk it at double stride and build pointers out of halves of two
  different entries — which does not fault, it reads the wrong memory. When
  somebody adds a 32-bit layout set (`Stat64`, `Timespec32`, `Iovec32` and the
  rest), `read_iovec` is the first thing to delete in favour of it. Anything of
  yours that reads a user structure wants the same question asked of it.

  Two things from them that bear on your vectors. On ARMv7-A a valid syscall
  number is **not** bounded by the table's length: `__ARM_NR_BASE` is
  `0x0f0000` and `ARM_set_tls` / `ARM_cacheflush` live there, so do not range
  check against the table size. And `rt_sigreturn` and `execve` cannot return a
  value into a register at all — they replace the register state — so they do
  not go through `dispatch`; that path is yours.
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
