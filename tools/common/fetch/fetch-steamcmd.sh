#!/usr/bin/env bash
# Fetch Valve's steamcmd and Debian's i386 glibc, and put them on a btrfs
# volume: the disk `cargo xtask test-steamcmd` runs Steam's text-mode client
# from (docs/I386.md, I5a).
#
# steamcmd is the Steam client's core without its display: the same 32-bit
# glibc program family, which updates itself from Valve's servers, speaks
# Steam's protocol over TCP and logs in. Nothing here is built. The volume
# holds Debian 13's `libc6:i386` at `usr/lib/i386-linux-gnu` -- the loader,
# `libc.so.6` and the `libdl`, `librt`, `libm` and `libpthread` steamcmd
# names, and `libgcc_s.so.1` for the `libstdc++` it carries -- Debian's
# certificate authorities as the bundle `/etc/ssl/certs/ca-certificates.crt`
# steamcmd's OpenSSL reads, and steamcmd, unpacked and not yet updated, under `steamcmd/`.
# Ferrix mounts it at `/data`, and the test's image links the directories
# glibc names into it, as test-chrome's does. steamcmd's first run on Ferrix
# downloads its own update, as it does anywhere, so the test needs the
# network and room on the volume for it.
#
# Pinned by SHA-256: Debian's Packages file for trixie on 2026-09-27, and
# steamcmd_linux.tar.gz as downloaded on 2026-09-27. Valve replaces that
# tarball in place and publishes no digest, so when it changes the download
# fails here rather than running something unchecked; take the new file's
# hash and change STEAMCMD_SHA256.
#
# Writes $FERRIX_STEAMCMD_VOLUME/steamcmd.img (default
# ~/.local/share/ferrix/steamcmd/steamcmd.img). Needs curl, sha256sum,
# dpkg-deb, tar, readelf and mkfs.btrfs; no root.
#
# Usage: scripts/fetch/fetch-steamcmd.sh

set -euo pipefail

out=${FERRIX_STEAMCMD_VOLUME:-$HOME/.local/share/ferrix/steamcmd}
debian=${DEBIAN_MIRROR:-https://deb.debian.org/debian}

LIBC6=main/g/glibc/libc6_2.41-12+deb13u4_i386.deb
LIBC6_SHA256=67bb4b54cf4bb6a380449dfa1852cd9135659f85360caa0e41d312f4b30e573a
# What steamcmd's own libstdc++ needs beside glibc.
LIBGCC=main/g/gcc-14/libgcc-s1_14.2.0-19_i386.deb
LIBGCC_SHA256=a4c71fd856d2a48a7505a087b4186e3cca23f94603c05e3fb7c799b27e72f761
# The trust store steamcmd's OpenSSL reads, as one bundle.
CA_CERTIFICATES=main/c/ca-certificates/ca-certificates_20250419_all.deb
CA_CERTIFICATES_SHA256=ef590f89563aa4b46c8260d49d1cea0fc1b181d19e8df3782694706adf05c184
STEAMCMD=https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz
STEAMCMD_SHA256=cebf0046bfd08cf45da6bc094ae47aa39ebf4155e5ede41373b579b8f1071e7c

for tool in curl sha256sum dpkg-deb tar readelf mkfs.btrfs; do
    command -v "$tool" > /dev/null || { echo "fetch-steamcmd: $tool is not installed" >&2; exit 1; }
done

# Download $1 to the pool as $2 unless it is there, and check it against $3.
fetch() {
    local url=$1 name=$2 sum=$3
    local file="$out/pool/$name"
    if [ ! -f "$file" ]; then
        curl -fsSL -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    echo "$sum  $file" | sha256sum -c --quiet \
        || { echo "fetch-steamcmd: $file does not match its pinned checksum" >&2; rm -f "$file"; exit 1; }
    printf '%s\n' "$file"
}

mkdir -p "$out/pool"
tree="$out/tree"
rm -rf "$tree"
mkdir -p "$tree/steamcmd"

dpkg-deb -x "$(fetch "$debian/pool/$LIBC6" "${LIBC6##*/}" "$LIBC6_SHA256")" "$tree"
dpkg-deb -x "$(fetch "$debian/pool/$LIBGCC" "${LIBGCC##*/}" "$LIBGCC_SHA256")" "$tree"
dpkg-deb -x "$(fetch "$debian/pool/$CA_CERTIFICATES" "${CA_CERTIFICATES##*/}" \
    "$CA_CERTIFICATES_SHA256")" "$tree"
# What `update-ca-certificates` makes when the package is installed, which
# unpacking it does not run: every certificate it ships, in one file, where
# steamcmd opens it.
mkdir -p "$tree/etc/ssl/certs"
find "$tree/usr/share/ca-certificates" -name '*.crt' -print0 | sort -z \
    | xargs -0 cat > "$tree/etc/ssl/certs/ca-certificates.crt"
test -s "$tree/etc/ssl/certs/ca-certificates.crt" \
    || { echo "fetch-steamcmd: the certificate bundle is empty" >&2; exit 1; }
tar -xzf "$(fetch "$STEAMCMD" steamcmd_linux.tar.gz "$STEAMCMD_SHA256")" -C "$tree/steamcmd"

# Documentation and manuals, which nothing runs.
rm -rf "$tree/usr/share/doc" "$tree/usr/share/man" "$tree/usr/share/lintian"

test -x "$tree/steamcmd/linux32/steamcmd" \
    || { echo "fetch-steamcmd: no linux32/steamcmd in the tarball" >&2; exit 1; }
test -e "$tree/usr/lib/i386-linux-gnu/ld-linux.so.2" \
    || { echo "fetch-steamcmd: no i386 loader at usr/lib/i386-linux-gnu" >&2; exit 1; }

# Every library an ELF file on the volume needs must be on it: in glibc's
# directory, or beside steamcmd.
missing=0
while IFS= read -r -d '' file; do
    head -c 4 "$file" | grep -q $'\x7fELF' || continue
    for needed in $(readelf -d "$file" 2> /dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
        if [ ! -e "$tree/usr/lib/i386-linux-gnu/$needed" ] \
            && [ ! -e "$tree/steamcmd/linux32/$needed" ]; then
            echo "fetch-steamcmd: ${file#"$tree"/} needs $needed, which is not on the volume" >&2
            missing=1
        fi
    done
done < <(find "$tree/steamcmd/linux32" -maxdepth 1 -type f -print0)
[ "$missing" = 0 ] || exit 1

# Room for steamcmd's own update, which it downloads and unpacks beside
# itself on its first run, and for its logs and caches.
size=$(( $(du -sm "$tree" | cut -f1) + 1536 ))
image="$out/steamcmd.img"
rm -f "$image"
truncate -s "${size}M" "$image"
mkfs.btrfs -q --rootdir "$tree" "$image"
# The tree stays beside the image, so the same files can be tried on the host.
echo "steamcmd volume: $image (${size} MiB), unpacked in $tree"
