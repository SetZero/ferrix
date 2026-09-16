#!/usr/bin/env bash
# Builds zlib 1.3.2 as a static library against ferrousli, for git.
#
#     tools/ports/zlib/build.sh            # from ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   x86_64/include/zlib.h, zconf.h
#   x86_64/lib/libz.a
#
# Pinned, and refused if its checksum differs: zlib.net's zlib-1.3.2.tar.gz,
# by the sha256 zlib.net publishes.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"

ZLIB_VERSION=1.3.2
ZLIB_TARBALL=zlib-$ZLIB_VERSION.tar.gz
ZLIB_URL=https://zlib.net/$ZLIB_TARBALL
ZLIB_SHA256=bb329a0a2cd0274d05519d61c667c062e06990d72e125ee2dfa8de64f0119d16

work=$ports/zlib
src=$ports/src

step "sources"
mkdir -p "$src" "$work"
fetch "$ZLIB_URL" "$src/$ZLIB_TARBALL" sha256sum "$ZLIB_SHA256"
echo "zlib $ZLIB_VERSION, verified"

build_ferrousli
make_compilers

step "build"
build=$work/build
rm -rf "$build"
mkdir -p "$build"
tar -xzf "$src/$ZLIB_TARBALL" -C "$build" --strip-components=1
if ! (cd "$build" && CC="$CC" CFLAGS=-O2 ./configure --static --prefix="$prefix" \
    && make -j"$jobs" libz.a && make install) > "$work/build.log" 2>&1; then
    tail -30 "$work/build.log" >&2
    fail "zlib did not build; the log is $work/build.log"
fi
ls -l "$prefix/lib/libz.a" "$prefix/include/zlib.h"
