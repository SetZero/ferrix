# Verification evidence

What has been verified about the item in [ITEM.md](ITEM.md), by what, and what
the result does and does not support.

The short version: there is a great deal of verification, and almost none of it
is *traced*. That gap is finding F-14 and it is the difference between evidence
of something and evidence for something.

---

## 1. The reachability problem, and what changed

`docs/sysml/11-assurance.sysml` states the constraint that shapes everything
here. `libs/` is reachable by `cargo test`, Miri and the fuzzers because it is
architecture-neutral logic over bytes. `kernel/` is reachable **only by booting
it** — Miri cannot interpret a privileged instruction and a fuzzer cannot drive
a page-fault handler. That is the whole argument for `libs/` existing.

The consequence was that the item — which is entirely `kernel/` — had no
structural coverage measurement at all. As of 2026-09-25 it does, via QEMU's
`drcov` TCG plugin and the kernel's own DWARF line table. See §3.

---

## 2. What exercises the item

| Layer | Mechanism | Scale |
|---|---|---|
| In-kernel self-tests | `check.rs` / `*_check.rs`, run on every boot | **31,135 lines**, 29 files |
| Boot gates | `cargo xtask test-*` under QEMU | 18 commands, 3 architectures |
| Host unit tests | `cargo test` over `libs/` | ~1,950 plus doc tests (2026-09-23), `xtask` 242 |
| UB detection | `cargo miri test` | 13 crates |
| Fuzzing | `cargo fuzz`, corpora committed | 30 targets |
| Supply chain | `cargo deny check` | empty ignore list |
| Static analysis | `clippy` at ten configurations | denies `unwrap`, `expect`, `panic`, indexing, slicing |

**The in-kernel self-tests are the item's primary evidence**, and they are
unusual enough to be worth describing. Roughly a quarter of the kernel is test
code that runs inside ring 0 on every boot and prints what it proved rather
than that it passed:

```
w^x       3485 mappings swept, 917 executable, none writable
sealed    4416 KiB of text and read-only data, 1697 mappings of it, none writable
handles   1 device aperture mapped into a process and reached from a forked
          child, 1 interrupt held from delivery to acknowledgement, 2 VMO
          pages pinned for a device and found at their device addresses,
          18 refusals as specified
wake      16 of 16 interrupt deliveries ended their wait by waking it,
          the slowest returning after 518 us
reclaim   84 MiB from the loader and ACPI, 434 free; arena 34 live, 708 KiB
```

Counted quantities, not assertions of success. For the Security Target's
objectives this is directly usable: the `w^x` and `sealed` lines are O.WXN
demonstrated on every run, the second over the direct map's alias of the text
that the first cannot see (F-34), the `handles` line is O.CAPABILITY's refusals exercised,
and the `quota` line, since 2026-09-26, is O.QUOTA's: each job limit refused at
exactly its value while a sibling job goes on (F-35).

The largest bodies of in-kernel test code sit against the item: `syscall/
check.rs` at 9,537 lines, `object/check.rs` at 3,318, `user/check.rs` at 1,263,
`sched/check.rs` at 1,201.

---

## 3. Structural coverage

Measured 2026-09-26, and **not comparable with the figures published on
2026-09-25**, which two defects in the measurement made wrong in opposite
directions (§3.4). Raw per-file data in `coverage-*.json`; the ratchet in
`coverage-floor.json`. **x86-64's column is re-measured** later the same day,
on main at 195a2e93 with F-10's x86-64 tests and with two more defects of the
tool fixed (§3.4), which change what counts as a statement; the Arm columns
are still a6d505a2's under the earlier tool, and are not comparable with it
until they are re-measured.

### 3.1 The suite, every architecture

The union of every boot gate that exercises the item and passes under the
plugin — `cargo xtask coverage` runs them. Sixteen on x86-64: `test-boot`,
`test-shell`, `test-vfs`, `test-net`, `test-threads`, `test-pty`, `test-btrfs`,
`test-powerfail`, `test-display`, `test-input`, `test-jobs`, `test-restart` and
`test-sysfs`, and three more boots of `test-boot` on machines the rest do not
present: `boot-legacy`, a q35 with no HPET and a processor with `RDRAND` and
no `RDSEED` (`FERRIX_X86_MACHINE=hpet=off`, `FERRIX_X86_CPU`), whose clock is
the TSC measured against the PIT; `boot-reset`, which ends in the firmware's
reset rather than a power-off; and `boot-single`, one processor. Nine on AArch64 and on ARMv7-A (`--smp 2`): the x86-64-only four
are `test-jobs`, `test-restart` and `test-sysfs`, which carry uutils, and
`test-vfs`, which fails off x86-64 for a reason of its own (§3.5). AArch64 runs
`test-boot` a second time on a `virt` with a GICv3 and its ITS
(`FERRIX_ARM_MACHINE=gic-version=3`): the default `virt` has a GICv2, and
without that boot the Pixel 7's interrupt controller reads as 187 statements
nobody reached. Debug profile, measured on main at a6d505a2, with KASLR:

| Ring | x86-64 | AArch64 | ARMv7-A |
|---|---:|---:|---:|
| `core` | 4,050 / 5,022 — 80.6% | 3,890 / 5,441 — 71.5% | 3,625 / 5,325 — 68.1% |
| `item` | 1,483 / 1,713 — 86.6% | 1,299 / 1,600 — 81.2% | 1,270 / 1,583 — 80.2% |
| **Certified item** | **5,533 / 6,735 — 82.2%** | **5,189 / 7,041 — 73.7%** | **4,895 / 6,908 — 70.9%** |
| `load` (not claimed) | 8,644 / 11,954 — 72.3% | 8,000 / 11,709 — 68.3% | 8,115 / 11,809 — 68.7% |

x86-64 at 195a2e93, the Arm pair at a6d505a2. On x86-64 the same suite's
traces read 74.8% under the earlier tool before the tests below were written;
the tool's two fixes account for most of the rise, by taking out of the
denominator lines with no statement in the image and rows filed under the
wrong file, and the tests for the rest.

**Every gate now counts.** `test-btrfs`, `test-shell`, `test-sysfs`,
`test-restart` and the rest used to pass under the plugin and write an empty
trace: the plugin writes its table when QEMU exits, and those gates ended by
killing it. xtask now asks QEMU to stop (SIGTERM, which QEMU treats as a host
shutdown and exits from normally) before it kills it, and numbers the trace of
every boot after a gate's first, since the plugin truncates its file at each
start and `test-shell` boots four times. The one boot that still leaves
nothing is `test-powerfail`'s churn, whose point is that QEMU is killed with no
chance to finish anything; its replay boots count.

**Most of it is the boot.** One `test-boot` reaches 78.7% of the item on
x86-64; the other fifteen gates add 3.5 points between them. The self-checks
that run on every boot (§2) are the item's real test suite, and the gates
mostly exercise the uncertified load ring above it — 72.3% of `load` against
one boot's 59.8%.

### 3.1.1 The residual

`coverage-residual-<arch>.json` lists every statement in the item the suite did
not reach, by file and line, on each architecture. DO-178C wants each one
either driven by a new requirements-based test or justified as unreachable
defensive code, and neither conversation can start from a percentage.
[COVERAGE-RESIDUAL.md](COVERAGE-RESIDUAL.md) sorts them:

| | x86-64 | AArch64 | ARMv7-A |
|---|---:|---:|---:|
| Unreached | 1,202 | 1,852 | 2,013 |
| Argued: another architecture or board | 130 | 124 | 144 |
| Argued: reached only when stopping | 56 | 8 | 61 |
| Argued: reached only when something has failed | 9 | - | - |
| Argued: run, and credited to another line | 1 | - | - |
| Hardware the machine does not present | 249 | 239 | 362 |
| **Needs a test** | **757** | **1,481** | **1,446** |

[COVERAGE-WORKLIST.md](COVERAGE-WORKLIST.md) groups the last row by module,
with each file's count on every architecture and the lines no architecture
reaches, so that a module can be taken as one piece of work. The largest on
x86-64: `user/space.rs` 110, `iommu.rs` 83, `main.rs` 68, `syscall/native.rs`
49, `console/screen.rs` 44, `mm.rs` and `user/vmo.rs` 41.

**Argued one statement at a time.** A file-level category cannot argue a line
of an otherwise covered file, and most of what F-10 leaves is exactly that.
`coverage-argued-<arch>.json` holds an argument per statement or small range:
the category, why no run of the measured machine reaches it -- for a
defensive path, what would have to go wrong -- and text the first line must
contain, so it cannot slide onto another statement when the file changes.
The generator fails on an argument for a line that is no longer in the
residual, as the boundary gate does on a stale debt entry. Two categories
join the file-level ones: *reached only when something has already failed*,
and *run, and credited to another line*, for a statement a test executes
whose only row in the line table sits in an inlined copy that cannot run it.

**x86-64's architecture code, `trap` and `smp` have nothing left that needs
a test.** Of `arch/x86_64`'s 637 statements 593 are reached and 44 argued:
22 on hardware the TCG machine lacks and gate 6's KVM boot has (the invariant
TSC, the speculation controls), 15 on the stopping path, 6 defensive (the
reset's fallbacks, a stopped HPET, a trampoline past a page) and one credited
to another line. Stage 3 on x86-64 now runs six programs that end by
their own exceptions -- a divide error, `ud2`, an unmapped read, `hlt` in
ring 3, an x87 zero divide, a read past a mapped file's end -- and a seventh
that asks `arch_prctl` both of its refusals; stage 4 checks shootdown page sets merged and
past their ceiling. `trap.rs`'s 22 left are the stopping path and three
invariants; `smp.rs`'s 6 are `debug_assert!` and `fatal!` messages.

The failure path is mostly covered now, and by a passing test: `test-shell`'s
last boot asks for `ferrix.onexit=panic` and gets FX-1501, so the panic report
runs — 16 statements of it are left on x86-64 and 8 on AArch64. ARMv7-A's
61 are mostly the panic screen's renderer (53), which that boot does not draw
there.

### 3.2 One boot, and both profiles

One `test-boot` each, for the question F-11 and F-12 asked — whether each
configuration can be measured at all — and not for comparison with §3.1.

| Configuration | Certified item | Core | Statements |
|---|---:|---:|---:|
| x86-64, debug, one boot | 71.6% | 69.3% | 6,828 |
| x86-64, **release**, one boot | **75.2%** | 74.3% | 4,462 |
| AArch64, debug, one boot | 68.6% | 65.6% | 7,041 |
| ARMv7-A, debug, one boot | 69.0% | 66.3% | 6,908 |

**The profile moves the denominator more than the percentage.** Release
optimisation cuts the item's statement count by a third — 6,828 to 4,462 —
because inlining and merging leave fewer distinct `is_stmt` rows to reach. The
proportion reached moves 3.6 points. So a submission has to say which profile
it measured, and this one measures both, which is what F-11 asked for.

**The three architectures agree**, within four points, on one boot and on the
suite. The table published on 2026-09-25 had ARMv7-A at 70.8% against 46.6%
and 46.1% for the 64-bit pair, and explained the gap by x86-64's larger
arch-specific share. That explanation was wrong: the gap was a defect in the
tool that only 64-bit addresses met (§3.4).

### 3.3 Method

QEMU's `drcov` TCG plugin records every basic block the guest translates and
executes. The kernel's DWARF line table says which source statement each
address belongs to; the denominator is the rows the compiler marked `is_stmt`,
which is what gcov-shaped tools count. `scripts/coverage-report.py` intersects
the two and attributes each file to a ring using the same classifier the
boundary gate uses, so the two cannot disagree.

Gates build different kernels — `test-shell` builds its program in, `test-vfs`
and `test-net` their command lists — so each boot's trace is kept with the ELF
it ran (`<trace>.kernel` names it), each trace is read against its own ELF,
less the slide KASLR gave that boot (`<trace>.slide`), and the union is of
*statements*, a file and a line. The denominator is the
plain `test-boot` build's.

To reproduce, with QEMU's `contrib/plugins/libdrcov.so` built:

```
FERRIX_DRCOV=/path/to/qemu/build/contrib/plugins/libdrcov.so \
  cargo xtask coverage --arch x86_64 \
    --init "$HOME/.local/share/ferrix/busybox/{arch}/bin/busybox.static"
```

That runs the suite with `--accel tcg` (a TCG plugin observes nothing under
KVM, and the launcher refuses rather than reporting zero), writes the traces to
`build/coverage/<arch>`, prints the `coverage-report.py` command it runs, and
fails below the architecture's floor in `coverage-floor.json`. It needs boots,
so it is not part of `cargo xtask check`. Adding `--json` and `--residual` to
the printed command regenerates the evidence, and
`scripts/gen-coverage-justification.py` the two documents from it.

**What this supports.** DO-178C table A-7 objective 5 at DAL C asks for
statement coverage. This is that measurement, for ring-0 code, on every
architecture and both profiles in the reference configuration, without
modifying the toolchain.

**What it does not.** 82.2% is not 100%. The residual is enumerated and
sorted, and 757 of x86-64's 1,202 statements still need a test rather than an
argument (F-10); AArch64 and ARMv7-A owe 1,481 and 1,446 as last measured. There is no decision
or MC/DC coverage, which DAL C does not require and DAL B and A do (F-13).

Two biases, both optimistic and both declared in the tool's own docstring: a
basic block credits every statement inside it even if a trap left it early, and
optimised builds map one address to several source lines.

### 3.4 Two defects, and what they did to the published figures

Found 2026-09-26 by comparing the three architectures' per-file results, which
disagreed on generic code that every boot runs — `syscall/mod.rs`'s dispatch
arms for `brk`, `munmap` and `wait4` read unreached on x86-64 and reached on
ARMv7-A — and then reading the raw blocks against `objdump`.

1. **A sentinel below the higher half.** The lookup that asks whether an
   address lies in an executed block searched with `(address, 1 << 62)`,
   meant to sort after every block starting at that address. Every block end
   in a kernel linked at `0xffffffff80000000` is above `1 << 62`, so a block
   starting *exactly* on a statement was never found. On x86-64 and AArch64
   that under-reported by about a third: one boot of the 2026-09-25 tree read
   46.6% and was 73.4%. ARMv7-A's 32-bit addresses never met it.
2. **Every trace read against one ELF.** The union read all the gates' traces
   against whichever kernel was built last, and the gates build different
   kernels whose code sits at different addresses. On the 2026-09-25 tree,
   `test-vfs`'s trace read against the `test-boot` build gives 66.4% of the
   item; against its own, 73.6%. Misattributed blocks land on statements
   nobody ran, so this over-reported, and more the more gates were in the
   union.

The published 81.9% had both, one pulling each way, and neither this tool nor
anyone could have told it from a correct figure. Both are fixed in the tool,
and TOOLS.md TOR-3 records them beside the three found before.

Two more, found the same day by walking the residual line by line, both in
how the line table is read:

3. **Rows of discarded functions.** At `opt-level = 1` a small function is
   inlined into every caller and its out-of-line copy dropped by the linker,
   whose rows stay behind at a few bytes above zero. A line whose only row
   was one of those -- the closing brace of `cpu.rs`'s `outb` -- counted as a
   statement nobody reached: about a quarter of every residual.
4. **Rows under the wrong file.** After a line-table sequence ends objdump
   prints no header for the next one, so its rows were filed under whichever
   file came last: `check.rs`, `drm.rs` and `signal.rs` statements read as
   `syscall/uaccess.rs`'s, and a line 1392 appeared in the 431 lines of
   `gdt.rs`. This one over-reported as well as under-reported.

Both are fixed, and x86-64's column above is measured with the fix; TOOLS.md
TOR-3 records them.

### 3.5 What the suite leaves out

`test-vfs` on AArch64 and ARMv7-A fails with and without the plugin: its
permissions command expects uutils' `cat: /tmp/dac-private: Permission denied`,
and the Arm images carry busybox, which says `cat: can't open ...`. The kernel
refused correctly; the expectation is x86-64's. `test-seat` and
`test-compositor` pass under TCG but not under the plugin, which slows TCG
enough that the first misses its redraw and the second trips the TLB
shootdown's bound (`processor 0 never flushed its TLB for a shootdown`). A
failing run is not coverage evidence, so none of the three counts. `test-foot`,
`test-video`, `test-vkgears`, `test-rustc`, `test-chrome` and `test-selfhost`
need a GL host, ports or fetched volumes.

## 4. The traceability gap

Everything in §2 and §3 verifies *behaviour*. Almost none of it is linked to a
*requirement*.

`docs/sysml/` carries 33 requirements with stable ids and 32 `verify` /
`objective` links — real bidirectional traceability, and better than most
projects have. But those requirements are at system level (`<'G.1'>` kernel
threads, `<'G.2'>` address-space scale), and 49,431 lines of item product code
trace to 33 of them.

No test names a requirement id. The boot gates assert that 16 of 16 interrupt
deliveries woke their waiter; nothing records which requirement that
discharges.

This is findings F-14, F-15 and F-16 together, and it is the largest structural
gap between this body of work and a DAL C or Class C submission. The fix is not
more testing — there is a great deal of testing. It is low-level requirements
for the item's modules, and a requirement id attached to each assertion.

The [Security Target](SECURITY-TARGET.md) §7 is the first piece of that work: it
maps each of eight security objectives to the code implementing it and the test
exercising it. Eight objectives is not 49,431 lines of traceability, but it is
the shape the rest should take.

---

## 5. Independence

None. Every test in §2 was written by the same process that wrote the code it
tests. DO-178C DAL C requires independence for 5 of its 62 objectives; EN 50716
at SIL 2 permits combined roles with justification, and no justification is
written. Finding F-27.
