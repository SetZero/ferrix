#!/usr/bin/env python3
"""Hold the certified item to fallible allocation (finding F-23).

The kernel's allocator returns null when memory has run out, and every
ordinary `Box`, `Arc`, `Vec` or map turns that null into the allocation error
handler, which stops the machine. So in the certified item -- the `core` and
`item` rings of `scripts/certification-item.json`, product code only -- every
allocation goes through `kernel/src/fallible.rs`, which reports failure as an
error its caller can return. This gate is what keeps that true: it finds calls
to the standard library's allocating APIs in the item's source and fails when
one is not argued.

What it looks for
-----------------

Token patterns in source whose comments and literals `scripts/rustlex.py` has
blanked:

  constructors  `Box::new`, `Arc::new`, `Arc::new_cyclic`, `Rc::new`, their
                `from`, `pin`, `new_uninit` and `new_zeroed`; `with_capacity`,
                `from` and `from_iter` of `Vec`, `String`, `VecDeque` and the
                maps. `Vec::new()` and friends are not flagged: an empty
                collection allocates nothing.
  macros        `vec!`, `format!`.
  methods       every method of `alloc`'s collections that can grow one --
                `push`, `insert`, `extend`, `append`, `resize`, `entry`,
                `collect`, `to_vec`, `to_string`, `sort` (a stable sort
                allocates a buffer), and the rest in `METHODS` below.

What it cannot see, said here because a gate whose blind spots travel
separately is worse than none:

  * **Types.** A method is matched by name. `CpuSet::insert` sets a bit and is
    flagged like `BTreeSet::insert`; such a site carries a `NOALLOC:` comment
    saying why, and the comment is what a reviewer checks.
  * **`.clone()`** of a `Vec`, `String`, `Box` or map allocates; of an `Arc` it
    does not. The two cannot be told apart without types, so `clone` is not
    flagged at all. The item's clones were audited by hand when this gate was
    written (docs/certification/MEMORY-AND-TIMING.md section 1).
  * **Conversions.** `.into()` from a `&str` to a `String`, a slice to a `Vec`,
    and `?` converting into a boxed error allocate invisibly.
  * **Callees.** A call into `libs/` or into the uncertified load that
    allocates is not the item's source and is not scanned here.

Markers
-------

A flagged call is accepted when the line it is on, or the block of `//`
comments directly above that line, says one of:

  `NOALLOC: <why>`       it does not allocate: a type-blind false positive, or
                         a push into capacity reserved fallibly just before.
  `FALLIBLE: <why>`      it may allocate and reports failure: a first-party
                         method named like a standard one, such as the handle
                         table's `insert` and `reserve`.
  `FATAL-ALLOC: <why>`   it allocates, and failure stops the machine by
                         design -- only at boot, before the first program runs,
                         where there is nothing to return an error to.
                         Listed with `--report`, and in MEMORY-AND-TIMING.md.

The ratchet
-----------

Like the other gates over the item: `scripts/fallible-alloc-baseline.json`
records, per file, the flagged calls that carry no marker. A file may improve
freely; a new unmarked call fails the build, and so does an entry that has
shrunk without being re-recorded, so a converted site cannot leave an
allowance behind. The target is an empty baseline.

    python3 scripts/check-fallible-alloc.py
    python3 scripts/check-fallible-alloc.py --report     # every site, and the markers
    python3 scripts/check-fallible-alloc.py --record     # rewrite the baseline
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import rustlex  # noqa: E402  (after the path insert)

ROOT = Path(__file__).resolve().parent.parent
KERNEL_SRC = ROOT / "kernel" / "src"
BASELINE = ROOT / "scripts" / "fallible-alloc-baseline.json"

# The toolkit itself: the one place the infallible APIs are called on
# purpose, each inside a reserved section or behind a fallible reservation.
TOOLKIT = {"fallible.rs"}

CONSTRUCTORS = re.compile(
    r"\b(?:Box|Arc|Rc)\s*::\s*(?:new|new_cyclic|new_uninit|new_zeroed|pin|from|from_iter)\b"
    r"|\b(?:Vec|String|VecDeque|BTreeMap|BTreeSet|BinaryHeap|HashMap|HashSet)\s*::\s*"
    r"(?:with_capacity|from|from_iter)\b"
)
MACROS = re.compile(r"\b(?:vec|format)\s*!")
METHODS = re.compile(
    r"\.\s*(?:push|push_back|push_front|push_str|insert|extend|extend_from_slice"
    r"|extend_from_within|append|resize|resize_with|entry|reserve|reserve_exact"
    r"|collect|to_vec|to_string|to_owned|into_boxed_slice|into_boxed_str"
    r"|shrink_to_fit|shrink_to|sort|sort_by|sort_by_key|sort_by_cached_key"
    r"|concat|join|repeat|split_off|or_insert|or_insert_with|or_default"
    r"|to_ascii_lowercase|to_ascii_uppercase|to_lowercase|to_uppercase"
    r"|into_owned|clone_from|partition|unzip)\s*(?:\(|::)"
)
PATTERNS = (CONSTRUCTORS, MACROS, METHODS)

MARKERS = {"NOALLOC:": "noalloc", "FALLIBLE:": "fallible", "FATAL-ALLOC:": "fatal"}
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")


def without_test_modules(masked: str) -> str:
    """`masked` with every `#[cfg(test)] mod x { ... }` blanked, length kept."""
    out = masked
    for match in TEST_MODULE.finditer(masked):
        depth = 0
        for index in range(match.end() - 1, len(masked)):
            if masked[index] == "{":
                depth += 1
            elif masked[index] == "}":
                depth -= 1
                if depth == 0:
                    span = out[match.start() : index + 1]
                    out = out[: match.start()] + re.sub(r"[^\n]", " ", span) + out[index + 1 :]
                    break
    return out


def marker_for(lines: list[str], line: int) -> str | None:
    """The marker covering 1-based `line`: on it, or in the comments above."""
    candidates = []
    if 0 < line <= len(lines):
        candidates.append(lines[line - 1])
    above = line - 1
    while above >= 1 and lines[above - 1].strip().startswith("//"):
        candidates.append(lines[above - 1])
        above -= 1
    for text in candidates:
        comment = text[text.find("//") :] if "//" in text else ""
        for marker, kind in MARKERS.items():
            if marker in comment:
                return kind
    return None


def scan_source(source: str) -> list[dict]:
    """Every flagged call in `source`, with its line, text and marker."""
    masked = without_test_modules(rustlex.mask(source))
    lines = source.splitlines()
    found = []
    for pattern in PATTERNS:
        for match in pattern.finditer(masked):
            line = masked.count("\n", 0, match.start()) + 1
            found.append(
                {
                    "line": line,
                    "call": re.sub(r"\s+", "", match.group()).rstrip("(:"),
                    "marker": marker_for(lines, line),
                }
            )
    return sorted(found, key=lambda site: site["line"])


def load_gate():
    spec = importlib.util.spec_from_file_location(
        "boundary", ROOT / "scripts" / "check-item-boundary.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def item_sites() -> dict[str, list[dict]]:
    """Flagged calls in every product-code file of the certified item."""
    gate = load_gate()
    manifest = gate.load_manifest()
    ring_of, _, _ = gate.classify(manifest, gate.kernel_files())
    out: dict[str, list[dict]] = {}
    for rel, ring in sorted(ring_of.items()):
        if ring == "load" or gate.is_test_file(rel, manifest) or rel in TOOLKIT:
            continue
        source = (KERNEL_SRC / rel).read_text(encoding="utf-8", errors="replace")
        sites = scan_source(source)
        if sites:
            out[rel] = sites
    return out


def unmarked_counts(sites: dict[str, list[dict]]) -> dict[str, int]:
    counts = {}
    for rel, found in sites.items():
        count = sum(1 for site in found if site["marker"] is None)
        if count:
            counts[rel] = count
    return counts


def report(sites: dict[str, list[dict]]) -> None:
    unmarked = unmarked_counts(sites)
    total = sum(unmarked.values())
    noalloc = sum(1 for found in sites.values() for s in found if s["marker"] == "noalloc")
    fallible = sum(1 for found in sites.values() for s in found if s["marker"] == "fallible")
    fatal = [
        (rel, site) for rel, found in sites.items() for site in found if site["marker"] == "fatal"
    ]
    print(
        f"fallible-alloc: {total} unmarked allocating call(s) in {len(unmarked)} file(s); "
        f"{noalloc} marked NOALLOC, {fallible} FALLIBLE, {len(fatal)} FATAL-ALLOC"
    )
    for rel, count in sorted(unmarked.items(), key=lambda item: -item[1]):
        calls: dict[str, int] = {}
        for site in sites[rel]:
            if site["marker"] is None:
                calls[site["call"]] = calls.get(site["call"], 0) + 1
        detail = ", ".join(f"{call} {n}" for call, n in sorted(calls.items(), key=lambda c: -c[1]))
        print(f"  {count:>4}  {rel}: {detail}")
    if fatal:
        print("  fatal by design, at boot only:")
        for rel, site in fatal:
            print(f"    {rel}:{site['line']}  {site['call']}")


# --- self-test ---------------------------------------------------------------

_SELF_TEST = r"""
fn a() {
    let v = Vec::new();
    v.push(1);
    let s = format!("{}", 1);
    // NOALLOC: a bit set, not a tree.
    cpus.insert(3);
    let b = Box::new(1); // FATAL-ALLOC: boot only
    let text = "Arc::new(x) and v.push(y) in a string";
    // Arc::new in a comment
    let a = crate::fallible::try_arc(1)?;
    let c: Vec<_> = it.collect();
    let d = it.collect::<Vec<_>>();
    let e = alloc::vec![0; 4];
}

#[cfg(test)]
mod tests {
    fn t() { let v = vec![1]; v.push(2); }
}
"""

_SELF_EXPECT = [
    (4, "push", None),
    (5, "format!", None),
    (7, "insert", "noalloc"),
    (8, "Box::new", "fatal"),
    (12, "collect", None),
    (13, "collect", None),
    (14, "vec!", None),
]


def self_test() -> list[str]:
    failures = [f"lexer: {f}" for f in rustlex.self_test()]
    got = [(site["line"], site["call"].lstrip("."), site["marker"]) for site in scan_source(_SELF_TEST)]
    if got != _SELF_EXPECT:
        failures.append(f"scan: got {got}, expected {_SELF_EXPECT}")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--record", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    failures = self_test()
    if failures:
        for failure in failures:
            print(f"fallible-alloc: self-test: {failure}", file=sys.stderr)
        return 1
    if args.self_test:
        print("fallible-alloc: self-test passes")
        return 0

    sites = item_sites()
    if args.report:
        report(sites)
        return 0

    current = unmarked_counts(sites)
    if args.record:
        BASELINE.write_text(
            json.dumps(
                {
                    "//": [
                        "Calls to allocating standard-library APIs in the certified",
                        "item's product code that carry no NOALLOC or FATAL-ALLOC",
                        "marker, per file, as scripts/check-fallible-alloc.py counts",
                        "them. A debt register for finding F-23, not an allowance:",
                        "a count may only fall, and the gate fails when one rises,",
                        "when a new file appears, or when one has fallen without",
                        "being re-recorded. The target is an empty map.",
                    ],
                    "files": dict(sorted(current.items())),
                },
                indent=2,
            )
            + "\n"
        )
        print(f"fallible-alloc: recorded {sum(current.values())} call(s) in {len(current)} file(s)")
        return 0

    if not BASELINE.exists():
        print("fallible-alloc: no baseline; run with --record and commit it.", file=sys.stderr)
        return 1
    baseline = json.loads(BASELINE.read_text())["files"]

    status = 0
    grown = [(rel, baseline.get(rel, 0), now) for rel, now in current.items() if now > baseline.get(rel, 0)]
    shrunk = [(rel, was, current.get(rel, 0)) for rel, was in baseline.items() if current.get(rel, 0) < was]
    if grown:
        print(
            "fallible-alloc: infallible allocation added to the certified item.\n"
            "  Use kernel/src/fallible.rs, or argue the site with NOALLOC: or\n"
            "  FATAL-ALLOC: (see this script's docstring):",
            file=sys.stderr,
        )
        for rel, was, now in grown:
            print(f"    {rel}: {was} -> {now}", file=sys.stderr)
            for site in sites.get(rel, []):
                if site["marker"] is None:
                    print(f"      line {site['line']}: {site['call']}", file=sys.stderr)
        status = 1
    if shrunk:
        print(
            "fallible-alloc: fewer unmarked calls than the baseline records.\n"
            "  Re-record so the converted sites leave no allowance behind:",
            file=sys.stderr,
        )
        for rel, was, now in shrunk:
            print(f"    {rel}: {was} -> {now}", file=sys.stderr)
        status = 1

    fatal = sum(1 for found in sites.values() for s in found if s["marker"] == "fatal")
    print(
        f"fallible-alloc: {sum(current.values())} unmarked allocating call(s) in the item "
        f"({len(current)} file(s)), {fatal} fatal by design"
    )
    return status


if __name__ == "__main__":
    sys.exit(main())
