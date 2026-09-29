#!/usr/bin/env python3
"""Resolve a Debian package's dependency closure into pinned download lines.

Reads Debian 13's Packages indexes for one architecture -- trixie, and
trixie-updates and trixie-security, whose newer builds win -- and walks
`Pre-Depends` and `Depends` from the named package, taking the first of each
alternative and leaving out every package named with `--skip`: daemons,
maintainer-script tools and data nothing on a Ferrix volume runs. Prints one
line per package, `<archive> <pool path> <sha256>`, which is the list
`tools/common/fetch/fetch-chromium-arm64.sh` pins, and the total installed size.

The closure is where to start, not the answer: the fetch script's check that
every library an ELF file needs is on the volume is what says it is enough.

Usage: tools/common/gen/debian-closure.py --arch arm64 chromium --skip debconf ...
Needs curl-free network access (urllib), xz support in Python, and
`dpkg --compare-versions`.
"""

import argparse
import functools
import lzma
import subprocess
import sys
import urllib.request

INDEXES = [
    ("main", "https://deb.debian.org/debian/dists/trixie/main/binary-{arch}/Packages.xz"),
    ("updates", "https://deb.debian.org/debian/dists/trixie-updates/main/binary-{arch}/Packages.xz"),
    ("security", "https://security.debian.org/debian-security/dists/trixie-security/main/binary-{arch}/Packages.xz"),
]


def read(url, archive):
    """Every stanza of one Packages.xz, by package name."""
    with urllib.request.urlopen(url) as response:
        text = lzma.decompress(response.read()).decode()
    packages = {}
    for block in text.split("\n\n"):
        fields, key = {}, None
        for line in block.split("\n"):
            if line.startswith(" ") and key:
                fields[key] += line
            elif ":" in line:
                key, _, value = line.partition(":")
                fields[key] = value.strip()
        if "Package" in fields:
            fields["archive"] = archive
            packages.setdefault(fields["Package"], []).append(fields)
    return packages


def newer(a, b):
    """-1 when `a`'s version is above `b`'s, which sorts it first."""
    above = subprocess.run(["dpkg", "--compare-versions", a["Version"], "gt", b["Version"]]).returncode
    return -1 if above == 0 else 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", required=True)
    parser.add_argument("package")
    parser.add_argument("--skip", nargs="*", default=[])
    args = parser.parse_args()

    indexes = [read(url.format(arch=args.arch), archive) for archive, url in INDEXES]
    provides = {}
    for index in indexes:
        for name, stanzas in index.items():
            for stanza in stanzas:
                for provided in stanza.get("Provides", "").split(","):
                    provided = provided.strip().split(" ")[0]
                    if provided:
                        provides.setdefault(provided, name)

    def newest(name):
        found = [stanza for index in indexes for stanza in index.get(name, [])]
        return sorted(found, key=functools.cmp_to_key(newer))[0] if found else None

    skip, chosen, queue = set(args.skip), {}, [args.package]
    while queue:
        name = queue.pop()
        if name in chosen or name in skip:
            continue
        stanza = newest(name)
        if stanza is None:
            real = provides.get(name)
            if real:
                queue.append(real)
            else:
                print(f"debian-closure: nothing is {name}", file=sys.stderr)
            continue
        chosen[name] = stanza
        for depends in (stanza.get("Pre-Depends", "") + "," + stanza.get("Depends", "")).split(","):
            first = depends.strip().split("|")[0].strip().split(" ")[0].split(":")[0]
            if first:
                queue.append(first)

    total = 0
    for name in sorted(chosen):
        stanza = chosen[name]
        total += int(stanza.get("Installed-Size", "0"))
        print(f'    "{stanza["archive"]} {stanza["Filename"]} {stanza["SHA256"]}"')
    print(f"debian-closure: {len(chosen)} packages, {total // 1024} MiB installed", file=sys.stderr)


if __name__ == "__main__":
    main()
