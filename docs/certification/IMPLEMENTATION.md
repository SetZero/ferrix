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

**Mostly done 2026-09-25.** Steps 1, 2 and 4 are done (F-11 and F-12 closed,
the residual sorted in COVERAGE-RESIDUAL.md). Step 3 added `test-jobs` and
took the item to 81.9%; the other four gates write an empty trace because
they end by killing QEMU, and the plugin only flushes when QEMU exits. Step 5
is not wired up. F-10 stays open on 1,054 statements that need a test.

**Closes:** F-10, F-11, F-12. **Size:** medium, mostly running things.

Tooling exists and works. Reproduction:

```
FERRIX_QEMU_PLUGIN="/home/sebastian/Documents/qemu/qemu/build/contrib/plugins/libdrcov.so,filename=/tmp/cov-boot.drcov" \
  cargo run -q -p xtask -- test-boot --arch x86_64 --accel tcg

python3 scripts/coverage-report.py --drcov /tmp/cov-boot.drcov \
  --elf target/x86_64-unknown-none/debug/ferrix-kernel
```

`--accel tcg` is mandatory — a TCG plugin observes nothing under KVM, and the
launcher refuses rather than reporting zero. Gates with a userland need
`--init "$HOME/.local/share/ferrix/busybox/{arch}/bin/busybox.static"`;
`test-vfs` and `test-net` fail without it. ARMv7-A wants `--smp 2`.

1. **AArch64 and ARMv7-A** (F-12) — no data at all today.
2. **Release profile** (F-11) — the reference configuration is release, the
   audit measured debug. The number will move; either adopt release as the
   coverage configuration or write the argument for the difference.
3. **More gates in the union** (F-10) — add `test-btrfs`, `test-shell`,
   `test-restart`, `test-sysfs`, `test-jobs`. `--drcov` takes several traces.
4. **Enumerate the residual** — split what is left into "needs a test" and
   "unreachable defensive code, justified". The second list is legitimate and
   must be written.
5. **Ratchet it** — `--min-item N` already fails below a threshold. Wire it
   into a CI job (not `xtask check`; it needs a boot).

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
   FX-0204).

---

## Suggested order

**Done:** order zero, W-3, W-2, W-6, W-9, W-4 (with F-08), W-1, W-5 (with
F-09 and F-33), most of W-7, and W-11.
**Remaining:** W-7's last gates → W-8 (largest), with W-10 in parallel
whenever someone can answer step 1.

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
