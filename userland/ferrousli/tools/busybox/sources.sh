# The sources both busybox builds use, sourced by build.sh and build-windows.sh.
#
# Pinned, and refused if their checksums differ:
#   * busybox.net's busybox-1.37.0.tar.bz2, by the sha256 busybox.net publishes;
#   * Alpine's busyboxconfig at aports commit a8dcedab, by the sha512 in that
#     commit's APKBUILD. It is the config behind Alpine's busybox-static
#     1.37.0-r20, the musl busybox Ferrix's test-shell already runs, so the two
#     binaries differ in their C library and not in their applets.

VERSION=1.37.0
TARBALL=busybox-$VERSION.tar.bz2
TARBALL_URL=https://busybox.net/downloads/$TARBALL
TARBALL_SHA256=3311dff32e746499f4df0d5df04d7eb396382d7e108bb9250e7b519b837043a4
APORTS_COMMIT=a8dcedab8fd001d8a35a703a70140fa071b3fa29
CONFIG_URL=https://gitlab.alpinelinux.org/alpine/aports/-/raw/$APORTS_COMMIT/main/busybox/busyboxconfig
CONFIG_SHA512=93f0fa512a394a07f760b13bd70a28424d3ad1a6c283b77fd4894114c5b472cfbe65f3441afa28596771d75527aaa9794706a3b329b4898e5d83bce799169929

fetch() { # url file checksum-command checksum
    local url=$1 file=$2 tool=$3 sum=$4
    if [ ! -f "$file" ]; then
        curl -fsSL --max-time 300 -o "$file.part" "$url"
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
    fetch "$CONFIG_URL" "$1/busyboxconfig" sha512sum "$CONFIG_SHA512"
    echo "busybox $VERSION and Alpine's config at $APORTS_COMMIT, both verified"
}
