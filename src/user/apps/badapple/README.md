# badapple

Bad Apple!! on Ferrix: Anira's shadow-art video on the screen and its song
through `/dev/snd`, the picture kept in step by the sound card's clock
(`docs/MEDIA.md` has the whole design).

```
badapple VIDEO.bav SONG.m4a [SECONDS]
```

Straight on `/dev/dri/card0` as init, or in a window when `WAYLAND_DISPLAY`
names a compositor. Its lines start `badapple:`.

## What is here

| Crate | What |
|---|---|
| `player` | The program, and its arithmetic -- command line, clock, scaling, frame stepping -- as a library the host tests |
| `bav` | `.bav`, a grey-scale run-length video format, and `bav-pack`, the host's converter from ffmpeg's grey frames |

The player plays through the system's `pcm` (playback through `/dev/snd`)
and `resample` (44.1 kHz to 48 kHz), which the sound server shares, and
draws through the compositor's `drm` and `toolkit`: four relative paths out
of this folder, the only ones.

## Running it

The video is not in the tree. `tools/common/fetch/fetch-badapple.sh`
downloads the original upload, and xtask converts it once with ffmpeg and
`bav-pack`. Then:

* `cargo xtask run-badapple` plays all of it in a window, heard on the
  host's sound server;
* `cargo xtask run-compositor --everything` carries it on the desktop,
  started from the launcher or with `SUPER M`;
* `cargo xtask build --app badapple` puts the player alone at
  `/bin/badapple`, without the video.

## Its gate

`cargo xtask test-badapple`: 30 s as init and 12 s in a window, with the
held frame found pixel for pixel in a screendump, the song found in what
QEMU's sound card wrote, the two in step, and a negative control -- the
player built with `negative-control`, every shade inverted -- that the
picture check must fail. `cargo xtask test-apps --app badapple` only starts
it from a shell.
