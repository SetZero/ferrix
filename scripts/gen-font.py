#!/usr/bin/env python3
"""Generate the framebuffer font from the Spleen BDF.

`ferrix-fbtext` draws the panic screen, which runs on a machine already in a bad
state: there is no filesystem to load a font from and no allocator to decode
one into. So the font is Rust source, a table of rows the drawing code indexes
directly -- and a table of rows is exactly what nobody should type by hand.

This reads the upstream BDF, committed verbatim beside the crate, and writes
`libs/fbtext/src/font.rs`: printable ASCII (0x20..=0x7E) as one `[u8; 16]` per
glyph, one byte per scanline, most significant bit leftmost, plus Spleen's own
U+25A1 WHITE SQUARE as the glyph for everything else.

The output is committed, and `--check` regenerates into memory and compares.
Reproducible means byte-identical: nothing here writes a timestamp, a path
outside the repository or a tool version. What identifies the input is its
SHA-256, which is embedded in the header, along with the font's own version
and copyright line, read from the BDF so that they cannot go stale on an update.

The font's licence travels with it in `libs/fbtext/LICENSE-spleen`.

Usage:
    python3 scripts/gen-font.py              # write the output
    python3 scripts/gen-font.py --check      # fail if it is stale
"""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import sys

BDF = pathlib.Path("libs/fbtext/font/spleen-8x16.bdf")
OUTPUT = pathlib.Path("libs/fbtext/src/font.rs")
LICENCE = pathlib.Path("libs/fbtext/LICENSE-spleen")

WIDTH = 8
HEIGHT = 16
FIRST = 0x20
LAST = 0x7E
# U+25A1 WHITE SQUARE: a hollow box, which is what a missing glyph looks like
# on every console, and Spleen draws one in its own style.
REPLACEMENT = 0x25A1


def repository_root() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parent.parent


def fail(message: str) -> SystemExit:
    return SystemExit(f"gen-font: {message}")


def parse(text: str) -> tuple[dict[str, str], dict[int, list[int]]]:
    """The font properties and every glyph's rows, keyed by code point.

    Only the subset of BDF this font uses is accepted. A glyph whose bounding
    box is not the full 8x16 cell is refused rather than placed, because the
    table has no way to say where inside the cell the bitmap sits, and a
    silently misplaced glyph is worse than a generator that stops.
    """
    properties: dict[str, str] = {}
    glyphs: dict[int, list[int]] = {}
    lines = iter(text.splitlines())
    encoding: int | None = None
    for line in lines:
        keyword, _, rest = line.partition(" ")
        if keyword in ("FONT_VERSION", "COPYRIGHT", "FAMILY_NAME"):
            properties[keyword] = rest.strip().strip('"')
        elif keyword == "FONTBOUNDINGBOX":
            properties[keyword] = rest.strip()
        elif keyword == "ENCODING":
            encoding = int(rest)
        elif keyword == "BBX":
            if rest.split() != [str(WIDTH), str(HEIGHT), "0", "-4"]:
                raise fail(f"glyph {encoding}: bounding box {rest!r} is not the full cell")
        elif keyword == "BITMAP":
            if encoding is None:
                raise fail("BITMAP before ENCODING")
            glyphs[encoding] = bitmap(lines, encoding)
            encoding = None
    return properties, glyphs


def bitmap(lines, encoding: int) -> list[int]:
    rows: list[int] = []
    for line in lines:
        if line == "ENDCHAR":
            break
        rows.append(int(line, 16))
    if len(rows) != HEIGHT or any(not 0 <= row <= 0xFF for row in rows):
        raise fail(f"glyph {encoding}: expected {HEIGHT} rows of one byte each")
    return rows


def art(row: int) -> str:
    return "".join("#" if row & (0x80 >> bit) else "." for bit in range(WIDTH))


def describe(code: int) -> str:
    if code == REPLACEMENT:
        return "U+25A1 WHITE SQUARE"
    character = chr(code)
    shown = {"'": "\\'", "\\": "\\\\"}.get(character, character)
    return f"0x{code:02X} '{shown}'"


def glyph_block(code: int, rows: list[int], indent: str) -> list[str]:
    out = [f"{indent}// {describe(code)}", f"{indent}["]
    out += [f"{indent}    0b{row:08b}, // {art(row)}" for row in rows]
    out.append(f"{indent}]")
    return out


def render(source: bytes) -> str:
    properties, glyphs = parse(source.decode("ascii"))
    if properties.get("FONTBOUNDINGBOX", "").split()[:2] != [str(WIDTH), str(HEIGHT)]:
        raise fail(f"{BDF} is not an {WIDTH}x{HEIGHT} font")
    wanted = list(range(FIRST, LAST + 1)) + [REPLACEMENT]
    missing = [f"U+{code:04X}" for code in wanted if code not in glyphs]
    if missing:
        raise fail(f"{BDF} has no glyph for {', '.join(missing)}")

    family = properties.get("FAMILY_NAME", "Spleen")
    version = properties.get("FONT_VERSION", "unknown version")
    copyright_line = properties.get("COPYRIGHT")
    if copyright_line is None:
        raise fail(f"{BDF} carries no COPYRIGHT property")
    digest = hashlib.sha256(source).hexdigest()
    count = LAST - FIRST + 1

    out = [
        f"// @generated by scripts/gen-font.py from {BDF.as_posix()}.",
        "// Do not edit: regenerate with `python3 scripts/gen-font.py`, and check with `--check`.",
        "//",
        f"// Font:    {family} {WIDTH}x{HEIGHT} {version}",
        f"// Licence: {copyright_line}. BSD-2-Clause;",
        f"//          see {LICENCE.as_posix()}.",
        f"// Source:  SHA-256 {digest}",
        "",
        f"//! The {family} {WIDTH}x{HEIGHT} bitmap font, as rows.",
        "//!",
        "//! One byte per scanline, top row first, most significant bit leftmost.",
        "",
        f"/// The first code point in [`GLYPHS`].",
        f"pub(crate) const FIRST: u32 = 0x{FIRST:02X};",
        "",
        f"/// Printable ASCII, 0x{FIRST:02X}..=0x{LAST:02X}, indexed by code point minus [`FIRST`].",
        "#[rustfmt::skip]",
        f"pub(crate) static GLYPHS: [[u8; {HEIGHT}]; {count}] = [",
    ]
    for code in range(FIRST, LAST + 1):
        block = glyph_block(code, glyphs[code], "    ")
        block[-1] += ","
        out += block
    out += [
        "];",
        "",
        f"/// Drawn for every character outside [`GLYPHS`]: {describe(REPLACEMENT)}.",
        "#[rustfmt::skip]",
        f"pub(crate) static REPLACEMENT: [u8; {HEIGHT}] = [",
    ]
    out += [f"    0b{row:08b}, // {art(row)}" for row in glyphs[REPLACEMENT]]
    out.append("];")
    return "\n".join(out) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=f"Generate {OUTPUT} from {BDF}.")
    parser.add_argument(
        "--check",
        action="store_true",
        help="do not write; exit 1 if the committed output is stale",
    )
    arguments = parser.parse_args()

    root = repository_root()
    text = render((root / BDF).read_bytes())
    path = root / OUTPUT

    if arguments.check:
        current = path.read_text(encoding="utf-8") if path.exists() else None
        if current == text:
            return 0
        reason = "missing" if current is None else "out of date"
        print(f"gen-font: {OUTPUT} is {reason} with respect to {BDF}.")
        print("\nRun python3 scripts/gen-font.py and commit the result.")
        return 1

    # newline="\n" so a Windows checkout does not produce CRLF and fail the
    # line-endings gate on a file nobody typed.
    path.write_text(text, encoding="utf-8", newline="\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
