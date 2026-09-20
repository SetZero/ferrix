#!/usr/bin/env bash
# Builds build.sh's uutils/coreutils natively on Windows, without WSL: the same
# pinned release, target and feature set, and the same static x86-64 program
# against ferrousli.
#
#     bash tools/uutils/build-windows.sh    # from ferrousli/, in Git Bash
#
# `cargo xtask uutils` runs it on Windows. It needs Git for Windows (bash and
# the POSIX tools) and LLVM, whose clang cross-compiles the one C dependency
# in uutils' tree and whose lld links. LLVM is looked for on PATH and then in
# its installer's directory; $LLVM_BIN overrides both.
#
# There is much less to do here than in tools/busybox/build-windows.sh, and
# the reason is the point of this whole change: uutils is Rust. cargo
# cross-compiles it for the musl target on any host, rustc ships that target's
# unwinder, and no kernel UAPI headers are needed by anything. What is left is
# the one C library in the tree, oniguruma, which `expr` uses.
#
# What differs from build.sh, each explained where it is done:
#   * ferrousli is built for x86_64-unknown-linux-gnu, not the host's target;
#   * clang compiles oniguruma against ferrousli's headers;
#   * clang links, driven by rustc's -C link-arg rather than by a wrapper
#     script, because Windows cannot run a shell script as a linker.
#
# The output is build.sh's: under $FERRIX_UUTILS, by default
# ~/.local/share/ferrix/uutils/ferrousli, x86_64/coreutils once it links, and
# otherwise undefined-symbols.txt.
set -euo pipefail

FERROUSLI_TARGET=x86_64-unknown-linux-gnu

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ferrousli=$(cd "$here/../.." && pwd)
out=$(cygpath -u "${FERRIX_UUTILS:-$HOME/.local/share/ferrix/uutils/ferrousli}")
src=$out/src
build=$out/build

step() { printf '\n== %s\n' "$*"; }
fail() {
    echo "${0##*/}: $*" >&2
    exit 1
}
# A path for a native program's command line.
win() { cygpath -m "$1"; }

# shellcheck source=sources.sh
. "$here/sources.sh"
# Which of sources.sh's projects this builds. It takes coreutils alone, and
# the two others -- findutils and diffutils -- are a row of their own: every
# name they own is busybox's in the image until then. Naming it is what
# sources.sh asks of a caller; without this the variables it sets are unset
# and `set -u` stops the script at its first use of one.
project coreutils

step "toolchain"
toolchain=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ferrousli/../rust-toolchain.toml")
[ -n "$toolchain" ] || fail "no channel in rust-toolchain.toml"
export RUSTUP_TOOLCHAIN=$toolchain
echo "rust $toolchain"

if [ -z "${LLVM_BIN:-}" ]; then
    if command -v clang >/dev/null 2>&1; then
        LLVM_BIN=$(dirname "$(command -v clang)")
    else
        LLVM_BIN="/c/Program Files/LLVM/bin"
    fi
fi
clang=$LLVM_BIN/clang.exe
llvm_ar=$LLVM_BIN/llvm-ar.exe
[ -x "$clang" ] || fail "no clang at $clang; install LLVM or set LLVM_BIN"
[ -x "$llvm_ar" ] || fail "no llvm-ar at $llvm_ar"
"$clang" --version | head -1

step "sources"
fetch_sources "$src"

step "ferrousli, release"
# Built for a Linux target rather than the host's: this is a Windows machine,
# and the library is a Linux one. It is no_std, so which Linux target does not
# matter; the gnu one is what tools/busybox/build-windows.sh already uses.
target=${CARGO_TARGET_DIR:-$ferrousli/target}
(cd "$ferrousli" && cargo build --release --lib --target "$FERROUSLI_TARGET")
lib=$target/$FERROUSLI_TARGET/release/libferrousli.a
crt1=$(ls -t "$target"/$FERROUSLI_TARGET/release/build/ferrousli-*/out/crt1.o | head -1)
[ -f "$lib" ] && [ -f "$crt1" ] || fail "cargo built no libferrousli.a and crt1.o"
echo "$lib"
echo "$crt1"

step "link libraries"
libs=$out/link-libs
rm -rf "$libs"
mkdir -p "$libs"
for l in c m rt pthread dl util gcc_s; do
    "$llvm_ar" rc "$(win "$libs/lib$l.a")"
done
# rustc's own unwinder for the musl target, copied rather than reached with
# -L so that musl's libc.a beside it cannot answer -lc. build.sh says more.
sysroot=$(cygpath -u "$(rustc --print sysroot)")
selfcontained=$sysroot/lib/rustlib/$TARGET/lib/self-contained
[ -f "$selfcontained/libunwind.a" ] ||
    fail "no libunwind.a in $selfcontained; run: rustup target add $TARGET"
cp "$selfcontained/libunwind.a" "$libs/libunwind.a"

step "unpack"
rm -rf "$build"
mkdir -p "$build"
tar -xzf "$src/$TARBALL" -C "$build" --strip-components=1

step "build"
# The linker is clang, driven by rustc. build.sh uses a wrapper script to put
# crt1.o and libferrousli.a on the linker's command line; Windows cannot run a
# shell script as a linker, so they are passed as -C link-arg instead, which
# rustc appends after every object and rlib -- which is where the library has
# to be.
#
# Oniguruma's C is compiled by the same clang against ferrousli's headers
# rather than a host's, since this host has none for Linux. It includes no
# kernel UAPI header, so unlike busybox nothing else is needed.
cflags="--target=$TARGET -nostdinc -isystem $(win "$ferrousli/include")"

# CARGO_ENCODED_RUSTFLAGS, not RUSTFLAGS: LLVM installs into "C:\Program
# Files", and cargo splits RUSTFLAGS at spaces, so the linker's path would
# arrive as two arguments. The encoded form separates flags with US (0x1f) and
# cannot be split by accident.
rustflags=(
    -C target-feature=+crt-static
    -C link-self-contained=no
    -C "linker=$(win "$clang")"
    -C relocation-model=static
    -C "link-arg=--target=$TARGET"
    -C link-arg=-fuse-ld=lld
    -C link-arg=-static
    -C link-arg=-no-pie
    -C link-arg=-nostdlib
    -C "link-arg=-L$(win "$libs")"
    -C "link-arg=$(win "$crt1")"
    -C "link-arg=$(win "$lib")"
)
encoded=$(printf '%s\x1f' "${rustflags[@]}")
encoded=${encoded%$'\x1f'}

set +e
(
    cd "$build" || exit 1
    # shellcheck disable=SC2086
    CC_x86_64_unknown_linux_musl="$(win "$clang")" \
    CFLAGS_x86_64_unknown_linux_musl="$cflags" \
    AR_x86_64_unknown_linux_musl="$(win "$llvm_ar")" \
    CARGO_TARGET_DIR="$build/target" \
    CARGO_ENCODED_RUSTFLAGS="$encoded" \
        cargo build --release --locked --target "$TARGET" $FEATURES --bin coreutils
) > "$out/build.log" 2>&1
status=$?
set -e
echo "cargo exited $status; log in $out/build.log"

step "undefined symbols"
grep -oE "undefined symbol: [A-Za-z_][A-Za-z0-9_]*" "$out/build.log" |
    sed -E 's/undefined symbol: //' |
    sort -u > "$out/undefined-symbols.txt" || true
count=$(wc -l < "$out/undefined-symbols.txt")
echo "$count undefined: $out/undefined-symbols.txt"

if [ "$status" -ne 0 ]; then
    if [ "$count" -eq 0 ]; then
        echo "build-windows.sh: the build failed before linking; the errors:" >&2
        grep -E '^error' "$out/build.log" | head -30 >&2
    fi
    exit 1
fi

step "install"
mkdir -p "$out/x86_64"
install -m 755 "$build/target/$TARGET/release/coreutils" "$out/x86_64/coreutils"
ls -l "$out/x86_64/coreutils"
echo "$out/x86_64/coreutils"
