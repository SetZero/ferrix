#!/usr/bin/env bash
# Print libxkbcommon's own keymap and tables for the compositor's layout.
#
# Needs a Linux host with libxkbcommon's development files and the XKB data
# (xkb-data), and gcc. Writes one keymap-<layout>.txt beside this script for
# every layout the compositor ships and one reference-<combination>.txt for
# every multi-group combination the Rust side is tested against, which the
# crate reads at build time and its tests read again. Record the libxkbcommon
# version the output came from in the commit that changes it; each file's
# first line carries it.
#
# `keymap.c` documents the format of what comes out.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

version="$(pkg-config --modversion xkbcommon)"
cflags=$(pkg-config --cflags xkbcommon)
libs=$(pkg-config --libs xkbcommon)

# Whether this libxkbcommon can say which modifiers reach a shift level.
# `xkb_keymap_key_get_mods_for_level` arrived in 1.0.0, but the installed
# header is the authority and a version string is not: ask the compiler, and
# on a host too old for it the probe still prints every level it can see, with
# `no-mods-for-level` in the section header so nobody reads the missing masks
# as "no modifier reaches this level".
mods_for_level=""
if printf '%s\n' \
    '#include <xkbcommon/xkbcommon.h>' \
    'void *probe = (void *)&xkb_keymap_key_get_mods_for_level;' |
    gcc -x c -c -o "$work/probe.o" - $cflags 2> /dev/null; then
    mods_for_level="-DHAVE_MODS_FOR_LEVEL=1"
else
    echo "warning: this libxkbcommon ($version) has no" \
        "xkb_keymap_key_get_mods_for_level; the mods fields will be -" >&2
fi

gcc -O0 -Wall -Werror \
    -DXKBCOMMON_VERSION="\"$version\"" \
    $mods_for_level \
    $cflags \
    -o "$work/keymap" "$here/keymap.c" \
    $libs

# One file per layout the compositor ships. A person who writes
# `kb_layout = de` gets the letters on their keyboard only if the compositor
# was given that keymap; `us` is the default and the fallback, and the rest
# are here because somebody asked for them.
#
#   <file stem>  <model>  <layout>  <variant>
while read -r stem model layout variant; do
    [ -n "$stem" ] || continue
    "$work/keymap" "$model" "$layout" "${variant:-}" > "$here/keymap-$stem.txt"
    echo "wrote $here/keymap-$stem.txt"
done <<'LAYOUTS'
us pc105 us
de pc105 de
de-nodeadkeys pc105 de nodeadkeys
gb pc105 gb
fr pc105 fr
LAYOUTS

# The multi-group references. `kb_layout = de,us` is one keymap with two
# groups, not two keymaps, and the order of the list decides which group a
# key falls back to; both orders are here so that the Rust side is read
# against a real one rather than against an assumption. These are not
# layouts the compositor ships, so they are named apart from `keymap-*.txt`:
# the generator turns every one of those into a Rust module, and `de,us` is
# not a module name.
#
#   <file stem>  <model>  <layout>  <variant>
while read -r stem model layout variant; do
    [ -n "$stem" ] || continue
    "$work/keymap" "$model" "$layout" "${variant:-}" \
        > "$here/reference-$stem.txt"
    echo "wrote $here/reference-$stem.txt"
done <<'COMBINATIONS'
de-us pc105 de,us
us-de pc105 us,de
COMBINATIONS
