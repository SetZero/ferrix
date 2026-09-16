#!/usr/bin/env bash
# Replay the server's answer to a real libwayland client.
#
# Needs a Linux host with libwayland's development files (libwayland-dev),
# wayland-scanner, gcc and this repository's Rust toolchain. xdg-shell is
# scanned from the XML vendored in compositor/protocol, which is the same
# file the server's own tables come from. Writes roundtrip.txt beside this
# script, which the crate's tests read: the transcript the server produced,
# and what libwayland made of it.
#
# The transcript is regenerated here rather than read from the committed
# file, so a server that changed its answer and a committed file that did not
# show up as a failing test rather than a passing one.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

hex="$(cd "$here/../.." && cargo run --quiet --example transcript -p compositor-server)"

protocols="$here/../../protocol/protocols"
wayland-scanner client-header "$protocols/xdg-shell.xml" \
    "$work/xdg-shell-client-protocol.h"
wayland-scanner private-code "$protocols/xdg-shell.xml" \
    "$work/xdg-shell-protocol.c"

for program in roundtrip live; do
    gcc -O0 -Wall -Werror -I"$work" \
        $(pkg-config --cflags wayland-client) \
        -o "$work/$program" "$here/$program.c" "$work/xdg-shell-protocol.c" \
        $(pkg-config --libs wayland-client)
done

# The live half: the server on a socket of its own, and a real client having
# the whole conversation with it. XDG_RUNTIME_DIR is not used -- the path has
# a slash in it, which Wayland takes as an absolute path.
socket="$work/wayland-probe"
( cd "$here/../.." && cargo run --quiet --example serve -p compositor-server -- "$socket" ) \
    > "$work/server.txt" 2>&1 &
server=$!
for _ in $(seq 1 200); do
    [ -S "$socket" ] && break
    sleep 0.05
done
"$work/live" "$socket" > "$work/client.txt" 2>&1 || true
wait "$server" || true

{
    echo "# libwayland $(pkg-config --modversion wayland-client)"
    echo "transcript $hex"
    "$work/roundtrip" "$hex"
    echo "--- live client ---"
    sed 's/^/client /' "$work/client.txt"
    echo "--- live server ---"
    # The socket lives in a temporary directory whose name is different every
    # run; the path is not what is under test, so it is replaced with a
    # fixed one and the recording stays byte-identical between runs.
    sed -e "s|$work|<work>|g" -e 's/^/server /' "$work/server.txt"
} > "$here/roundtrip.txt"
echo "wrote $here/roundtrip.txt"
cat "$here/roundtrip.txt"
