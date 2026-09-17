# The compositor

A Hyprland-shaped Wayland compositor, written in Rust, running on Ferrix: the
goal after `rustc`, carried by `docs/ROADMAP.md` stages 17 (display and
input), 18 (the compositor) and 19 (fidelity and the GPU).

It lives in the Ferrix tree but stands alone, as zinc and ferrousli do: its
own cargo workspace, a std Linux program reaching the kernel only through
Linux system calls. Hyprland is the reference for behaviour, and each module
names the part of Hyprland or hyprlang it follows.

## The plan it is built to

* **Pure cores first.** The configuration, the layouts, the dispatchers, the
  renderer and the IPC's request format hold no socket and no descriptor, so
  every one of them is host-tested and fuzzed before anything of it runs on
  Ferrix. The server is written from scratch rather than on Smithay, decided
  on 2026-09-17 with the reasoning in `docs/BACKLOG.md`.
* **No C device stack, ever.** No udev, libinput, libseat, GBM, EGL or
  libwayland, not even on a Linux host: the backend is DRM dumb buffers and
  raw evdev through the ioctl subset Ferrix implements, the protocol server
  is pure Rust, and rendering is on the CPU. `xkbcommon` is the one C
  library allowed, from stage 18. libwayland is linked by one thing and
  never by the compositor: `wire/probe/wire.c`, a probe that runs on the
  development host to print the bytes a real implementation sends, the way
  `libs/linux-abi/probe` prints the kernel's numbers.
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
* **`wire`** is Wayland's wire protocol with no libwayland: the message
  header, every argument type, descriptors travelling beside the bytes
  rather than in them, and the per-client object map that keeps a client's
  ids and the server's in their own halves. It holds no socket -- a
  descriptor is an `i32` and nothing more -- so it is host-tested and
  fuzzed. `probe/wire.c` drives a real libwayland client and a real
  libwayland server over socket pairs and prints what they wrote;
  `probe/wire.txt` is that output, committed, and the tests require this
  crate to write the same bytes and to read them back.
* **`protocol`** is the interface tables: what each interface's requests and
  events are called, at which opcode, with which argument types, and every
  enumeration value. Generated from the protocol XML by
  `scripts/gen-wayland-protocol.py`, which `cargo xtask check` runs with
  `--check`. The XML is vendored under `protocol/protocols/` rather than read
  from the machine, so the tables cannot change under the compositor without
  a commit. `probe/interfaces.c` links against libwayland's own compiled
  `wl_*_interface` structures and prints them; the tests require the
  generated tables to agree, message for message. Adding a protocol is
  vendoring one XML and adding a line to the generator's `FILES`.
* **`server`** is what a client's requests do: one `Client` holds a
  connection's objects, is handed the bytes that arrived and the descriptors
  with them, and gives back the bytes to send. It holds no socket and no
  pixel, so object lifetimes, versions and every way a client can break the
  rules are host-tested. A protocol error is the end of a connection --
  Wayland has no way to refuse one request and carry on -- so every refusal
  queues one `wl_display.error` and stops reading. `probe/roundtrip.c`
  replays the server's answer to a real libwayland client and records the
  globals it reports, then drives it through everything it does to show a
  window and records the requests it sent, which the tests replay into the
  server. `probe/live.c` goes further: it runs a real client against
  `examples/serve.rs` over a real socket and has the whole two-way
  conversation -- bind, make a window, take the configure, ack it, attach a
  buffer and commit. That is the handshake every application performs when it
  starts, and a compositor that gets any step of it wrong is one no
  application will run on.
* **`socket`** is the one crate that has to be on Ferrix to be tried: an
  `AF_UNIX` listener, and `sendmsg`/`recvmsg` with the `SCM_RIGHTS` control
  messages that carry a client's descriptors beside its bytes, which the
  standard library has no stable way to do. A read gives a whole number of
  bytes and not a whole number of messages, so it keeps what has arrived and
  hands the server as much as makes messages.
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

* **`hyprix`** is the compositor: it reads a `hyprland.conf`, listens on a
  Wayland socket, tiles what connects to it, draws on the CPU and puts the
  frame on a screen. Nothing in it parses a file, works out a layout, draws a
  pixel or decodes a message -- the crates above do those -- so it is the loop
  that joins them and the two places the compositor touches the world: a
  client's shared memory, and the screen. `--headless WxH` draws into memory
  instead, and `--dump <dir>` writes each frame as a PPM.
* **`pattern`** is a Wayland client in one file, over `wire` and `socket`
  rather than a toolkit, that draws one of `render`'s test patterns. It is
  what the compositor's tests put on screen, and it exercises the same crates
  from the client's side.

## Testing



From this directory, on any host:

```
cargo test
```

`cargo xtask check` runs formatting, clippy and the tests here as part of the
whole gate.

The one that matters most is `hyprix/tests/two_clients.rs`: it runs the
compositor and two pattern clients in threads of one process, over a real
socket, and compares the frame the compositor composed against the image
`render`'s own tests bless. The two are built by different paths -- one by
calling the renderer with rectangles, the other by two programs talking
Wayland to a server that works those rectangles out from their requests -- so
a difference between them is a real one. A second test runs one client
instead of two and requires the comparison to notice.
