#!/usr/bin/env bash
# Builds alsa-lib 1.2.16.1 as a static library against ferrousli, for
# alsa-utils' aplay and speaker-test (docs/AUDIO.md, U1).
#
#     tools/ports/alsa-lib/build.sh [--arch <arch>]    # from src/user/system/linux/ferrousli/
#
# Installs, under $FERRIX_PORTS (see ../common.sh):
#   <arch>/include/alsa/*.h
#   <arch>/lib/libasound.a
#   <arch>/usr/share/ferrousli/alsa/   alsa.conf and the rest, where this
#                                      alsa-lib looks for them on the guest
#
# Pinned, and refused if its checksum differs: alsa-project.org's
# alsa-lib-1.2.16.1.tar.bz2. alsa-project publishes signatures, not
# checksums, so the sha256 below is the tarball's own, taken on 2026-09-27
# after its src/ and include/ were found identical to the v1.2.16.1 tag of
# the git tree (~/.local/share/ferrix/audio-ref/alsa-lib) but for the
# Makefile.in files autoreconf makes.
#
# Static, so without dlopen: every PCM and control plugin alsa-lib has is
# built into the library, and nothing is looked for in a plugin directory.
#
# Its configuration is at /usr/share/ferrousli/alsa, not /usr/share/alsa:
# that is where Chrome's own alsa-lib, Debian's, finds Debian's configuration
# on its volume (tools/common/xtask/src/chrome.rs), and the two are not the same build.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../common.sh
. "$here/../common.sh"

ALSA_LIB_VERSION=1.2.16.1
ALSA_LIB_TARBALL=alsa-lib-$ALSA_LIB_VERSION.tar.bz2
ALSA_LIB_URL=https://www.alsa-project.org/files/pub/lib/$ALSA_LIB_TARBALL
ALSA_LIB_SHA256=f740db7f488255944ffd4428416ee3390a96742856916433df468c281436480e

work=$builds/alsa-lib
src=$ports/src

step "sources"
mkdir -p "$src" "$work"
fetch "$ALSA_LIB_URL" "$src/$ALSA_LIB_TARBALL" sha256sum "$ALSA_LIB_SHA256"
echo "alsa-lib $ALSA_LIB_VERSION, verified"

build_ferrousli
make_compilers

step "build"
build=$work/build
stage=$work/stage
rm -rf "$build" "$stage"
mkdir -p "$build" "$stage"
tar -xjf "$src/$ALSA_LIB_TARBALL" -C "$build" --strip-components=1
host=()
[ -n "$triple" ] && host=(--host="$triple")
# The configuration directory is the guest's path, compiled into the
# library; the install is staged and copied into the prefix below, so that
# nothing is written to the build host's /usr/share.
if ! (cd "$build" && CC="$CC" AR="$AR" CFLAGS=-O2 ./configure "${host[@]}" \
    --prefix="$prefix" --with-configdir=/usr/share/ferrousli/alsa --with-plugindir=/usr/lib/alsa-lib \
    --enable-static --disable-shared --with-libdl=no \
    --with-pcm-plugins=all --with-ctl-plugins=all \
    --disable-python --disable-topology --disable-alisp --disable-old-symbols \
    && make -j"$jobs" && make install DESTDIR="$stage") > "$work/build.log" 2>&1; then
    tail -30 "$work/build.log" >&2
    undefined_symbols "$work/undefined-symbols.txt" "$work/build.log" || true
    fail "alsa-lib did not build; the log is $work/build.log"
fi

step "install"
mkdir -p "$prefix/lib" "$prefix/include" "$prefix/usr/share/ferrousli"
cp "$stage$prefix/lib/libasound.a" "$prefix/lib/"
rm -rf "$prefix/include/alsa" "$prefix/usr/share/ferrousli/alsa" "$prefix/usr/share/alsa"
cp -r "$stage$prefix/include/alsa" "$prefix/include/"
cp -r "$stage/usr/share/ferrousli/alsa" "$prefix/usr/share/ferrousli/"
ls -l "$prefix/lib/libasound.a" "$prefix/include/alsa/asoundlib.h" \
    "$prefix/usr/share/ferrousli/alsa/alsa.conf"
