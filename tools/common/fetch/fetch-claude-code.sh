#!/usr/bin/env bash
# Fetch Anthropic's prebuilt Claude Code and the Debian programs it runs,
# and put them on a btrfs volume: the disk `cargo xtask test-claude-code`
# runs Claude Code from (docs/CLAUDE-CODE.md).
#
# Nothing here is built. `claude` is Claude Code's native linux-x64 release,
# the one https://claude.ai/install.sh installs: a 234 MB glibc program, a
# JavaScript runtime with Claude Code bundled into it, which needs nothing
# of the system's but glibc itself -- libc, libm, libpthread, libdl and
# librt. The glibc and loader are Debian 13's, the same pin
# tools/common/fetch/fetch-chrome.sh takes. Beside them are bash, which
# Claude Code's Bash tool runs every command in and will not do without,
# and ripgrep, which its Grep and Glob tools search with when it is told to
# use the system's.
#
# The volume holds a Debian-shaped tree at its root -- `usr/lib64`,
# `usr/lib/x86_64-linux-gnu`, `usr/bin` -- and Claude Code at
# `claude-code/claude`. Ferrix mounts it at `/data`, and the test's image
# links the directories glibc names into it, as test-chrome's does.
#
# Every download is pinned by its SHA-256: Debian's from trixie's Packages
# file on 2026-09-30, and Claude Code's from its release's manifest.json,
# which publishes one for each platform. After unpacking, every library each
# ELF file on the volume needs is looked for on it, so a dependency this
# list lacks fails here rather than on Ferrix.
#
# Writes $FERRIX_CLAUDE_CODE_VOLUME/claude-code.img (default
# ~/.local/share/ferrix/claude-code/claude-code.img). Needs curl, sha256sum,
# dpkg-deb, readelf and mkfs.btrfs; no root.
#
# Usage: tools/common/fetch/fetch-claude-code.sh

set -euo pipefail

debian=${DEBIAN_MIRROR:-https://deb.debian.org/debian}
releases=${CLAUDE_CODE_RELEASES:-https://downloads.claude.ai/claude-code-releases}
out=${FERRIX_CLAUDE_CODE_VOLUME:-$HOME/.local/share/ferrix/claude-code}

# The stable channel's release on 2026-09-30, and its linux-x64 binary's
# checksum from that release's manifest.json.
CLAUDE_CODE_VERSION=2.1.280
CLAUDE_CODE_SHA256=1e08503dbdf3c2cb0d706d32f3408277388d1c76ef108673e8fe42c1b322925b

# Pool path and SHA-256 of each Debian 13 package.
debs=(
    "main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb 967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9"
    "main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb 3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150"
    "main/b/bash/bash_5.2.37-2+b10_amd64.deb 2fd7b04f1b7caa29c4e683f5216e6af354a41e3cdcf6e75394d7cda680f9ab82"
    "main/n/ncurses/libtinfo6_6.5+20250216-2_amd64.deb 8b9f6a7983e9418564e48a627518de4c03917b56efe68d7f3e93bd8fffa1cc10"
    "main/r/rust-ripgrep/ripgrep_14.1.1-1+b4_amd64.deb 7e0c32510c264c31335fe3b9ae37ab76dcd22f7d1627a0a08518a5bf28b17ac2"
    "main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb 1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e"
)

for tool in curl sha256sum dpkg-deb readelf mkfs.btrfs; do
    command -v "$tool" > /dev/null || { echo "fetch-claude-code: $tool is not installed" >&2; exit 1; }
done

# Download $2/$1 into the pool as $4 (default: $1's last component) unless
# it is there, and check it against $3.
fetch() {
    local path=$1 base=$2 sum=$3 name=${4:-${1##*/}}
    local file="$out/pool/$name"
    if [ ! -f "$file" ]; then
        curl -fsSL -o "$file.part" "$base/$path"
        mv "$file.part" "$file"
    fi
    echo "$sum  $file" | sha256sum -c --quiet \
        || { echo "fetch-claude-code: $file does not match its pinned checksum" >&2; rm -f "$file"; exit 1; }
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

binary=$(fetch "$CLAUDE_CODE_VERSION/linux-x64/claude" "$releases" "$CLAUDE_CODE_SHA256" \
    "claude-$CLAUDE_CODE_VERSION-linux-x64")
mkdir -p "$tree/claude-code"
install -m 755 "$binary" "$tree/claude-code/claude"

# Documentation and manuals, which nothing runs.
rm -rf "$tree/usr/share/doc" "$tree/usr/share/man" "$tree/usr/share/lintian"

test -x "$tree/claude-code/claude" \
    || { echo "fetch-claude-code: no claude in the tree" >&2; exit 1; }
test -x "$tree/usr/bin/bash" \
    || { echo "fetch-claude-code: no bash in the tree" >&2; exit 1; }
test -e "$tree/usr/lib64/ld-linux-x86-64.so.2" \
    || { echo "fetch-claude-code: no linker at usr/lib64" >&2; exit 1; }

# Every library an ELF file on the volume needs must be on it.
missing=0
while IFS= read -r -d '' file; do
    head -c 4 "$file" | grep -q $'\x7fELF' || continue
    for needed in $(readelf -d "$file" 2> /dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
        if [ ! -e "$tree/usr/lib/x86_64-linux-gnu/$needed" ] \
            && [ ! -e "$tree/usr/lib64/$needed" ]; then
            echo "fetch-claude-code: ${file#"$tree"/} needs $needed, which is not on the volume" >&2
            missing=1
        fi
    done
done < <(find "$tree/claude-code" "$tree/usr/bin" "$tree/usr/lib/x86_64-linux-gnu" \
    -maxdepth 1 -type f -print0)
[ "$missing" = 0 ] || exit 1

# Room for Claude Code's configuration and what its tools write, and a size
# that does not depend on how mkfs rounds.
size=$(( $(du -sm "$tree" | cut -f1) + 512 ))
image="$out/claude-code.img"
rm -f "$image"
truncate -s "${size}M" "$image"
mkfs.btrfs -q --rootdir "$tree" "$image"
# The tree stays beside the image, so the same files can be tried on the host.
echo "claude-code volume: $image (${size} MiB), unpacked in $tree"
