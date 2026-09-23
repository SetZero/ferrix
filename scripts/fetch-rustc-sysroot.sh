#!/usr/bin/env bash
# Fetch the upstream Rust compiler, Cargo and the Debian userland they need,
# and put them on a btrfs volume: the disk stage 16's exit compiles on
# (docs/ROADMAP.md, `cargo xtask test-rustc`), and the toolchain stage 20's
# `cargo xtask test-selfhost` builds Ferrix with.
#
# Nothing here is built. rustc and Cargo are the rust-lang.org releases, both
# dynamically linked against glibc; rustc's LLVM is a shared library of its
# own. rustc links through `cc`, which is Debian's gcc 14 driver, and gcc runs
# `collect2`, which runs the `ld.lld` rustc points it at, which runs
# `rust-lld`. Every one of those is somebody else's glibc binary, and the
# glibc is Debian 13's, the same one scripts/fetch-debian-busybox.sh pins.
# Beside the host's standard library are the two Ferrix's x86-64 image is
# built against: `x86_64-unknown-none` for the kernel and its native
# programs, `x86_64-unknown-uefi` for the loader.
#
# The volume holds a Debian-shaped tree at its root -- `usr/bin`,
# `usr/lib/x86_64-linux-gnu`, `usr/lib64`, `usr/lib/gcc`, `usr/libexec` --
# and the toolchain under `rust/`. Ferrix mounts it at `/data`, and its images
# link the directories glibc's and gcc's own paths name into it.
#
# Every download is pinned by the SHA-256 its own index gave: Debian's
# Packages file on 2026-09-22, and the channel manifest
# channel-rust-1.97.1.toml (the `xz_hash` of each component).
# Only what running the compiler and linking a program needs is taken: no
# `cc1`, since nothing is compiled from C, and no binutils, since the linker
# is rust-lld.
#
# Writes $FERRIX_RUSTC_SYSROOT/rustc.img (default
# ~/.local/share/ferrix/rustc/rustc.img). Needs curl, sha256sum, dpkg-deb, tar
# and mkfs.btrfs; no root, since mkfs.btrfs --rootdir writes an image file.
#
# Usage: scripts/fetch-rustc-sysroot.sh

set -euo pipefail

debian=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
rust=${RUST_DIST_SERVER:-https://static.rust-lang.org}
out=${FERRIX_RUSTC_SYSROOT:-$HOME/.local/share/ferrix/rustc}

# Pool path and SHA-256 of each Debian 13 package.
debs=(
    "main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb 967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"
    "main/g/glibc/libc6-dev_2.41-12+deb13u4_amd64.deb 1fda734dabcd80b77266a09ab62b0f1e3e16d8091db62899890b2200745632a2"
    "main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb 3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150"
    "main/g/gcc-14/libgcc-14-dev_14.2.0-19_amd64.deb ca6f2d36d96b19b3eb71405b0b80134d8c89380b02204a2512e5c58ceb090628"
    "main/g/gcc-14/gcc-14-x86-64-linux-gnu_14.2.0-19_amd64.deb a17ef039f1ba482051c3efb5c2c24070002e60dd0bd09fd456ec31481a11b725"
    "main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb 015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d"
)

# Path under the dist server and SHA-256 of each Rust component.
components=(
    "dist/2026-07-16/rustc-1.97.1-x86_64-unknown-linux-gnu.tar.xz 9819d0a32d56bd339585319c80260e332779f5541fd66838ab7e016d6c814819"
    "dist/2026-07-16/rust-std-1.97.1-x86_64-unknown-linux-gnu.tar.xz 1c1e704ae80126b7de34f72ea2825f7fd01736dec20732faed47374b95282fba"
    "dist/2026-07-16/rust-std-1.97.1-x86_64-unknown-none.tar.xz 24e213f586ecb1811a11bd40dbb53690fbae469cce89dc60f7cf20eaeaaeab29"
    "dist/2026-07-16/rust-std-1.97.1-x86_64-unknown-uefi.tar.xz 35f18a13185697e26540ef139de4408689fa77fb6427b355e0a9954d632f20fc"
    "dist/2026-07-16/cargo-1.97.1-x86_64-unknown-linux-gnu.tar.xz e1be5f5ff7f7f80ca506fb65770b759edbdc6d303781ed71c5de8ec8a8394779"
)

for tool in curl sha256sum dpkg-deb tar mkfs.btrfs; do
    command -v "$tool" > /dev/null || { echo "fetch-rustc-sysroot: $tool is not installed" >&2; exit 1; }
done

# Download $2/$1 into the pool unless it is there, and check it against $3.
fetch() {
    local path=$1 base=$2 sum=$3
    local file="$out/pool/${path##*/}"
    if [ ! -f "$file" ]; then
        curl -fsSL -o "$file.part" "$base/$path"
        mv "$file.part" "$file"
    fi
    echo "$sum  $file" | sha256sum -c --quiet \
        || { echo "fetch-rustc-sysroot: $file does not match its pinned checksum" >&2; rm -f "$file"; exit 1; }
    printf '%s\n' "$file"
}

mkdir -p "$out/pool"
tree="$out/tree"
rm -rf "$tree"
mkdir -p "$tree"

for entry in "${debs[@]}"; do
    read -r path sum <<< "$entry"
    dpkg-deb -x "$(fetch "$path" "$debian/pool" "$sum")" "$tree"
done

# Each component carries an install.sh and a `components` list; its files
# are the directories beside them, laid out from the prefix down.
unpacked=$(mktemp -d)
trap 'rm -rf "$unpacked"' EXIT
for entry in "${components[@]}"; do
    read -r path sum <<< "$entry"
    tar -xf "$(fetch "$path" "$rust" "$sum")" -C "$unpacked"
done
mkdir -p "$tree/rust"
for component in "$unpacked"/*/*/; do
    case "$component" in
        */rustc/ | */cargo/ | */rust-std-x86_64-unknown-*/) cp -a "$component". "$tree/rust/" ;;
    esac
done

# What Debian's postinst scripts and alternatives would have made. `cc` is
# the name rustc runs.
ln -sf x86_64-linux-gnu-gcc-14 "$tree/usr/bin/cc"
ln -sf x86_64-linux-gnu-gcc-14 "$tree/usr/bin/gcc"

# Documentation and manuals, which nothing runs.
rm -rf "$tree/usr/share/doc" "$tree/usr/share/man" "$tree/usr/share/lintian" "$tree/rust/share"

test -x "$tree/rust/bin/rustc" || { echo "fetch-rustc-sysroot: no rustc in the tree" >&2; exit 1; }
test -x "$tree/rust/bin/cargo" || { echo "fetch-rustc-sysroot: no cargo in the tree" >&2; exit 1; }
test -x "$tree/rust/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld" \
    || { echo "fetch-rustc-sysroot: no rust-lld in the tree" >&2; exit 1; }
for target in x86_64-unknown-none x86_64-unknown-uefi; do
    test -d "$tree/rust/lib/rustlib/$target/lib" \
        || { echo "fetch-rustc-sysroot: no standard library for $target in the tree" >&2; exit 1; }
done
test -e "$tree/usr/lib64/ld-linux-x86-64.so.2" \
    || { echo "fetch-rustc-sysroot: no linker at usr/lib64" >&2; exit 1; }

# Room to spare for what the compile writes when it runs on the volume
# itself, and a size that does not depend on how mkfs rounds.
size=$(( $(du -sm "$tree" | cut -f1) + 1024 ))
image="$out/rustc.img"
rm -f "$image"
truncate -s "${size}M" "$image"
mkfs.btrfs -q --rootdir "$tree" "$image"
# The tree stays beside the image, so the same files can be tried on the host.
echo "rustc sysroot: $image (${size} MiB), unpacked in $tree"
