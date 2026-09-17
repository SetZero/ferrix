# The sources both uutils builds use, sourced by build.sh and build-windows.sh.
#
# Pinned, and refused if their checksum differs: uutils/coreutils 0.9.0, the
# release tag, by the sha256 of GitHub's archive of it as first downloaded on
# 2026-09-17. That tag is commit 840c36d3964833e1dc107fcb9ade9c0ad63076c0.
#
# The tree carries its own Cargo.lock, and both builds pass --locked, so the
# ~340 crates under it are pinned by uutils rather than by this file, and
# cargo checks each against crates.io's own checksum.

VERSION=0.9.0
COMMIT=840c36d3964833e1dc107fcb9ade9c0ad63076c0
TARBALL=coreutils-$VERSION.tar.gz
TARBALL_URL=https://github.com/uutils/coreutils/archive/refs/tags/$VERSION.tar.gz
TARBALL_SHA256=dafe0126ee4ed55c7cd60c6b559f43724a74751deed3c1b078f4f510311acab2

# The Rust target. Not x86_64-unknown-linux-gnu, though ferrousli aims at
# glibc's ABI and links that target perfectly well: uutils reads which utility
# it is from argv[0] only when the target environment is musl, and asks rustix
# for the kernel's AT_EXECFN otherwise. rustix reads that through prctl and
# /proc/self/auxv rather than through the C library, and in a static binary it
# comes back empty, so the multicall dispatch that makes /bin/ls run `ls`
# never happens. docs/UUTILS.md §3a has the measurement and the control.
TARGET=x86_64-unknown-linux-musl

# feat_os_unix_musl rather than feat_os_unix: uutils maintains it for targets
# that cannot produce the cdylib `stdbuf` needs, and a static program cannot.
FEATURES=feat_os_unix_musl

fetch() { # url file checksum-command checksum
    local url=$1 file=$2 tool=$3 sum=$4
    if [ ! -f "$file" ]; then
        curl -fsSL --max-time 900 -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    local got
    got=$("$tool" "$file" | cut -d' ' -f1)
    if [ "$got" != "$sum" ]; then
        echo "${0##*/}: $file does not match its pinned $tool" >&2
        echo "  expected $sum" >&2
        echo "  got      $got" >&2
        exit 1
    fi
}

fetch_sources() { # directory
    mkdir -p "$1"
    fetch "$TARBALL_URL" "$1/$TARBALL" sha256sum "$TARBALL_SHA256"
    echo "uutils/coreutils $VERSION ($COMMIT), verified"
}
