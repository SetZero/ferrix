# Certification

A theoretical assessment of Ferrix against four assurance ratings, conducted
2026-09-25. Theoretical because no accredited laboratory, notified body or
independent assessor has been engaged — the audit is internal, and says so
everywhere it matters.

| Target | Standard | Verdict |
|---|---|---|
| EAL5+ | Common Criteria (ISO/IEC 15408) | **Not met.** Security Target now written; blocked on a vulnerability analysis and design evidence at module granularity. |
| DAL C | DO-178C / ED-12C | **Not met.** The coverage blocker is retired; planning data and requirements traceability are not. |
| Class C | IEC 62304 | **Closest of the four.** Technically strong — no SOUP in the item — and blocked on a risk management file and a QMS. |
| SIL 2 | EN 50716:2023 | **Reachable.** Most of the Annex A technique table is already satisfied; blocked on a safety case. |

None of the four can be claimed today. What changed is that the reasons are now
specific, measured, and mostly documents rather than code.

* [ITEM.md](ITEM.md) — what the ratings attach to, and why it is not all of Ferrix
* [FINDINGS.md](FINDINGS.md) — the audit register, 31 open findings and 3 closed
* [SECURITY-TARGET.md](SECURITY-TARGET.md) — EAL5+ claim, SFRs, and where it would fail evaluation
* [SOUP.md](SOUP.md) — generated; the item contains none
* [VERIFICATION.md](VERIFICATION.md) — what exercises the item, and the traceability gap
* [TOOLS.md](TOOLS.md) — tool classification under EN 50716 §6.7 and DO-330
* `coverage-x86_64.json` — statement coverage evidence, per file

**Not written, and why.** A *hazard analysis* and a *risk management file*
(F-20) need an application: hazards belong to a device or a train, not to a
kernel, and a generic list would be a document rather than evidence. A *safety
case* (F-22) needs the same. The *DO-178C planning set* — PSAC, SDP, SVP, SCMP,
SQAP — needs an organisation to describe (F-28). Writing any of them from here
would produce paperwork that an assessor would reject and that would make this
directory look more finished than it is.

---

## 1. What the item is

Not Ferrix. Ferrix's acceptance test is that it hosts `rustc` and builds
itself, which requires a general-purpose OS with a browser, a compositor and a
self-hosting toolchain — the opposite of a frozen, analysable configuration.

The item is a **48,887-line subset of the kernel**, defined in
[`scripts/certification-item.json`](../../scripts/certification-item.json) and
enforced on every build by `scripts/check-item-boundary.py`. Memory protection,
scheduling, capability objects, the trap and syscall entry paths, the IOMMU,
SMP and device enumeration are inside; the VFS, btrfs, the network stack, the
Linux personality and the drivers are uncertified load above it, 44,330 lines
of it.

The boundary is nested so it can ratchet inward: a 38,003-line `core` ring is
named now as the destination for a later EAL6+ or ASIL D effort, so that
raising the target does not mean rewriting every artifact scoped to the old
boundary.

## 2. What was measured, not asserted

| | |
|---|---:|
| Item product code | 48,887 lines |
| Uncertified load | 44,330 lines |
| In-kernel self-tests | 31,107 lines |
| Statement coverage, certified item | **71.4%** |
| Statement coverage, core ring | 69.5% |
| SOUP in the item | **0** |
| External crates, host-side | 21 |
| Upward boundary references | 62, in 27 files |
| `unsafe` blocks, all documented | 662 |
| Assembly | 303 lines, 19 allow-listed sites |
| Cargo features in `kernel/`/`boot/` | 0 |

Two of these were unknown before this audit and are the reason it was worth
doing. The kernel had **no structural coverage measurement at all** — the
assurance model records why, since a fuzzer cannot drive a page-fault handler
and Miri cannot interpret a privileged instruction. And nobody had established
what third-party code ships inside ring 0.

## 3. The four verdicts

### EAL5+ — not met

EAL5 wants a semiformally designed and tested TOE: `ADV_FSP.4`, `ADV_TDS.3`,
`ADV_IMP.1`, `ADV_INT.2`, `AVA_VAN.4`, `ALC_CMC.4`, plus `ALC_FLR` for the `+`.

*In place:* a defined TOE boundary, machine-checked. A semiformal design
notation already in use — `docs/sysml/`, 5,890 lines of SysML v2 across twelve
packages, with a gate that fails the build when the model and the generated
document disagree. Well-structured internals supported by eleven gates.
Reproducible byte-identical builds. A complete implementation representation,
since the item is 100% first-party source.

*Written since this audit began:* [SECURITY-TARGET.md](SECURITY-TARGET.md),
which closes F-21 — TOE scope, assets, seven threats, four assumptions, eight
objectives, SFRs from CC Part 2 (FDP_ACC/ACF, FDP_IFC/IFF, FDP_RIP.2, FMT_MSA,
FPT_FLS/STM/TDC, FRU_RSA), and a summary specification mapping each objective to
the code and the test that exercises it.

*Missing:* a vulnerability analysis to `AVA_VAN.4`'s moderate attack potential
(F-21a) — the largest single gap. Design evidence at module granularity for
`ADV_TDS.3`; the SysML model describes Ferrix, not the TOE (F-15). And the TOE
claims neither audit nor authentication (F-21b), which is defensible for an
isolation kernel and is why no Protection Profile is claimed.

*Nearest credible claim:* EAL4+ looks defensible on this evidence with an ST
written, which is also where RHEL and SUSE sit. EAL5 needs the design
decomposed to the item's modules.

### DO-178C DAL C — not met

62 objectives, 5 requiring independence.

*In place:* statement coverage is now measurable and measured (F-10 at 71.4%),
which was the objective everyone assumes is impossible for a kernel. 303 lines
of assembly across 19 allow-listed sites makes the source-to-object question
tractable. Zero Cargo features in the item means no configuration space to
enumerate. Deactivated code is confined to three architectures behind `cfg`.

*Missing:* every planning document — PSAC, SDP, SVP, SCMP, SQAP — none drafted.
Low-level requirements (F-15) and requirements-to-test traceability (F-14),
which are the spine of the standard. Tool qualification (F-17 to F-19).

*Honest gap:* this is the furthest of the four, because DO-178C wants a
document set that does not exist rather than a property the code lacks.

### IEC 62304 Class C — closest

*In place:* **zero SOUP in the item**, measured across all three architectures
and the loader, and now gated. This is the obligation that dominates a Class C
submission built on Linux — where the kernel is always SOUP, because §5
lifecycle evidence cannot be produced retroactively — and here it is empty.
§4.3(c) segregation is satisfied architecturally: ring-3 drivers behind an
IOMMU, with the boundary enforced at build time.

*Missing:* ISO 14971 risk management and a hazard analysis (F-20, blocking);
ISO 13485 QMS (F-28, blocking); §5.4 detailed design to unit level and §5.5.3
unit verification acceptance criteria (F-15).

*Why it is closest:* the blockers are an organisation and a document set. The
technical position — first-party code, gated boundary, measured coverage,
memory-safe language, no SOUP — is unusually strong, and none of it has to be
retrofitted.

### EN 50716:2023 SIL 2 — reachable

*In place:* most of the Annex A technique table. Strongly typed language (HR) —
Rust. Defensive programming (HR) — the panic audit plus a lint table denying
`unwrap`, `expect`, `panic`, indexing and slicing in production code. Static
analysis (HR) — clippy at ten configurations, Miri over 13 crates, 30 fuzz
targets with committed corpora. Structured methodology (HR) — the SysML model.
Role independence is permissive at SIL 2, where roles may be combined with
justification, so F-27 is survivable here as it is not elsewhere.

*Missing:* a safety case under EN 50126/50129 (F-22, blocking). Tool
classification to §6.7 (F-18, F-19). A coding standard with complexity metrics
(F-25). Dynamic memory is discouraged at SIL 2 and is pervasive (F-23).

## 4. What would actually move the needle

In order of value per unit of effort:

1. **Run a vulnerability analysis against the ST's threat model (F-21a).** The
   Security Target now states seven threats; nothing has systematically tried
   to realise them. It is the last EAL5 gap that is engineering rather than
   paperwork.
2. **Extract `Process` into the core (F-01).** Twelve of the 62 boundary
   violations are one misplaced type, and it is the most-cited structural
   defect in the register.
3. **Trace tests to requirements (F-14, F-15).** The boot gates already assert
   rich properties; they need requirement ids attached and low-level
   requirements to attach them to.
4. **Adopt Ferrocene (F-17).** A qualified toolchain is the difference between
   "written in a memory-safe language" as a talking point and as evidence.
5. **Raise coverage past 71.4% and measure the release profile on all three
   architectures (F-10 to F-12).** The tooling exists; nobody has run it.

## 5. What cannot be fixed from here

A hazard analysis needs an application — hazards belong to a device or a train,
not to a kernel (F-20). Independence needs people who do not work on the code
(F-27). A QMS needs an organisation (F-28). Field history needs years (F-30).

And one genuinely unsettled question, worth raising with a certification body
early rather than at assessment: **no scheme has decided how to treat
AI-authored code in a certified item** (F-29). Every commit in this repository
has that provenance.
