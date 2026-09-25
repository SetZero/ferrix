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

**Closes:** F-01 (10 references), and most of F-09 (14) downstream.
**Size:** large. Do it first anyway — it is the keystone.

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

### The change

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

### What the measurement says (2026-09-25)

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

### Order, to keep the tree green

Land as a sequence, not one commit:

1. Create `object/process.rs` with the core fields, `Process` re-exported from
   `syscall::process` so nothing breaks.
2. Move core consumers (`object/mod.rs`, `object/job.rs`, `sched/mod.rs`,
   `sched/task.rs`, `trap.rs`) to the new path, one file per commit.
3. Move the personality fields out into the extension.
4. Delete the re-export and remove the F-01 entries from
   `scripts/certification-item.json`.

### Verify

`python3 scripts/check-item-boundary.py` reports ~24 fewer references, and the
`F-01`/`F-09` entries are gone from the manifest (the gate fails on stale
entries, so it will tell you which). `cargo xtask test-boot --arch all` passes;
`object/check.rs` and `sched/check.rs` cover this code and run on every boot.

### Pitfall

`docs/sysml/` describes the object model. `gen-arch-doc.py --check` fails if
the model and the generated document disagree, so a new core object means a
model edit. That is a feature: the design document cannot drift.

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

**Half done 2026-09-25:** F-05 closed. The board half, F-04, remains.

**Closes:** F-04 (6), F-05 (1). **Size:** small.

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

**Closes:** F-07 (9 references). **Size:** medium.

`syscall/native.rs:98` is a `match call { … }` naming eleven load-ring modules.
Replace with a table subsystems register handlers into, so the item's exported
interface can be analysed without the whole load ring.

**Pitfall:** the current `match` is exhaustive over the call enum, so the
compiler catches an unhandled call. A table loses that. Keep the guarantee: a
boot-time check that every call number has a registered handler, in
`syscall/check.rs` where the rest of the native ABI's self-tests live. Losing a
compile-time guarantee for a runtime one is only acceptable if the runtime one
actually runs, and here it runs on every boot.

---

## W-6 — Complexity and recursion gate

**Done 2026-09-25.** See F-25.

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

## Suggested order

**Done:** order zero, W-3, W-2, W-6, W-9, F-05 (the first half of W-4), and
most of W-7.
**Remaining:** W-1 (keystone) → W-4's board half → W-5 → W-7's last gates →
W-8 (largest), with W-10 in parallel whenever someone can answer step 1.

W-1 is still first among what is left: twenty-four of the 48 remaining
boundary references are it and its downstream, and W-5 and W-8 both read
better once the core object exists.

## Not on this list

F-20, F-22, F-27, F-28 and F-30 need an application, an organisation or years —
see [TODO.md](TODO.md) §6. Do not write a hazard analysis or a planning set
from here; it produces documents an assessor rejects and makes this directory
look more finished than it is.
