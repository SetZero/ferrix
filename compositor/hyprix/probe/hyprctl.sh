#!/usr/bin/env bash
# Drive the compositor with Hyprland's own `hyprctl`, and record what it said.
#
# `compositor/ipc`'s tests check the answers against Hyprland's source. This
# checks them against Hyprland's client: the program people actually type, and
# the one every script and bar is written around. If `hyprctl clients` prints
# nothing here, no Hyprland script works on this compositor whatever the JSON
# says.
#
# Needs a Linux host with `hyprctl` on the PATH; it is a development-host
# check and not on Ferrix's image, so the script says so and stops if it is
# missing rather than failing.
#
# Writes hyprctl.txt beside this script.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$here/../.."

if ! command -v hyprctl > /dev/null; then
    echo "hyprctl is not installed; nothing to record" >&2
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work" || true' EXIT
export XDG_RUNTIME_DIR="$work/runtime"
export HYPRLAND_INSTANCE_SIGNATURE=ferrix-probe
mkdir -p "$XDG_RUNTIME_DIR"

pattern="$(cd "$root" && cargo build --quiet -p compositor-pattern -p hyprix \
    && echo "${CARGO_TARGET_DIR:-$root/target}/debug/pattern")"

( cd "$root" && cargo run --quiet -p hyprix -- \
    --headless 1024x768 \
    --display "$work/wayland" \
    --instance "$HYPRLAND_INSTANCE_SIGNATURE" \
    --deadline 14000 \
    --exec "$pattern checkerboard one" \
    --exec "$pattern gradient two" ) > "$work/server.txt" 2>&1 &
server=$!

# Wait for both windows rather than for a fixed time: the clients are started
# in order and the second takes a moment to draw.
for _ in $(seq 1 200); do
    if [ "$(hyprctl -j clients 2>/dev/null | grep -c '"address"' || true)" = 2 ]; then
        break
    fi
    sleep 0.1
done

say() { echo; echo "\$ hyprctl $*"; hyprctl "$@" 2>&1 || true; }

{
    echo "# hyprctl $(hyprctl --version 2>&1 | head -1)"
    say version
    say monitors
    say workspaces
    say activewindow
    say -j activewindow
    say clients
    # The focus starts on the window opened last; moving it left is the
    # dispatcher every Hyprland configuration binds.
    say dispatch movefocus l
    say activewindow
    # An option changed while it runs, which re-tiles every window.
    say keyword general:gaps_in 40
    sleep 0.5
    say -j clients
    say nonsense
} > "$here/hyprctl.txt"

wait "$server" 2>/dev/null || true
echo "wrote $here/hyprctl.txt"
