#!/usr/bin/env python3
"""Carry the coverage evidence's line anchors across a change to the kernel.

The evidence in docs/certification -- coverage-<arch>.json,
coverage-residual-<arch>.json and coverage-argued-<arch>.json -- names
statements by file and line, as measured on one tree. Any change that moves a
line in the item leaves those anchors pointing at the wrong statement, and
`gen-coverage-justification.py --check` then fails, as it should: an argument
must not drift onto another statement.

This carries the anchors from the tree the evidence was last written on to the
working tree, file by file, through a diff:

    python3 scripts/gen/carry-coverage.py            # from `git merge-base HEAD main`
    python3 scripts/gen/carry-coverage.py --from REV
    python3 scripts/gen/gen-coverage-justification.py   # then regenerate the pages
    python3 scripts/gen/gen-coverage-justification.py --check

It carries, it does not measure. A line the change left untouched keeps its
place in the residual and its argument, at its new number. A line the change
edited or removed is dropped: it is new code now, unmeasured until the next
run of `cargo xtask coverage`, and an argument written for the old text is not
evidence for the new one. What is dropped is printed, so the landing can say
so. The figures in coverage-<arch>.json stay as measured, and a renamed file
keeps its figures under its new name.
"""

import argparse
import difflib
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CERT = ROOT / "docs" / "certification"
ARCHES = ("x86_64", "aarch64", "armv7a")
SRC = "kernel/src/"


def git(*args, check=True):
    done = subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True)
    if check and done.returncode:
        sys.exit(f"carry-coverage: git {' '.join(args)}: {done.stderr.strip()}")
    return done


def renames(base):
    """kernel/src-relative old path -> new path, for files the change renamed."""
    out = {}
    listing = git("diff", "-M", "--name-status", base, "--", SRC).stdout
    for row in listing.splitlines():
        fields = row.split("\t")
        if fields[0].startswith("R") and len(fields) == 3:
            out[fields[1][len(SRC):]] = fields[2][len(SRC):]
    return out


def line_map(old, new):
    """Old line -> new line, for lines the change left exactly as they were."""
    out = {}
    matcher = difflib.SequenceMatcher(None, old, new, autojunk=False)
    for tag, i1, i2, j1, _ in matcher.get_opcodes():
        if tag == "equal":
            for k in range(i2 - i1):
                out[i1 + k + 1] = j1 + k + 1
    return out


def parse_lines(spec):
    out = []
    for part in str(spec).split(","):
        part = part.strip()
        if "-" in part:
            low, high = part.split("-")
            out.extend(range(int(low), int(high) + 1))
        elif part:
            out.append(int(part))
    return out


def format_lines(numbers):
    numbers = sorted(numbers)
    parts, i = [], 0
    while i < len(numbers):
        j = i
        while j + 1 < len(numbers) and numbers[j + 1] == numbers[j] + 1:
            j += 1
        parts.append(str(numbers[i]) if i == j else f"{numbers[i]}-{numbers[j]}")
        i = j + 1
    return ", ".join(parts)


class Carrier:
    def __init__(self, base):
        self.base = base
        self.moved = renames(base)
        self.maps = {}

    def carry(self, path):
        """(new path, line map or None for unchanged, or (new path, {}) if gone)."""
        if path in self.maps:
            return self.maps[path]
        new = self.moved.get(path, path)
        old = git("show", f"{self.base}:{SRC}{path}", check=False)
        here = ROOT / SRC / new
        if old.returncode or not here.is_file():
            result = (new, {})
        else:
            before, after = old.stdout, here.read_text()
            result = (new, None if before == after else line_map(before.splitlines(), after.splitlines()))
        self.maps[path] = result
        return result


def carry_residual(carrier, arch, report):
    path = CERT / f"coverage-residual-{arch}.json"
    residual = json.loads(path.read_text())
    files = {}
    for name, entry in residual["files"].items():
        new, mapping = carrier.carry(name)
        lines = entry["lines"]
        if mapping is not None:
            kept = [mapping[line] for line in lines if line in mapping]
            gone = [line for line in lines if line not in mapping]
            if gone:
                report.append(f"{arch} residual {name}: {len(gone)} line(s) edited or removed, "
                              f"unmeasured until the next run: {format_lines(gone)}")
            lines = kept
        if lines:
            files[new] = {**entry, "lines": lines}
    residual["files"] = files
    residual["unreached"] = sum(len(entry["lines"]) for entry in files.values())
    path.write_text(json.dumps(residual, indent=2) + "\n")


def carry_arguments(carrier, arch, report):
    path = CERT / f"coverage-argued-{arch}.json"
    if not path.is_file():
        return
    argued = json.loads(path.read_text())
    kept = []
    for argument in argued["arguments"]:
        new, mapping = carrier.carry(argument["file"])
        lines = parse_lines(argument["lines"])
        if mapping is None:
            kept.append({**argument, "file": new})
            continue
        moved = [mapping[line] for line in lines if line in mapping]
        gone = [line for line in lines if line not in mapping]
        if gone:
            report.append(f"{arch} argument {argument['file']}:{argument['lines']} "
                          f"({argument['category']}): line(s) {format_lines(gone)} edited or removed, "
                          f"{'argument kept for the rest' if moved else 'argument dropped'}")
        if moved:
            kept.append({**argument, "file": new, "lines": format_lines(moved)})
    argued["arguments"] = kept
    path.write_text(json.dumps(argued, indent=2, ensure_ascii=False) + "\n")


def carry_figures(carrier, arch):
    path = CERT / f"coverage-{arch}.json"
    figures = json.loads(path.read_text())
    figures["files"] = {carrier.carry(name)[0]: entry for name, entry in figures["files"].items()}
    path.write_text(json.dumps(figures, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--from", dest="base", help="the tree the evidence was written on "
                        "(default: git merge-base HEAD main)")
    options = parser.parse_args()
    base = options.base or git("merge-base", "HEAD", "main").stdout.strip()
    for arch in ARCHES:
        for kind in ("", "residual-", "argued-"):
            name = f"coverage-{kind}{arch}.json"
            if git("diff", "--quiet", base, "--", f"docs/certification/{name}", check=False).returncode:
                sys.exit(f"carry-coverage: {name} changed since {base}; carry from the commit that last wrote it")
    carrier = Carrier(base)
    report = []
    for arch in ARCHES:
        carry_residual(carrier, arch, report)
        carry_arguments(carrier, arch, report)
        carry_figures(carrier, arch)
    for line in report:
        print(line)
    changed = sum(1 for _, mapping in carrier.maps.values() if mapping is not None)
    print(f"carry-coverage: carried from {base[:12]}, {changed} file(s) of the evidence changed, "
          f"{len(report)} anchor(s) dropped as unmeasured")


if __name__ == "__main__":
    main()
