#!/usr/bin/env bash
# Builds btop 1.4.7 as a static x86-64 program against ferrousli and the C++
# runtime tools/ports/libcxx builds on it, which must be built first.
#
#     tools/ports/btop/build.sh            # from ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   x86_64/bin/btop
#
# Pinned, and refused if its checksum differs: GitHub's archive of the v1.4.7
# tag, by its sha256 as first downloaded on 2026-09-16; btop publishes no
# checksum of its own.
#
# Built with btop's own Makefile, as its README describes a static build:
# STATIC=true, and GPU_SUPPORT=false, since the GPU collectors load vendor
# libraries at run time, which a static program cannot do.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"
# x86-64 only so far: another architecture needs libcxx built for it first.
[ "$arch" = x86_64 ] || fail "btop is built for x86_64 only so far, not for $arch"

BTOP_VERSION=1.4.7
BTOP_TARBALL=btop-$BTOP_VERSION.tar.gz
BTOP_URL=https://github.com/aristocratos/btop/archive/refs/tags/v$BTOP_VERSION.tar.gz
BTOP_SHA256=933de2e4d1b2211a638be463eb6e8616891bfba73aef5d38060bd8319baeefc6

work=$builds/btop
src=$ports/src

step "sources"
mkdir -p "$src" "$work"
fetch "$BTOP_URL" "$src/$BTOP_TARBALL" sha256sum "$BTOP_SHA256"
echo "btop $BTOP_VERSION, verified"

build_ferrousli
make_compilers
[ -f "$prefix/lib/libc++.a" ] || fail "no C++ runtime in $prefix/lib; run tools/ports/libcxx/build.sh first"

step "build"
build=$work/build
rm -rf "$build"
mkdir -p "$build"
tar -xzf "$src/$BTOP_TARBALL" -C "$build" --strip-components=1
set +e
make -C "$build" -j"$jobs" CXX="$CXX" PLATFORM=Linux ARCH=x86_64 \
    STATIC=true GPU_SUPPORT=false VERBOSE=true > "$work/build.log" 2>&1
status=$?
set -e
echo "make exited $status; log in $work/build.log"
undefined_symbols "$work/undefined-symbols.txt" "$work/build.log"
if [ "$status" -ne 0 ]; then
    grep -E 'error' "$work/build.log" | sort -u | head -30 >&2
    exit 1
fi

step "install"
mkdir -p "$prefix/bin"
# Stripped: the image carries it, and nothing on the guest reads its symbols.
install -m 755 -s --strip-program="$STRIP" "$build/bin/btop" "$prefix/bin/btop"
file "$prefix/bin/btop"
"$prefix/bin/btop" --version
