# Media: Bad Apple!! and Doom

The customer's order of 2026-09-26: Ferrix plays **Bad Apple!! with its
sound**, and runs **Doom, written in Rust**. Both are Linux programs on
Ferrix's Linux ABI, in `src/user/system/linux/media/` (and `userland/doom/` for Doom),
and both stand on what stages 17 and 22 built: `/dev/dri/card0`'s dumb
buffers (`docs/DISPLAY.md`), `/dev/snd` (`docs/AUDIO.md`) and
`/dev/input/eventN` (`docs/INPUT.md`). Neither needs anything new from the
kernel.

## 1. Nothing copyrighted in the repository

* **Bad Apple!!** is ZUN's song as Alstroemeria Records arranged it, with
  Anira's shadow-art PV. `tools/common/fetch/fetch-badapple.sh` downloads the
  original upload (niconico sm8628149) from archive.org, and its SHA-256 is
  the one thing about it the repository holds.
* **Doom**'s game data is Freedoom's (BSD), fetched by a script. id's
  shareware or retail WADs are never used by a gate or committed.
* **The Doom engine** is room4doom (github.com/flukejones/room4doom). It is
  labelled MIT, but its own README calls it a transliteration of id's Doom
  C source, which is GPL-2.0, so Ferrix treats it as GPL. The customer's
  decision (2026-09-26): cargo fetches it at build time as a git
  dependency pinned to one revision, the way Chrome is fetched. What the
  repository holds is Ferrix's backend for it (the screen, the sound and
  the input), which is written for Ferrix and is MIT like the rest.

## 2. Bad Apple!!

`src/user/system/linux/media/` is a cargo workspace of its own:

| Crate | What |
|---|---|
| `bav` | `.bav`, the video format, and `bav-pack`, the host's converter |
| `resample` | Rational polyphase resampling (44.1 kHz → 48 kHz is 160/147) |
| `pcm` | Playback through `/dev/snd`, as alsa-lib's `hw` plugin drives it |
| `badapple` | The player |

**The video is converted on the host.** No H.264 decoder written in Rust
could run on Ferrix, and a shadow play needs few of H.264's tools. `bav-pack`
takes ffmpeg's grey frames and writes 16 shades of grey in runs. Each run
either paints a shade or keeps the previous frame's pixels (the format is
documented in `bav/src/lib.rs`). Before packing, ffmpeg snaps near-black
and near-white to pure black and white, and a pixel within one shade of what
is shown is kept rather than repainted. The source's compression noise
would otherwise be most of the file. All 6572 frames come to 32 MB, in the
initramfs.

**The song is decoded on Ferrix.** The player reads the original's own AAC
track (copied out by ffmpeg, not re-encoded) with symphonia, which is Rust
and MPL-2.0, used unmodified. It converts the song to the card's 48 kHz
with `resample` and writes it to the card.

**The sound card is the clock.** The song's thread writes a period at a
time. The card's buffer is full, so each write waits, and after each one
the thread records how far the speaker has got: frames written, less the
card's `DELAY`. The picture thread shows the frame for that moment and
decodes any it skipped without drawing them. The picture follows the song
however loaded the machine is, and skipping frames is how it keeps up.
Between the card's reports the clock runs on for at most 100 ms, so if the
card stalls, the picture waits for it. Without a card, the clock is the wall
clock.

**The screen** is one dumb buffer the size of the mode. The picture is
scaled by nearest pixel to fill it and centred. On a virtio-gpu each frame
is a `DIRTYFB` of the rows that changed, and the host's copy is what is
scanned out, so a half-drawn frame is never shown.

**On the desktop** the player is a window. When `WAYLAND_DISPLAY` is set it
opens a toplevel through the compositor toolkit (`Client::toplevel`, added
to `src/user/system/linux/compositor/toolkit` for it) instead of the card. The
compositor tiles and sizes the window, and the picture is fitted into it.
The sound card is still the clock. Frame callbacks only say when the
compositor is ready for the next picture, so a frame is never drawn over
one not yet shown. Closing the window stops the song and ends the program.
At the end of the video the last frame stays until the window is closed.
`run-compositor --everything` carries the player, the video and the song,
a `.desktop` entry for a launcher, and `SUPER M` to start it. The image
has no launcher yet, so the keybind is the way in today.

`cargo xtask test-badapple` makes four checks (details in
`tools/common/xtask/src/badapple.rs`). It checks two boots: 30 s with the player as init
on the card, and 12 s with it as a fullscreen window in hyprix, started by
`exec-once`.
* **The picture:** the frame the player holds at the end must be on the
  screen exactly, at every one of its 196608 pixels.
* **The song:** each two seconds of what the card played must correlate
  with ffmpeg's own decoding of the song.
* **The sync:** the picture and the song must be within 200 ms of each
  other.
* **The negative control:** a player that inverts the picture must fail the
  picture check at every pixel.

`cargo xtask run-badapple` plays all of it in a window, heard on the host's
sound server.

## 3. Doom (in the backlog)

The customer put Doom in the backlog on 2026-09-26, once Bad Apple!! was
done; nothing of it is built. This is the plan it was estimated from, and
what reading room4doom (revision `891b7ddf`, 2026-08-11) found.

**What room4doom offers a new platform.** The game, level, WAD, renderer,
UI and OPL2 crates depend on no platform. `game-exe` (the binary),
`render/backend`, `sound/rodio` and `input` do: SDL2, winit, softbuffer,
wgpu and cpal. Three things follow:

* `render/backend`'s `ActiveBackend` is defined only when one of its
  display features is on, so the crate does not build without SDL2, winit
  or wgpu. Ferrix's frontend should skip it and drive `software25d`
  directly: `Software25D::new(hfov, w, h, hi_res)`, then `draw_view(view,
  level_data, pic_data, &mut PixelTarget)`. It then draws the status bar,
  messages, menu, intermission and finale (`doom-ui`, through
  `hud-util`) into anything that implements `render_common::DrawBuffer`,
  and shows that in a dumb buffer as `badapple` does. The melt wipe is
  `render_backend::Frame`'s, which is not reachable either, so it has to
  be written again or skipped.
* The loop to write is `game-exe/src/d_main.rs`'s, about 450 lines: a tic
  (`game.ticker`, the menu's ticker, `build_tic_cmd` from input) at 35 Hz,
  and a display pass (a `RenderView` built from the console player, the
  palette set by damage and bonus counts, the view, then the UI). Write it
  against the libraries' API, not by copying the file.
* Sound is a `std::sync::mpsc` channel of `sound_common::SoundAction`.
  Ferrix's mixer consumes it: effects from the WAD's `DS*` lumps at
  11025 Hz, and music as MUS to MIDI (`sound_common::read_mus_to_midi`)
  into `opl2_emulator`.

Planned, in the order it would be built:

* **D1: the engine for Ferrix.** Pull in room4doom's crates that don't
  depend on a platform (`wad`, `level`, `gameplay`, `gamestate`,
  `render/software25d`, `ui`, `sound/common`, `sound/opl2_emulator`). Add a
  main loop of Ferrix's own that draws the software renderer's frame into a
  dumb buffer as `badapple` does. 8 points.
* **D2: sound.** A mixer of Ferrix's own behind room4doom's `SoundAction`
  channel: the 11025 Hz effects at 640/147 through `resample`, and the OPL2
  music, through `pcm`. 5 points.
* **D3: input, and the gate.** Keys from `/dev/input/eventN` as room4doom's
  input events. `cargo xtask test-doom` plays Freedoom's `DEMO1` for a fixed
  number of tics, then checks the screen against the tic's frame and the
  sound as Bad Apple's check does. It also presses keys over QMP as
  `test-input` does. 5 points.
* **D4: `run-doom`,** playable in a window with the keyboard. 3 points.

21 points, estimated before starting, none spent.

## 4. Where it stands

* **2026-09-26:** Bad Apple!! plays with its sound. `test-badapple` passes
  on x86-64, AArch64, and ARMv7-A at `--smp 2`:
  * Frame 899 was exact at every pixel, and every window of the song
    correlated at 1.000 with no lag.
  * Picture and song were within 41 ms of each other.
  * The negative control failed at every pixel.
  * Under a host load of 40, the player showed 789 to 889 of 900 frames,
    skipping the rest to stay with the song.
* **2026-09-26, later:** Bad Apple!! is on the desktop, at the customer's
  order: a window in hyprix, in the `--everything` image, started with
  `SUPER M`. The window boot in `test-badapple` passes on all three
  architectures: frame 359 was exact at every pixel, every window of the
  song correlated at 1.000, and picture and song were 0 to 7 ms apart. On
  the `--everything` desktop itself (x86-64, KVM, GL), `SUPER M` started it
  tiled beside the terminal and Chrome, 11 ms off the song at 20 s.
* **Doom:** in the backlog by the customer's word of 2026-09-26. Not
  started; §3 is where to begin.
