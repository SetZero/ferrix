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

## Testing

From this directory, on any host:

```
cargo test
```

`cargo xtask check` runs formatting, clippy and the tests here as part of the
whole gate.
