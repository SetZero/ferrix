#!/usr/bin/env bash
# Replay the server's answer to a real libwayland client.
#
# Needs a Linux host with libwayland's development files (libwayland-dev),
# gcc and this repository's Rust toolchain. Writes roundtrip.txt beside this
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

gcc -O0 -Wall -Werror \
    $(pkg-config --cflags wayland-client) \
    -o "$work/roundtrip" "$here/roundtrip.c" \
    $(pkg-config --libs wayland-client)

{
    echo "# libwayland $(pkg-config --modversion wayland-client)"
    echo "transcript $hex"
    "$work/roundtrip" "$hex"
} > "$here/roundtrip.txt"
echo "wrote $here/roundtrip.txt"
cat "$here/roundtrip.txt"
