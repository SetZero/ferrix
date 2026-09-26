#!/usr/bin/env python3
"""Sort the uncovered statements into the categories DO-178C asks about.

Table A-7 wants every statement either exercised by a requirements-based test
or justified as unreachable. `scripts/coverage-report.py --residual` produces
the list, one per architecture; this sorts it, because a list of line numbers
is a number and not an argument.

The categories, in the order the analysis applies them:

  other-architecture   Code for an architecture or a board the measured run
                       was not. `iommu/smmuv3.rs` is AArch64's IOMMU and no
                       x86-64 run can reach it. **Legitimately unreachable,
                       per configuration** -- and the same statements are
                       ordinary covered code on the architecture that owns
                       them, which is why the justification has to be made per
                       configuration rather than once.

  failure-path         Reached only when the kernel is stopping: the panic
                       report, the catalogue, the backtrace walker.
                       Legitimately unreachable in a passing run, and testing
                       it means deliberately crashing.

  absent-hardware      Enumeration and setup for devices the measured machine
                       does not have. Unreachable *on this machine*, which is
                       weaker than the first category: a different QEMU
                       invocation would reach some of it.

  needs-a-test         Everything else. This is the real work, and it is the
                       number that has to reach zero.

The first two are arguments. The third is a configuration statement. Only the
fourth is a gap, and separating them is the whole point: a single percentage
cannot be argued with and these four numbers can.

Two documents come out:

  COVERAGE-RESIDUAL.md  the four categories on each architecture.
  COVERAGE-WORKLIST.md  the fourth category alone, grouped by module, with
                        each file's count on every architecture: the list
                        test-writing starts from, one module at a time.

    python3 scripts/gen-coverage-justification.py
    python3 scripts/gen-coverage-justification.py --check
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CERT = ROOT / "docs" / "certification"
OUTPUT = CERT / "COVERAGE-RESIDUAL.md"
WORKLIST = CERT / "COVERAGE-WORKLIST.md"

ARCHES = ("x86_64", "aarch64", "armv7a")


def residual_path(arch: str) -> Path:
    return CERT / f"coverage-residual-{arch}.json"


def coverage_path(arch: str) -> Path:
    return CERT / f"coverage-{arch}.json"


# Files belonging to an architecture other than the measured one, or to a
# board the measured machine is not. Only what the build compiles for the
# measured architecture can appear in its residual, so each list names the
# generic-path code that belongs elsewhere: another architecture's IOMMU, a
# board's UART, the Pixel 7's SoC.
OTHER_ARCH_MARKERS = {
    "x86_64": (
        "arch/aarch64/",
        "arch/armv7a/",
        "arch/gicv2.rs",
        "arch/pl011.rs",
        "arch/stm32_usart.rs",
        "iommu/smmuv3.rs",
        "stm32mp1",
    ),
    "aarch64": (
        "arch/x86_64/",
        "arch/armv7a/",
        "arch/stm32_usart.rs",
        "arch/aarch64/gs201.rs",
        "iommu/vtd.rs",
        "stm32mp1",
    ),
    "armv7a": (
        "arch/x86_64/",
        "arch/aarch64/",
        "arch/stm32_usart.rs",
        "iommu/vtd.rs",
        "stm32mp1",
    ),
}
FAILURE_PATH = ("panic.rs", "panic/", "backtrace.rs")
ABSENT_HARDWARE_COMMON = ("device.rs", "pci.rs", "pci/", "acpi.rs", "fdt.rs")
# Per architecture, the unit QEMU's machine could present and this one does
# not: x86-64's VT-d is attached but its fault paths are not provoked, and
# ARMv7-A's `virt` is given no SMMU at all. AArch64 has none: the suite boots
# its `virt` with the default GICv2 and once more with a GICv3 and its ITS.
ABSENT_HARDWARE_ARCH = {
    "x86_64": ("iommu/vtd.rs",),
    "aarch64": (),
    "armv7a": ("iommu/smmuv3.rs",),
}


def categorise(path: str, arch: str) -> str:
    if any(marker in path for marker in OTHER_ARCH_MARKERS[arch]):
        return "other-architecture"
    if any(path.endswith(m) or m in path for m in FAILURE_PATH):
        return "failure-path"
    absent = ABSENT_HARDWARE_COMMON + ABSENT_HARDWARE_ARCH[arch]
    if any(path.endswith(m) or path.startswith(m) for m in absent):
        return "absent-hardware"
    return "needs-a-test"


HEADINGS = {
    "other-architecture": (
        "Unreachable on the measured architecture",
        "Justified. These statements belong to another architecture or another "
        "board, and no run on {arch} can reach them. The same code is ordinary "
        "covered code where it belongs, so this justification is per "
        "configuration and the other architectures owe their own.",
    ),
    "failure-path": (
        "Reached only when the kernel is stopping",
        "Justified. The panic report, its catalogue and the backtrace walker "
        "run when the kernel has already decided to stop. Exercising them means "
        "crashing deliberately, which only `test-shell`'s `ferrix.onexit=panic` "
        "boot does -- and a passing run that reached the rest would be a "
        "failing run.",
    ),
    "absent-hardware": (
        "Hardware the measured machine does not have",
        "**Not a justification, a configuration statement.** Enumeration and "
        "setup for devices this QEMU invocation does not present. A different "
        "machine would reach some of it, so the honest closure is either to "
        "measure on a machine that has the hardware or to state which devices "
        "the claim excludes.",
    ),
    "needs-a-test": (
        "Needs a test",
        "**The real gap.** No argument covers these; they are reachable on the "
        "measured configuration and nothing exercised them. This is the number "
        "that has to reach zero for DO-178C table A-7 objective 5. "
        "[COVERAGE-WORKLIST.md](COVERAGE-WORKLIST.md) groups them by module.",
    ),
}

ARGUED = ("other-architecture", "failure-path")


def buckets_of(residual: dict, arch: str) -> dict[str, dict[str, int]]:
    buckets: dict[str, dict[str, int]] = {name: {} for name in HEADINGS}
    for path, entry in residual["files"].items():
        buckets[categorise(path, arch)][path] = len(entry["lines"])
    return buckets


def render(residuals: dict[str, dict]) -> str:
    lines = [
        "# Coverage residual",
        "",
        "*Generated by `scripts/gen-coverage-justification.py`. Do not edit.*",
        "",
        "The statements in the certified item that the measured suite did not "
        "reach, on each architecture, sorted into the categories DO-178C table "
        "A-7 asks about. The suite is every boot gate `cargo xtask coverage` "
        "runs on that architecture; see VERIFICATION.md §3.",
        "",
        "| Architecture | Profile | Unreached | Argued | Hardware absent | Needs a test |",
        "|---|---|---:|---:|---:|---:|",
    ]
    for arch, residual in residuals.items():
        buckets = buckets_of(residual, arch)
        argued = sum(sum(buckets[n].values()) for n in ARGUED)
        absent = sum(buckets["absent-hardware"].values())
        gap = sum(buckets["needs-a-test"].values())
        lines.append(
            f"| {arch} | {residual['profile']} | {residual['unreached']} | "
            f"{argued} | {absent} | **{gap}** |"
        )
    lines += [
        "",
        "*Argued* is the first two categories below; *hardware absent* is a "
        "statement about which machine was measured rather than an argument; "
        "*needs a test* is the gap.",
        "",
    ]

    for arch, residual in residuals.items():
        buckets = buckets_of(residual, arch)
        total = residual["unreached"]
        lines += [
            "---",
            "",
            f"## {arch}",
            "",
            f"**{total}** unreached statements, {residual['profile']} profile.",
            "",
            "| Category | Statements | Share |",
            "|---|---:|---:|",
        ]
        for name in HEADINGS:
            count = sum(buckets[name].values())
            lines.append(
                f"| {HEADINGS[name][0]} | {count} | "
                f"{100.0 * count / (total or 1):.0f}% |"
            )
        lines.append("")

        for name, (heading, rationale) in HEADINGS.items():
            entries = sorted(buckets[name].items(), key=lambda kv: (-kv[1], kv[0]))
            count = sum(buckets[name].values())
            lines += [
                f"### {arch}: {heading} — {count} statements",
                "",
                rationale.format(arch=arch),
                "",
            ]
            if not entries:
                lines += ["None.", ""]
                continue
            lines += ["| Statements | Ring | File |", "|---:|---|---|"]
            for path, n in entries[:25]:
                ring = residual["files"][path]["ring"]
                lines.append(f"| {n} | `{ring}` | `{path}` |")
            if len(entries) > 25:
                rest = sum(n for _, n in entries[25:])
                lines.append(f"| {rest} | | *and {len(entries) - 25} more files* |")
            lines.append("")

    lines += [
        "---",
        "",
        "## What this does not do",
        "",
        "It sorts by file, not by statement. A file in *needs-a-test* may hold "
        "individual lines that are genuinely unreachable -- a defensive `else` "
        "on an invariant the type system already forces -- and a file in a "
        "justified category may hold a line that is not. Closing F-10 means "
        "walking the fourth category line by line; this says which lines to "
        "walk and which not to bother with, which is the part a percentage "
        "could not.",
        "",
    ]
    return "\n".join(lines) + "\n"


def module_of(path: str) -> str:
    """The module a file belongs to: `iommu.rs` and `iommu/vtd.rs` are both
    `iommu`, `arch/x86_64/cpu.rs` is `arch/x86_64`, and a file of its own is
    its own module."""
    parts = path.split("/")
    if parts[0] == "arch":
        return "/".join(parts[:2]) if len(parts) > 2 else "arch"
    return parts[0].removesuffix(".rs")


def ranges(numbers: list[int]) -> str:
    """`[1, 2, 3, 7]` as `1-3, 7`."""
    out: list[str] = []
    numbers = sorted(numbers)
    start = previous = None
    for n in numbers:
        if start is None:
            start = previous = n
        elif n == previous + 1:
            previous = n
        else:
            out.append(f"{start}" if start == previous else f"{start}-{previous}")
            start = previous = n
    if start is not None:
        out.append(f"{start}" if start == previous else f"{start}-{previous}")
    return ", ".join(out)


def render_worklist(residuals: dict[str, dict], coverage: dict[str, dict]) -> str:
    # {file: {arch: [lines]}} for the needs-a-test category only, with an
    # empty list where the architecture compiles the file, counts it as a gap
    # and reached all of it: that architecture has nothing left to close, and
    # a statement it reached is not unreached "everywhere".
    gap: dict[str, dict[str, list[int]]] = {}
    rings: dict[str, str] = {}
    for arch, measured in coverage.items():
        unreached = residuals[arch]["files"]
        for path, entry in measured["files"].items():
            if entry["ring"] not in ("core", "item"):
                continue
            if categorise(path, arch) != "needs-a-test":
                continue
            gap.setdefault(path, {})[arch] = unreached.get(path, {}).get("lines", [])
            rings[path] = entry["ring"]
    gap = {path: arches for path, arches in gap.items() if any(arches.values())}

    modules: dict[str, list[str]] = {}
    for path in gap:
        modules.setdefault(module_of(path), []).append(path)

    def everywhere(path: str) -> set[int]:
        """Lines unreached on every architecture that counts the file a gap."""
        sets = [set(v) for v in gap[path].values()]
        return set.intersection(*sets) if sets else set()

    def module_total(module: str, arch: str) -> int:
        return sum(len(gap[p].get(arch, [])) for p in modules[module])

    totals = {arch: sum(module_total(m, arch) for m in modules) for arch in residuals}
    order = sorted(
        modules,
        key=lambda m: (-max(module_total(m, a) for a in residuals), m),
    )

    head = " | ".join(residuals)
    rule = "|".join("---:" for _ in residuals)
    lines = [
        "# Coverage worklist",
        "",
        "*Generated by `scripts/gen-coverage-justification.py`. Do not edit.*",
        "",
        "The *needs a test* category of [COVERAGE-RESIDUAL.md](COVERAGE-RESIDUAL.md), "
        "grouped by module so that each module can be taken as one piece of "
        "work. The argued categories -- another architecture's code, the "
        "failure path, hardware the machine does not present -- are not here; "
        "their argument is in COVERAGE-RESIDUAL.md.",
        "",
        "Counts are statements unreached by the whole suite on that "
        "architecture. A dash is a file the architecture does not compile, or "
        "one whose residual there is argued rather than a gap; a zero is one "
        "it reached in full. *Everywhere* is the statements unreached on every "
        "architecture that counts the file a gap -- the ones a single generic "
        "test would close on all of them at once -- and its lines are listed, "
        "so a test can be aimed without re-running anything. The "
        "per-architecture lines are in `coverage-residual-<arch>.json`.",
        "",
        "**Taking a module.** Write the test on the architecture with the "
        "largest count first, re-run `cargo xtask coverage --arch <arch>`, "
        "regenerate the evidence (VERIFICATION.md §3.3) and raise the floor in "
        "`coverage-floor.json`. A line that turns out to be unreachable "
        "defensive code needs its argument written, not a test.",
        "",
        f"| Module | {head} | Files |",
        f"|---|{rule}|---:|",
    ]
    for module in order:
        counts = " | ".join(str(module_total(module, a) or "-") for a in residuals)
        lines.append(f"| [`{module}`](#{anchor(module)}) | {counts} | {len(modules[module])} |")
    total_row = " | ".join(f"**{totals[a]}**" for a in residuals)
    lines += [f"| **Total** | {total_row} | {len(gap)} |", ""]

    for module in order:
        lines += [
            "---",
            "",
            f"## `{module}`",
            "",
            f"| File | Ring | {head} | Everywhere | Lines unreached everywhere |",
            f"|---|---|{rule}|---:|---|",
        ]
        files = sorted(
            modules[module],
            key=lambda p: (-max(len(gap[p].get(a, [])) for a in residuals), p),
        )
        for path in files:
            counts = " | ".join(
                str(len(gap[path][a])) if a in gap[path] else "-" for a in residuals
            )
            common = everywhere(path)
            lines.append(
                f"| `{path}` | `{rings[path]}` | {counts} | {len(common)} | "
                f"{ranges(list(common)) or '-'} |"
            )
        lines.append("")
    return "\n".join(lines) + "\n"


def anchor(module: str) -> str:
    """The anchor GitHub gives a `## \\`module\\`` heading."""
    return "".join(c for c in module.lower() if c.isalnum() or c in "-_")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    residuals: dict[str, dict] = {}
    coverage: dict[str, dict] = {}
    for arch in ARCHES:
        for path, into in ((residual_path(arch), residuals), (coverage_path(arch), coverage)):
            if not path.is_file():
                print(
                    f"gen-coverage-justification: {path.relative_to(ROOT)} is missing.\n"
                    f"  Produce it with `coverage-report.py --json --residual`.",
                    file=sys.stderr,
                )
                return 1
            into[arch] = json.loads(path.read_text(encoding="utf-8"))

    outputs = {
        OUTPUT: render(residuals),
        WORKLIST: render_worklist(residuals, coverage),
    }

    if args.check:
        stale = [
            path
            for path, rendered in outputs.items()
            if not path.exists() or path.read_text(encoding="utf-8") != rendered
        ]
        for path in stale:
            print(
                "gen-coverage-justification: "
                f"{path.relative_to(ROOT)} is stale. Run the generator.",
                file=sys.stderr,
            )
        if stale:
            return 1
        print("gen-coverage-justification: current")
        return 0

    for path, rendered in outputs.items():
        path.write_text(rendered, encoding="utf-8")
        print(f"gen-coverage-justification: wrote {path.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
