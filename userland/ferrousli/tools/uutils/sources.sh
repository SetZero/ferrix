# The uutils projects both builds build, sourced by build.sh and
# build-windows.sh.
#
# Each is pinned by the sha256 of GitHub's archive of a release tag, as first
# downloaded. Every tree carries its own Cargo.lock and both builds pass
# --locked, so the crates under them are pinned by uutils rather than by this
# file, and cargo checks each against crates.io's own checksum.
#
# `project <name>` sets the variables for one of them:
#
#   TAG       the release tag, which is also the archive's name
#   SHA256    the archive's checksum
#   BINS      the binaries it builds, in the order they are installed
#   FEATURES  the cargo feature arguments, or empty for the defaults
#
# PROJECTS is every one of them, in the order `cargo xtask uutils` builds
# them: coreutils first, because it is the one the image cannot do without.

# procps and util-linux are not here, and it is not for want of trying. Their
# only release, 0.0.1 in both cases, does not compile at all on this
# toolchain: procps' pinned `time` fails, and unpinning it pulls a `uucore`
# whose API its own code no longer matches. Their main branches carry the
# utilities that would be worth having -- `sysctl`, `ps`, `top`, `dmesg` --
# and do not build here either: procps' `top` wants libsystemd through
# pkg-config and refuses to cross-compile, and util-linux's `blockdev` and
# `fsfreeze` assume glibc's `ioctl`, whose request argument is a different
# type on musl. docs/UUTILS.md §6 has the row and what it would take.
PROJECTS="coreutils findutils diffutils"

# The Rust target. Not x86_64-unknown-linux-gnu, though ferrousli aims at
# glibc's ABI and links that target perfectly well: uutils reads which utility
# it is from argv[0] only when the target environment is musl, and asks rustix
# for the kernel's AT_EXECFN otherwise. rustix reads that through prctl and
# /proc/self/auxv rather than through the C library, and in a static binary it
# comes back empty, so the multicall dispatch that makes /bin/ls run `ls`
# never happens. docs/UUTILS.md §3a has the measurement and the control.
TARGET=x86_64-unknown-linux-musl

project() { # name
    case $1 in
        coreutils)
            TAG=0.9.0
            SHA256=dafe0126ee4ed55c7cd60c6b559f43724a74751deed3c1b078f4f510311acab2
            BINS=coreutils
            # feat_os_unix_musl rather than feat_os_unix: uutils maintains it
            # for targets that cannot produce the cdylib `stdbuf` needs, and a
            # static program cannot.
            FEATURES="--no-default-features --features feat_os_unix_musl"
            ;;
        findutils)
            TAG=0.9.1
            SHA256=d6dc466b7953f170cc7a4332c1576c5171b7d497b64e08cc63b3fcf54085e0ac
            # No multicall binary here: findutils builds one program per
            # utility, and `testing-commandline`, which is its own test
            # harness and not a utility.
            BINS="find xargs locate updatedb"
            FEATURES=
            ;;
        diffutils)
            TAG=v0.5.0
            SHA256=4c05d236ebddef7738446980a59cd13521b6990ea02242db6b32321dd93853ca
            BINS=diffutils
            FEATURES=
            ;;
        *)
            echo "${0##*/}: no such uutils project: $1" >&2
            return 1
            ;;
    esac
    PROJECT=$1
    TARBALL=$PROJECT-$TAG.tar.gz
    TARBALL_URL=https://github.com/uutils/$PROJECT/archive/refs/tags/$TAG.tar.gz
}

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
    fetch "$TARBALL_URL" "$1/$TARBALL" sha256sum "$SHA256"
    echo "uutils/$PROJECT $TAG, verified"
}
