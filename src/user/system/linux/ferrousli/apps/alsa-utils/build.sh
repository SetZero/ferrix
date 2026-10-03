#!/usr/bin/env bash
# Builds aplay and speaker-test from alsa-utils 1.2.16, static against
# ferrousli and the alsa-lib app's library (docs/AUDIO.md, U1).
#
#     build.sh <arch> <out>     # as an app's script is run (docs/APPS.md §3.1)
#
# and copies what app.toml installs into <out>.
#
# Needs the alsa-lib app built first, which app.toml's depends sees to.
# Installs, under $FERRIX_PORTS (see ferrousli's tools/ports/common.sh):
#   <arch>/bin/aplay           aplay, and arecord as a link to it
#   <arch>/bin/speaker-test
#
# Pinned, and refused if its checksum differs: alsa-project.org's
# alsa-utils-1.2.16.tar.bz2. alsa-project publishes signatures, not
# checksums, and no git tree of alsa-utils is kept beside the ports, so the
# sha256 below is only the tarball's own, as downloaded over HTTPS on
# 2026-09-27.
#
# Only aplay/ and speaker-test/ are built: alsamixer needs curses, and the
# rest (alsactl, amixer, the UCM and topology tools) nothing U1 asks for.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: build.sh <arch> <out>" >&2; exit 2; }
out=$2
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# ferrousli's port toolkit: pinned downloads, the release library, and the
# compilers that build against it. It reads `--arch <name>` first.
set -- --arch "$1"
# shellcheck source=../../tools/ports/common.sh
. "$here/../../tools/ports/common.sh"
# One build of alsa-utils at a time: every checkout's shares its work directory.
lock_port alsa-utils

ALSA_UTILS_VERSION=1.2.16
ALSA_UTILS_TARBALL=alsa-utils-$ALSA_UTILS_VERSION.tar.bz2
ALSA_UTILS_URL=https://www.alsa-project.org/files/pub/utils/$ALSA_UTILS_TARBALL
ALSA_UTILS_SHA256=092399d5e8749a1d5e188e393157521cec4b75693b60ebb79bbce728cff2232c

work=$builds/alsa-utils
src=$ports/src

[ -f "$prefix/lib/libasound.a" ] || fail "no $prefix/lib/libasound.a: build the alsa-lib app first"

step "sources"
mkdir -p "$src" "$work"
fetch "$ALSA_UTILS_URL" "$src/$ALSA_UTILS_TARBALL" sha256sum "$ALSA_UTILS_SHA256"
echo "alsa-utils $ALSA_UTILS_VERSION, verified"

build_ferrousli
make_compilers

step "build"
build=$work/build
rm -rf "$build"
mkdir -p "$build"
tar -xjf "$src/$ALSA_UTILS_TARBALL" -C "$build" --strip-components=1
host=()
[ -n "$triple" ] && host=(--host="$triple")
# A static libasound needs what it links spelled out; ferrousli is all of
# them, and the stubs make_compilers writes stand in for their names.
if ! (cd "$build" && CC="$CC" AR="$AR" CFLAGS=-O2 LIBS="-lm -lpthread -lrt" ./configure "${host[@]}" \
    --prefix=/usr --with-alsa-prefix="$prefix/lib" --with-alsa-inc-prefix="$prefix/include" \
    --disable-nls --disable-alsatest --disable-alsamixer --disable-alsaconf --disable-alsaloop \
    --disable-bat --disable-nhlt --disable-xmlto --disable-rst2man \
    && make -j"$jobs" -C include && make -j"$jobs" -C aplay && make -j"$jobs" -C speaker-test) \
    > "$work/build.log" 2>&1; then
    tail -30 "$work/build.log" >&2
    undefined_symbols "$work/undefined-symbols.txt" "$work/build.log" || true
    fail "aplay and speaker-test did not build; the log is $work/build.log"
fi

step "install"
mkdir -p "$prefix/bin"
install -m 0755 "$build/aplay/aplay" "$prefix/bin/aplay"
ln -sf aplay "$prefix/bin/arecord"
install -m 0755 "$build/speaker-test/speaker-test" "$prefix/bin/speaker-test"
"$STRIP" "$prefix/bin/aplay" "$prefix/bin/speaker-test"
ls -l "$prefix/bin/aplay" "$prefix/bin/speaker-test"
run_built "$prefix/bin/aplay" --version

step "the app's files"
# What app.toml installs, copied out of the prefix the ports share, links
# kept as links.
rm -rf "$out"
mkdir -p "$out"
(cd "$prefix" && cp -a --parents bin/aplay bin/arecord bin/speaker-test "$out")
