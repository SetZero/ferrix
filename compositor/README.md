# The compositor

A Hyprland-shaped Wayland compositor, written in Rust, running on Ferrix: the
goal after `rustc`, carried by `docs/ROADMAP.md` stages 17 (display and
input), 18 (the compositor) and 19 (fidelity and the GPU).

It lives in the Ferrix tree but stands alone, as zinc and ferrousli do: its
own cargo workspace, a std Linux program reaching the kernel only through
Linux system calls. Hyprland is the reference for behaviour, and each module
names the part of Hyprland or hyprlang it follows.

## The plan it is built to

* **Pure cores first.** The configuration, the layouts, the dispatchers and
  the IPC's request format hold no socket, device or Wayland object, so they
  are written and tested before the Smithay decision in `docs/BACKLOG.md` is
  made and carry over whichever way it goes.
* **No C device stack, ever.** No udev, libinput, libseat, GBM, EGL or
  libwayland, not even on a Linux host: the backend is DRM dumb buffers and
  raw evdev through the ioctl subset Ferrix implements, the protocol server
  is pure Rust, and rendering is on the CPU. `xkbcommon` is the one C
  library allowed, from stage 18.
* **Headless pixel tests are the everyday gate.** Rendering into a buffer on
  the host and comparing pixels is the same comparison stage 17 and 18's
  exit tests make against QEMU's screendump.

## Crates

* **`config`** parses `hyprland.conf`: categories, the `category:key`
  shorthand, `$variables`, comments, `source`, the option table with
  Hyprland's types and defaults (integers that are also booleans and
  colours, floats, gradients, CSS gaps), `bind` with every flag letter,
  `unbind` and submaps, and the keywords the rest of the compositor
  interprets (`windowrule`, `monitor`, `workspace`, `exec-once`, `env`, …),
  with Hyprland's diagnostics. `Config::keyword` is `hyprctl keyword`. Fuzzed
  by `fuzz/fuzz_targets/hyprconf_parse.rs`.
* **`layout`** is Hyprland's window management with nothing but rectangles
  and ids: monitors, workspaces created on demand, the dwindle and master
  layouts with `gaps_in`, `gaps_out` and `border_size`, floating and
  fullscreen windows, Hyprland's focus history, and the dispatchers
  `movefocus`, `movewindow`, `workspace`, `movetoworkspace`(`silent`),
  `killactive`, `togglefloating` and `fullscreen`, parsed from a `Bind`.
  Each call returns the changes it caused; `State::layout` gives every
  visible window's client rectangle.
* **`render`** draws the frame: a `Canvas` over a `tiny-skia` pixmap with
  `clear`, `fill`, `border` and `composite` (`ARGB8888` source-over,
  `XRGB8888` copied and made opaque), each drawn only inside a `Damage` of
  disjoint rectangles; `present` into an `XRGB8888` target of any stride;
  `render` of one monitor from `layout`'s output with `config`'s border
  colours; and `damage_between` two layouts. The two pattern clients the
  stage 18 tests run are drawn here too, so the tests and the clients draw
  the same thing. Its gate is a committed expected image compared byte for
  byte (`tests/data/*.xrle`, written only with `COMPOSITOR_RENDER_BLESS=1`;
  `COMPOSITOR_RENDER_PPM=<dir>` dumps the frames to look at) with a
  one-pixel negative control.
* **`blank`** is iteration 1 on screen (`docs/DISPLAY.md`): it opens
  `/dev/dri/card0`, sets the connected connector's preferred mode with the
  legacy calls, fills a dumb buffer with `0x1E1E2E` and prints
  `compositor: scanout <mode> <W>x<H> colour 0x1e1e2e`, or
  `compositor: failed: <why>`, then waits, since it runs as init.
  `cargo xtask test-display` boots it and checks QEMU's screendump pixel by
  pixel; the `negative-control` feature draws one pixel wrong for the check
  to catch. The card code builds on Linux only; the choice of mode and CRTC
  and the fill are tested on any host.

## Testing

From this directory, on any host:

```
cargo test
```

`cargo xtask check` runs formatting, clippy and the tests here as part of the
whole gate.
