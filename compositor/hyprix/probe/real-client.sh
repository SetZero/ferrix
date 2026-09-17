#!/usr/bin/env bash
# Run a real, unmodified Wayland application on the compositor and record
# what it said.
#
# The compositor's own tests use `compositor/pattern`, which is written
# against the same crates the server is: it proves the two halves agree, not
# that the protocol is right. A third-party toolkit does the other half. It
# was written against libwayland and every other compositor, it knows nothing
# about this one, and every warning it prints is a protocol this compositor
# does not offer.
#
# Needs a Linux host with `foot` (a Wayland terminal) on the PATH; it is not
# on Ferrix's image and this is a development-host check, so the script says
# so and stops if it is missing rather than failing.
#
# The client is given a command shorter than the compositor's deadline, so
# it closes its own connection and the record ends in a clean goodbye rather
# than in the broken pipe a compositor exiting first would give it.
#
# Writes real-client.txt beside this script: the client's log, and the busiest
# frame's size and a count of the pixels that are not the background, which
# is how "it drew something" is recorded without committing a picture of
# somebody else's font rendering.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$here/../.."

if ! command -v foot > /dev/null; then
    echo "foot is not installed; nothing to record" >&2
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
socket="$work/wayland"
frames="$work/frames"

( cd "$root" && cargo run --quiet -p hyprix -- \
    --headless 1024x768 \
    --display "$socket" \
    --dump "$frames" \
    --deadline 9000 \
    --exec "foot --log-level=info --log-no-syslog -e sleep 3" ) \
    > "$work/log.txt" 2>&1 || true


{
    echo "# foot $(foot --version 2>&1 | head -1)"
    # The paths and thread counts are this machine's, not the compositor's.
    sed -e "s|$work|<work>|g" \
        -e 's/using [0-9]* rendering threads/using <n> rendering threads/' \
        -e 's/^/client /' "$work/log.txt" \
        | grep -vE 'client (info: (fcft|config)|$)'
    if ls "$frames"/*.ppm > /dev/null 2>&1; then
        python3 "$here/frame-summary.py" "$frames"/*.ppm | sed 's/^/frame /'
    else
        echo "frame none"
    fi
} > "$here/real-client.txt"
echo "wrote $here/real-client.txt"
cat "$here/real-client.txt"
