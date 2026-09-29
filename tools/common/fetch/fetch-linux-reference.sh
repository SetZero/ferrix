#!/usr/bin/env bash
# Fetch the stock Linux kernel the seam measurement reads the same QEMU disk
# with, as the in-kernel reference docs/BACKLOG.md's "seam measured, 1" row
# asks for (docs/OPAQUE-KERNEL.md, S0).
#
# Debian 13's cloud kernel: virtio and virtio-pci are built in, virtio-blk is
# a module, so the module is taken from the same package. Both packages are
# pinned by the SHA-256 Debian's own index gave for them on 2026-09-27, so a
# later point release changes nothing here until this script is changed.
#
# Each architecture's two files land under
# `$FERRIX_LINUX_REFERENCE/<arch>/` (default ~/.local/share/ferrix/linux-ref),
# in Ferrix's architecture names: `vmlinuz`, and `virtio_blk.ko`, decompressed
# so that busybox's insmod loads it.
#
# Needs curl, sha256sum, dpkg-deb and xz. A mirror other than deb.debian.org
# can be named with DEBIAN_MIRROR.
#
# Usage: tools/common/fetch/fetch-linux-reference.sh

set -euo pipefail

mirror=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
out=${FERRIX_LINUX_REFERENCE:-$HOME/.local/share/ferrix/linux-ref}
release=6.12.107+deb13

# Ferrix's arch, Debian's arch, then the package's pool path and SHA-256.
packages=(
    "x86_64 amd64
     main/l/linux-signed-amd64/linux-image-${release}-cloud-amd64_6.12.107-1_amd64.deb
     04434ff520f860423eafe4203d986a35682ce73c2f71baa39e988ca0da93ac91"
    "aarch64 arm64
     main/l/linux-signed-arm64/linux-image-${release}-cloud-arm64_6.12.107-1_arm64.deb
     a1bbfc22454e984feba6353f957152ef416728b627ec0cf94d3ac04b18c14d84"
)

for tool in curl sha256sum dpkg-deb xz; do
    command -v "$tool" > /dev/null || { echo "fetch-linux-reference: $tool is not installed" >&2; exit 1; }
done

mkdir -p "$out/pool"
for entry in "${packages[@]}"; do
    # Split on any whitespace, newlines included, which `read` would stop at.
    # shellcheck disable=SC2086
    set -- $entry
    arch=$1 debian=$2 path=$3 sum=$4
    deb="$out/pool/${path##*/}"
    if [ ! -f "$deb" ]; then
        curl -fsSL -o "$deb.part" "$mirror/pool/$path"
        mv "$deb.part" "$deb"
    fi
    echo "$sum  $deb" | sha256sum -c --quiet || {
        echo "fetch-linux-reference: $deb is not the pinned package" >&2
        exit 1
    }
    tree=$(mktemp -d)
    dpkg-deb -x "$deb" "$tree"
    mkdir -p "$out/$arch"
    cp "$tree/boot/vmlinuz-${release}-cloud-${debian}" "$out/$arch/vmlinuz"
    xz -dc "$tree/usr/lib/modules/${release}-cloud-${debian}/kernel/drivers/block/virtio_blk.ko.xz" \
        > "$out/$arch/virtio_blk.ko"
    rm -rf "$tree"
    echo "fetch-linux-reference: $arch: $out/$arch/vmlinuz and virtio_blk.ko"
done
