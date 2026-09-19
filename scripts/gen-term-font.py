#!/usr/bin/env python3
"""Generate the terminal's font from the Hack TrueType faces.

The panic screen's font (`scripts/gen-font.py`) is an 8x16 bitmap, because a
panic has no filesystem to load a font from and every byte it costs is a byte
of kernel image. The terminal is a different program with a different problem:
it is what a person actually reads, for as long as they are logged in, and a
bitmap font designed for a 1990s console is what makes an otherwise finished
desktop look like one.

So `compositor/term` carries its own font: Hack, rendered from its TrueType
outlines at 20 pixels per em into a 12x24 cell with one coverage byte a pixel.
Antialiasing is the whole point -- a stem that falls between two pixels is two
grey pixels rather than one black one snapped to the grid -- and coverage is
what `paint.rs` blends the foreground and background colours with.

The outlines are rasterised here, not by FreeType, and nothing in this file
imports anything that is not in the standard library. A gate that needs a pip
install is a gate that does not run on a stock checkout, and this one has to
run in CI: the output is committed, `--check` regenerates it into memory and
compares, and byte-identical is only a meaningful demand if every machine's
rasteriser is this one. The price is that the TrueType hinting programs are
ignored, which is what every modern renderer at this size does anyway.

What identifies the input is its SHA-256, embedded in the header along with
the family, version and copyright read from the font's own `name` table, so
that an updated face cannot leave a stale credit behind.

The licence travels with the fonts in `compositor/term/LICENSE-hack`.

Usage:
    python3 scripts/gen-term-font.py           # write the output
    python3 scripts/gen-term-font.py --check   # fail if it is stale
"""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import struct
import sys

FONTS = {
    "REGULAR": pathlib.Path("compositor/term/font/Hack-Regular.ttf"),
    "BOLD": pathlib.Path("compositor/term/font/Hack-Bold.ttf"),
}
OUTPUT = pathlib.Path("compositor/term/src/font.rs")
LICENCE = pathlib.Path("compositor/term/LICENSE-hack")

# Hack advances 1233/2048 of an em, so 20 pixels per em is a cell 12.05 pixels
# wide and, from the face's own ascent and descent, 24 tall: the one size in
# the range a terminal is read at where both come out whole, and the same 1:2
# cell the panic screen has.
PIXELS_PER_EM = 20
WIDTH = 12
HEIGHT = 24
# Rows above the baseline. The face's ascent is 1901/2048 em, which at this
# size is 18.56 pixels, and the descent 4.72: 19 and 5 fill the cell exactly.
BASELINE = 19

FIRST = 0x20
LAST = 0x7E
# U+25A1 WHITE SQUARE, drawn for everything the table has no glyph for, in the
# font's own style rather than a box this script invents.
REPLACEMENT = 0x25A1

# Line segments a quadratic curve is flattened into. At 20 pixels per em the
# longest curve in the face is a few pixels across, so eight segments put every
# joint well inside a sixteenth of a pixel.
CURVE_STEPS = 8
# Sub-scanlines sampled a pixel row. Coverage across a sub-scanline is exact,
# so the sampling is vertical only and 16 rows give the 256 levels a byte holds.
SAMPLES = 16

# Densest last: what a coverage byte looks like in the comment beside its row.
# `.` rather than a space for none, so that a blank row leaves no trailing
# whitespace behind in the generated file.
RAMP = ".:-=+*#%@"


def repository_root() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parent.parent


def fail(message: str) -> SystemExit:
    return SystemExit(f"gen-term-font: {message}")


# ---------------------------------------------------------------------------
# The TrueType file
# ---------------------------------------------------------------------------


class Face:
    """The parts of a TrueType face this script reads.

    Only what Hack uses is accepted: quadratic outlines in `glyf`, a format 4
    `cmap`, and no composite glyph in the range wanted. Anything else stops the
    generator rather than producing a glyph that is quietly wrong, because a
    wrong glyph is a font that nobody looks at twice.
    """

    def __init__(self, data: bytes, path: pathlib.Path) -> None:
        self.data = data
        self.path = path
        if len(data) < 12:
            raise fail(f"{path} is not a TrueType file")
        count = struct.unpack(">H", data[4:6])[0]
        self.tables: dict[str, tuple[int, int]] = {}
        for index in range(count):
            entry = 12 + 16 * index
            tag, _checksum, start, length = struct.unpack(">4sIII", data[entry : entry + 16])
            self.tables[tag.decode("latin-1")] = (start, length)

        head = self.table("head")
        self.units_per_em = struct.unpack(">H", head[18:20])[0]
        long_loca = struct.unpack(">h", head[50:52])[0]
        self.num_glyphs = struct.unpack(">H", self.table("maxp")[4:6])[0]
        self.loca = self._loca(long_loca)
        self.glyf = self.table("glyf")
        self.cmap = self._cmap()
        self.names = self._names()

    def table(self, tag: str) -> bytes:
        entry = self.tables.get(tag)
        if entry is None:
            raise fail(f"{self.path} has no {tag} table")
        start, length = entry
        return self.data[start : start + length]

    def _loca(self, long_loca: int) -> list[int]:
        raw = self.table("loca")
        count = self.num_glyphs + 1
        if long_loca:
            return list(struct.unpack(f">{count}I", raw[: 4 * count]))
        return [offset * 2 for offset in struct.unpack(f">{count}H", raw[: 2 * count])]

    def _cmap(self) -> dict[int, int]:
        """Code point to glyph index, from the Unicode format 4 subtable."""
        raw = self.table("cmap")
        count = struct.unpack(">H", raw[2:4])[0]
        chosen: int | None = None
        for index in range(count):
            platform, encoding, offset = struct.unpack(">HHI", raw[4 + 8 * index : 12 + 8 * index])
            if (platform, encoding) in ((3, 1), (0, 3), (0, 4)):
                if struct.unpack(">H", raw[offset : offset + 2])[0] == 4:
                    chosen = offset
        if chosen is None:
            raise fail(f"{self.path} has no format 4 Unicode cmap")

        sub = raw[chosen:]
        double = struct.unpack(">H", sub[6:8])[0]
        segments = double // 2
        ends = struct.unpack(f">{segments}H", sub[14 : 14 + double])
        starts = struct.unpack(f">{segments}H", sub[16 + double : 16 + 2 * double])
        deltas = struct.unpack(f">{segments}h", sub[16 + 2 * double : 16 + 3 * double])
        ranges_at = 16 + 3 * double
        ranges = struct.unpack(f">{segments}H", sub[ranges_at : ranges_at + double])

        table: dict[int, int] = {}
        for index in range(segments):
            for code in range(starts[index], min(ends[index], 0xFFFF) + 1):
                if ranges[index] == 0:
                    glyph = (code + deltas[index]) & 0xFFFF
                else:
                    at = ranges_at + 2 * index + ranges[index] + 2 * (code - starts[index])
                    glyph = struct.unpack(">H", sub[at : at + 2])[0]
                    if glyph:
                        glyph = (glyph + deltas[index]) & 0xFFFF
                if glyph:
                    table[code] = glyph
        return table

    def _names(self) -> dict[int, str]:
        """The English Windows name records, by name ID."""
        raw = self.table("name")
        count, storage = struct.unpack(">HH", raw[2:6])
        names: dict[int, str] = {}
        for index in range(count):
            record = raw[6 + 12 * index : 18 + 12 * index]
            platform, encoding, language, name, length, offset = struct.unpack(">6H", record)
            if (platform, encoding, language) != (3, 1, 0x409):
                continue
            text = raw[storage + offset : storage + offset + length]
            names[name] = text.decode("utf-16-be", errors="replace")
        return names

    def name(self, identifier: int, what: str) -> str:
        value = self.names.get(identifier)
        if not value:
            raise fail(f"{self.path} carries no {what}")
        return value

    def outline(self, code: int) -> list[list[tuple[int, int, bool]]]:
        """One glyph's contours: points in font units, each on or off curve."""
        glyph = self.cmap.get(code)
        if glyph is None:
            raise fail(f"{self.path} has no glyph for U+{code:04X}")
        start, end = self.loca[glyph], self.loca[glyph + 1]
        if start >= end:
            return []
        data = self.glyf[start:end]
        contours = struct.unpack(">h", data[:2])[0]
        if contours < 0:
            raise fail(f"{self.path}: U+{code:04X} is a composite glyph")

        ends = struct.unpack(f">{contours}H", data[10 : 10 + 2 * contours])
        points = ends[-1] + 1 if contours else 0
        at = 10 + 2 * contours
        at += 2 + struct.unpack(">H", data[at : at + 2])[0]  # skip the hinting

        flags: list[int] = []
        while len(flags) < points:
            flag = data[at]
            at += 1
            flags.append(flag)
            if flag & 0x08:  # repeat
                repeats = data[at]
                at += 1
                flags += [flag] * repeats
        if len(flags) != points:
            raise fail(f"{self.path}: U+{code:04X} has {len(flags)} flags for {points} points")

        def coordinates(short_bit: int, same_bit: int, at: int) -> tuple[list[int], int]:
            values: list[int] = []
            value = 0
            for flag in flags:
                if flag & short_bit:
                    delta = data[at]
                    at += 1
                    value += delta if flag & same_bit else -delta
                elif not flag & same_bit:
                    value += struct.unpack(">h", data[at : at + 2])[0]
                    at += 2
                values.append(value)
            return values, at

        xs, at = coordinates(0x02, 0x10, at)
        ys, _ = coordinates(0x04, 0x20, at)

        outline = []
        first = 0
        for last in ends:
            outline.append([(xs[i], ys[i], bool(flags[i] & 0x01)) for i in range(first, last + 1)])
            first = last + 1
        return [contour for contour in outline if contour]


# ---------------------------------------------------------------------------
# Rasterising
# ---------------------------------------------------------------------------


def polygon(contour: list[tuple[int, int, bool]], scale: float) -> list[tuple[float, float]]:
    """A contour flattened into pixel-space points.

    TrueType curves are quadratic, and two consecutive off-curve points imply
    an on-curve point halfway between them, which is the one piece of the
    format a reader cannot skip.
    """
    start = next((index for index, point in enumerate(contour) if point[2]), None)
    if start is None:
        # Every point is off-curve: the implied on-curve point between the
        # last and the first starts the contour.
        x = (contour[0][0] + contour[-1][0]) / 2
        y = (contour[0][1] + contour[-1][1]) / 2
        contour = [(x, y, True), *contour]
        start = 0
    contour = contour[start:] + contour[:start]

    points = [(contour[0][0] * scale, contour[0][1] * scale)]
    current = (contour[0][0], contour[0][1])
    index = 1
    count = len(contour)
    while index <= count:
        point = contour[index % count]
        if point[2]:
            points.append((point[0] * scale, point[1] * scale))
            current = (point[0], point[1])
            index += 1
            continue
        following = contour[(index + 1) % count]
        if following[2]:
            end = (following[0], following[1])
            index += 2
        else:
            end = ((point[0] + following[0]) / 2, (point[1] + following[1]) / 2)
            index += 1
        for step in range(1, CURVE_STEPS + 1):
            t = step / CURVE_STEPS
            rest = 1 - t
            x = rest * rest * current[0] + 2 * rest * t * point[0] + t * t * end[0]
            y = rest * rest * current[1] + 2 * rest * t * point[1] + t * t * end[1]
            points.append((x * scale, y * scale))
        current = end
    return points


def coverage(polygons: list[list[tuple[float, float]]]) -> list[int]:
    """A glyph's cell, one byte a pixel, top row first.

    The outline is filled by the non-zero winding rule, the rule TrueType is
    defined by, so a counter-clockwise inner contour -- the hole in an `o` --
    cancels the outer one. Each pixel row is sampled at [`SAMPLES`] heights,
    and what a sample contributes is the exact width of the spans crossing it,
    not whether its centre is inside: a stem 1.6 pixels wide comes out as 1.6
    pixels of ink, wherever it happens to fall.
    """
    edges = []
    for points in polygons:
        for index, (x0, y0) in enumerate(points):
            x1, y1 = points[(index + 1) % len(points)]
            # Font units grow upwards from the baseline; rows grow downwards
            # from the top of the cell.
            top0, top1 = BASELINE - y0, BASELINE - y1
            if top0 != top1:
                edges.append((x0, top0, x1, top1))

    cell = [0.0] * (WIDTH * HEIGHT)
    for row in range(HEIGHT):
        line = row * WIDTH
        for sample in range(SAMPLES):
            y = row + (sample + 0.5) / SAMPLES
            crossings = []
            for x0, y0, x1, y1 in edges:
                if (y0 <= y < y1) or (y1 <= y < y0):
                    t = (y - y0) / (y1 - y0)
                    crossings.append((x0 + t * (x1 - x0), 1 if y1 > y0 else -1))
            if not crossings:
                continue
            crossings.sort()
            winding = 0
            spans = []
            left = 0.0
            for x, direction in crossings:
                if winding == 0:
                    left = x
                winding += direction
                if winding == 0:
                    spans.append((left, x))
            for left, right in spans:
                left, right = max(left, 0.0), min(right, float(WIDTH))
                if right <= left:
                    continue
                first, last = int(left), min(int(right), WIDTH - 1)
                if first == last:
                    cell[line + first] += (right - left) / SAMPLES
                    continue
                cell[line + first] += (first + 1 - left) / SAMPLES
                for column in range(first + 1, last):
                    cell[line + column] += 1.0 / SAMPLES
                cell[line + last] += (right - last) / SAMPLES
    return [min(255, int(value * 255 + 0.5)) for value in cell]


# ---------------------------------------------------------------------------
# The Rust source
# ---------------------------------------------------------------------------


def art(row: list[int]) -> str:
    return "".join(RAMP[value * (len(RAMP) - 1) // 255] for value in row)


def describe(code: int) -> str:
    if code == REPLACEMENT:
        return "U+25A1 WHITE SQUARE"
    character = chr(code)
    shown = {"'": "\\'", "\\": "\\\\"}.get(character, character)
    return f"0x{code:02X} '{shown}'"


def cell_block(cell: list[int], indent: str) -> list[str]:
    out = [f"{indent}["]
    for row in range(HEIGHT):
        values = cell[row * WIDTH : (row + 1) * WIDTH]
        bytes_ = " ".join(f"0x{value:02X}," for value in values)
        out.append(f"{indent}    {bytes_} // {art(values)}")
    out.append(f"{indent}]")
    return out


def credit(face: Face, source: bytes) -> list[str]:
    version = face.name(5, "version").split(";")[0].strip()
    return [
        f"//   {face.name(1, 'family name')} {version}, {face.path.name}",
        f"//     {face.name(0, 'copyright notice')}",
        f"//     SHA-256 {hashlib.sha256(source).hexdigest()}",
    ]


def render(sources: dict[str, bytes]) -> str:
    faces = {name: Face(data, FONTS[name]) for name, data in sources.items()}
    wanted = list(range(FIRST, LAST + 1))
    count = len(wanted)

    out = [
        "// @generated by scripts/gen-term-font.py from the fonts in"
        " compositor/term/font/.",
        "// Do not edit: regenerate with `python3 scripts/gen-term-font.py`,"
        " and check with",
        "// `--check`.",
        "//",
    ]
    for name in FONTS:
        out += credit(faces[name], sources[name])
    out += [
        f"// Licence: MIT and the Bitstream Vera License; see {LICENCE.as_posix()},",
        "//          which a binary shipping this file must reproduce.",
        "",
        f"//! The Hack faces at {PIXELS_PER_EM} pixels per em, as cells of coverage.",
        "//!",
        "//! One byte a pixel, `0x00` where the glyph covers none of it and `0xFF`",
        f"//! where it covers all of it: {WIDTH} bytes a row, {HEIGHT} rows a cell, top",
        f"//! row first, with the baseline below row {BASELINE - 1}.",
        "",
        "/// The cell a glyph is drawn in, in pixels.",
        f"pub(crate) const WIDTH: usize = {WIDTH};",
        "/// Rows in a cell, ascent and descent together.",
        f"pub(crate) const HEIGHT: usize = {HEIGHT};",
        "",
        "/// The first code point in the tables.",
        f"pub(crate) const FIRST: u32 = 0x{FIRST:02X};",
        "",
        "/// One glyph: coverage, row by row.",
        "pub(crate) type Cell = [u8; WIDTH * HEIGHT];",
    ]

    for name in FONTS:
        face = faces[name]
        scale = PIXELS_PER_EM / face.units_per_em
        glyphs = {
            code: coverage([polygon(contour, scale) for contour in face.outline(code)])
            for code in [*wanted, REPLACEMENT]
        }
        style = name.capitalize()
        out += [
            "",
            f"/// {style}: printable ASCII, 0x{FIRST:02X}..=0x{LAST:02X}, indexed by code",
            "/// point minus [`FIRST`].",
            "#[rustfmt::skip]",
            f"pub(crate) static {name}: [Cell; {count}] = [",
        ]
        for code in wanted:
            out.append(f"    // {describe(code)}")
            block = cell_block(glyphs[code], "    ")
            block[-1] += ","
            out += block
        out += [
            "];",
            "",
            f"/// {style}, for every character outside [`{name}`]: {describe(REPLACEMENT)}.",
            "#[rustfmt::skip]",
            f"pub(crate) static {name}_REPLACEMENT: Cell = ",
        ]
        block = cell_block(glyphs[REPLACEMENT], "")
        out[-1] += block[0]
        out += block[1:]
        out[-1] += ";"

    return "\n".join(out) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=f"Generate {OUTPUT} from the Hack faces.")
    parser.add_argument(
        "--check",
        action="store_true",
        help="do not write; exit 1 if the committed output is stale",
    )
    arguments = parser.parse_args()

    root = repository_root()
    sources = {name: (root / path).read_bytes() for name, path in FONTS.items()}
    text = render(sources)
    path = root / OUTPUT

    if arguments.check:
        current = path.read_text(encoding="utf-8") if path.exists() else None
        if current == text:
            return 0
        reason = "missing" if current is None else "out of date"
        print(f"gen-term-font: {OUTPUT} is {reason} with respect to the Hack faces.")
        print("\nRun python3 scripts/gen-term-font.py and commit the result.")
        return 1

    # newline="\n" so a Windows checkout does not produce CRLF and fail the
    # line-endings gate on a file nobody typed.
    path.write_text(text, encoding="utf-8", newline="\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
