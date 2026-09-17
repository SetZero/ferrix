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

# One file per layout the compositor ships. A person who writes
# `kb_layout = de` gets the letters on their keyboard only if the compositor
# was given that keymap; `us` is the default and the fallback, and the rest
# are here because somebody asked for them.
#
#   <file stem>  <model>  <layout>  <variant>
while read -r stem model layout variant; do
    [ -n "$stem" ] || continue
    "$work/keymap" "$model" "$layout" "$variant" > "$here/keymap-$stem.txt"
    echo "wrote $here/keymap-$stem.txt"
done <<'LAYOUTS'
us pc105 us
de pc105 de
de-nodeadkeys pc105 de nodeadkeys
gb pc105 gb
fr pc105 fr
LAYOUTS
