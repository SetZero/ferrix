# Auditor's work list

The audit-side list: what to re-measure, how to know a finding closed, and
what deliberately must not be written. For the engineering work itself see
[IMPLEMENTATION.md](IMPLEMENTATION.md), which carries the design detail this
file only gestures at — notably that the obvious reading of F-01 is the wrong
change.

Ordered by value per unit of effort, not by finding number. Each item says what to do, which finding it
closes, and how to know it is done — because "done" here means something in the
build says so, not that a document claims it.

Finding ids refer to [FINDINGS.md](FINDINGS.md). The scope of everything below
is the item defined in [ITEM.md](ITEM.md) and enforced by
`scripts/certification-item.json`.

---

## 0. Before touching anything

### 0.1 Run the full gate set on this branch
**Done 2026-09-25** (IMPLEMENTATION.md, order zero). Kept for the next audit
branch, which will be in the same position.

The audit ran the eleven fast gates, `cargo fmt`, crate layering, the
SysML/arch-doc gate, and an `xtask` build. It did **not** run the ten
cross-target clippy passes or the host test suite against these changes.

```
cargo xtask check
```

Expect this to be the first thing that fails. `check.rs` gained two steps and
`qemu.rs` gained an env-var branch; clippy for three kernel targets, three
loader targets and the native programs has not seen any of it.

### 0.2 Re-measure before citing
The audit branch was cut at `bd78a286` (2026-09-24) and has since landed on
`main`. Its numbers were reconciled against the gates on 2026-09-26. Sibling
sessions land concurrently, and the load ring in particular grows with every
unrelated feature, so re-measure before citing:

```
python3 scripts/check-item-boundary.py --report
```

---

## 1. Boundary — the cheapest real wins

The debt register in `scripts/certification-item.json` holds 48 upward
references, down from 62 when the audit began. It may shrink freely; it may not grow. After each fix, delete the
entries it retires — the gate fails on stale entries, so it will tell you which.

### 1.1 Extract `Process` into the core — **F-01, 10 references**
`Process`, `ProcessRef` and `current()` are core concepts (a process *is* the
address-space and capability container) living in `kernel/src/syscall/
process.rs`, 2,229 lines of Linux personality.

**Do not move the type wholesale.** It carries the file-descriptor table, the
filesystem context and the signal state, and moving it would pull all three
into the core while the gate went green. It has to be split, and the
`Task -> Thread -> Process` chain crosses the boundary twice, so `Thread` needs
a home decided too. IMPLEMENTATION.md W-1 has the measurement.

*Highest value single change in this list.* It is 10 of 48 references, it is
the most-cited structural defect, and F-09's 14 references are mostly
downstream of it — so it plausibly retires ~24 at once. It is also the clearest
`ADV_INT.2` counter-example in the item.

*Done when:* `check-item-boundary.py` reports 10 fewer, the `F-01` entries are
gone from the manifest, and `core` no longer names `syscall::process`.

### 1.2 Invert the `StatLayout` dependency — **F-03, 3 references**
**Done 2026-09-25.**

Each `arch/*/mod.rs` declares `STAT_LAYOUT: crate::syscall::stat::StatLayout`.
The personality should ask the arch facade which layout it wants. Smallest fix
here; do it while learning the gate.

### 1.3 Board and ring registration — **F-04 and F-05, 7 references**
**F-05 done 2026-09-25.** F-04's six references from `device.rs` remain.

`device.rs` names `stm32mp1*`; `claim.rs` names `block_ring`. Both want
registration into the core rather than the core naming them.

### 1.4 Invert the trap-return upcall — **F-02, 7 references**
**Done 2026-09-25**, with F-02a. The trap path names nothing above the core.

`arch/*/signal.rs`, `arch/*/trap.rs`, `arch/x86_64/syscall.rs` and `trap.rs`
call `syscall::deliver::{needs_attention, return_to_user, sigreturn}`. The core
should define a hook the personality registers into at init.

*Hardest of the boundary items and the most valuable after F-01*: it is on the
most trusted path in the system, and while it stands the core cannot be built
or analysed without the personality present.

### 1.5 A registration table for the native dispatcher — **F-07, 9 references**
`syscall/native.rs` names eleven load-ring modules. Subsystems should register
handlers in a table the dispatcher walks.

---

## 2. Coverage — the tooling exists, nobody has run it

All three commands below work today. Reproducing the audit's number:

```
FERRIX_QEMU_PLUGIN="/home/sebastian/Documents/qemu/qemu/build/contrib/plugins/libdrcov.so,filename=/tmp/cov-boot.drcov" \
  cargo run -q -p xtask -- test-boot --arch x86_64 --accel tcg

python3 scripts/coverage-report.py \
  --drcov /tmp/cov-boot.drcov \
  --elf target/x86_64-unknown-none/debug/ferrix-kernel
```

`--accel tcg` is mandatory — a TCG plugin sees nothing under KVM, and the
launcher refuses rather than reporting zero. Gates needing a userland want
`--init "$HOME/.local/share/ferrix/busybox/{arch}/bin/busybox.static"`;
`test-vfs` and `test-net` fail without it.

### 2.1 Measure AArch64 and ARMv7-A — **F-12**
**Done 2026-09-25**: AArch64 46.1%, ARMv7-A 70.8%, one `test-boot` each.
Raising them to the full suite is part of 2.3.

Both are in the reference configuration with no data at all. The tooling is
architecture-agnostic; the ELF path and QEMU binary change. Note ARMv7-A wants
`--smp 2` to match the board.

### 2.2 Measure the release profile — **F-11**
**Done 2026-09-25**: 47.6% against debug's 46.6% on the same gate. The
percentage barely moves and the denominator shrinks by a third; VERIFICATION.md
§3.2 states it.

The reference configuration is `release`; the audit measured `debug`. Optimised
builds inline, so the line table is approximate and the number will move.
Measure it, then either adopt release as the coverage configuration or write
the argument for the difference. DAL C requires one or the other.

### 2.3 Push past 81.9% — **F-10**
**Advanced 2026-09-25** from 71.4%, by adding `test-jobs`. The residual is
enumerated and sorted in COVERAGE-RESIDUAL.md: 103 statements argued, 121
depending on which machine was measured, and **1,054 that need a test**.

`test-btrfs`, `test-shell`, `test-sysfs` and `test-restart` pass under the
plugin and write an *empty* trace, because the plugin flushes when QEMU exits
and those gates end by killing it. Make them power the guest down, then add
them and `test-powerfail` to the union.

### 2.4 Make coverage a ratchet
`coverage-report.py --min-item N` already fails below a threshold. Once the
number is stable, wire it into `cargo xtask check` — or a slower CI job, since
it needs a boot — so coverage cannot regress silently.

### 2.5 Build a complexity and recursion gate — **F-25**
**Done 2026-09-25** by `scripts/check-complexity.py`.

The one code gate the audit did not build. A SIL 2 coding standard must specify
complexity metrics; eleven gates enforce other things and none bounds
cyclomatic complexity, function length or recursion.

Follow the ratchet pattern `check-item-boundary.py` uses: measure, record the
baseline, refuse growth. Without a Rust parser, approximate complexity by
counting branch keywords per function and **say in the docstring that it is an
approximation** — the house rule is that a number whose caveats travel
separately is worse than none.

---

## 3. Traceability — the largest structural gap

F-14, F-15 and F-16 together. 49,431 lines of item product code trace to 33
system-level requirements, and no test names a requirement id. **The fix is not
more testing** — there are 31,135 lines of in-kernel self-test. It is
requirements for the existing tests to discharge.

### 3.1 Write low-level requirements for the item's modules — **F-15**
In `docs/sysml/`, with stable bracketed ids as `01-requirements.sysml` already
does. Verifiable statements with pass/fail criteria, not the narrative
rationale the current 33 are (F-16). This unblocks DAL C, 62304 §5.4 and
`ADV_TDS.3` simultaneously.

Start with the eight objectives in [SECURITY-TARGET.md](SECURITY-TARGET.md) §7,
which already map objective → code → test. That is the shape; it needs to
reach module granularity.

### 3.2 Attach requirement ids to assertions — **F-14**
The in-kernel checks print counted quantities already (*"2387 mappings swept,
899 executable, none writable"*). Each needs the requirement it discharges.

### 3.3 Gate the matrix
Generate a traceability matrix and fail the build on a requirement with no
verification or a test naming a requirement that does not exist. Same pattern
as `gen-arch-doc.py --check`.

---

## 4. EAL5-specific

### 4.1 Vulnerability analysis against the ST's threat model — **F-21a**
**Done 2026-09-25** by VULNERABILITY-ANALYSIS.md. It found V-01 (no SMAP, SMEP
or PAN), which F-32 then fixed on x86-64 and AArch64; it stands on ARMv7-A
and on the reference `cortex-a72`, which lacks PAN.

`AVA_VAN.4` wants methodical analysis against moderate attack potential. The
Security Target states seven threats — T.MEMORY, T.ESCALATE, T.FORGE, T.DMA,
T.RESIDUAL, T.EXHAUST, T.CONFUSE — and nothing has systematically tried to
realise any of them.

*The last EAL5 gap that is engineering rather than paperwork.* The 30 fuzz
targets and `syscall/check.rs`'s 9,537 lines of refusal tests are raw material;
what is missing is an analysis structured by threat with a documented verdict
per attack path.

---

## 5. Tools

### 5.1 Evaluate Ferrocene — **F-17**
Establish which `rustc` versions it ships, and whether the qualified target
list covers `armv7a-none-eabi` and the three UEFI targets. Expect those four to
fall outside it; if they do, the reference configuration has to say so.

This is a procurement question as much as a technical one. Answer it before
planning anything that depends on a qualified toolchain.

### 5.2 Tool operational requirements for the two generators in scope — **F-18**
**Written 2026-09-25** as TOR-1 and TOR-2 in TOOLS.md §6. F-18 stays open:
generator and `--check` share code, so the verification is not independent.

Only `gen-panic-catalog.py` and `gen-font.py` reach the item; the other four
generate compositor code outside the boundary. Both already have `--check`
modes that re-derive output from input on every build, which is the
output-verification route DO-330 permits. Writing it up is a page, not a
project. See [TOOLS.md](TOOLS.md) §5.

---

## 6. Cannot be closed from this repository

Do not write these. A hazard analysis without an application, or a planning set
without an organisation, produces documents an assessor rejects and makes this
directory look more finished than it is.

| Finding | Needs |
|---|---|
| F-20 | An application. Hazards belong to a device or a train, not a kernel. |
| F-22 | The same, plus EN 50126/50129 system context. |
| F-27 | People who do not work on the code. |
| F-28 | An organisation, for ISO 13485 and the DO-178C planning set. |
| F-30 | Years of field history. |

### 6.1 One question to raise externally — **F-29**
No certification scheme has settled how to treat AI-authored code in a
certified item, and every commit in this repository has that provenance. Raise
it with a certification body early rather than discovering it at assessment. It
may constrain which of the four targets is worth pursuing at all.

---

## 7. Standing rules

* **Verify gates by their output, not a pipeline's exit status.**
  `gate | tail && commit` commits on failure. The audit made this exact mistake
  once and caught it only because the result looked wrong.
* **A finding closes when the build says so**, not when a document says so.
* **Re-measure before citing.** Numbers here were reconciled on 2026-09-26.
* **`docs/CONVENTIONS.md` governs commits:** one author, no `Co-authored-by:`
  trailer, no tool signature.
* **Keep the debt register honest.** `known_violations` may shrink without
  ceremony and may not grow without a diff somebody argues for.
