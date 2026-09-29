#!/usr/bin/env bash
# Fetch Valve's Steam client bootstrap and the Debian userland it runs on,
# and put them on a btrfs volume: the disk `cargo xtask test-steam-bootstrap`
# runs `steam.sh` from (docs/I386.md, I5b).
#
# The bootstrap is what Valve's `steam-launcher` package installs as
# `/usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz` and `/usr/bin/steam`
# unpacks into the user's Steam directory: `steam.sh`, the 32-bit
# `ubuntu12_32/steam` and the Steam Runtime 1 ("scout") beside it. Its first
# run downloads the client proper (about 500 MB, 2 GB unpacked), installs it
# and starts again, then loads `steamui.so` and asks for an X display.
#
# Nothing here is built. The volume holds, from Debian 13:
#
# * i386: `libc6` and `libgcc-s1`, and the GL dispatch libraries
#   `steamui.so` names -- `libGL.so.1` from `libgl1`, `libglvnd0` and
#   `libglx0` -- and `libdrm.so.2`. `steam.sh` reports them missing and
#   `steamui.so` does not load without them; scout does not carry them.
# * amd64: `libc6`, for scout's own 64-bit helpers
#   (`steam-runtime-check-requirements`, `srt-logger`,
#   `steam-runtime-identify-library-abi`) and `libcap2` for its `srt-bwrap`,
#   and the shell tools `steam.sh`
#   and scout's `setup.sh` call beyond what busybox answers the same way:
#   `bash`, GNU `tar` (`--checkpoint-action`) with `xz`, `grep`, `sed`,
#   `mawk`, `find`, `which`, and `ldd`, which `steam.sh` runs over the client.
# * The certificate bundle the client's libcurl reads.
#
# Steam's bootstrap goes under `steam/`, and `home/` is the user's home.
# Ferrix mounts the volume at `/data`, and the test's image links the paths
# glibc names into it.
#
# Pinned by SHA-256: Debian's Packages files for trixie on 2026-09-28, and
# `steam-launcher_1.0.0.87_amd64.deb` from repo.steampowered.com's stable
# Packages on 2026-09-28. When Valve replaces it, take the new file's name
# and hash from https://repo.steampowered.com/steam/dists/stable/steam/binary-amd64/Packages.
#
# Writes $FERRIX_STEAM_VOLUME/steam.img (default
# ~/.local/share/ferrix/steam-bootstrap/steam.img). Needs curl, sha256sum,
# dpkg-deb, tar, xz, readelf and mkfs.btrfs; no root.
#
# Usage: tools/common/fetch/fetch-steam.sh

set -euo pipefail

out=${FERRIX_STEAM_VOLUME:-$HOME/.local/share/ferrix/steam-bootstrap}
debian=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
valve=${STEAM_MIRROR:-https://repo.steampowered.com/steam}

LAUNCHER=pool/steam/s/steam/steam-launcher_1.0.0.87_amd64.deb
LAUNCHER_SHA256=765aba9a0ed339a50226ceb614fcc9879a991ba184098bc8de920efb12c714a4

# Debian's packages: the pool path, then its SHA-256.
DEBS=(
    # i386: glibc, and what steamui.so needs that scout does not carry.
    pool/main/g/glibc/libc6_2.41-12+deb13u4_i386.deb 67bb4b54cf4bb6a380449dfa1852cd9135659f85360caa0e41d312f4b30e573a
    pool/main/g/gcc-14/libgcc-s1_14.2.0-19_i386.deb a4c71fd856d2a48a7505a087b4186e3cca23f94603c05e3fb7c799b27e72f761
    pool/main/libg/libglvnd/libgl1_1.7.0-1+b2_i386.deb 2d61aa7c7b39a07e3bc691239b524fee99a2fc92da2b6706b518db7b0e315554
    pool/main/libg/libglvnd/libglvnd0_1.7.0-1+b2_i386.deb 756fce6f41fae7c61ce16cdb80815083833f78e382819b93d2b65d63f6dd71a0
    pool/main/libg/libglvnd/libglx0_1.7.0-1+b2_i386.deb 94b2d3e10d5fa4aac9fd98bbdff7d7285f70051276225b67ac8017d0516d5830
    pool/main/libd/libdrm/libdrm2_2.4.124-2_i386.deb 366cf26a5c5f1003225ef57b442b3dce8a5dd555487633bd2de5278edca9e854
    # amd64: glibc, and the tools steam.sh and setup.sh run.
    pool/main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb 967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9
    pool/main/g/glibc/libc-bin_2.41-12+deb13u4_amd64.deb dc8c79647fe3a5f37f72a400414f70263d0212f010ff2c09f990e30d91e38d1c
    pool/main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb 3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150
    # scout's srt-bwrap, which its requirements check runs, takes libcap
    # from the system.
    pool/main/libc/libcap2/libcap2_2.75-10+deb13u1+b3_amd64.deb 89fc4d34fc7a28ad6f0fcd0c561ab253b9dedf6f77f5a000b47c276c8295bf67
    pool/main/b/bash/bash_5.2.37-2+b10_amd64.deb 2fd7b04f1b7caa29c4e683f5216e6af354a41e3cdcf6e75394d7cda680f9ab82
    pool/main/n/ncurses/libtinfo6_6.5+20250216-2_amd64.deb 8b9f6a7983e9418564e48a627518de4c03917b56efe68d7f3e93bd8fffa1cc10
    pool/main/t/tar/tar_1.35+dfsg-3.1_amd64.deb 214c02e1aa291076a3147b8a4c4cae02fadf4c67df629186e3e313726faef1de
    pool/main/a/acl/libacl1_2.3.2-2+b1_amd64.deb 08074f01e384bc07c0c2d79a58cf4a6523f71cf75d1808101c79617656c9a39d
    pool/main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb 68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5
    pool/main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb 1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e
    pool/main/x/xz-utils/xz-utils_5.8.1-1+deb13u1_amd64.deb 9e0a39d95373afdaad3c9dfaab3c8b0beb9a2c4654e6be10b4bcf458509a3217
    pool/main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_amd64.deb 1cfcc6e0dc36f438a79b6e2189facdb9d150b08f57d190a60e01c98075c7f896
    pool/main/g/grep/grep_3.11-4_amd64.deb 2d78ed07d86a530ebf538f05cb2415f37c8daf908596bc32fc2563cc7e88c55a
    pool/main/s/sed/sed_4.9-2+deb13u1_amd64.deb 7071270ed4f6adda55bc4f926347fb847bacaffbdee3dff917bc6006ed7e3775
    pool/main/m/mawk/mawk_1.3.4.20250131-1_amd64.deb ade14470c4dff8921bca6ca8b1ea20eadc0f2178b48caff3c74534b17a633292
    pool/main/f/findutils/findutils_4.10.0-3_amd64.deb 3edf02b016456b6a98cefdbab99c5ce6f829b786f63affe970badea221aecfc7
    pool/main/d/debianutils/debianutils_5.23.2_amd64.deb 720e43fda316676076498b4224ad3318049deafe60cc7c601011048de03ee97d
    pool/main/c/ca-certificates/ca-certificates_20250419_all.deb ef590f89563aa4b46c8260d49d1cea0fc1b181d19e8df3782694706adf05c184
)

for tool in curl sha256sum dpkg-deb tar xz readelf mkfs.btrfs; do
    command -v "$tool" > /dev/null || { echo "fetch-steam: $tool is not installed" >&2; exit 1; }
done

# Download $1 to the pool unless it is there, check it against $2, and print
# where it is.
fetch() {
    local url=$1 sum=$2
    local file="$out/pool/${url##*/}"
    if [ ! -f "$file" ]; then
        curl -fsSL -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    echo "$sum  $file" | sha256sum -c --quiet \
        || { echo "fetch-steam: $file does not match its pinned checksum" >&2; rm -f "$file"; exit 1; }
    printf '%s\n' "$file"
}

mkdir -p "$out/pool"
tree="$out/tree"
rm -rf "$tree"
mkdir -p "$tree/steam" "$tree/home"

set -- "${DEBS[@]}"
while [ $# -gt 0 ]; do
    dpkg-deb -x "$(fetch "$debian/$1" "$2")" "$tree"
    shift 2
done

# What installing the packages would have made, and unpacking does not: the
# alternatives `awk` and `which`, and the certificate bundle
# `update-ca-certificates` writes.
ln -s mawk "$tree/usr/bin/awk"
ln -s which.debianutils "$tree/usr/bin/which"
mkdir -p "$tree/etc/ssl/certs"
find "$tree/usr/share/ca-certificates" -name '*.crt' -print0 | sort -z \
    | xargs -0 cat > "$tree/etc/ssl/certs/ca-certificates.crt"
test -s "$tree/etc/ssl/certs/ca-certificates.crt" \
    || { echo "fetch-steam: the certificate bundle is empty" >&2; exit 1; }

# The bootstrap, as /usr/bin/steam unpacks it into the Steam directory.
launcher="$out/launcher"
rm -rf "$launcher"
dpkg-deb -x "$(fetch "$valve/$LAUNCHER" "$LAUNCHER_SHA256")" "$launcher"
tar -xJf "$launcher/usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz" -C "$tree/steam"
rm -rf "$launcher"

# Documentation and manuals, which nothing runs.
rm -rf "$tree/usr/share/doc" "$tree/usr/share/man" "$tree/usr/share/lintian" \
    "$tree/usr/share/info" "$tree/usr/share/locale"

test -x "$tree/steam/steam.sh" \
    || { echo "fetch-steam: no steam.sh in the bootstrap" >&2; exit 1; }
test -x "$tree/steam/ubuntu12_32/steam" \
    || { echo "fetch-steam: no ubuntu12_32/steam in the bootstrap" >&2; exit 1; }
test -e "$tree/usr/lib/i386-linux-gnu/ld-linux.so.2" \
    || { echo "fetch-steam: no i386 loader at usr/lib/i386-linux-gnu" >&2; exit 1; }
test -e "$tree/usr/lib64/ld-linux-x86-64.so.2" \
    || { echo "fetch-steam: no x86-64 loader at usr/lib64" >&2; exit 1; }

# Room for the client the bootstrap downloads and unpacks on its first run
# (about 2.2 GB on the host, 2026-09-28), its downloaded packages, and the
# runtime it unpacks again.
size=$(( $(du -sm "$tree" | cut -f1) + 4096 ))
image="$out/steam.img"
rm -f "$image"
truncate -s "${size}M" "$image"
mkfs.btrfs -q --rootdir "$tree" "$image"
# The tree stays beside the image, so the same files can be tried on the host.
echo "steam volume: $image (${size} MiB), unpacked in $tree"
