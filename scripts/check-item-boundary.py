#!/usr/bin/env python3
"""Hold the boundary of the certified item.

`scripts/certification-item.json` says which kernel files are inside the thing
four assurance ratings attach to. That file is the scope of every artifact in
`docs/certification`: the Security Target's TOE, the hazard analysis's safety
item, the traceability matrix, the coverage obligation. A boundary that lives
only in a document is a boundary that has already moved.

So this gate asserts three things.

  1. **Every kernel source file is classified.** A file in no ring fails the
     build. Without this the item grows by accretion -- somebody writes
     `kernel/src/thing.rs`, nobody decides whether it is trusted, and the
     answer defaults to whatever the reader assumes. A new file should cost
     one line of JSON and the thought that goes with it.

  2. **Rings do not reach upward.** The core may not name the item, and
     neither may name the uncertified load. This is the claim that makes a
     small item meaningful: if the trusted core calls into the filesystem,
     then the filesystem is in the trusted core no matter what a document
     says. This is the check that can actually fail, and the one worth having.

  3. **Known violations do not grow.** Today's breaches are listed in the
     manifest with finding ids. The list may shrink without ceremony; it may
     not grow without editing this file's input, which is a diff somebody has
     to argue for. Stale entries fail too, so a fixed violation cannot leave
     a permanent exemption behind.

Product code and in-kernel test code are counted apart. A standard asks
different questions of each, and conflating them flatters the item's size
while hiding how much verification exists.

Run it directly, or as part of `cargo xtask check`:

    python3 scripts/check-item-boundary.py
    python3 scripts/check-item-boundary.py --report   # sizes, no pass/fail
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "scripts" / "certification-item.json"
KERNEL_SRC = ROOT / "kernel" / "src"

# Which module does this file name? The whole path matters, not its first
# segment: `crate::syscall::registry` is the dispatcher and `crate::syscall::
# sockets` is the Linux personality, they sit in different rings, and a check
# that cannot tell them apart reports mostly noise.
#
# Comments and strings are stripped first -- a doc comment that mentions
# `crate::fs` is prose about the design, not a dependency.
CRATE_REF = re.compile(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)")
# `use crate::{a::b, c}` is the same set of edges written shorter. Expanded
# before matching rather than parsed, which one level of nesting handles and
# deeper nesting does not -- so the gate also refuses to be silently wrong
# about that, below.
CRATE_BRACE = re.compile(r"\bcrate::\{([^{}]*)\}")
LINE_COMMENT = re.compile(r"//.*$", re.MULTILINE)
BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.DOTALL)
STRING_LIT = re.compile(r'"(?:[^"\\]|\\.)*"')


def load_manifest() -> dict:
    with MANIFEST.open(encoding="utf-8") as handle:
        return json.load(handle)


def kernel_files() -> list[str]:
    """Every kernel source file, as a path relative to kernel/src."""
    return sorted(
        str(path.relative_to(KERNEL_SRC)) for path in KERNEL_SRC.rglob("*.rs")
    )


def matches(pattern: str, rel: str) -> bool:
    """`arch/**` matches anything under arch/; `mm.rs` matches exactly."""
    if pattern.endswith("/**"):
        return rel.startswith(pattern[:-2])
    return fnmatch.fnmatch(rel, pattern)


def is_test_file(rel: str, manifest: dict) -> bool:
    name = rel.rsplit("/", 1)[-1]
    for pattern in manifest["test_file_patterns"]:
        if "/" in pattern:
            if matches(pattern, rel):
                return True
        elif fnmatch.fnmatch(name, pattern):
            return True
    return False


def classify(manifest: dict, files: list[str]) -> tuple[dict[str, str], list[str], list[tuple[str, list[str]]]]:
    """Map each file to its ring. Returns (ring_of, unclassified, ambiguous)."""
    ring_of: dict[str, str] = {}
    unclassified: list[str] = []
    ambiguous: list[tuple[str, list[str]]] = []

    for rel in files:
        hits = [
            name
            for name, ring in manifest["rings"].items()
            if any(matches(p, rel) for p in ring["members"])
        ]
        if not hits:
            unclassified.append(rel)
        elif len(hits) > 1:
            ambiguous.append((rel, hits))
        else:
            ring_of[rel] = hits[0]

    return ring_of, unclassified, ambiguous


def module_ring(path: str, ring_of: dict[str, str]) -> tuple[str | None, str]:
    """Which ring does the module named by `crate::<path>` belong to?

    Resolved longest-prefix-first, because that is how the reference resolves:
    `syscall::registry::dispatch` is a function in `syscall/registry.rs`, and
    reporting it against the `syscall/` directory would lump the dispatcher in
    with the Linux personality that shares its parent.

    A prefix may land on a file (`a/b.rs`, `a/b/mod.rs`) or on a directory.
    A directory whose files sit in different rings resolves to the strictest
    one present, since that is the rule that would break.

    Returns the ring and the module path actually resolved, so a finding can
    name what it found rather than what was searched for.
    """
    order = ["load", "item", "core"]
    segments = path.split("::")

    for end in range(len(segments), 0, -1):
        prefix = "/".join(segments[:end])

        for candidate in (f"{prefix}.rs", f"{prefix}/mod.rs"):
            if candidate in ring_of:
                return ring_of[candidate], prefix

        candidates = {
            ring for rel, ring in ring_of.items() if rel.startswith(f"{prefix}/")
        }
        if candidates:
            for ring in order:
                if ring in candidates:
                    return ring, prefix

    return None, path


def source_without_comments(path: Path) -> str:
    text = path.read_text(encoding="utf-8", errors="replace")
    text = BLOCK_COMMENT.sub(" ", text)
    text = LINE_COMMENT.sub(" ", text)
    text = STRING_LIT.sub('""', text)

    # `use crate::{sched::Task, fs::File}` -> `crate::sched::Task crate::fs::File`
    def expand(match: re.Match[str]) -> str:
        inner = match.group(1)
        return " ".join(f"crate::{part.strip()}" for part in inner.split(",") if part.strip())

    return CRATE_BRACE.sub(expand, text)


def find_violations(manifest: dict, ring_of: dict[str, str]) -> list[dict]:
    """Every reference that breaks a dependency rule, one entry per edge."""
    forbidden = {
        rule["from"]: set(rule["may_not_reference"])
        for rule in manifest["dependency_rules"]
    }
    violations: list[dict] = []

    for rel, ring in sorted(ring_of.items()):
        if ring not in forbidden:
            continue
        if is_test_file(rel, manifest):
            # In-kernel tests drive the thing they test from inside the kernel;
            # a core test that reaches the filesystem to build a fixture is not
            # the core depending on the filesystem. Counted in VERIFICATION.md
            # instead, where the question is what the test reaches.
            continue

        text = source_without_comments(KERNEL_SRC / rel)
        seen: set[str] = set()
        for match in CRATE_REF.finditer(text):
            target_ring, resolved = module_ring(match.group(1), ring_of)
            if resolved in seen:
                continue
            seen.add(resolved)
            if target_ring in forbidden[ring]:
                violations.append(
                    {
                        "file": rel,
                        "from_ring": ring,
                        "references": resolved.replace("/", "::"),
                        "to_ring": target_ring,
                    }
                )

    return violations


def unexpanded_braces() -> list[str]:
    """Files whose `crate::{...}` nests deeper than one level.

    `source_without_comments` expands one level. Nesting deeper would leave
    edges unseen, and an edge this gate cannot see is an edge it would report
    as absent -- so it says so instead of quietly under-reporting.
    """
    offenders = []
    for path in sorted(KERNEL_SRC.rglob("*.rs")):
        text = BLOCK_COMMENT.sub(" ", path.read_text(encoding="utf-8", errors="replace"))
        text = LINE_COMMENT.sub(" ", text)
        for match in re.finditer(r"\bcrate::\{", text):
            depth, index = 0, match.end() - 1
            while index < len(text):
                if text[index] == "{":
                    depth += 1
                elif text[index] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                index += 1
            if "{" in text[match.end() : index]:
                offenders.append(str(path.relative_to(KERNEL_SRC)))
                break
    return offenders


def line_counts(manifest: dict, ring_of: dict[str, str]) -> dict[str, dict[str, int]]:
    counts: dict[str, dict[str, int]] = defaultdict(lambda: {"product": 0, "test": 0})
    for rel, ring in ring_of.items():
        lines = sum(
            1 for _ in (KERNEL_SRC / rel).open(encoding="utf-8", errors="replace")
        )
        kind = "test" if is_test_file(rel, manifest) else "product"
        counts[ring][kind] += lines
    return counts


def report(manifest: dict, ring_of: dict[str, str], violations: list[dict]) -> None:
    counts = line_counts(manifest, ring_of)
    print("item-boundary: sizes, in lines")
    total_product = total_test = 0
    for ring in ("core", "item", "load"):
        product = counts[ring]["product"]
        test = counts[ring]["test"]
        total_product += product
        total_test += test
        print(f"  {ring:<6} {product:>7} product  {test:>7} test")

    # The item is the core plus its own ring: nested, not disjoint.
    certified = counts["core"]["product"] + counts["item"]["product"]
    print(f"  {'':<6} {'-' * 7}")
    print(f"  certified item (core+item): {certified} lines of product code")
    print(f"  uncertified load:           {counts['load']['product']} lines")
    print(f"  in-kernel verification:     {total_test} lines")
    share = 100.0 * certified / (total_product or 1)
    print(f"  the item is {share:.1f}% of the kernel's product code")

    if violations:
        by_file: dict[str, list[dict]] = defaultdict(list)
        for violation in violations:
            by_file[violation["file"]].append(violation)
        print(f"\nitem-boundary: {len(violations)} upward reference(s) "
              f"in {len(by_file)} file(s)")
        for rel in sorted(by_file):
            edges = ", ".join(
                f"{v['references']} ({v['to_ring']})" for v in by_file[rel]
            )
            ring = by_file[rel][0]["from_ring"]
            print(f"  {ring:<5} {rel} -> {edges}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--report",
        action="store_true",
        help="print sizes and violations without failing",
    )
    args = parser.parse_args()

    manifest = load_manifest()
    files = kernel_files()
    ring_of, unclassified, ambiguous = classify(manifest, files)

    status = 0

    if unclassified:
        print(
            f"item-boundary: {len(unclassified)} kernel file(s) in no ring.\n"
            f"  Add each to a ring in {MANIFEST.relative_to(ROOT)}, which is a\n"
            f"  decision about whether it is trusted, not a formality:",
            file=sys.stderr,
        )
        for rel in unclassified:
            print(f"    {rel}", file=sys.stderr)
        status = 1

    if ambiguous:
        print(
            f"item-boundary: {len(ambiguous)} file(s) match more than one ring:",
            file=sys.stderr,
        )
        for rel, hits in ambiguous:
            print(f"    {rel}: {', '.join(hits)}", file=sys.stderr)
        status = 1

    nested = unexpanded_braces()
    if nested:
        print(
            f"item-boundary: {len(nested)} file(s) nest `crate::{{...}}` deeper\n"
            f"  than this gate expands, so their edges would go unseen. Flatten\n"
            f"  the import or teach the gate to parse it; do not let it report\n"
            f"  an absence it did not establish:",
            file=sys.stderr,
        )
        for rel in nested:
            print(f"    {rel}", file=sys.stderr)
        status = 1

    violations = find_violations(manifest, ring_of)

    if args.report:
        report(manifest, ring_of, violations)
        return status

    known = {
        (entry["file"], entry["references"])
        for entry in manifest["known_violations"]["entries"]
    }
    found = {(v["file"], v["references"]) for v in violations}

    new = sorted(found - known)
    stale = sorted(known - found)

    if new:
        print(
            f"item-boundary: {len(new)} new upward reference(s). The certified\n"
            f"  item may not depend on the uncertified load above it; a core\n"
            f"  that reaches into a filesystem has put the filesystem in the\n"
            f"  core, whatever docs/certification/ITEM.md says:",
            file=sys.stderr,
        )
        for rel, target in new:
            ring = ring_of[rel]
            print(f"    {ring:<5} {rel} -> crate::{target}", file=sys.stderr)
        status = 1

    if stale:
        print(
            f"item-boundary: {len(stale)} known violation(s) no longer occur.\n"
            f"  Remove them from known_violations so a fixed breach cannot\n"
            f"  leave a permanent exemption behind:",
            file=sys.stderr,
        )
        for rel, target in stale:
            print(f"    {rel} -> crate::{target}", file=sys.stderr)
        status = 1

    counts = line_counts(manifest, ring_of)
    certified = counts["core"]["product"] + counts["item"]["product"]
    print(
        f"item-boundary: {certified} lines of product code in the item "
        f"({counts['core']['product']} of it core), "
        f"{counts['load']['product']} uncertified, "
        f"{len(found)} known upward reference(s)"
    )
    return status


if __name__ == "__main__":
    sys.exit(main())
