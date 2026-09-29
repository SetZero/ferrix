#!/usr/bin/env bash
# Fetch Debian's dynamically linked busybox and the glibc it was linked
# against, for the first half of the dynamic-linking exit in docs/ROADMAP.md.
#
# Debian 13's `busybox` (not `busybox-static`) is a position-independent
# executable that asks for glibc's `ld-linux` and needs `libc.so.6` and
# `libresolv.so.2`. Both packages are pinned by the SHA-256 Debian's own
# index gave for them on 2026-09-21, so a later point release changes
# nothing here until this script is changed.
#
# Each architecture's four files land flat under
# `$FERRIX_DEBIAN_BUSYBOX/<arch>/` (default ~/.local/share/ferrix/busybox/debian),
# in Ferrix's architecture names, so one `{arch}` template serves every run:
#
#   D=~/.local/share/ferrix/busybox/debian
#   cargo xtask test-shell --arch all --init "$D/{arch}/busybox" \
#       --interpreter "$D/{arch}/ld.so" \
#       --library "$D/{arch}/libc.so.6" --library "$D/{arch}/libresolv.so.2"
#
# `i386` is not one of Ferrix's architectures but a program it runs on
# x86-64 (docs/I386.md, I4), so its four files land in `$D/i386/` and are
# named in an `--arch x86_64` run:
#
#   cargo xtask test-shell --arch x86_64 --init "$D/i386/busybox" \
#       --interpreter "$D/i386/ld.so" \
#       --library "$D/i386/libc.so.6" --library "$D/i386/libresolv.so.2"
#
# Needs curl, sha256sum and dpkg-deb. A mirror other than deb.debian.org can
# be named with DEBIAN_MIRROR.
#
# Usage: scripts/fetch/fetch-debian-busybox.sh

set -euo pipefail

mirror=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
out=${FERRIX_DEBIAN_BUSYBOX:-$HOME/.local/share/ferrix/busybox/debian}

# Ferrix's arch, Debian's arch (for the reader), multiarch directory, linker, then each package as
# pool path and SHA-256.
packages=(
    "x86_64 amd64 x86_64-linux-gnu ld-linux-x86-64.so.2
     main/b/busybox/busybox_1.37.0-6+b9_amd64.deb b88c3b899e16bf64347d63235128df9efbcf1f964e4841bc9b37d7856bed35bf
     main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb 967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"
    "aarch64 arm64 aarch64-linux-gnu ld-linux-aarch64.so.1
     main/b/busybox/busybox_1.37.0-6+b9_arm64.deb 38bc174119c9f1f07073d1dc28464b672eb0a1402fdee3022198e3138420e3a0
     main/g/glibc/libc6_2.41-12+deb13u4_arm64.deb 8784eda966b189c777a384dac5ce009e8fc9b52d006926c5a013e7fa8aa688cc"
    "armv7a armhf arm-linux-gnueabihf ld-linux-armhf.so.3
     main/b/busybox/busybox_1.37.0-6+b9_armhf.deb 4427d67d42f782494b730c2c817a0c99cb74752dc993787419a5c849b6fdb3f4
     main/g/glibc/libc6_2.41-12+deb13u4_armhf.deb 4fc6fed8d77d01c0dacb5fda8b880a8c8a669575936de5761b693077a6f6c2ec"
    "i386 i386 i386-linux-gnu ld-linux.so.2
     main/b/busybox/busybox_1.37.0-6+b9_i386.deb f582430a73981f4692e24988353ad1714bf4d4e2541e8aa5f8d3c844d9615c55
     main/g/glibc/libc6_2.41-12+deb13u4_i386.deb 67bb4b54cf4bb6a380449dfa1852cd9135659f85360caa0e41d312f4b30e573a"
)

for tool in curl sha256sum dpkg-deb; do
    command -v "$tool" > /dev/null || { echo "fetch-debian-busybox: $tool is not installed" >&2; exit 1; }
done

mkdir -p "$out/pool"
for entry in "${packages[@]}"; do
    # Split on any whitespace, newlines included, which `read` would stop at.
    # shellcheck disable=SC2086
    set -- $entry
    arch=$1 multiarch=$3 linker=$4 busybox=$5 busybox_sum=$6 libc=$7 libc_sum=$8
    tree=$(mktemp -d)
    for pair in "$busybox $busybox_sum" "$libc $libc_sum"; do
        read -r path sum <<< "$pair"
        deb="$out/pool/${path##*/}"
        if [ ! -f "$deb" ]; then
            curl -fsSL -o "$deb.part" "$mirror/pool/$path"
            mv "$deb.part" "$deb"
        fi
        echo "$sum  $deb" | sha256sum -c --quiet \
            || { echo "fetch-debian-busybox: $deb does not match its pinned checksum" >&2; rm -f "$deb"; exit 1; }
        dpkg-deb -x "$deb" "$tree"
    done
    mkdir -p "$out/$arch"
    cp -L "$tree/usr/bin/busybox" "$out/$arch/busybox"
    cp -L "$tree/usr/lib/$multiarch/$linker" "$out/$arch/ld.so"
    cp -L "$tree/usr/lib/$multiarch/libc.so.6" "$tree/usr/lib/$multiarch/libresolv.so.2" "$out/$arch/"
    rm -rf "$tree"
    echo "$arch: $out/$arch"
done
