#!/usr/bin/env bash
# Print the exact bytes libwayland puts on the socket, both directions.
#
# Needs a Linux host with libwayland's development files (libwayland-dev) and
# gcc. Writes wire.txt beside this script, which the crate's tests read.
# Record the libwayland version the output came from in the commit that
# changes it; the file's first line carries it.
#
# The bytes are the same on every little-endian machine, which is every
# architecture Ferrix builds for, so one file is enough: unlike the evdev
# probe there is no second width to take. Wayland's words are the machine's
# own byte order and this file is a little-endian machine's.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

gcc -O0 -Wall -Werror \
    $(pkg-config --cflags wayland-client wayland-server) \
    -o "$work/wire" "$here/wire.c" \
    $(pkg-config --libs wayland-client wayland-server)
"$work/wire" > "$here/wire.txt"
echo "wrote $here/wire.txt"
