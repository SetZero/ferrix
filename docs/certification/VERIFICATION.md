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
| In-kernel self-tests | `check.rs` / `*_check.rs`, run on every boot | **31,107 lines**, 29 files |
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
w^x       2387 mappings swept, 899 executable, none writable
handles   1 device aperture mapped into a process and reached from a forked
          child, 1 interrupt held from delivery to acknowledgement, 2 VMO
          pages pinned for a device and found at their device addresses,
          18 refusals as specified
wake      16 of 16 interrupt deliveries ended their wait by waking it,
          the slowest returning after 518 us
reclaim   84 MiB from the loader and ACPI, 434 free; arena 34 live, 708 KiB
```

Counted quantities, not assertions of success. For the Security Target's
objectives this is directly usable: the `w^x` line is O.WXN demonstrated on
every run, and the `handles` line is O.CAPABILITY's refusals exercised.

The largest bodies of in-kernel test code sit against the item: `syscall/
check.rs` at 9,537 lines, `object/check.rs` at 3,318, `user/check.rs` at 1,263,
`sched/check.rs` at 1,201.

---

## 3. Structural coverage

Measured 2026-09-25. Raw per-file data in `coverage-*.json`.

### 3.1 The suite, x86-64

Union of four boot gates — `test-boot`, `test-threads`, `test-vfs`,
`test-net` — on the debug profile:

| Ring | Reached | Total | Covered |
|---|---:|---:|---:|
| `core` | 3,138 | 4,516 | **69.5%** |
| `item` | 1,872 | 2,500 | **74.9%** |
| **Certified item** | **5,010** | **7,016** | **71.4%** |
| `load` (not claimed) | 6,620 | 10,916 | 60.6% |

### 3.2 Every architecture, and both profiles

One `test-boot` each, so these are **not** comparable with the four-gate suite
above; they answer whether the configuration can be measured at all, which
until now it could not.

| Configuration | Certified item | Core | Statements |
|---|---:|---:|---:|
| x86-64, debug, one boot | 46.6% | 44.9% | 7,065 |
| x86-64, **release**, one boot | **47.6%** | 47.8% | 4,798 |
| AArch64, debug, one boot | 46.1% | 44.2% | 6,841 |
| ARMv7-A, debug, one boot | **70.8%** | 66.8% | 7,127 |

Two things in that table are worth reading carefully.

**The profile moves the denominator, not the percentage.** Release optimisation
cuts the item's statement count by a third — 7,065 to 4,798 — because inlining
and merging leave fewer distinct `is_stmt` rows to reach. The proportion
reached barely moves, 46.6% to 47.6%. So the coverage *figure* survives the
change of configuration while the *population being counted* does not, and a
submission has to say which one it measured. This one measures release as well
as debug, which is what F-11 asked for.

**ARMv7-A reaches far more from one boot** — 70.8% against x86-64's 46.6% on
the same gate. Not a better-tested architecture: x86-64 carries more
arch-specific code that a plain boot never touches (the APIC, MSI, the GDT, the
`SYSCALL` entry, the paranoid-stack path), so its denominator contains more
code that only later gates reach. It is a reminder that a coverage percentage
is only meaningful against a stated configuration and a stated test set.

### 3.3 Method

QEMU's `drcov` TCG plugin records every basic block the guest translates and
executes. The kernel's DWARF line table says which source statement each
address belongs to; the denominator is the rows the compiler marked `is_stmt`,
which is what gcov-shaped tools count. `scripts/coverage-report.py` intersects
the two and attributes each file to a ring using the same classifier the
boundary gate uses, so the two cannot disagree.

**What this supports.** DO-178C table A-7 objective 5 at DAL C asks for
statement coverage. This is that measurement, for ring-0 code, on every
architecture and both profiles in the reference configuration, without
modifying the toolchain.

**What it does not.** 71.4% is not 100%, and the residual is neither covered
nor justified as unreachable (F-10). The Arm architectures have only a single
gate's worth of data, not the suite (F-12 is closed in the sense that they can
now be measured; raising them is part of F-10). There is no decision or MC/DC
coverage, which DAL C does not require and DAL B and A do (F-13).

Two biases, both optimistic and both declared in the tool's own docstring: a
basic block credits every statement inside it even if a trap left it early, and
optimised builds map one address to several source lines.

## 4. The traceability gap

Everything in §2 and §3 verifies *behaviour*. Almost none of it is linked to a
*requirement*.

`docs/sysml/` carries 33 requirements with stable ids and 32 `verify` /
`objective` links — real bidirectional traceability, and better than most
projects have. But those requirements are at system level (`<'G.1'>` kernel
threads, `<'G.2'>` address-space scale), and 48,887 lines of item product code
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
exercising it. Eight objectives is not 48,887 lines of traceability, but it is
the shape the rest should take.

---

## 5. Independence

None. Every test in §2 was written by the same process that wrote the code it
tests. DO-178C DAL C requires independence for 5 of its 62 objectives; EN 50716
at SIL 2 permits combined roles with justification, and no justification is
written. Finding F-27.
