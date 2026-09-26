# Implementation work orders

For the agent building the things, not the one assessing them. Each order below
is scoped so it can be landed on its own with the tree green.

[TODO.md](TODO.md) is the audit-side companion: what to re-measure and how to
know a finding closed. This file is what to write.

Everything here is scoped to the item in [ITEM.md](ITEM.md). Finding ids are
[FINDINGS.md](FINDINGS.md).

---

## 0. Before the first change

**Run `cargo xtask check` once.** It has never been run on this branch. The ten
cross-target clippy passes and the host test suite have not seen the two new
gates in `check.rs` or the env-var branch in `qemu.rs`. Fix whatever falls out
before starting real work, so a later failure is attributable to your change.

**House rules that will otherwise cost you a rewrite.** `docs/CONVENTIONS.md`
is the authority; the load-bearing ones:

* One author per commit. **No `Co-authored-by:` trailer**, no tool signature,
  no "Generated with" line. Enforced by a hook and a CI job.
* The lint table denies `unwrap`, `expect`, `panic`, `unreachable`, indexing
  and string slicing in production code. An exemption is an `#[expect]` whose
  reason begins `AUDIT:`, and it fails once the lint stops firing.
* Every `unsafe` block gets a `SAFETY:` comment and does **one** operation;
  every `unsafe fn` gets a `# Safety` section. `check-unsafe-audit.py` enforces
  it and prints per-crate counts, so growth is visible.
* Commit messages are a subject, a blank line, and a body that argues the
  *why*. Look at `git log` before writing one.
* Generic kernel code never names an architecture; `cfg(target_arch)` lives
  only under `arch/`. Reach architecture code through the `crate::arch` facade.

**Verify gates by their output, not a pipeline's exit status.** `gate | tail &&
commit` commits on failure. Check the status and the text separately.

---

## W-1 — Split `Process` into a core object and a POSIX extension

**Done 2026-09-26.** F-01 and F-06 closed; F-09 re-scoped. The design as built
is below, then the trap and the measurement that shaped it, kept because the
trap is still the obvious wrong change.

**Closes:** F-01 (10 references), F-06 (3). **Size:** large.

### The design as built

Three decisions, each answering one link of the `Task -> Thread -> Process`
chain.

**1. The core process is a type of its own, and the POSIX process contains
it.** `kernel/src/object/process.rs` holds `Process { space, pid, started,
handles, membership, counted, exit }` -- what the core enforces or reports,
and nothing else -- with `Exit`, `ProcessRef` and `Control`, which are what a
native handle to a process holds. The personality's `syscall::process::
Process` has it as its *first* field (so it drops first, as the pid and the
job count were given back before the descriptors closed) and implements
`Deref` to it, so `process.space()` and `process.pid()` read as they did at the
personality's call sites; one, a `Process::pid` path in `syscall/mod.rs`, had
to become a closure. The
core type has no field leading back to the extension: not a typed one, and
not a type-erased one either.

**2. Where the core must hold a process whole, it holds a `Host`.** A job kill
walks every process; a native handle to an unstarted process must kill it
when the last handle goes; the pid table must find processes by number. Each
needs the *whole* process -- whose ending closes descriptors and tells a
parent -- without naming what that is. `object::process::Host` is the
personality's object seen through the five questions the core asks of it:

| `Host` method | Asked by | The personality answers with |
|---|---|---|
| `core()` | everything | its core half |
| `kill(status)` | `Job::kill`, `Control`'s drop | `syscall::process::kill` |
| `thread_starting()` | `sched::prepare_user` | its live-thread count |
| `thread_gone(ended)` | `sched`, a prepared task dropped unlaunched | the count, and the end or release it triggers |
| `wait_interrupted()` | `futex` waits | `signal_pending` |

`Arc<PosixProcess>` coerces to `Arc<dyn Host>`, and the personality has its
own type back by `object::process::downcast` (`Any`, a type-id compare). The
pid table moved into the core with it -- the numbers, their cyclic
allocation, and a weak `Host` per number -- because a job kill has to find
every process and could not name the item-ring registry to do it.
`syscall/registry.rs` kept only the personality's typed view and the Linux
rule that thread ids share the pid space, and moved to `load` in a commit of
its own, since that is what it now is.

**3. The scheduler holds a `UserThread`, and the POSIX thread stays the
personality's.** This was the real design question. A thread is what the
scheduler schedules, which argued for the core; but every field of
`syscall::thread::Thread` beyond the process reference is POSIX -- the thread
id from the pid space, the signals sent to it alone, the mask, the address
`set_tid_address` registered. The scheduler used none of them. What it needs
is the process the thread runs in, to count it starting and gone, and the
reference that keeps the thread alive while the task is. That is
`sched::UserThread`, a trait with one method, `process() -> &dyn Host`;
`sched::Task` holds `Arc<dyn UserThread>`, and `thread::of_task` downcasts it
back to the POSIX thread on the syscall path, where it used to clone an `Arc`.

So the chain is now `Task (core) -> dyn UserThread (core) -> dyn Host (core)`,
with the concrete `Thread` and `Process` behind the two trait objects, and no
core file names the personality.

**Lock order and the preemption rule** are unchanged: `Host::kill` is the same
`end` as before, reached from the same places; the pid table is the same
`SpinLock` with the same "drop outside the lock" rule. One thing did change:
entries are now compared by address (`ptr::addr_eq` on the weak pointer)
rather than by upgrading, because an upgrade under the table lock could
produce a process's last reference and dropping a process takes that lock.

### What is left, and where it is filed

`check-item-boundary.py` went from 36 references to 29 (48 to 41 when first
measured, before F-04 and F-08 closed; all four are the gate's counts of the
day, which were lower bounds -- FINDINGS.md §A): seven removed (both
core references to `syscall::process`, all three of F-06, `futex.rs`'s, and
`registry.rs`'s by the ring move). Six item-ring files still name
`syscall::process`, and they do so for POSIX *state*, not for the core
concept, so they are no longer F-01's:

| File | Why it names the POSIX process | Now filed as |
|---|---|---|
| `syscall/mod.rs` | it is the Linux dispatcher: `current()`, `exit_group` | F-09 |
| `syscall/thread.rs` | the POSIX thread holds its POSIX process and its signal state | F-09 |
| `syscall/memory.rs` | `brk` is the POSIX heap; `mmap` of a file needs the fd table | F-09 |
| `syscall/limits.rs` | rlimits read the fd table and the credentials | F-09 |
| `syscall/system.rs` | `sethostname` checks credentials | F-09 |
| `syscall/native.rs` | process creation goes through the Linux loader | F-07 |

Each is a Linux-personality syscall sitting in the item ring, which is F-09's
defect exactly. The next step for them is not another trait on `Host` -- that
would pull POSIX questions into the core's interface -- but deciding, file by
file, whether the item ring should hold them at all. `syscall/thread.rs` is the
clearest case: after this change nothing in the core or the item needs the
POSIX thread except the Linux dispatcher, and it belongs in `load` once the
dispatcher does. It was not moved here because `syscall/mod.rs` names it by a
module-relative path the gate cannot see, and a move that hides an edge is not
a fix.

W-5 took that step (2026-09-26), with a gate that sees module-relative paths:
the Linux dispatcher's routing went above the item first, and then the five
files, with no edge left behind.

### The trap

The obvious reading of F-01 is "move `Process` to `kernel/src/object/
process.rs`". **That is the wrong change and it would make the item worse.**
`Process` is not a clean core type. Read `kernel/src/syscall/process.rs:65` —
its fields include:

| Field | Belongs to |
|---|---|
| `space: Arc<AddressSpace>` | **core** |
| `pid: u32` | **core** |
| `handles: SpinLock<HandleTable>` | **core** (native ABI) |
| `started: u64` | **core** |
| `umask: AtomicU32` | personality |
| `identity: SpinLock<Identity>` | personality |
| `files: Arc<SpinLock<FdTable<Arc<OpenFile>>>>` | personality |
| `fs: Arc<SpinLock<Context>>` | personality (root and cwd) |
| `state: SpinLock<State>` | personality (signal masks, `brk`) |

Moving the type wholesale would drag the file-descriptor table, the filesystem
context and the signal state into the trusted core — the exact inversion the
boundary exists to prevent. `check-item-boundary.py` would go green while the
item got structurally worse, which is the failure mode worth naming loudest.

### The change, as first written

Split it. A core object holding what the core enforces, and a personality
extension holding what POSIX needs.

1. `kernel/src/object/process.rs` — `Process { space, pid, started, handles }`,
   plus `pid()`, `space()`, `with_handles()` and job membership. This is what
   `object/`, `sched/` and `trap.rs` consume.
2. The personality keeps a `PosixProcess` (name it as the tree prefers) holding
   `umask`, `identity`, `files`, `fs`, `state` and the `brk` lock, reached from
   the core `Process` by a handle or a side table the personality owns — **not**
   by a field on the core type, which would restore the dependency in the other
   direction.
3. `current()` moves with the core type.

### What the measurement said (2026-09-25)

Investigated properly before starting, and the shape is different from what
F-01's wording suggests.

**The core-facing interface is small.** Everything the trusted rings actually
need from `Process` is about seven operations:

| Caller | Needs |
|---|---|
| `sched/mod.rs` | `space()`, `thread_starting()`, `thread_gone(bool)` |
| `object/job.rs` | `move_to(job)`, and `process::kill` |
| `object/mod.rs` | `ProcessRef::exit()` |
| `trap.rs` | `current()`, `pid()`, `space()` |

Seven operations against a type with 78 methods. A core-side trait would be
cheap, and `Arc<Process>` coerces to `Arc<dyn CoreProcess>` without touching
the personality.

**But the ownership chain crosses two boundaries, not one.** `sched::Task` does
not hold a `Process`: `task.rs:450` is
`self.thread.as_ref().map(|thread| thread.process())`. It holds a **`Thread`**,
and `Thread` holds `Arc<Process>`. So the chain is

    Task (core) -> Thread (item) -> Process (load)

which is why the register carries both F-01 and F-06 and why closing either
alone does not help. The 34 personality callers all go through
`Thread::process()`, so they are *not* affected by a change to `Task` — that is
the good news, and it is what makes a trait viable at all.

**Consequence for the work order.** A correct fix needs a core-side abstraction
for `Thread` as well as for `Process`, and `Thread` in turn names
`syscall::process` and `syscall::signal`. Deciding where `Thread` belongs is
the real design question: a thread is what the scheduler schedules, which
argues for the core, but `syscall/thread.rs` reaches into the personality for
signals and process state.

That is an architectural restructuring of the process/thread/task ownership
model, not a file move. It wants a deliberate design pass, and it is the reason
this order remains open after a session that closed eleven other findings.

### How it landed

The order above (a re-export first, one consumer per commit) assumed the type
would move; it was split instead, which cannot be done a consumer at a time.
Four code commits, each green, then the documents:

1. `object::process` with the core fields, `Exit`, `ProcessRef`, `Control`,
   `Host` and the pid table; the POSIX process contains the core one; `object/
   mod.rs` and `object/job.rs` switch. Retires 3 (F-01 ×2, F-06 ×1).
2. `sched::UserThread`; `Task` holds one. Retires 2 (F-06).
3. `Host::wait_interrupted`; `futex.rs` takes a `Host`. Retires 1 (F-01).
4. `syscall/registry.rs` to `load`. Retires 1 (F-01). A boundary change,
   argued in its own commit so it can be judged -- or reverted -- on its own.

### Verify

`python3 scripts/check-item-boundary.py --report` shows no `F-01` or `F-06`
entry and nothing from `object/` or `sched/` above the core. Every boot gate
exercises this code: `object/check.rs` and `sched/check.rs` on every boot,
`test-threads` for the thread path, `test-jobs` for job control, and `test-shell`
for fork, exec and wait.

### Pitfall

`docs/sysml/` describes the object model; `06-objects.sysml` now has `Process`
(core), `PosixProcess :> Process` and `Thread`, and `05-scheduling.sysml`
`UserThread`. `gen-arch-doc.py --check` fails if the model and the generated
document disagree.

---

## W-2 — Invert the trap-return upcall

**Done 2026-09-25.** See F-02 and F-02a.

**Closes:** F-02 (7 references). **Size:** small. **Value:** high — it is on
the most trusted path in the system.

Three functions are called from `arch/*/signal.rs`, `arch/*/trap.rs`,
`arch/x86_64/syscall.rs` and `trap.rs`:

```rust
needs_attention() -> bool
return_to_user(context: &mut arch::UserContext)
sigreturn(context: &mut arch::UserContext, rt: bool)
```

Simple signatures, so the inversion is cheap. Define the interface in the core
— a struct of three function pointers, or a trait object behind a
`SpinLock<Option<_>>` — and have the personality register into it during init.
`arch/` then names only the core interface.

**Verify:** the core no longer names `crate::syscall::deliver`. A kernel built
without the personality registered must still return to user mode for a process
that has no signals pending; if it cannot, the interface is not actually
inverted.

**Pitfall:** this is on the return-to-user path, so an indirect call costs on
every trap. Measure it — `docs/ROADMAP.md` tracks boot cost lines and the
project cares about this. If it shows, an `Option<fn>` checked once beats a
trait object.

---

## W-3 — Invert `StatLayout`

**Done 2026-09-25.** See F-03.

**Closes:** F-03 (3 references). **Size:** tiny. Good first task.

Each `arch/*/mod.rs` declares `STAT_LAYOUT: crate::syscall::stat::StatLayout`.
Invert it: the personality asks the `crate::arch` facade which layout the ABI
wants, via an arch-owned enum or a plain discriminant the personality maps.

---

## W-4 — Registration for board support and the block ring

**Done 2026-09-26.** F-05 closed on 2026-09-25 and F-04 on 2026-09-26, and
F-08, which had no order of its own, went with it: power, init and `devmgr`
took the same shape, an interface the item defines and the load ring
registers into. `kernel/src/hooks.rs` is the list type the item keeps
registrations in, and `main.rs`'s `register_load` is the one place they are
made, in bring-up order, with a check that each was (FX-0006). FINDINGS.md
F-04 and F-08 say what moved and what the gate still cannot see.

**Closes:** F-04 (6), F-05 (1), and F-08 (6). **Size:** small.

`device.rs` names `stm32mp1`, `stm32mp1_gpu`, `stm32mp1_usb`; `claim.rs` names
`block_ring`. Both want the dependency the other way: board support registers
itself with the core registry at init instead of the registry naming each
board.

**Pitfall:** registration must happen before the first consumer runs.
`main.rs` and `init.rs` own bring-up order — put the call there explicitly
rather than relying on a link-time trick, which is unanalysable and would be a
finding of its own.

---

## W-5 — A registration table for the native dispatcher

**Done 2026-09-26.** F-07, F-09 and F-33 closed: 56 references, and the debt
register is empty. The order was written for F-07's 12; the resolving gate
sized it at 56, and the same pattern answered most of them.

**Closes:** F-07 (12 references), F-09 (39), F-33 (5). **Size:** medium.

### The design as built

Six commits, each green, then the documents.

1. **The core's syscall entry.** `SyscallArgs` and `Outcome` moved into
   `trap.rs`; the three architectures call `trap::system_call`, which
   answers through a `SyscallEntry` in a `Once` that `main.rs` points at
   `syscall::dispatch`, beside the personality's `ReturnPath`. Retires F-09's
   three `arch` entries.
2. **The native table.** `native.rs` answers the calls on the core's objects
   in its exhaustive `match`, as before. The six about a subsystem above the
   item go to a table of `Handler`s the subsystems register with
   `native::serve` -- the rings, the display, the renderer and input through
   an `install` each, cgroupfs through `fs::install` -- and the device handle,
   its right and the driver's handle stay in the item
   (`native::control_channel`). Process creation and start, for the ABI and
   for `devmgr`, go through `native::Processes`, which `syscall/launch.rs`
   lends; a quiesce waits out registered `Server`s. Retires F-07's 12.
3. **The Linux dispatcher above the item.** `syscall/mod.rs` keeps the way in,
   the native range and the decode with its clamp, and hands the decoded call
   to a `Personality` -- a trait it defines -- that `syscall/linux.rs`
   implements. Retires 21 of F-09. First held in a pointer; then, measured,
   composed by `main.rs` at compile time (`dispatch_with::<Linux>`) so it
   costs no second indirect call.
4. **Five files to `load`**, by the manifest alone: `futex`, `limits`,
   `memory`, `system`, `thread`, each argued in ITEM.md §2. Retires 15 of F-09.
5. **The paranoid check to a verification file**, `arch/x86_64/paranoid/
   check.rs`. Retires F-33's 5.

`main.rs` gains five composition-root edges (`block_ring`, `net_ring`,
`render`, `input`, `syscall::linux`), each a registration call, and its
registration check covers the new interfaces.

### The exhaustiveness the `match` gave

This order's pitfall, and it is kept rather than traded: the calls the item
answers are still in an exhaustive `match`, so an unanswered one does not
compile. Only the six the table holds are checked at boot instead, on every
boot, before anything can make a native call (FX-0006). The check is in
`main.rs`'s `register_load`, with the other registrations, rather than in
`syscall/check.rs`: that file is load-ring verification, and the check is the
item holding the load to its registrations.

### Cost

Dispatch is the hottest path, so nothing on it locks or allocates. Every
call pays one indirect call through the core's `Once`; a native call to the
table a short search by decoded call besides. Measured under KVM on a Zen 5
host with busybox `dd bs=1` copying a million bytes from `/dev/zero` to
`/dev/null` -- two million `read`/`write` calls -- nine times a boot, six boots
alternating with the series' base:

| | min | median | mean |
|---|---:|---:|---:|
| base | 1.074 s | 1.185 s | 1.279 s |
| personality behind a second pointer | 1.112 s | 1.244 s | 1.274 s |
| base | 1.081 s | 1.242 s | 1.324 s |
| personality composed at compile time | 1.100 s | 1.257 s | 1.330 s |

The second pointer cost a median +5.0%, so it went; what is left is +1.2%
(minimum +1.8%), inside the noise of a host running other sessions at a load
of about 20. Not committed: the benchmark is a line added to `test-vfs`'s
command list for the run.

### Verify

`python3 scripts/check-item-boundary.py --report` shows no upward reference and
an empty register. The full boot gate row, `test-threads`, `test-jobs`,
`test-net` and `test-boot --mitigations off`: every native call devmgr and its
drivers make, every Linux call busybox makes, and the paranoid check's
breakpoints, go through the new paths.

---

## W-6 — Complexity and recursion gate

**Done 2026-09-25.** See F-25. **Corrected 2026-09-26:** the gate's string
stripping mis-paired quotes after a `\`-newline continuation and left 328 of
the item's 1,887 functions unmeasured. It now reads code through
`scripts/rustlex.py`, shared with the boundary gate, and the baseline was
re-recorded at 47 entries.

**Closes:** F-25. **Size:** medium. No kernel changes.

The one code gate the audit did not build. EN 50716 requires a coding standard
with metrics; eleven gates enforce other properties and none bounds cyclomatic
complexity, function length or recursion.

Follow `scripts/check-item-boundary.py` exactly — it is the current best
example of the ratchet pattern: measure, record a baseline, refuse growth,
fail on stale entries.

* Complexity: without a Rust parser, approximate by counting branch points per
  function (`if`, `match` arms, `while`, `for`, `&&`, `||`, `?`). **Say in the
  docstring that it is an approximation.** The house rule is that a number
  whose caveats travel separately is worse than none.
* Recursion: build a call graph from function names within a crate and report
  cycles. Direct recursion is easy and worth catching; mutual recursion through
  trait objects is not detectable this way, and the docstring must say so.
* Wire into `cargo xtask check` after the item-boundary step.

**Pitfall:** the baseline will be large. Do not tune thresholds until the item
is below them — record what exists, then ratchet.

---

## W-7 — Finish the coverage story

**Done 2026-09-26**, except that F-10 stays open on the statements that need a
test. Steps 1 to 5 are done: every architecture and both profiles measured,
every boot gate that exercises the item in the union on every architecture
(thirteen on x86-64, nine on ARMv7-A, and on AArch64 the same nine plus a
`test-boot` on a GICv3), the residual sorted per architecture, and the ratchet
wired up as `cargo xtask coverage`. Doing step 3 found two defects in
`coverage-report.py` that had made the published 81.9% wrong;
VERIFICATION.md §3.4. The corrected figures, on main at a6d505a2, are 74.7%
(x86-64), 73.7% (AArch64) and 70.9% (ARMv7-A).

**Closes:** F-10, F-11, F-12. **Size:** medium, mostly running things.

Reproduction, with QEMU's drcov plugin built from its source tree:

```
FERRIX_DRCOV=/home/sebastian/Documents/qemu/qemu/build/contrib/plugins/libdrcov.so \
  cargo xtask coverage --arch x86_64 \
    --init "$HOME/.local/share/ferrix/busybox/{arch}/bin/busybox.static"
```

It runs each gate with `--accel tcg` (a TCG plugin observes nothing under KVM)
and `--smp 2` on ARMv7-A, keeps every boot's trace and the kernel it ran in
`build/coverage/<arch>`, and runs `coverage-report.py` over the lot against the
floor in `coverage-floor.json`. One gate by hand is still
`FERRIX_QEMU_PLUGIN="<libdrcov.so>,filename=<dir>/x.drcov" cargo xtask <gate>
--accel tcg`.

1. **AArch64 and ARMv7-A** (F-12) — done, and since 2026-09-26 the suite.
2. **Release profile** (F-11) — done, one boot: 75.2% against debug's 71.6%.
3. **More gates in the union** (F-10) — done. The gates that ended by killing
   QEMU are now asked to stop first, which lets the plugin write its table,
   and each boot of a gate keeps its own trace. Not in the union, and why:
   VERIFICATION.md §3.5 (`test-vfs` off x86-64, `test-seat`,
   `test-compositor`, and the gates needing a GL host or fetched volumes).
4. **Enumerate the residual** — done per architecture: COVERAGE-RESIDUAL.md
   sorts it, COVERAGE-WORKLIST.md groups the *needs a test* category by module.
5. **Ratchet it** — done as `cargo xtask coverage`, not in `cargo xtask
   check` since it needs boots, and not in CI, whose packaged QEMU carries no
   drcov plugin (the one used here is built from QEMU's source tree). Floors
   are the measured figure less a point.

**What is left is F-10's test-writing**: 757 statements on x86-64, 1,481 on
AArch64 and 1,446 on ARMv7-A, by module in COVERAGE-WORKLIST.md. Take a module,
write the tests, re-run `cargo xtask coverage`, regenerate the evidence and
raise the floor. A statement no run of the measured machine can reach gets its
argument in `coverage-argued-<arch>.json` instead, which the generator checks
against the residual.

**Advanced 2026-09-26 on x86-64** (main at 195a2e93): `arch/x86_64`, x86-64's
share of `arch`, `trap` and `smp` have nothing left that needs a test -- 216,
8, 34 and 47 statements before, covered by new stage 3 and stage 4 checks and
three more boots in the suite (`boot-legacy`, `boot-reset`, `boot-single`),
or argued statement by statement (74). Two more tool defects were found
doing it (VERIFICATION.md §3.4), which is most of x86-64's move from 74.7% to
82.2%. The floor is raised to 81.0.

---

## W-8 — Low-level requirements, and ids on assertions

**Closes:** F-14, F-15, F-16. **Size:** large. **The biggest structural gap.**

49,431 lines of item product code trace to 33 system-level requirements, and no
test names a requirement id.

**This is not a testing task.** There are 31,135 lines of in-kernel self-test
already, asserting genuinely rich properties. What is missing is requirements
for them to discharge.

1. Write low-level requirements in `docs/sysml/` for the item's modules, with
   stable bracketed ids as `01-requirements.sysml` already uses. Verifiable
   statements with pass/fail criteria — not the narrative rationale the current
   33 are (F-16).
2. Start from [SECURITY-TARGET.md](SECURITY-TARGET.md) §7, which already maps
   eight objectives to code and test. That is the shape; extend to modules.
3. Attach ids to the in-kernel check assertions. They print counted quantities
   already (*"2387 mappings swept, 899 executable, none writable"*); each needs
   the requirement it discharges.
4. Generate a traceability matrix and gate it: fail on a requirement with no
   verification, or a test naming a requirement that does not exist. Same
   pattern as `gen-arch-doc.py --check`.

Unblocks DAL C, 62304 §5.4 and `ADV_TDS.3` at once.

---

## W-9 — Vulnerability analysis against the ST threat model

**Done 2026-09-25.** See F-21a and VULNERABILITY-ANALYSIS.md. It found that
no SMAP, SMEP or PAN was enabled, which F-32 then fixed on x86-64 and AArch64.
The attack tests per threat below remain worth writing as regression tests.
Corrected 2026-09-26: its T.EXHAUST paths credited job quotas that are not
built, and the verdict was *not resisted* but for CPU per task (F-35) until
W-13 built them the same day; it is now *partially resisted* (F-37).

**Closes:** F-21a. **Size:** medium. Last EAL5 gap that is engineering.

[SECURITY-TARGET.md](SECURITY-TARGET.md) §3.2 states seven threats: T.MEMORY,
T.ESCALATE, T.FORGE, T.DMA, T.RESIDUAL, T.EXHAUST, T.CONFUSE. Nothing has
systematically tried to realise them.

Write attack tests, one per threat, as ring-3 programs or in-kernel checks.
T.FORGE is the most tractable start — fabricate and guess handles, confirm
every attempt is refused. T.RESIDUAL has an oracle already: allocate, write a
pattern, free, reallocate, confirm zeroes.

Raw material: the 30 fuzz targets and `syscall/check.rs`'s 9,537 lines of
refusal tests. What is missing is an analysis *structured by threat* with a
documented verdict per attack path.

---

## W-10 — Evaluate Ferrocene

**Closes:** F-17. **Size:** unknown until step 1. Procurement as much as
engineering — **answer step 1 before planning anything that depends on it.**

1. Which `rustc` versions does Ferrocene ship, and does its qualified target
   list cover `armv7a-none-eabi` and the three UEFI targets? Expect those four
   to fall outside it.
2. If they do, the reference configuration in
   `scripts/certification-item.json` must say which targets are built with a
   qualified toolchain and which are not.
3. Pinning a Ferrocene release means editing `rust-toolchain.toml`, which is
   its own commit by house convention, and re-running every gate.

---

## W-11 — Side-channel defences, and layout randomisation

**Done 2026-09-26: both halves.** KASLR followed the side-channel defences the
same day: the loader moves the kernel image, the direct map and the vmap
arena's top each boot from `EFI_RNG_PROTOCOL` (18/16/17 bits on the 64-bit
pair, 11/8/9 on ARMv7-A), the 64-bit kernels are static PIEs and the ARMv7-A
kernel keeps its relocations (`--emit-relocs`), stage 1 checks the move, and
`cargo xtask test-kaslr` requires two boots to get two layouts
(SPECULATION.md §6.1). F-31 is closed. See F-31 and
[SPECULATION.md](SPECULATION.md). One build switch, `cargo xtask --mitigations
on|off`, `on` the default and the reference; `on` clamps every program-chosen
index at the system call boundary and applies each processor's speculation
controls, per architecture, read back on every processor at boot (FX-0307).
`cargo xtask check` builds the kernel both ways. Measured under KVM: +1.0% on
system calls, +2.8% on fork-exec-wait.

**Closes:** F-31, with the steps below. **Size:** large for KASLR, small for
each of the rest.

1. **KASLR — done.** As planned, with two departures. ARMv7-A could not be a
   PIE, since its prebuilt `core` uses `movw`/`movt`, so it is a fixed link
   with `--emit-relocs` and a 64 KiB step. And x86-64 needed UMIP, or `SIDT`
   reads the image's slide. Left open: moving the image in physical memory too,
   and the Pixel 7 loader, which keeps the fixed layout and says so.
2. **KPTI**, only if a Meltdown-affected processor enters the reference
   configuration: SPECULATION.md §6 lists the four pieces. Today AoU-11 excludes
   such a processor and the boot log names it.
3. **IBT and shadow stacks**: argue them out, or wait for stable compiler
   support; `-Z cf-protection` is nightly-only.
4. **The residuals in SPECULATION.md §9**: `csdb` for the libraries' clamps
   (needs the clamp to be the kernel's, reached through a trait the tables
   take), the tables deeper than the system call boundary, and a written
   position on cache partitioning.
5. **The direct map's alias of the text — done 2026-09-26 (F-34).** Found by
   step 1: the direct map aliased the image's text and read-only data
   writable, which W^X could not see. Both loaders now map that span read
   only, and every boot sweeps each mapping of its frames (`sealed` line,
   FX-0204). Every interface that maps a physical address a caller names
   refuses a range touching the image (`mm::overlaps_image`), checked each
   boot at stages 1, 2 and 6.

---

## W-12 — Fallible allocation in the item

**Done 2026-09-26.** F-23 is closed. Every allocation in the item's product
code reports failure, except 73 at bring-up that are fatal by design. The
design is [MEMORY-AND-TIMING.md](MEMORY-AND-TIMING.md) §1, and what follows is
what to know before changing code under it.

**The rule the gate enforces.** `scripts/check-fallible-alloc.py` fails
`cargo xtask check` on any call to an allocating standard-library API in the
item that is not argued at the site. So in the item:

* `Box::new`, `Vec::push`, `collect`, `format!`, `to_vec` and the rest go
  through `crate::fallible` (`try_box`, `try_push`, `try_collect`,
  `try_format`, `try_to_vec`, …), re-exported from `libs/fallible`.
* `Arc::new` is `fallible::try_arc`, `Arc::new_cyclic` is `try_arc_cyclic`,
  and a map or set insert is `fallible::insert` or `insert_into_set`. When the
  value must not be lost if the insert is refused, enter the section first
  with `fallible::reserve()` and use `insert_held` inside it. A section masks
  interrupts: hold it across nothing that waits.
* A push into room reserved fallibly just before says so, with `NOALLOC:` on
  the line or in the comment block above. A first-party method named like a
  standard one (the handle table's `insert`, the map's `reserve`) gets
  `FALLIBLE:`. `FATAL-ALLOC:` is for bring-up only.
* On a path that cannot fail -- a drop, a decommit, a close -- get the room
  before the first change. If there is no room, keep what you hold and count
  it; do not allocate.

**Failure injection.** `fallible::inject(task, period)` fails every
`period`th fallible allocation of one task until `stop_injecting()`. It is
what `object/alloc_check.rs` drives, and the quickest way to test a new path.
Room already reserved is never failed by it.

**What is left, and where it is filed.** No bound on the heap and no heap
quota (V-05). The load's allocations are infallible (AoU-5): converting a
load module the same way is mechanical, but it is outside the item. The gate
cannot see `.clone()`, conversions, or allocation in a callee; the 18 clones
were audited by hand, and the libraries on the item's paths were converted
with it.

**The hole, found and fixed the same day.** `process_create` and
`process_start` make a POSIX process and its first thread in the load, and
`Signals::default` there allocated with `vec!`: a refused frame stopped the
kernel on an item call. The signal tables are now fallible, and the gate
reads the three load files those calls lean on (`REACHED` in the script),
finds a `Default` that allocates in any kernel file and flags a call of it,
and flags a derived `Clone` over an owned heap field. `process.rs` has 14
sites left, recorded in the baseline; the rest of the load those calls reach
is MEMORY-AND-TIMING.md §1.3's table. When the item comes to lean on another
load file, add it to `REACHED`.

**Verify:** `cargo xtask check` (the "fallible allocation" step), and the
`no-mem` line of any boot, which reads the same on every architecture.

---

## W-13 — Job quotas (FRU_RSA.1)

**Done 2026-09-26.** F-35 is closed; V-05 is narrowed to F-37, the Linux
personality's heap. **Chosen 2026-09-26:** build the quotas, not withdraw the
claim. What follows is the design as it was argued before the code, and then
what was built and where it differs.

`object/job.rs` bounds the job tree's depth and descendants and nothing else.
`docs/CGROUPS.md` plans P1 (`pids`), M1 (`memory`) and S1 (`cpu.weight`) as
cgroupfs controllers. FRU_RSA.1 is a claim about the *job*, in the core, so
the charging goes in the core and cgroupfs is one view of it, as it is of the
tree; a native supervisor sets the same limits through a job handle.

### The counters: one quota slot per job, in a table of atomics

Every job but the tree's root gets a **slot** (`kernel/src/object/quota.rs`):
for each resource a use count, a limit and a count of refusals, plus the
CPU weight and load, all atomics. A slot names its parent's by index.

* **Why a table and not a field of `Job`.** A frame is freed under whatever
  lock its last holder had -- a VMO's pages lock, an address space's, a page
  table walk -- and has to find its charge there. An `Arc<Job>` cannot be
  dropped under those locks (a drop frees memory and may be a job's last), and
  a pointer needs `unsafe`. A `u32` index into a table that is never freed
  needs neither, and fits the frame record's link field, which an allocated
  frame does not use. The table grows by chunks of 256 slots behind `Once`,
  on demand, from process context; nothing is ever taken out of it, so an
  index read anywhere stays valid.
* **Hierarchical and exact.** A charge of *n* walks from the job to the top
  of its tree, and at each level adds *n* only if the level's use stays at or
  under its limit (a compare-and-swap loop, so two charges racing for the last
  unit cannot both win). A refusal at any level takes back what the levels
  below it took, and counts a refusal there. An uncharge walks the same path
  and subtracts. So a child's use is in every ancestor's count, a limit
  anywhere above refuses, and use never exceeds a limit even for an instant.
* **The root is not charged.** The tree's root has no slot, and a process in
  it charges nothing: the default configuration pays one load of a word on
  each path, and no shared cache line is written by every processor's page
  faults. A limit is only ever below the root, as on Linux.
* **A slot outlives its job for as long as anything is charged to it.** It
  counts holds: its job, each frame tagged with it, each object token, each
  child slot, each task that names it for the scheduler. When the last goes,
  it is free for reuse and lets go of its parent. A frame charged to a job
  that has since gone still uncharges exactly the levels it charged, because
  the chain of parents is kept with the slots. Nothing is reparented, and
  there is no zombie job: only its counters stay.

### What each resource is, and where it is charged

**Tasks** (`pids`, as Linux counts them): a process and each thread beside its
first. Charged in the core's `Process::new` before the process is counted in
its job, and by a thread's id allocation (`registry::allocate_thread`); let go
at `Drop for Process` and at a thread's release. A process moved to another
job takes its task count with it, without a limit check, as Linux's
`pids_can_attach` does. Refused: `EAGAIN` from `fork` and `clone`, as Linux
answers, and `SHOULD_WAIT` from native `process_create`.

**Memory** (`memory`, in pages): every frame a program's memory is built of,
charged to the job of the task that caused it -- Linux's first-touch rule --
and uncharged when the frame goes back to the allocator, wherever that is.
Charged: a fault's commit of an anonymous or file page, a copy-on-write copy,
`fork`'s copy of a held page, a `write` or native `vmo_write` that commits,
the page cache's fill from a disk, and the page tables `map_in` builds for a
user space. The frame record keeps the slot index, so the uncharge needs no
lookup and no lock: it is in `mm::release_frame` and `mm::deallocate_frames`,
under every free path at once. Charges do not move with a process (cgroup
v2's rule). A frame shared by `fork` is charged once, to whoever allocated it.
Refused: the allocation fails as if memory had run out, which F-23 made an
answer everywhere -- `ENOMEM` from a call, the fault's signal from a fault,
`NO_MEMORY` natively. *Not charged*, and argued: the kernel heap (V-05's
residual; bounded per job below), kernel stacks (one per task, so bounded by
the task limit), IOMMU tables and device memory (a driver's, from a device
handle only a driver holds).

**Kernel objects**: the objects the native ABI names and a program can
multiply without a handle to show for it -- a VMO, each end of a channel, a
port, a job, a pin. Charged to the running task's job when the object is
made, held by a token inside it, and uncharged when the object is dropped,
however long after and wherever it went (a channel end parked inside another
channel's queue is still counted). The handle limit alone does not bound
them: a chain of channels, each holding the last one's end in its queue,
keeps any number alive with one handle. Refused: `NO_MEMORY` and `ENOMEM`,
as Linux answers a kernel-memory charge. With the task limit and the per
process limits already there (4,096 handles, `RLIMIT_NOFILE`), this bounds
the heap a job's native objects hold. A page-cache object is the file's, not
a program's, and is not charged as an object; its pages are charged as memory.

**CPU**: a weight per job (`cpu.weight`, 1 to 10,000, default 100), so that a
job's share no longer grows with its runnable tasks. Not a group entity in
`libs/sched`'s EEVDF -- S1's 13 points, the largest change to the scheduler
since EEVDF -- but the same arithmetic done on each task's weight: a job's
*load* is the sum of its runnable tasks' weights and of its busy children's
weights, and a task's effective weight is its own weight times, at each level
from its job up to the root's child, that job's weight over that job's load.
A task in the root job keeps its weight exactly, so nothing changes until a
job exists; *n* runnable tasks in one job share one task's weight. That is
Linux's own approximation of a group's per-processor share
(`calc_group_shares`: the group's weight times this processor's part of its
load), without the per-processor refinement. The load is kept as a task
becomes runnable and stops (one atomic add, and a walk up only when a job
turns busy or idle); the weight is recomputed at enqueue and at each tick of
the running task. A task follows its process to a new job at its next trap or
system call. What it is not: a bandwidth cap (`cpu.max`, S2), and a bound on
the time a job's tasks spend in the kernel beyond EEVDF's own.

### Interfaces

* Native: `job_set_limit(job, resource, value)` and `job_get_quota(job,
  resource, out)`, needing `MANAGE` and `WAIT`; resources memory (bytes),
  objects, tasks and CPU weight. A limit binds the job and everything under
  it, so a supervisor bounds an untrusted program by a job *above* any it
  hands the program.
* cgroupfs: `BUILT` holds `cpu`, `memory` and `pids`. Each child cgroup whose
  parent enables them has `pids.max`, `pids.current`, `pids.events`,
  `memory.max`, `memory.current`, `memory.events` and `cpu.weight`, over the
  same slot. `memory` is a domain controller, so the no-internal-process rule
  becomes reachable.

### Evidence

Boot checks, under a `quota` line on every architecture: each limit refuses at
exactly its value; a parent's limit refuses a child's charge; a job filled to
its limits and emptied reads zero everywhere and frees its slot; a fork bomb
in a limited job is refused at its limit while a sibling can still make
processes; a memory hog in a limited job is refused while a sibling job keeps
committing; eight spinning tasks in one job and one in another share a
processor about evenly. Each with a negative control. A `test-vfs` command
sets `pids.max` to 10 and forks until refused. Cost: page fault, fork and a
null system call timed under KVM before and after, in the root job and in a
limited one.

### As built

Four commits on `cert-f35-quotas`: the design, the charging (core), the
cgroupfs view, and these documents. Where the code differs from the design
above:

* **Tasks are uncharged at reap**, not at `Drop for Process`: the reaped
  process's last reference may be a task the scheduler has not freed yet, and
  a shell running short commands under `pids.max` saw exited ones still
  counted. The parent's `reap_child` and `disown` call `uncharge_tasks`, and
  the drop takes back whatever is left.
* **A native `process_create` loads as a task of the target job**
  (`sched::set_current_group` around the load), so the child's first memory
  is its job's; `CLONE_INTO_CGROUP` does the same around `fork`'s copy. The
  move of a new native process into its job is checked against the task
  limit (`Process::move_new_to`); a move by `cgroup.procs` is not.
* **Objects** are charged by `Vmo::new_anonymous` and `Vmo::fork`, so each
  anonymous or private mapping a Linux program makes counts as one; a page
  cache object is the file's and is not. cgroupfs has no file for the object
  limit, which only a job handle sets.
* **A task follows its process to a new job** at its next trap from user
  mode or system call (`sched::regroup_current`): one per-processor word is
  compared with a global count of moves, 5.3 ns a call under KVM. A process
  that moves itself regroups before its call returns.
* **The effective weight** takes a job's load to be at least what the level
  below adds, so a task not yet counted is never scaled up; without that the
  first check spun one task at 16 million and starved the rest. A job's load
  and what it adds to its parent's are kept by atomics and can drift when a
  job turns busy and idle on two processors at once: the drift scales every
  sibling of that job's parent alike, so shares within the parent hold.
* **Page tables** are charged in `map_in` for user mappings only, through a
  `PhysMem` that tags the table with the running task's slot; their frees,
  F-36's deferred ones included, uncharge in `deallocate_frames` untouched.

Measured under KVM, x86-64, best of five, three runs each against `main`
without the quotas: a fault 848 ns against 832, the same in a job two levels
deep; a fork of 256 resident pages 79 µs against 77; within the runs' spread.

Negative controls, scratch, each stopping the boot by its own message: the
limit ignored in `quota::charge` (*"forks went past pids.max"*, the `cgroups`
check); `release_frame` uncharging nothing (*"address spaces gone and their
frames still charged"*); the job share taken out of `effective_weight` (*"a
job with many spinning tasks took more than its share from another job"*,
one task at 111 per mille); `Process::new` charging nothing (*"forks went
past pids.max"*).

**Verify:** the `quota` and `cgroups` lines of any boot, `test-vfs` command
19, and `cat /sys/fs/cgroup/cgroup.controllers` listing `cpu memory pids`.

**What is left:** F-37, the heap the Linux personality allocates for a job;
`cpu.max` (S2), a bandwidth cap, which the ST no longer claims; `memory`'s
reclaim and scoped OOM kill (M1's rest and M2 in `docs/CGROUPS.md`), without
which a job at `memory.max` is refused rather than reclaimed from.

---

## W-14 — Page tables go back after their shootdown

**Done 2026-09-26.** Closes F-36, found by the memory coverage work.

A user unmap freed each page table it emptied at once, before the shootdown,
while another processor could still walk through it from its paging-structure
or walk caches; IOMMU unmaps did the same before the unit's invalidation.
`mm::unmap_in` now puts the tables on the shootdown's `TlbPages`
(`mm/unlinked.rs`, a list linked through the tables themselves, needing no
memory), and `smp::flush_tlb_pages` gives them back after the last answer.
An IOMMU caller releases its list after the unit's flush.

**The rule for new code.** A tree some processor or unit may have walked is
unmapped with `mm::unmap_in` into the `TlbPages` its shootdown will flush, or
with `unmap_io` into a list released after the unit's flush. `unmap_unwalked`
is only for a tree nothing ever walked or everything left with a full flush:
a dropped space, a bring-up tree. A `TlbPages` is not `Copy`: merge with
`add_all`, which moves the tables.

**Verify:** stage 4's check (`tables_wait_for_their_shootdown`), and its
negative control: put `unmap_in`'s old callback back (scratch), and stage 4
must stop at *"an unmap gave back the tables it emptied before its
shootdown"*.

## W-15 — The Linux personality's heap, charged to the job

**Open.** Closes F-37. What follows is the design as argued before the code;
an "As built" section follows once it is.

W-13 charges a job for its programs' frames and page tables, their native
objects and their tasks. What it leaves out is the kernel heap a program
drives through the Linux personality and the libraries under it: an open
file, a tmpfs inode, a pipe's buffer, a region of its address space, a
message in a socket's queue. Each is bounded by the machine's memory and by
nothing that belongs to one job, so one job can take that heap from every
other, and the load's allocations -- still infallible, AoU-5 -- stop the
machine when it is gone (V-05).

### The audit

Every allocation in the load ring and its libraries that a program can make
*and keep* after its call returns, with the count in the program's hands,
was listed on 2026-09-26 by reading each path from the system call down.
Transient allocations freed before the call returns do not accumulate and
are not listed. Grouped by what is held:

| Kind | Where | Bound before this |
|---|---|---|
| Open file descriptions | `libs/vfs` `OpenFile::new`, `with_io` | descriptors per process -- and none in flight, in a mapping, or behind an epoll registration |
| Dentries, anonymous-file locations, mounts | `libs/vfs` `Dentry::new`, `Location::detached`, `Namespace::mount` | a 4,096-entry cache, plus whatever an open file or a working directory pins |
| tmpfs inodes, names, symbolic links, instances; a file's VMO | `libs/vfs/src/tmpfs.rs`, `fs/pages.rs` | none: `/tmp` and `/dev/shm` are mode 1777 |
| Pipes and their buffers | `fs/pipe.rs`, `libs/vfs/src/pipe.rs` | 64 KiB a pipe |
| `AF_UNIX` sockets, their queues, descriptors in flight | `fs/socket.rs`, `libs/vfs/src/socket.rs` | 212,992 bytes of payload a direction, but an empty record counts one byte and holds a hundred, and a message carrying 253 descriptors counts one |
| epoll sets and registrations; eventfd, timerfd, signalfd | `fs/epoll.rs`, `fs/eventfd.rs`, `fs/timerfd.rs`, `fs/signalfd.rs` | descriptors -- but a closed file's registration stays until the next wait |
| Regions of an address space; a shared file mapping's records | `libs/vma`, `user/space.rs` | the address space: 2^35 pages. No `max_map_count`, and a shared file mapping makes no VMO for the object limit to see |
| Record and whole-file locks | `syscall/flock.rs` | none: one owner may lock any number of disjoint ranges |
| Descriptor tables | `libs/vfs/src/fd.rs` | `RLIMIT_NOFILE` a process |
| A process's recorded program and arguments | `syscall/process.rs` `record_exec` | 256 KiB a process |
| `/proc` and cgroupfs snapshots | `fs/procfs.rs` | one a descriptor, sized by what it shows |
| Internet sockets and their queues | `net/socket.rs`, `libs/net`, `libs/nettcp` | 64 KiB each way a connection, 212,992 bytes of payload a datagram socket -- but an empty datagram counts nothing |
| Netlink queues | `net/netlink` | 256 KiB a socket |

And five that are not a missing charge but a leak or a missing check, which
no charge would fix: a closed TCP listener leaks the connections it had not
accepted, with their receive buffers; a process's list of tasks is never
pruned, so a loop of threads grows it for the process's life; netlink adds
addresses and routes to the global tables with no privilege check; an empty
datagram is queued without counting against its socket's capacity; and a
btrfs root's metadata changes are held in memory for up to the commit
interval without counting toward the commit threshold.

### The choice: bytes, charged at the site, to memory

(a) A kernel-memory counter charged at each site, folded into the job's
memory limit as Linux folds `kmem` into `memory.max`; or (b) a count limit
per kind. (b) is simpler at each site and wrong in aggregate: twelve limits
that each allow a job its share still let it take twelve shares, and a job's
supervisor has no single number to set. (a) is what cgroup v2 does, and what
a Linux program expects `memory.max` to mean: `memory.current` counts kernel
memory, and a charge past `memory.max` fails the allocation with `ENOMEM`.
**Chosen: (a)**, with (b) only where bytes cannot be attributed to a job --
the global tables netlink writes, which get the privilege check Linux has.

* **One counter, in bytes.** The memory resource W-13 counts in pages is
  counted in bytes, a frame charging 4,096, so heap and frames meet one
  limit exactly and the compare-and-swap argument holds unchanged. A second
  count, kernel bytes alone, is kept beside it for `memory.stat`'s `kernel`
  line, and never limited.
* **The token.** A new crate, `libs/kmem`, holds a `Charge`: a job's slot
  and a byte count, which uncharges as it drops. The object it pays for
  holds it, so every path that frees the object frees the charge, as W-13's
  object tokens do. It is a crate and not a kernel type because half the
  sites are in libraries (`ferrix-vfs`, `ferrix-vma`, `ferrix-net`) that
  cannot name the kernel. The kernel installs the account it calls through
  at boot; a library's host tests install a recording one; with none, a
  charge is to nobody, which is also what the root job's programs get.
* **What a charge is worth.** What the heap gave, not what was asked: the
  size class a request is served from, or the pages of a large one, from
  `libs/heap`'s own arithmetic. A buffer is charged at its capacity, not its
  length, since that is what it holds.
* **Who pays.** The job of the task whose call made the object, as Linux's
  `GFP_KERNEL_ACCOUNT` charges `current`'s memory cgroup. A buffer that
  grows later is charged where its object is: a pipe's or socket queue's
  growth to the job that made the pipe or the socket, as Linux charges a
  socket's buffers to the cgroup its socket was made in; a message's bytes
  to its writer; a connection a listener accepts, to the listener's job.
* **An object outlives its job, or moves to another.** It stays charged to
  the job that made it until it goes, as a frame does and as Linux's
  `obj_cgroup` does: the charge holds the job's slot, so a gone job's
  counters stay exact until the last thing charged to it is freed. A
  descriptor passed over a Unix socket (`SCM_RIGHTS`) stays its opener's;
  the message that carries it -- the list of files and the queue entry -- is
  the sender's, which is what Linux charges (`scm_fp_dup` is
  `GFP_KERNEL_ACCOUNT` in the sender).
* **Refused is `ENOMEM`**, from the call that would have made or grown the
  object, with nothing changed: a charge is made before the first mutation,
  so a rename refused its new name keeps its old one. A write that has
  queued some bytes reports those. The network's input path cannot answer
  anyone: a segment or datagram whose charge is refused is dropped, as one
  arriving at a full buffer is, and TCP's retransmission makes that
  back-pressure.

### What is argued rather than charged

* **Per-page bookkeeping of charged frames**: a VMO's page list entry is a
  few dozen bytes per 4,096-byte frame already charged, so it is bounded by
  the memory limit at under one per cent.
* **Futex waiters, signal state, a thread's kernel stack and queue nodes**:
  one per task, bounded by the task limit.
* **Pseudoterminals**: 256 pairs on the machine, and a few kilobytes each.
* **The dentry cache** keeps up to 4,096 dentries nobody holds, charged to
  whoever looked them up. A job whose limit they take meets `ENOMEM` where
  Linux would reclaim them; reclaim is M1's rest (`docs/CGROUPS.md`).
* **The load's own infallible allocations** stay infallible (AoU-5). What
  changes is that a limited job cannot drive the heap to exhaustion through
  them.

### Evidence

A boot check per kind: a job at a memory limit is refused one more of each
-- an open file, a tmpfs file, a name, a pipe and its buffer, a socket and
a message, a descriptor in flight, an epoll registration, a region, a lock
range -- while a sibling makes the same; and when the job's objects go, its
counter reads zero and its slot is given back. A `test-vfs` command fills
`/tmp` from a shell in a cgroup with a small `memory.max` until creation is
refused, reads `memory.current` and `memory.stat` against it, removes what
it made and sees the charge go. Negative controls in scratch. Cost timed
under KVM: open and close, a pipe write and read, a tmpfs create and write.

---

## Suggested order

**Done:** order zero, W-3, W-2, W-6, W-9, W-4 (with F-08), W-1, W-5 (with
F-09 and F-33), W-7's measurement and ratchet, W-11, W-12 (F-23), W-14
(F-36), and W-13 (F-35).
**Remaining:** F-10's tests, by module from COVERAGE-WORKLIST.md → W-8
(largest), with W-10 in parallel whenever someone can answer step 1. F-37,
the Linux personality's heap per job, is independent of all three.

W-1 landed as a split rather than a move, and took the boundary from 36
references to 29 by the gate's count of the day. What it leaves is F-09's:
six Linux-personality syscall files in the item ring that name the POSIX
process for its state. Since the gate learned to resolve module paths
(2026-09-26) the register reads 56, not 29 -- F-09 at 39, 21 of them the Linux
dispatcher's; F-07 at 12; and a new F-33, 5, a core boot check that is small
to move. W-5 took all 56 (2026-09-26): the boundary has no upward reference
left, and what the item holds of the personality is one function pointer to
its dispatcher and one `Processes` to make a native process with.

## Not on this list

F-20, F-22, F-27, F-28 and F-30 need an application, an organisation or years —
see [TODO.md](TODO.md) §6. Do not write a hazard analysis or a planning set
from here; it produces documents an assessor rejects and makes this directory
look more finished than it is.
