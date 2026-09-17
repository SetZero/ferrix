#!/usr/bin/env python3
"""Describe the busiest of a run's frames.

`real-client.sh` runs a third-party toolkit, whose exact pixels depend on the
fonts installed and the version of its renderer, so a picture of them is not
something to commit. What matters is that the compositor drew the client's
window at all, and this says so with the frame's size, how many distinct
colours are in it, and how much of it is not the compositor's background --
all of which are zero or one for a frame where nothing was drawn.

The busiest frame rather than the last, because the run ends after the client
has closed its window and the last frame is an empty screen.
"""

import collections
import pathlib
import sys

# `compositor/render`'s `Style::default` background, as the PPM holds it.
BACKGROUND = (0x11, 0x11, 0x11)


def describe(path: pathlib.Path):
    """One frame's size, colours and drawn pixels."""
    data = path.read_bytes()
    magic, size, depth, pixels = data.split(b"\n", 3)
    if magic != b"P6" or depth != b"255":
        raise SystemExit(f"{path} is not a binary PPM")
    width, height = (int(value) for value in size.split())
    counts: collections.Counter = collections.Counter()
    for at in range(0, width * height * 3, 3):
        counts[pixels[at : at + 3]] += 1
    drawn = sum(
        count for colour, count in counts.items() if tuple(colour) != BACKGROUND
    )
    return width, height, len(counts), drawn


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: frame-summary.py <frame.ppm>...", file=sys.stderr)
        return 2
    frames = [describe(pathlib.Path(name)) for name in sys.argv[1:]]
    if not frames:
        print("none")
        return 0
    width, height, colours, drawn = max(frames, key=lambda frame: frame[3])
    print(f"frames {len(frames)}")
    print(f"size {width}x{height}")
    print(f"colours {colours}")
    print(f"not-background {drawn}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
