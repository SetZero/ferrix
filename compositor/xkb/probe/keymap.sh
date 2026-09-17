#!/usr/bin/env bash
# Print libxkbcommon's own keymap and tables for the compositor's layout.
#
# Needs a Linux host with libxkbcommon's development files and the XKB data
# (xkb-data), and gcc. Writes keymap.txt beside this script, which the crate
# reads at build time and its tests read again. Record the libxkbcommon
# version the output came from in the commit that changes it; the file's
# first line carries it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

version="$(pkg-config --modversion xkbcommon)"
gcc -O0 -Wall -Werror \
    -DXKBCOMMON_VERSION="\"$version\"" \
    $(pkg-config --cflags xkbcommon) \
    -o "$work/keymap" "$here/keymap.c" \
    $(pkg-config --libs xkbcommon)
"$work/keymap" > "$here/keymap.txt"
echo "wrote $here/keymap.txt"
