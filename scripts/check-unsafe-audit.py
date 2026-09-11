#!/usr/bin/env python3
"""Assert that every `unsafe` in the tree carries a justification.

This is the gate that stands in for `unsafe_code = "deny"`, which the Starling
workspace this policy is ported from can hold and a kernel cannot: writing a
page table entry, storing to an MMIO register or moving a CPU system register
*is* the program here. So unsafe is not forbidden, it is made expensive --
every block has to say why it is sound, at the block.

Three rules:

  1. Every `unsafe { ... }` block is preceded by a `// SAFETY:` comment.
  2. Every `unsafe impl` is preceded by a `// SAFETY:` comment.
  3. Every `unsafe fn` has a `/// # Safety` section in its own doc comment,
     stating the contract a caller has to meet -- except one implementing a
     trait method, whose contract belongs to the trait. Restating
     `GlobalAlloc::alloc`'s requirements on our impl of it would add noise and
     invite the copy to drift from the original; the `unsafe impl` line still
     needs its `// SAFETY:` comment, and that is where the claim that this type
     upholds the contract belongs.

Clippy's `undocumented_unsafe_blocks` and `missing_safety_doc` enforce (1) and
(3) as well, and the workspace denies both. This script exists anyway because
those two are nursery/pedantic lints: a clippy release that softens or renames
one would silently retire the rule, and nobody would notice until an audit.
A script in CI cannot be softened by someone else's release.

The report also prints the unsafe-block count per crate. That number is meant
to be looked at in a diff: unsafe growing is not a failure, but it should never
grow without somebody noticing.

Usage:  python3 scripts/check-unsafe-audit.py
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOTS = ("kernel", "boot", "libs", "xtask")

# `unsafe {` opening a block, but not `unsafe fn`, `unsafe impl`, `unsafe trait`
# or `unsafe extern`. Also matches the `unsafe` in `unsafe { ... }` used as an
# expression on the right of `=`.
UNSAFE_BLOCK = re.compile(r"(?<![\w:])unsafe\s*\{")
UNSAFE_IMPL = re.compile(r"(?<![\w:])unsafe\s+impl\b")
UNSAFE_FN = re.compile(r"(?<![\w:])(?:pub(?:\([^)]*\))?\s+)?unsafe\s+fn\s+(\w+)")
# `impl Trait for Type {`, whose methods implement someone else's contract.
TRAIT_IMPL = re.compile(r"^\s*(?:unsafe\s+)?impl\s*(?:<[^>]*>)?\s+[^;{]*\bfor\b[^;{]*\{")
SAFETY_COMMENT = re.compile(r"//\s*SAFETY:", re.IGNORECASE)
SAFETY_DOC = re.compile(r"///\s*#+\s*Safety\b", re.IGNORECASE)
# Lines that may sit between a SAFETY comment and the thing it covers:
# attributes, comments, blank lines and closing delimiters.
INTERVENING = re.compile(r"^\s*(#\[|#!\[|\)|\}|//|$)")
# How far back to look. A SAFETY comment more than a few lines from what it
# covers is not documenting it any more.
LOOKBACK = 8


def balanced_lines(lines: list[str], start: int) -> int:
    """Index of the line closing the block opened on line `start`."""
    depth = 0
    for index in range(start, len(lines)):
        line = lines[index]
        depth += line.count("{") - line.count("}")
        if index > start or "{" in line:
            if depth <= 0:
                return index
    return len(lines) - 1


def trait_impl_spans(lines: list[str]) -> list[tuple[int, int]]:
    """Line ranges covered by `impl Trait for Type { ... }`."""
    spans = []
    for index, line in enumerate(lines):
        if TRAIT_IMPL.match(line):
            spans.append((index, balanced_lines(lines, index)))
    return spans


def continuation(line: str) -> bool:
    """True if `line` is the middle of a statement rather than a whole one.

    rustfmt wraps `let x = unsafe { ... }` so that the binding and the `unsafe`
    land on different lines, which puts the SAFETY comment above the
    *statement* rather than above the block. Clippy accepts that shape --
    `accept-comment-above-statement` is on by default -- so this must too, or
    the script is stricter than the lint it exists to backstop, and the fix
    would be to un-format the code.
    """
    stripped = line.strip()
    return bool(stripped) and not stripped.endswith((";", "{", "}"))


def preceded_by_safety(lines: list[str], index: int) -> bool:
    """True if a `// SAFETY:` comment covers the construct on line `index`."""
    # `let x = /* SAFETY: ... */ unsafe { ... }` is unusual, but a trailing
    # `// SAFETY:` on the same line is a shape people write.
    if SAFETY_COMMENT.search(lines[index]):
        return True

    for scan in range(index - 1, max(index - 1 - LOOKBACK, -1), -1):
        line = lines[scan]
        if SAFETY_COMMENT.search(line):
            return True
        # Comments, attributes and blank lines are always crossable; a partial
        # statement is crossable because the comment above it covers the whole
        # statement. A completed statement is where the search stops.
        if not INTERVENING.match(line) and not continuation(line):
            return False
    return False


def doc_has_safety_section(lines: list[str], index: int) -> bool:
    """True if the doc comment above line `index` has a `# Safety` section."""
    scan = index - 1
    saw_doc = False
    while scan >= 0:
        line = lines[scan].strip()
        if line.startswith("///"):
            saw_doc = True
            if SAFETY_DOC.search(lines[scan]):
                return True
        elif line.startswith("#[") or line.startswith("#!["):
            pass
        elif line == "" and not saw_doc:
            pass
        else:
            return False
        scan -= 1
    return False


def check(path: pathlib.Path) -> tuple[list[str], int]:
    source = path.read_text(encoding="utf-8")
    if "unsafe" not in source:
        return [], 0
    lines = source.splitlines()
    problems: list[str] = []
    blocks = 0
    impls = trait_impl_spans(lines)

    for index, line in enumerate(lines):
        stripped = line.strip()
        # A `//` comment line is not code; `///` doc text mentioning unsafe is
        # not code either.
        if stripped.startswith("//"):
            continue

        if UNSAFE_BLOCK.search(line):
            blocks += 1
            if not preceded_by_safety(lines, index):
                problems.append(f"{path}:{index + 1}: unsafe block with no `// SAFETY:` comment")

        if UNSAFE_IMPL.search(line):
            if not preceded_by_safety(lines, index):
                problems.append(f"{path}:{index + 1}: unsafe impl with no `// SAFETY:` comment")

        match = UNSAFE_FN.search(line)
        in_trait_impl = any(begin <= index <= end for begin, end in impls)
        if match and not in_trait_impl and not doc_has_safety_section(lines, index):
            problems.append(
                f"{path}:{index + 1}: `unsafe fn {match.group(1)}` "
                "has no `/// # Safety` section"
            )

    return problems, blocks


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    problems: list[str] = []
    per_crate: dict[str, int] = {}

    for name in ROOTS:
        directory = root / name
        if not directory.is_dir():
            continue
        for path in sorted(directory.rglob("*.rs")):
            if "target" in path.parts:
                continue
            found, blocks = check(path)
            problems.extend(found)
            if blocks:
                crate = str(path.relative_to(root).parent).split("src")[0].rstrip("/\\")
                per_crate[crate] = per_crate.get(crate, 0) + blocks

    if problems:
        for problem in problems:
            print(problem.replace(str(root) + "\\", "").replace(str(root) + "/", ""), file=sys.stderr)
        print(file=sys.stderr)
        print("Every unsafe construct must state why it is sound, at the site:", file=sys.stderr)
        print(file=sys.stderr)
        print("    // SAFETY: `phys` came from the frame allocator, so it is", file=sys.stderr)
        print("    // inside the physmap and 4 KiB aligned.", file=sys.stderr)
        print("    unsafe { core::ptr::write_bytes(virt, 0, PAGE_SIZE) };", file=sys.stderr)
        print(file=sys.stderr)
        print("See docs/RELIABILITY.md.", file=sys.stderr)
        return 1

    total = sum(per_crate.values())
    print(f"unsafe-audit: {total} unsafe block(s), all documented")
    for crate in sorted(per_crate):
        print(f"    {per_crate[crate]:5}  {crate}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
