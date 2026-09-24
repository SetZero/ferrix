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
