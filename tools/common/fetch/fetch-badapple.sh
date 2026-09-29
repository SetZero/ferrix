#!/usr/bin/env bash
# Fetch Bad Apple!!'s shadow-art video, which `cargo xtask test-badapple`
# plays on Ferrix.
#
# The video is ZUN's song as Alstroemeria Records arranged it, with Anira's
# shadow-art PV, and none of it is Ferrix's: it is not in the repository and
# never goes into it. This downloads the original upload (niconico
# sm8628149, 2009, 512x384 H.264 at 30 fps with AAC-LC stereo at 44.1 kHz)
# as archive.org keeps it, and checks it against the SHA-256 xtask checks
# too. xtask converts it, the first time it needs to, into the `.bav` video
# and the song file the player reads, beside it.
#
# Writes $FERRIX_BADAPPLE/sm8628149.mp4 (default
# ~/.local/share/ferrix/badapple). Needs curl and sha256sum; the conversion
# needs ffmpeg.
#
# Usage: scripts/fetch/fetch-badapple.sh

set -euo pipefail

archive=${ARCHIVE_ORG:-https://archive.org/download}
out=${FERRIX_BADAPPLE:-$HOME/.local/share/ferrix/badapple}

# The archive.org item and file, and the SHA-256 of that file as downloaded
# on 2026-09-26 (archive.org's own listing gives its SHA-1,
# a976e97333f4e897eaf5c38ec344518df9af3d92).
ITEM=nicovideo-sm8628149-orig
FILE=sm8628149.59970.mp4
SHA256=75d2261d1f75da80a3ba899641def55230c5f8564a310df3cbbe88b8c0ea2abb

for tool in curl sha256sum; do
    command -v "$tool" > /dev/null || { echo "fetch-badapple: $tool is not installed" >&2; exit 1; }
done
command -v ffmpeg > /dev/null \
    || echo "fetch-badapple: ffmpeg is not installed; xtask test-badapple will need it" >&2

mkdir -p "$out"
video="$out/sm8628149.mp4"
if [ ! -f "$video" ]; then
    curl -fsSL -o "$video.part" "$archive/$ITEM/$FILE"
    mv "$video.part" "$video"
fi
echo "$SHA256  $video" | sha256sum -c --quiet \
    || { echo "fetch-badapple: $video does not match its pinned checksum" >&2; rm -f "$video"; exit 1; }
printf '%s\n' "$video"
