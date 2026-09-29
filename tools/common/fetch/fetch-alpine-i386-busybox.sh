#!/usr/bin/env bash
# Fetch Alpine's static i386 busybox, the program I3 of docs/I386.md boots:
# a 32-bit ELF, linked against musl 1.2.5, that runs on the x86-64 kernel
# through the IA-32 system call entry.
#
# Alpine 3.22's `busybox-static` for `x86`, pinned by the SHA-256 of the
# package fetched on 2026-09-27. Alpine replaces a package's revision in
# place, so when the mirror has moved past r20 the download fails here
# rather than running something unchecked; take the new revision's hash
# from its APKINDEX and change the two lines below.
#
# The program lands at `$FERRIX_ALPINE_I386/x86/bin/busybox.static`
# (default ~/.local/share/ferrix/busybox), beside the per-architecture
# ones, and runs as:
#
#   cargo xtask test-shell --arch x86_64 \
#       --init ~/.local/share/ferrix/busybox/x86/bin/busybox.static
#
# Needs curl, sha256sum and tar. A mirror other than dl-cdn.alpinelinux.org
# can be named with ALPINE_MIRROR.
#
# Usage: tools/common/fetch/fetch-alpine-i386-busybox.sh

set -euo pipefail

mirror=${ALPINE_MIRROR:-https://dl-cdn.alpinelinux.org/alpine}
out=${FERRIX_ALPINE_I386:-$HOME/.local/share/ferrix/busybox}

package=v3.22/main/x86/busybox-static-1.37.0-r20.apk
sha256=1c7ed0ce5f367dbab335bb66d32b8762be4deea969b19190382a65c01314825c

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -fsSL -o "$work/package.apk" "$mirror/$package"
echo "$sha256  $work/package.apk" | sha256sum -c --quiet
# An apk is concatenated gzip streams: the signature, the control data,
# then the files. tar reads through all three and extracts the files.
tar -xzf "$work/package.apk" -C "$work" bin/busybox.static 2>/dev/null
mkdir -p "$out/x86/bin"
install -m 0755 "$work/bin/busybox.static" "$out/x86/bin/busybox.static"
echo "$out/x86/bin/busybox.static"
