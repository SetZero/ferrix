#!/usr/bin/env bash
# Fetch Debian's bubblewrap and the libraries it was linked against, for
# `cargo xtask test-bwrap` (docs/NAMESPACES.md §8).
#
# Debian 13's `bwrap` is a position-independent executable that asks for
# glibc's `ld-linux` and needs `libc.so.6`, `libcap.so.2` and
# `libselinux.so.1`, which needs `libpcre2-8.so.0`. Every package is pinned
# by the SHA-256 Debian's own index gave for it on 2026-09-30, so a later
# point release changes nothing here until this script is changed. glibc is
# the one fetch-debian-busybox.sh pins.
#
# The six files land flat under `$FERRIX_BWRAP/x86_64/` (default
# ~/.local/share/ferrix/bwrap), where `test-bwrap` looks for them:
#
#   bwrap  ld.so  libc.so.6  libcap.so.2  libselinux.so.1  libpcre2-8.so.0
#
# x86-64 only: the gate is Steam's, and Steam's container is x86-64's.
#
# Needs curl, sha256sum and dpkg-deb. A mirror other than deb.debian.org can
# be named with DEBIAN_MIRROR.
#
# Usage: tools/common/fetch/fetch-bwrap.sh

set -euo pipefail

mirror=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
out=${FERRIX_BWRAP:-$HOME/.local/share/ferrix/bwrap}

# Pool path and SHA-256 of each package.
packages=(
    "main/b/bubblewrap/bubblewrap_0.12.0-1~deb13u1_amd64.deb 70aca4fa8daeacb677ec00e8063eb586f08ae3d94b1f11e684370b5524c43431"
    "main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb 967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"
    "main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb 89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67"
    "main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb 68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5"
    "main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb 1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e"
)

for tool in curl sha256sum dpkg-deb; do
    command -v "$tool" > /dev/null || { echo "fetch-bwrap: $tool is not installed" >&2; exit 1; }
done

mkdir -p "$out/pool"
tree=$(mktemp -d)
trap 'rm -rf "$tree"' EXIT
for entry in "${packages[@]}"; do
    read -r path sum <<< "$entry"
    deb="$out/pool/${path##*/}"
    if [ ! -f "$deb" ]; then
        curl -fsSL -o "$deb.part" "$mirror/pool/$path"
        mv "$deb.part" "$deb"
    fi
    echo "$sum  $deb" | sha256sum -c --quiet \
        || { echo "fetch-bwrap: $deb does not match its pinned checksum" >&2; rm -f "$deb"; exit 1; }
    dpkg-deb -x "$deb" "$tree"
done

lib=$tree/usr/lib/x86_64-linux-gnu
mkdir -p "$out/x86_64"
cp -L "$tree/usr/bin/bwrap" "$out/x86_64/bwrap"
cp -L "$lib/ld-linux-x86-64.so.2" "$out/x86_64/ld.so"
cp -L "$lib/libc.so.6" "$lib/libcap.so.2" "$lib/libselinux.so.1" "$lib/libpcre2-8.so.0" \
    "$out/x86_64/"
echo "x86_64: $out/x86_64"
