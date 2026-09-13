#!/usr/bin/env python3
"""Assert that every lint exemption for a panic is an argued one.

The workspace lint table denies `panic!`, `unreachable!`, `expect`, `unwrap`,
`indexing_slicing` and `string_slice` in production code, so the reachable
panic set is empty by construction rather than by inspection. What keeps that
true is not the lint -- it is that the exemptions stay reviewable.

In a kernel this matters more than in a server, not less. There is no
supervisor to restart us: a reachable `unwrap` on a length that firmware, an
ELF header or a filesystem chose is not a 500, it is the machine.

Two rules, both about production code only. Test bodies are exempt from the
lints themselves (`allow-unwrap-in-tests` and friends in `.clippy.toml`), so an
exemption inside a `#[cfg(test)]` module is not making a claim about the kernel
and is skipped here. So is anything under a `tests/` directory: an integration
test is its own crate, which is why clippy's in-test exemptions do not reach it
and why it needs an attribute at all.

  1. Every exemption carries a reason beginning `AUDIT:`, so it reads as an
     argument in the diff that adds it rather than as a way past the lint.

  2. Exemptions use `#[expect]`, never `#[allow]`. `expect` fails the build once
     the lint stops firing, so a site refactored into safety loses its exemption
     instead of accumulating a stale one. An `allow` would sit there forever
     with nobody the wiser.

Usage:  python3 scripts/check-panic-audit.py
"""

from __future__ import annotations

import pathlib
import re
import sys

# The lints whose exemptions have to be argued.
LINTS = (
    "panic",
    "unreachable",
    "expect_used",
    "unwrap_used",
    "indexing_slicing",
    "string_slice",
    "panic_in_result_fn",
)
LINT_RE = re.compile(rf"clippy::({'|'.join(LINTS)})\b")
ATTRIBUTE = re.compile(r"#!?\[(expect|allow)\(")
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*(?:pub\s+)?mod\s+\w+\s*\{")

# The crates whose sources are production code.
ROOTS = ("kernel", "boot", "libs", "user", "xtask")


def balanced(source: str, opening: int, open_ch: str, close_ch: str) -> int:
    """The index of the delimiter closing the one at `opening`."""
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == open_ch:
            depth += 1
        elif source[index] == close_ch:
            depth -= 1
            if depth == 0:
                return index
    return len(source)


def test_spans(source: str) -> list[tuple[int, int]]:
    """Byte ranges covered by `#[cfg(test)] mod ... { }`."""
    spans = []
    for match in TEST_MODULE.finditer(source):
        brace = source.index("{", match.start())
        spans.append((match.start(), balanced(source, brace, "{", "}")))
    return spans


def check(path: pathlib.Path) -> tuple[list[str], int]:
    """Problems found in `path`, and the number of correctly argued exemptions."""
    source = path.read_text(encoding="utf-8")
    if not LINT_RE.search(source):
        return [], 0
    spans = test_spans(source)
    problems: list[str] = []
    audited = 0

    for match in ATTRIBUTE.finditer(source):
        start = match.start()
        if any(begin <= start < end for begin, end in spans):
            continue
        body = source[match.end() : balanced(source, match.end() - 1, "(", ")")]
        if not LINT_RE.search(body):
            continue

        line = source.count("\n", 0, start) + 1
        where = f"{path}:{line}"
        if match.group(1) == "allow":
            problems.append(f"{where}: allow() never expires; use expect()")
            continue

        reason = re.search(r'reason\s*=\s*"', body)
        if not reason:
            problems.append(f"{where}: no reason given")
            continue
        # Fold line continuations, so a wrapped reason is judged on what it
        # says rather than on how it is laid out.
        text = re.sub(r"\\\s*\n\s*", "", body[reason.end() :].lstrip())
        if not text.startswith("AUDIT:"):
            problems.append(f'{where}: reason does not begin "AUDIT:"')
            continue
        audited += 1

    return problems, audited


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    problems: list[str] = []
    audited = 0

    for name in ROOTS:
        for path in sorted((root / name).rglob("*.rs")):
            # Test code however clippy sees it: an integration test is its own
            # crate, so clippy's in-test exemptions do not reach it.
            if "tests" in path.parts or "target" in path.parts:
                continue
            found, count = check(path)
            problems.extend(found)
            audited += count

    if problems:
        for problem in problems:
            print(problem.replace(str(root) + "/", ""), file=sys.stderr)
        print(file=sys.stderr)
        print(
            "Every exemption from a panic lint must be argued at the site, as:",
            file=sys.stderr,
        )
        print(file=sys.stderr)
        print(
            '    #[expect(clippy::indexing_slicing, reason = "AUDIT: len checked at :212")]',
            file=sys.stderr,
        )
        print(file=sys.stderr)
        print("See docs/RELIABILITY.md.", file=sys.stderr)
        return 1

    print(f"panic-audit: {audited} exemption(s) in production code, all argued")
    return 0


if __name__ == "__main__":
    sys.exit(main())
