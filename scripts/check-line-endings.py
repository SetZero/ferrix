#!/usr/bin/env python3
"""Assert every text file in the tree uses LF, and holds no stray CR.

`.gitattributes` normalises line endings on checkout, and `rustfmt.toml` sets
`newline_style = "Unix"` so `cargo fmt` keeps `.rs` files honest. Neither of
those covers a file *written* on a Windows machine by a tool that translates
newlines -- Python's `write_text` does, silently -- and neither is checked
until something breaks.

Something did: a CRLF in `scripts/check-crate-layering.sh` made its `#!` line
unparseable, and bash reported

    scripts/check-crate-layering.sh: line 25: $'\r': command not found

which names neither the file's real problem nor the line that has it. That is
the whole argument for this gate: the failure mode is obscure and the check is
three lines.

Then the same bug returned in a shape this gate did not look for: a *lone* CR,
one not followed by LF. It sat inside a comment in `.github/workflows/ci.yml`
-- a comment about this very failure, which quoted a carriage return with a
real control character instead of the two characters that spell it. YAML counts
a bare CR as a line break, so the comment ended in the middle, the rest of the
line became a mapping key with no colon, and GitHub rejected the entire
workflow before it started a single job. The run failed in zero seconds, with
no job to open and no log to read.

So the check is both: CRLF anywhere, and CR anywhere else. A lone CR is
reported but never rewritten, because turning it into LF would insert a real
line break into whatever comment or string literal is holding it, and only the
author knows which of the two was meant.

Usage:  python3 scripts/check-line-endings.py [--fix]
"""

from __future__ import annotations

import argparse
import pathlib
import sys

# Extensions whose contents are text somebody or something parses.
TEXT_SUFFIXES = {
    ".rs",
    ".toml",
    ".md",
    ".sh",
    ".py",
    ".json",
    ".html",
    ".svg",
    ".yml",
    ".yaml",
    ".ld",
}

# Files with no extension that are still text.
TEXT_NAMES = {".gitattributes", ".gitignore", ".clippy.toml"}

# Directories that are not ours to normalise.
SKIP = {"target", ".git", "build", "artifacts", "corpus"}

CR = 0x0D
LF = 0x0A


def candidates(root: pathlib.Path):
    """Every text file in the tree, skipping build output and fuzz corpora."""
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        if any(part in SKIP for part in path.parts):
            continue
        if path.suffix in TEXT_SUFFIXES or path.name in TEXT_NAMES:
            yield path


def lone_carriage_returns(data: bytes):
    """Every CR that is not part of a CRLF, as (line, column) counting from 1.

    Lines are counted the way an editor counts them, so the position names the
    place somebody has to go and look.
    """
    line = 1
    column = 1
    for index, byte in enumerate(data):
        if byte == CR and data[index + 1 : index + 2] != bytes([LF]):
            yield line, column
        if byte == LF:
            line += 1
            column = 1
        else:
            column += 1


def main() -> int:
    parser = argparse.ArgumentParser()
    _ = parser.add_argument("--fix", action="store_true", help="rewrite offenders as LF")
    args = parser.parse_args()

    root = pathlib.Path(__file__).resolve().parent.parent
    offenders = []
    strays = []
    checked = 0

    crlf = bytes([CR, LF])

    for path in candidates(root):
        checked += 1
        data = path.read_bytes()
        name = path.relative_to(root).as_posix()

        for line, column in lone_carriage_returns(data):
            strays.append((name, line, column))

        if crlf not in data:
            continue
        if args.fix:
            path.write_bytes(data.replace(crlf, bytes([LF])))
        offenders.append(name)

    # An error even under `--fix`, because the repair is a judgement call and
    # this script is not the one to make it.
    if strays:
        for name, line, column in strays:
            print(
                f"{name}:{line}:{column}: carriage return that is not a line ending",
                file=sys.stderr,
            )
        print(file=sys.stderr)
        print(
            "A bare CR is a line break to YAML and to bash, and invisible in most\n"
            "editors. To *name* a carriage return, write the two characters for it\n"
            "rather than the control character; if it is a stray, delete it.",
            file=sys.stderr,
        )
        return 1

    if offenders and not args.fix:
        for name in offenders:
            print(f"{name}: has CRLF line endings", file=sys.stderr)
        print(file=sys.stderr)
        print("Run `python3 scripts/check-line-endings.py --fix`.", file=sys.stderr)
        print(
            "A CRLF in a shell script makes its `#!` line unparseable, and the "
            "error names neither\nthe file nor the cause.",
            file=sys.stderr,
        )
        return 1

    if offenders:
        print(f"line-endings: normalised {len(offenders)} file(s) to LF")
        for name in offenders:
            print(f"    {name}")
        return 0

    print(f"line-endings: {checked} text file(s), all LF and no stray CR")
    return 0


if __name__ == "__main__":
    sys.exit(main())
