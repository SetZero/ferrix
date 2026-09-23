#!/usr/bin/env python3
"""Record which version glibc gives each of its symbols, for one architecture.

A program linked against glibc asks for `printf@GLIBC_2.2.5`, not `printf`:
the version is part of the name it binds to. For `libferrousli.so` to stand
in glibc's place it has to define each symbol at the version glibc's own
libraries give it by default -- the `@@` one, which is what every program
linked since that version asks for.

This reads that from a glibc installation with binutils' `readelf`, and
writes `tools/glibc-versions/<arch>.txt`: one `name VERSION` line per
default-version symbol of the libraries ferrousli stands in for, sorted. It
records names and version strings only, the interface glibc publishes, and
nothing of glibc's code. `tools/build-shared.sh` turns it into the version
script the shared library is linked with.

On ARMv7-A glibc keeps two functions for many names: the old one with a
32-bit `time_t` or `off_t` under the plain name, and the 64-bit one under
another, which a program built with `_TIME_BITS=64` or `_FILE_OFFSET_BITS=64`
calls instead (`time` and `__time64`, `lseek` and `lseek64`, `stat` and
`__stat64_time64`). ferrousli has only the 64-bit forms, so each such plain
name is written with the version `-`: build-shared.sh leaves it out of the
library, and a program that needs the 32-bit form fails to load, naming it,
rather than calling a function with the wrong structure.

Usage: gen-glibc-versions.py <arch> <directory holding libc.so.6 and the rest>
       gen-glibc-versions.py <arch> <directory> --check
"""

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "tools" / "glibc-versions"

# glibc's libraries whose names ferrousli's one library answers to.
LIBRARIES = (
    "libc.so.6",
    "libm.so.6",
    "libresolv.so.2",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libutil.so.1",
    "libanl.so.1",
)

ARCHES = ("x86_64", "aarch64", "armv7a")

# ARMv7-A names glibc and musl both use, for different structures, which
# libferrousli.a answers with musl's meaning: `__clock_adjtime64` takes
# musl's `struct timex` there and the kernel's in glibc. Left out of
# libc.so.6 with the 32-bit names.
MISMATCHED = {"__clock_adjtime64"}

# A dynamic symbol line: `Num: Value Size Type Bind Vis Ndx Name@@VERSION`.
DEFINED = re.compile(r"^\s*\d+:\s+\S+\s+\S+\s+\S+\s+(GLOBAL|WEAK)\s+\S+\s+(?!UND)\S+\s+(\S+?)@@(\S+)")


def table(directory: pathlib.Path) -> dict[str, str]:
    versions: dict[str, str] = {}
    for library in LIBRARIES:
        path = directory / library
        if not path.exists():
            continue
        out = subprocess.run(
            ["readelf", "--dyn-syms", "-W", str(path)],
            check=True,
            capture_output=True,
            text=True,
            env={"LC_ALL": "C", "PATH": "/usr/bin:/bin"},
        ).stdout
        for line in out.splitlines():
            found = DEFINED.match(line)
            if not found:
                continue
            name, version = found.group(2), found.group(3)
            # A name is in one library; `libc.so.6`, read first, wins if two
            # ever disagree.
            versions.setdefault(name, version)
    if "libc.so.6" not in {p.name for p in directory.iterdir()}:
        sys.exit(f"gen-glibc-versions: no libc.so.6 in {directory}")
    return versions


def time64_base(name: str) -> str | None:
    """The name a glibc time64 entry point replaces, or None if it is not one.

    `__clock_gettime64` replaces `clock_gettime`, `__gmtime64_r` `gmtime_r`,
    `__stat64_time64` `stat64`, and `___adjtimex64` `adjtimex`.
    """
    if not name.startswith("__"):
        return None
    bare = name.lstrip("_")
    if bare.endswith("_time64"):
        return bare[: -len("_time64")]
    if bare.endswith("64_r"):
        return bare[: -len("64_r")] + "_r"
    if bare.endswith("64"):
        return bare[: -len("64")]
    return None


def narrow_names(versions: dict[str, str]) -> set[str]:
    """Every name glibc answers only with a 32-bit `time_t` or `off_t`.

    A time64 entry point is one of the `__...64` names glibc added at 2.34 or
    later; what it replaces is left out, and so is that name's own 32-bit
    `off_t` form (`stat64` replaced, so `stat` too). And a name whose `64`
    twin glibc also has is the 32-bit `off_t` form of it (`lseek` beside
    `lseek64`, `readdir_r` beside `readdir64_r`).
    """
    def at_least_2_34(version: str) -> bool:
        found = re.fullmatch(r"GLIBC_2\.(\d+)(?:\.\d+)?", version)
        return bool(found) and int(found.group(1)) >= 34

    narrow: set[str] = set()
    for name, version in versions.items():
        base = time64_base(name) if at_least_2_34(version) else None
        if base is not None and base in versions:
            narrow.add(base)
            if "64" in base:
                narrow.add(base.replace("64", "", 1))
        # `acosf64` and `strtof64_l` are `_Float64`'s names, which have
        # nothing to do with `off_t`: `acosf` is not a 32-bit form of them.
        floatn = re.search(r"f64x?(_l|_r)?$", name)
        if "64" in name and time64_base(name) is None and not floatn:
            plain = name.replace("64", "", 1)
            if plain != name and plain in versions:
                narrow.add(plain)
    return {name for name in narrow if name in versions}


def render(arch: str, versions: dict[str, str]) -> str:
    lines = [
        f"# Generated by tools/gen-glibc-versions.py for {arch}. Do not edit.",
        "# Each symbol glibc's libraries define, and the version they define it",
        "# at by default: the version a program linked against glibc asks for.",
    ]
    if arch == "armv7a":
        lines.append("# `-`: glibc's 32-bit time_t or off_t form, which ferrousli does not export.")
    lines += [f"{name} {version}" for name, version in sorted(versions.items())]
    return "\n".join(lines) + "\n"


def main() -> int:
    args = sys.argv[1:]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    if len(args) != 2 or args[0] not in ARCHES:
        sys.exit(__doc__)
    arch, directory = args[0], pathlib.Path(args[1])
    versions = table(directory)
    if arch == "armv7a":
        for name in narrow_names(versions) | MISMATCHED:
            if name in versions:
                versions[name] = "-"
    text = render(arch, versions)
    path = OUT / f"{arch}.txt"
    if check:
        if not path.exists() or path.read_text(encoding="utf-8") != text:
            print(f"gen-glibc-versions: {path.relative_to(ROOT).as_posix()} is out of date")
            return 1
        return 0
    OUT.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")
    print(f"gen-glibc-versions: {len(text.splitlines()) - 3} symbols for {arch}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
