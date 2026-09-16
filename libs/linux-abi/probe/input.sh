#!/usr/bin/env bash
# Print the evdev numbers and layouts from the UAPI headers, at both widths.
#
# Needs a Linux host with the kernel's UAPI headers (linux-libc-dev), gcc,
# arm-linux-gnueabihf-gcc and qemu-arm. Writes input-64.txt and input-32.txt
# beside this script, which the crate's tests read. Record the header version
# the output came from in the commit that changes them.
#
# Each width is built four times: once for every line, and once per view of
# struct input_event (input.c says why), whose lines are appended.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

views=(
    "time32 -U_TIME_BITS -U_FILE_OFFSET_BITS"
    "time64 -D_FILE_OFFSET_BITS=64 -D_TIME_BITS=64"
    "time64-undef -D_FILE_OFFSET_BITS=64 -D_TIME_BITS=64 -DUNDEF_TIME_BITS64"
)

# probe <width> <compiler> <runner> <extra flags...>
probe() {
    local width="$1" cc="$2" run="$3"
    shift 3
    local out="$here/input-$width.txt"
    $cc -O0 -Wall -Werror "$@" -o "$work/input-$width" "$here/input.c"
    $run "$work/input-$width" > "$out"
    for view in "${views[@]}"; do
        read -r name flags <<< "$view"
        # shellcheck disable=SC2086 # flags are words
        $cc -O0 -Wall -Werror "$@" $flags -DVIEW="\"$name\"" \
            -o "$work/input-$width-$name" "$here/input.c"
        $run "$work/input-$width-$name" >> "$out"
    done
}

probe 64 gcc env

# The cross compiler's linux/ may be an older linux-libc-dev than the host's;
# the evdev headers are the same files for every architecture, so it is given
# the host's, and its own asm/ still answers for ARMv7-A.
mkdir -p "$work/include/linux"
for header in input.h input-event-codes.h major.h; do
    ln -s "/usr/include/linux/$header" "$work/include/linux/$header"
done
probe 32 arm-linux-gnueabihf-gcc qemu-arm -static -I"$work/include"

dpkg-query -W -f='${Package} ${Version}\n' linux-libc-dev 2>/dev/null \
    || dpkg -l linux-libc-dev | tail -1
