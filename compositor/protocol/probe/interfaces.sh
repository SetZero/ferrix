#!/usr/bin/env bash
# Print libwayland's own compiled interface tables, for every protocol the
# compositor speaks.
#
# Needs a Linux host with libwayland's development files (libwayland-dev),
# wayland-scanner and gcc. The core protocol's tables come from libwayland
# itself; the rest are compiled here from the XML vendored beside this
# directory, which is the same XML the generator reads -- so a disagreement
# is the generator's and not a version skew.
#
# Writes interfaces.txt beside this script, which the crate's tests read.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
protocols="$here/../protocols"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

scan() {
    local xml="$1" stem="$2"
    wayland-scanner client-header "$xml" "$work/$stem-client-protocol.h"
    wayland-scanner private-code "$xml" "$work/$stem-protocol.c"
}

scan "$protocols/xdg-shell.xml" xdg-shell
scan "$protocols/xdg-decoration-unstable-v1.xml" xdg-decoration
scan "$protocols/wlr-layer-shell-unstable-v1.xml" wlr-layer-shell
scan "$protocols/wlr-foreign-toplevel-management-unstable-v1.xml" wlr-foreign-toplevel-management

gcc -O0 -Wall -Werror -I"$work" \
    $(pkg-config --cflags wayland-client) \
    -o "$work/interfaces" "$here/interfaces.c" \
    "$work"/*-protocol.c \
    $(pkg-config --libs wayland-client)
"$work/interfaces" > "$here/interfaces.txt"
echo "wrote $here/interfaces.txt ($(wc -l < "$here/interfaces.txt") lines)"
