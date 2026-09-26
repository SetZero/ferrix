# Tool qualification register

Every tool that builds, generates or verifies the item in
[ITEM.md](ITEM.md), classified by what a defect in it could do.

Three standards ask nearly the same question in different words. EN 50716 §6.7
classifies tools **T1** (cannot affect the product), **T2** (can fail to detect
a defect), **T3** (can introduce a defect into the product undetected).
DO-178C defers to DO-330, whose TQL-1..5 turn on the same distinction plus the
software level. IEC 62304 has no tool clause but §5.1.4 expects development
tools to be identified and controlled.

This register classifies. It does not qualify anything: no tool here has a
qualification package, which is findings F-17, F-18 and F-19.

---

## 1. The compiler and linker

| Tool | Version | Class | Note |
|---|---|---|---|
| `rustc` | 1.97.1, pinned in `rust-toolchain.toml` | **T3** | Generates the product. |
| `rust-lld` | bundled with the toolchain | **T3** | Links the product. |
| `cargo` | bundled | T2 | Selects what is compiled; a wrong selection is visible in the artifact. |

**T3 and unqualified — finding F-17.** Good practice is in place and is not
qualification evidence: the channel is pinned exactly rather than floating,
`kernel/` and `boot/` use no `#![feature]`, and the assembly budget keeps 303
lines across 19 sites outside the compiler's remit where hand analysis is
cheap.

The concrete route is **Ferrocene**, a qualified Rust toolchain with evidence
packages for IEC 62304 Class C, IEC 61508 SIL 4 and ISO 26262 ASIL D. Adopting
it means pinning a Ferrocene-released `rustc` in place of 1.97.1 and checking
the qualified target list. `armv7a-none-eabi` and the three UEFI targets are
the ones expected to fall outside it, and the reference configuration would
have to say so.

---

## 2. Generators that emit product code

Each writes Rust that is committed and compiled into the artifact, so each is
**T3**. Each also has a `--check` mode that fails the build when its output and
its input disagree — which is not qualification, but is the difference between
a generator whose output is verified on every run and one whose output is
trusted.

| Generator | Emits | In the item? | Class |
|---|---|---|---|
| `gen-panic-catalog.py` | the panic explanation catalogue | **yes** | T3 |
| `gen-font.py` | the panic screen's font | **yes** | T3 |
| `gen-term-font.py` | the terminal's font | no — compositor | T3 |
| `gen-wayland-protocol.py` | compositor interface tables | no — compositor | T3 |
| `gen-xkb-tables.py` | keymap tables | no — compositor | T3 |
| `gen-btrfs-fixtures.py` | test fixtures | no — verification only | T2 |

**Only two touch the item** (finding F-18). That is the useful result of doing
this classification against a boundary rather than against the repository: four
of the six are out of scope at the present item definition, and would come into
scope only if the boundary widened to the compositor, which it will not.

---

## 3. Verification tools

A defect here cannot put a fault into the product; it can fail to reveal one.
**T2** throughout, and DO-330 would ask for qualification only of those whose
output is used to *satisfy* an objective rather than to find defects.

| Tool | Role | Class |
|---|---|---|
| `xtask` | build driver and boot-test harness; 242 own tests | T2 |
| `check-item-boundary.py` | the item boundary — this scheme's own scope | T2 |
| `check-complexity.py` | complexity, length and recursion in the item | T2 |
| `rustlex.py` | tells code from comments and literals for the two above; self-tested on every run | T2 |
| `check-unsafe-audit.py` | every `unsafe` block documented, one operation | T2 |
| `check-panic-audit.py` | every panic-lint exemption justified | T2 |
| `check-asm-budget.py` | the assembly allow-list and budget | T2 |
| `check-device-access.py` | the kernel-enumerates-drivers-drive seam | T2 |
| `check-crate-layering.sh` | layering; `libs/` depends on nothing above | T2 |
| `gen-soup.py` | the item links no external crate | T2 |
| `coverage-report.py` | statement coverage | **T2, and load-bearing** |
| `clippy` | ten configurations; denies `unwrap`, `panic`, indexing | T2 |
| `miri` | UB detection over 13 crates | T2 |
| `cargo fuzz` | 30 targets with committed corpora | T2 |
| `cargo deny` | RUSTSEC advisories, licences, bans | T2 |
| QEMU 9.2.4 + `libdrcov.so` | executes the item; records coverage | **T2, and load-bearing** |

**`coverage-report.py` and QEMU deserve the emphasis** (finding F-19). Coverage
is not a defect-finding activity whose failures are self-revealing — it is
evidence offered directly against DO-178C table A-7. A tool that over-reports
coverage produces a number nobody can distinguish from a correct one. Its two
known biases are documented in its own docstring and in F-11: it measures the
profile actually booted, and a basic block credits every statement inside it,
which errs optimistically.

The two ratchets share the failure mode. A boundary or complexity gate that
under-reports passes, and a pass is indistinguishable from a correct one. Both
did until 2026-09-26: `check-item-boundary.py` saw 29 of 56 upward references
and `check-complexity.py` measured 1,559 of 1,887 functions, because they
found strings with a pattern that mis-paired quotes and the boundary gate read
only the literal text `crate::a::b` (FINDINGS.md §A, F-25). Both now read code
through `rustlex.py` and run their own self-tests before every measurement,
so a lexer or resolver regression fails the build instead of shrinking a count.

QEMU is also the *execution platform* for all boot evidence, not merely an
observer of it. Every claim in [VERIFICATION.md](VERIFICATION.md) except the
host unit tests is a claim about the item's behaviour under emulation, and the
STM32MP157D-DK1 is the only hardware any of it has run on.

---

## 4. Host crates

The 21 external crates in `Cargo.lock` are tools by this register's definition,
since none is in the item — see [SOUP.md](SOUP.md) §2 for the list with
versions and licences. They build, test and package; `syn`, `quote` and
`proc-macro2` are T3 by the strict reading, since a procedural macro emits
code, though none is used in `kernel/` or `boot/`.

All are watched by `cargo deny check` with an empty `advisories.ignore` list
and `yanked = "deny"`.

---

## 5. What qualification would actually require

For the two T3 tools that reach the item — `rustc` and the two generators —
DO-330 offers two routes, and the cheaper one is available here.

* **Qualify the tool.** For `rustc` this means Ferrocene, and buying it rather
  than building it.
* **Verify the output instead.** The generators already do this: `--check`
  re-derives the output from the input on every build, so a generator defect
  that changed its output would fail the gate. Extending that argument into a
  DO-330 tool operational requirements document is a page of writing, not a
  project.

For `rustc` the output-verification route is the DAL A source-to-object
analysis, and it is not proportionate at DAL C. Ferrocene is the answer.

---

## 6. Tool operational requirements

DO-330 asks, for each tool whose output is relied on, what the tool must do,
what it must not do, and how that is verified. For the three T3 tools inside
the item boundary the output-verification route applies, and this is that
argument written down. It closes the *documentation* half of F-18 and F-19; the
qualification of `rustc` (F-17) is untouched and is the reason neither finding
is struck out.

### TOR-1 — `gen-panic-catalog.py`

| | |
|---|---|
| Output in the item | the panic explanation catalogue compiled into `kernel/src/panic/catalog.rs` |
| **Shall** | derive every entry from the catalogue source, deterministically |
| **Shall not** | emit an entry that its input does not contain, or omit one it does |
| Failure mode | a panic prints the wrong explanation; the kernel's behaviour is unchanged |
| Verification | `--check` re-derives the output and fails the build on any difference, on every run of `cargo xtask check` |
| Residual | a defect present in *both* the generator and its `--check` path would not be caught. The two share code, so this is not independent verification. |

### TOR-2 — `gen-font.py`

| | |
|---|---|
| Output in the item | the panic screen's glyph table |
| **Shall** | rasterise from the committed BDF, byte-identically on any host |
| **Shall not** | depend on a font library installed on the build machine |
| Failure mode | the panic screen is unreadable. It cannot affect any other behaviour: the table is read only by the panic path, after the serial report has already been written. |
| Verification | `--check`, as TOR-1 |
| Residual | as TOR-1, plus: nothing verifies the glyphs are *legible*, only that they match the input |

### TOR-3 — `coverage-report.py`

The one whose failure is least visible, and the only one whose output is
offered directly as evidence against an objective rather than used to find
defects.

| | |
|---|---|
| Output | the statement-coverage figure in [VERIFICATION.md](VERIFICATION.md) |
| **Shall** | count as reached only statements whose address lies in a basic block the run executed |
| **Shall not** | over-report; where it cannot be exact it must err low, or declare the direction |
| Failure mode | **a coverage figure nobody can distinguish from a correct one** |
| Verification | none independent. This is the gap. |
| Known biases | two, both optimistic and both declared in the tool's docstring and in F-11: a basic block credits every statement inside it even when a trap left it early, and optimised builds map one address to several source lines |
| Evidence it is not wildly wrong | three measurement defects were found and fixed by cross-checking its output against raw `objdump` and against the source — the bare-name recursion match, the `Drop::drop` case, and `extern "C"` declarations taking the following item's body. Two more on 2026-09-26, found by comparing architectures: a search sentinel below the higher half that dropped every block starting on a statement (under-reported x86-64 and AArch64 by about a third), and a union that read every gate's trace against one kernel although the gates build different ones (over-reported). The published 81.9% had both; VERIFICATION.md §3.4. A third the same day, found walking AArch64's residual line by line: line-table rows the linker had left behind for functions it discarded (every caller inlined them) were counted as statements, at addresses a few bytes above zero that no run can execute. They were about 30% of every architecture's residual and under-reported; rows outside the image's loadable span are now not statements. And a fourth: objdump prints a file header only when the file changes, and after a sequence ends the line program returns to the unit's first file without printing it again, so those rows were filed under whichever header came last -- `core`'s `map.rs` as lines of `arch/gicv2.rs`, other files' 210 as `syscall/uaccess.rs`'s. Both directions, about 700 rows on AArch64; the parser now reads objdump's wide output, which names each unit's first file |

**TOR-3 has no independent verification and should not pretend to.** The
honest mitigation is that its biases are documented, its residual output is
enumerable (`coverage-residual-<arch>.json`), and a reviewer can spot-check any
file against the source. Measuring three architectures is itself a cross-check:
generic code every boot runs must read alike on all three, and the two defects
of 2026-09-26 were found because it did not. A qualification effort would need a second
implementation to compare against.
