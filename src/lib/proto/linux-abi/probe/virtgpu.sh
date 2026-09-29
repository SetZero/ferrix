#!/usr/bin/env bash
# Print the virtio-gpu numbers and layouts from the UAPI headers, at both
# widths.
#
# Needs a Linux host with the kernel's UAPI headers (linux-libc-dev), gcc,
# arm-linux-gnueabihf-gcc and qemu-arm. Writes virtgpu-64.txt and
# virtgpu-32.txt beside this script, which the crate's tests read. Record the
# header version the output came from in the commit that changes them.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

gcc -O0 -Wall -Werror -o "$work/virtgpu-64" "$here/virtgpu.c"
"$work/virtgpu-64" > "$here/virtgpu-64.txt"

# The cross compiler has no drm/ of its own; the UAPI headers are the same
# files for every architecture, so it is given the host's.
mkdir -p "$work/include"
ln -s /usr/include/drm "$work/include/drm"
arm-linux-gnueabihf-gcc -O0 -Wall -Werror -static -I"$work/include" \
    -o "$work/virtgpu-32" "$here/virtgpu.c"
qemu-arm "$work/virtgpu-32" > "$here/virtgpu-32.txt"

dpkg-query -W -f='${Package} ${Version}\n' linux-libc-dev 2>/dev/null \
    || dpkg -l linux-libc-dev | tail -1
