#!/usr/bin/env python3
"""Statement coverage for the kernel, measured from a QEMU boot.

`docs/sysml/11-assurance.sysml` records the reachability problem plainly: a
fuzzer cannot drive a page-fault handler and Miri cannot interpret a privileged
instruction, so `kernel/` is reachable only by booting it. That left the kernel
with no structural coverage measurement at all, which is the single largest
open objective for DO-178C DAL C (table A-7) and a recommended technique for
EN 50716 SIL 2.

This closes it without touching the toolchain. QEMU's `drcov` TCG plugin
records every basic block the guest translates and executes; the kernel's own
DWARF line table says which source statement each address belongs to. Intersect
the two and you have statement coverage of ring 0, per certification ring.

    cargo xtask test-boot --arch x86_64 --accel tcg   # with FERRIX_QEMU_PLUGIN
    python3 scripts/coverage-report.py \
        --drcov /tmp/ferrix-cov.drcov \
        --elf target/x86_64-unknown-none/debug/ferrix-kernel

What is being counted
---------------------

The denominator is every source line the compiler marked `is_stmt` in the DWARF
line table -- the line table's own notion of "a statement begins here", which is
what gcov-shaped tools count and the closest available analogue to the
statement coverage DAL C asks for. The numerator is those whose address fell
inside a basic block the boot executed.

Line 0 rows are compiler-generated and are not statements; they are skipped.

Two limitations, stated here rather than in a footnote, because a coverage
number people trust is worse than none if its caveats travel separately:

  * Coverage is of the profile actually booted. On an optimised build the line
    table is approximate -- inlining maps one address to several source lines,
    and a line with no instructions of its own cannot be reached. A DAL C
    submission has to either measure the configuration it ships or argue the
    mapping; this tool reports which profile it measured so the question
    cannot be quietly skipped.
  * A basic block that was translated and executed counts every statement
    inside it. That over-reports where a block was entered and left early by a
    trap. The effect is small and always in the optimistic direction, which is
    the direction worth declaring.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import struct
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# `kernel/src/syscall/image.rs:` -- objdump groups the line table by source
# file and prints the path, relative to the compilation directory, as a header.
#
# Some headers carry no directory at all (`mod.rs:`), which names a file this
# script cannot place. Those must still be recognised as headers: treating one
# as an ordinary row leaves the parser attributing another file's statements to
# whichever path came before it, which silently inflates that file's coverage.
# An unplaceable header clears the current file instead, so its rows are
# skipped rather than misfiled.
FILE_HEADER = re.compile(r"^(\S+\.rs):$")
# `image.rs   86   0xffffffff80000110   x` -- name, line, address, then an
# optional view column and an `x` when the row begins a statement.
LINE_ROW = re.compile(r"^\S+\s+(\d+)\s+(0x[0-9a-fA-F]+)\s*(\d*)\s*(x?)\s*$")


def load_gate():
    """Reuse the boundary gate's classifier so both agree on the rings."""
    spec = importlib.util.spec_from_file_location(
        "boundary", ROOT / "scripts" / "check-item-boundary.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def read_drcov(path: Path) -> list[tuple[int, int]]:
    """Every (start, size) basic block the run executed.

    The format is a short text header, a `BB Table: N bbs` line, then N packed
    records of `uint32 start, uint16 size, uint16 mod_id`. The start is only 32
    bits wide, which for a higher-half kernel means the top half is missing;
    `reconstruct` puts it back.
    """
    blob = path.read_bytes()
    marker = b"BB Table: "
    index = blob.find(marker)
    if index < 0:
        raise SystemExit(f"{path}: no BB table -- did the plugin run under tcg?")
    newline = blob.index(b"\n", index)
    count = int(blob[index + len(marker) : newline].split()[0])

    body = blob[newline + 1 :]
    if len(body) < count * 8:
        raise SystemExit(
            f"{path}: truncated, {len(body)} bytes for {count} blocks. A boot "
            f"killed before QEMU exited does not flush the table."
        )

    return [
        (start, size)
        for start, size, _mod in struct.iter_unpack("<IHH", body[: count * 8])
    ]


def elf_text_range(elf: Path) -> tuple[int, int]:
    """The virtual address span of the image's loadable segments."""
    output = subprocess.run(
        ["readelf", "-lW", str(elf)], capture_output=True, text=True, check=True
    ).stdout
    lows, highs = [], []
    for line in output.splitlines():
        parts = line.split()
        if len(parts) > 5 and parts[0] == "LOAD":
            vaddr = int(parts[2], 16)
            memsz = int(parts[5], 16)
            lows.append(vaddr)
            highs.append(vaddr + memsz)
    if not lows:
        raise SystemExit(f"{elf}: no LOAD segments")
    return min(lows), max(highs)


def reconstruct(low32: int, low: int, high: int) -> int | None:
    """Put back the top 32 bits drcov's record dropped.

    An image whose loadable span sits inside one 4 GiB window -- which a kernel
    linked at a fixed base does -- is uniquely identified by the bottom 32 bits
    of its addresses. Anything that does not land in the span is the loader, a
    user program or firmware, and is not this image's coverage.
    """
    candidate = (low & ~0xFFFFFFFF) | low32
    return candidate if low <= candidate < high else None


def trace_slide(trace: Path, given: int | None) -> int:
    """How far KASLR moved the kernel in the boot `trace` recorded.

    The trace holds the addresses the kernel ran at; the line table holds the
    ones it was linked at. The loader moves the image each boot and prints the
    difference (`boot/src/kaslr.rs`), and xtask writes it beside the trace as
    `<trace>.slide` (`xtask/src/kaslr.rs`). `--slide` overrides it for every
    trace; a trace with neither is taken to be of a kernel that did not move,
    as one built `--mitigations off` or booted with `nokaslr` does not.
    """
    if given is not None:
        return given
    sidecar = trace.with_name(trace.name + ".slide")
    if sidecar.exists():
        return int(sidecar.read_text().strip(), 16)
    return 0


def read_line_table(elf: Path) -> dict[str, dict[int, list[int]]]:
    """{source path: {line: [addresses]}} for every statement row."""
    output = subprocess.run(
        ["objdump", "--dwarf=decodedline", str(elf)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout

    table: dict[str, dict[int, list[int]]] = defaultdict(lambda: defaultdict(list))
    current: str | None = None

    for raw in output.splitlines():
        text = raw.strip()
        header = FILE_HEADER.match(text)
        if header:
            path = header.group(1)
            current = path if "/" in path else None
            continue
        if current is None:
            continue
        row = LINE_ROW.match(text)
        if not row:
            continue
        line = int(row.group(1))
        # Line 0 is compiler-generated; the `x` column is DWARF's is_stmt, and
        # a row without it is a continuation of a statement already counted.
        if line == 0 or row.group(4) != "x":
            continue
        table[current][line].append(int(row.group(2), 16))

    return table


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--drcov",
        type=Path,
        required=True,
        nargs="+",
        help="one or more traces; coverage is the union, as a test suite's is",
    )
    parser.add_argument("--elf", type=Path, required=True)
    parser.add_argument(
        "--slide",
        type=lambda text: int(text, 16),
        default=None,
        help="how far KASLR moved the kernel, in hex, for every trace; by "
        "default each trace's own `<trace>.slide`, or 0 without one",
    )
    parser.add_argument("--json", type=Path, help="write the per-file detail here")
    parser.add_argument(
        "--residual",
        type=Path,
        help="write every unreached statement in the item here, by file and "
        "line. DO-178C wants each one either driven by a test or justified "
        "as unreachable, and neither can start from a percentage.",
    )
    parser.add_argument(
        "--min-item",
        type=float,
        default=None,
        help="fail below this percent covered in the certified item",
    )
    args = parser.parse_args()

    gate = load_gate()
    manifest = gate.load_manifest()
    ring_of, _, _ = gate.classify(manifest, gate.kernel_files())

    low, high = elf_text_range(args.elf)

    # Executed address ranges, as a sorted list to test membership against.
    # The union across traces: a statement reached by any test in the suite is
    # a covered statement, which is how every coverage tool aggregates a run.
    blocks: list[tuple[int, int]] = []
    ranges: set[tuple[int, int]] = set()
    for trace in args.drcov:
        found = read_drcov(trace)
        blocks.extend(found)
        # Reconstructed against where the image ran, then moved back to where
        # it was linked, which is what the line table is in.
        slide = trace_slide(trace, args.slide)
        for low32, size in found:
            start = reconstruct(low32, low + slide, high + slide)
            if start is not None:
                start -= slide
                ranges.add((start, start + max(size, 1)))
        print(f"coverage: {trace.name}: {len(found)} blocks, slide {slide:#x}")

    executed = sorted(ranges)

    def was_executed(address: int) -> bool:
        import bisect

        index = bisect.bisect_right(executed, (address, 1 << 62))
        # A block starting at or before this address may contain it. Blocks are
        # short, so walking back a few is cheaper than an interval tree.
        for start, end in reversed(executed[max(0, index - 64) : index]):
            if start <= address < end:
                return True
        return False

    table = read_line_table(args.elf)

    totals: dict[str, dict[str, int]] = defaultdict(lambda: {"total": 0, "hit": 0})
    per_file: dict[str, dict[str, int]] = {}
    residual: dict[str, dict] = {}

    for path, lines in table.items():
        if "kernel/src/" not in path:
            continue
        rel = path.split("kernel/src/", 1)[1]
        ring = ring_of.get(rel)
        if ring is None:
            continue
        kind = "test" if gate.is_test_file(rel, manifest) else "product"
        if kind == "test":
            continue  # a test file's own coverage is not the item's coverage

        total = hit = 0
        missed: list[int] = []
        for line, addresses in lines.items():
            total += 1
            if any(was_executed(address) for address in addresses):
                hit += 1
            else:
                missed.append(line)

        totals[ring]["total"] += total
        totals[ring]["hit"] += hit
        per_file[rel] = {"ring": ring, "total": total, "hit": hit}
        if missed:
            residual[rel] = {"ring": ring, "lines": sorted(missed)}

    profile = "release" if "/release/" in str(args.elf) else "debug"
    print(f"coverage: {len(blocks)} basic blocks executed, "
          f"{len(executed)} inside the kernel image ({profile} profile)")
    print("coverage: statements reached, by certification ring")

    item_total = item_hit = 0
    for ring in ("core", "item", "load"):
        total = totals[ring]["total"]
        hit = totals[ring]["hit"]
        if not total:
            continue
        if ring in ("core", "item"):
            item_total += total
            item_hit += hit
        print(f"  {ring:<6} {hit:>6} / {total:<6} {100.0 * hit / total:5.1f}%")

    if item_total:
        share = 100.0 * item_hit / item_total
        print(f"  {'':<6} {'-' * 20}")
        print(f"  certified item  {item_hit} / {item_total}  {share:.1f}%")

    if args.json:
        args.json.write_text(
            json.dumps(
                {
                    "profile": profile,
                    "blocks_executed": len(blocks),
                    "blocks_in_image": len(executed),
                    "rings": {k: dict(v) for k, v in totals.items()},
                    "files": per_file,
                },
                indent=2,
            )
            + "\n"
        )
        print(f"coverage: per-file detail in {args.json}")

    if args.residual:
        inside = {k: v for k, v in residual.items() if v["ring"] in ("core", "item")}
        count = sum(len(v["lines"]) for v in inside.values())
        args.residual.write_text(
            json.dumps(
                {
                    "//": [
                        "Every statement in the certified item that the measured",
                        "runs did not reach. DO-178C table A-7 wants each either",
                        "covered by a requirements-based test or justified as",
                        "unreachable defensive code; this is the list that work",
                        "starts from. A percentage cannot be argued with.",
                    ],
                    "profile": profile,
                    "unreached": count,
                    "files": dict(sorted(inside.items(), key=lambda kv: -len(kv[1]["lines"]))),
                },
                indent=2,
            )
            + "\n"
        )
        print(f"coverage: {count} unreached statements listed in {args.residual}")

    if args.min_item is not None and item_total:
        if share < args.min_item:
            print(
                f"coverage: {share:.1f}% of the certified item, below the "
                f"{args.min_item:.1f}% this gate requires",
                file=sys.stderr,
            )
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
