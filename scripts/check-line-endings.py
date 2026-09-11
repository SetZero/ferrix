#!/usr/bin/env python3
"""Assert every text file in the tree uses LF.

`.gitattributes` normalises line endings on checkout, and `rustfmt.toml` sets
`newline_style = "Unix"` so `cargo fmt` keeps `.rs` files honest. Neither of
those covers a file *written* on a Windows machine by a tool that translates
newlines -- Python's `write_text` does, silently -- and neither is checked
until something breaks.

Something did: a CRLF in `scripts/check-crate-layering.sh` made its `#!` line
unparseable, and bash reported

    scripts/check-crate-layering.sh: line 25: $'\\r': command not found

which names neither the file's real problem nor the line that has it. That is
the whole argument for this gate: the failure mode is obscure and the check is
three lines.

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
    ".yml",
    ".yaml",
    ".ld",
}

# Files with no extension that are still text.
TEXT_NAMES = {".gitattributes", ".gitignore", ".clippy.toml"}

# Directories that are not ours to normalise.
SKIP = {"target", ".git", "build", "artifacts", "corpus"}


def candidates(root: pathlib.Path):
    """Every text file in the tree, skipping build output and fuzz corpora."""
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        if any(part in SKIP for part in path.parts):
            continue
        if path.suffix in TEXT_SUFFIXES or path.name in TEXT_NAMES:
            yield path


def main() -> int:
    parser = argparse.ArgumentParser()
    _ = parser.add_argument("--fix", action="store_true", help="rewrite offenders as LF")
    args = parser.parse_args()

    root = pathlib.Path(__file__).resolve().parent.parent
    offenders = []
    checked = 0

    for path in candidates(root):
        checked += 1
        data = path.read_bytes()
        if b"\r\n" not in data:
            continue
        if args.fix:
            path.write_bytes(data.replace(b"\r\n", b"\n"))
        offenders.append(path.relative_to(root).as_posix())

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

    print(f"line-endings: {checked} text file(s), all LF")
    return 0


if __name__ == "__main__":
    sys.exit(main())
