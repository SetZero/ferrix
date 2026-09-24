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

Beside it, `tools/glibc-versions/<arch>-compat.txt` lists the older versions
of a function that ferrousli's definition answers too, because glibc's older
version is the same function (see `table`): a program built against an older
glibc asks for those. build-shared.sh gives each an alias at that version.

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

# A dynamic symbol line: `Num: Value Size Type Bind Vis Ndx Name@@VERSION`,
# or `Name@VERSION` for a version that is not the default.
DEFINED = re.compile(
    r"^\s*\d+:\s+(\S+)\s+\S+\s+(\S+)\s+(GLOBAL|WEAK)\s+\S+\s+(?!UND)\S+\s+(\S+?)(@@?)(\S+)"
)

# Names whose newest glibc version changed the interface, where ferrousli
# keeps the older one. Each is written at its newest older version, so a
# program built against the newer interface fails to load, naming it, rather
# than calling a function that reads its arguments another way.
#
# glibc 2.42 made `speed_t` the baud rate itself, `B9600` 9600, where it had
# been the termios code, `B9600` 015. ferrousli's headers are musl's, with
# the codes, and its functions take the codes.
OLDER_INTERFACE = {"cfgetispeed", "cfgetospeed", "cfsetispeed", "cfsetospeed", "cfsetspeed"}

# libm families whose older version is another function, not the same one
# with the SVID error handling glibc retired: `totalorder` took its arguments
# by value before 2.31 and by pointer since, and `fromfp` returned `intmax_t`
# before 2.43 and a floating type since.
LIBM_OTHER_FUNCTION = re.compile(r"^(u?fromfpx?|totalorder)")

# libc names whose older version is another function, but one whose contract
# ferrousli's answers: `pthread_kill@GLIBC_2.2.5` is the function before 2.34,
# which returned ESRCH for a thread that had exited and not been joined.
# POSIX leaves that case undefined.
LIBC_SAME_CONTRACT = {"pthread_kill"}


def glibc_version_key(version: str) -> tuple[int, ...]:
    """`GLIBC_2.3.4` as (2, 3, 4), for ordering versions."""
    return tuple(int(part) for part in re.findall(r"\d+", version))


def table(directory: pathlib.Path) -> tuple[dict[str, str], dict[str, list[str]]]:
    """Each name's default version, and the older versions it answers too.

    glibc keeps a function's older version beside its default when the
    default moved. A program linked against the older glibc asks for the
    older one: Chrome, built against glibc 2.31, asks for
    `pthread_create@GLIBC_2.2.5`. Where the two are one function -- the same
    address, as every name that moved from `libpthread.so.0` into `libc.so.6`
    in 2.34 has -- or libm's older version is the same function with the SVID
    error handling that 2.27 to 2.43 retired name by name, the older version
    is recorded, and `build-shared.sh` makes ferrousli's definition answer it
    too.
    """
    versions: dict[str, str] = {}
    compat: dict[str, list[str]] = {}
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
        # Each name's definitions: (version, is the default, address, type).
        defined: dict[str, list[tuple[str, bool, str, str]]] = {}
        for line in out.splitlines():
            found = DEFINED.match(line)
            if not found:
                continue
            address, kind, name = found.group(1), found.group(2), found.group(4)
            default, version = found.group(5) == "@@", found.group(6)
            defined.setdefault(name, []).append((version, default, address, kind))
        for name, entries in defined.items():
            # A name is in one library; `libc.so.6`, read first, wins if two
            # ever disagree.
            defaults = [entry for entry in entries if entry[1]]
            if not defaults or name in versions:
                continue
            version, _, address, kind = defaults[0]
            older = [entry for entry in entries if not entry[1] and entry[0] != "GLIBC_PRIVATE"]
            if name in OLDER_INTERFACE and older:
                versions[name] = max((entry[0] for entry in older), key=glibc_version_key)
                continue
            versions[name] = version
            answered = {
                entry[0]
                for entry in older
                if kind in ("FUNC", "IFUNC")
                and (
                    entry[2] == address
                    or (library == "libm.so.6" and not LIBM_OTHER_FUNCTION.match(name))
                    or name in LIBC_SAME_CONTRACT
                )
            }
            if answered:
                compat[name] = sorted(answered, key=glibc_version_key)
    if "libc.so.6" not in {p.name for p in directory.iterdir()}:
        sys.exit(f"gen-glibc-versions: no libc.so.6 in {directory}")
    return versions, compat


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


def render_compat(arch: str, compat: dict[str, list[str]]) -> str:
    lines = [
        f"# Generated by tools/gen-glibc-versions.py for {arch}. Do not edit.",
        "# Older versions of a function that glibc keeps beside its default and",
        "# that ferrousli's one definition answers too: one `name VERSION` a line.",
    ]
    lines += [f"{name} {version}" for name, older in sorted(compat.items()) for version in older]
    return "\n".join(lines) + "\n"


def main() -> int:
    args = sys.argv[1:]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    if len(args) != 2 or args[0] not in ARCHES:
        sys.exit(__doc__)
    arch, directory = args[0], pathlib.Path(args[1])
    versions, compat = table(directory)
    if arch == "armv7a":
        for name in narrow_names(versions) | MISMATCHED:
            if name in versions:
                versions[name] = "-"
                compat.pop(name, None)
    outputs = {
        OUT / f"{arch}.txt": render(arch, versions),
        OUT / f"{arch}-compat.txt": render_compat(arch, compat),
    }
    if check:
        stale = 0
        for path, text in outputs.items():
            if not path.exists() or path.read_text(encoding="utf-8") != text:
                print(f"gen-glibc-versions: {path.relative_to(ROOT).as_posix()} is out of date")
                stale = 1
        return stale
    OUT.mkdir(parents=True, exist_ok=True)
    for path, text in outputs.items():
        path.write_text(text, encoding="utf-8", newline="\n")
    print(
        f"gen-glibc-versions: {len(versions)} symbols and "
        f"{sum(len(older) for older in compat.values())} older versions for {arch}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
