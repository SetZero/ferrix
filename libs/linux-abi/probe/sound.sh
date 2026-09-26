#!/usr/bin/env bash
# Print the ALSA numbers and layouts from the UAPI header, at both widths.
#
# Needs a Linux host with the kernel's UAPI headers (linux-libc-dev), gcc,
# arm-linux-gnueabihf-gcc and qemu-arm. Writes sound-64.txt and sound-32.txt
# beside this script, which the crate's tests read. Record the header version
# the output came from in the commit that changes them.
#
# The 32-bit build has a 64-bit time_t, the view musl and ferrousli give a
# program (sound.c says why only that one).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# probe <width> <compiler> <runner> <extra flags...>
probe() {
    local width="$1" cc="$2" run="$3"
    shift 3
    $cc -O0 -Wall -Werror "$@" -o "$work/sound-$width" "$here/sound.c"
    $run "$work/sound-$width" > "$here/sound-$width.txt"
}

probe 64 gcc env

# The cross compiler's sound/ may be an older linux-libc-dev than the host's;
# asound.h is the same file for every architecture, so it is given the
# host's, and its own asm/ and linux/ still answer for ARMv7-A.
mkdir -p "$work/include/sound"
ln -s /usr/include/sound/asound.h "$work/include/sound/asound.h"
probe 32 arm-linux-gnueabihf-gcc qemu-arm -static -I"$work/include" \
    -D_FILE_OFFSET_BITS=64 -D_TIME_BITS=64

dpkg-query -W -f='${Package} ${Version}\n' linux-libc-dev 2>/dev/null \
    || dpkg -l linux-libc-dev | tail -1
