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
  visible window's client rectangle. `popup` is the other geometry a
  compositor owes a client: `xdg_positioner`'s rules for where a menu goes,
  in the protocol's own order -- anchor, offset, gravity, then flip, slide
  and resize -- which is arithmetic and so is tested against the rules
  rather than against a screenshot.
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
  vendoring one XML, adding a line to the generator's `FILES` and a module to
  `src/generated/mod.rs`, and naming its interfaces in `probe/interfaces.c`
  and `probe/interfaces.sh` so libwayland's own tables are compared against
  the new ones too -- a table nothing compares is a table nothing checks.
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
* **`ipc`** is `hyprctl`'s request shape and its answers: the flags a request
  carries, `[[BATCH]]`, and the JSON and readable forms of `version`,
  `monitors`, `workspaces`, `clients`, `activewindow`, `activeworkspace`,
  `submap`, `binds`, `devices`, `layers`, `cursorpos`, `locked`,
  `workspacerules` and `globalshortcuts`, with Hyprland 0.56.2's own field
  names in its own order. `dispatch`,
  `keyword` and `reload` come back for the compositor to run, since this
  crate holds no compositor. It holds no socket either, so the answers are
  host-tested; a JSON parser in the tests is what says an answer is JSON
  without the compositor taking a dependency for it.
* **`drm`** is the card: `/dev/dri/card0` through the legacy mode-setting
  calls, and nothing else a compositor does not need -- no atomic commit, no
  GEM import, no render node. It finds a connected connector and a mode,
  finds a CRTC that can drive it, makes dumb buffers, maps them and shows
  one. It is the only crate here that builds on Linux alone, because
  `/dev/dri` is Linux's and Ferrix's through its Linux ABI; the parts that
  are arithmetic rather than ioctls are tested on any host. `blank` and
  `hyprix` both drive the screen through it.
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
  `cursor` is the arrow the compositor draws when no client has said
  otherwise: a shape in code rather than a theme file, since Ferrix has
  neither the files nor a library to read them with.
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
  client's shared memory, and the screen. The screen is `compositor/drm`'s
  card, with two dumb buffers drawn into in turn and shown with a page flip;
  `--headless WxH` draws into memory instead, and `--dump <dir>` writes each
  frame as a PPM. `cargo xtask test-compositor` boots it as init on Ferrix
  with two `pattern` clients in the initramfs, and requires QEMU's screendump
  to be the same picture the host test makes, pixel for pixel.
* **`pattern`** is a Wayland client in one file, over `wire` and `socket`
  rather than a toolkit, that draws one of `render`'s test patterns. It is
  what the compositor's tests put on screen, and it exercises the same crates
  from the client's side.
* **`term`** is the terminal: a character grid with the escape sequences a
  shell and its programs actually send, drawn with Hack -- rasterised from
  the TrueType faces in `term/font/` into coverage cells by
  `scripts/gen-term-font.py`, and blended a pixel at a time -- into a
  `wl_shm` buffer. It starts a program on a pseudoterminal with
  the slave for its session and its three descriptors, and `--headless` runs
  one with no window at all, which is what `cargo xtask test-pty` boots.
* **`clip`** is `wl-copy` and `wl-paste`, neither of which is on Ferrix:
  `clip copy <text>` offers text as `text/plain;charset=utf-8` and stays
  alive to answer -- Wayland's clipboard is a promise, and the data lives in
  the program that made it -- and `clip paste` asks for the selection on a
  pipe and prints what comes back. Between them they are the compositor's
  clipboard tested with no window and no screen. `--primary` does the same
  to the selection a middle click pastes, which is the same protocol under
  another name.
* **`lswt`** is a taskbar with the drawing taken out: it binds
  `zwlr_foreign_toplevel_management_v1`, takes the handle the compositor
  makes for each window, and prints the title, the application id and the
  states -- `lswt activate <title>` and `lswt close <title>` send the two
  requests a click on a taskbar entry makes. Named after Leon Henrik
  Plickat's program of the same name, and there for the same reason `clip`
  is: it is the part of a bar that can be tested with no screen.
* **`shot`** is `grim` without the file format: it binds
  `zwlr_screencopy_manager_v1` and a `wl_output`, makes the `wl_shm` buffer
  the compositor says to make, hands it over, and prints the size and an FNV
  digest of every pixel it was given. The digest is how a whole screen is
  compared through a serial port, and comparing it against the image
  `render` blesses is the strongest picture check in the tree: it is what
  the compositor handed a *program*, not what QEMU read off the scanout.
* **`lock`** is `hyprlock` with the password taken out: it takes the screen
  through `ext-session-lock-v1`, draws a checkerboard over every screen,
  holds it and gives it back. It asks for no password because Ferrix has no
  notion of one; what it tests is the compositor's half -- that the windows
  stop being drawn the moment the lock is taken, that a keybind that is not
  `bindl` stops firing, and that the screen comes back.
* **`ctl`** is `hyprctl`, over `ipc`'s request shape; **`plug`** is the
  example plugin, a program the compositor starts and talks to rather than a
  shared object it loads; **`anim`** is Hyprland's bezier curves and its
  animation tree, holding no window and no clock; **`regex`** is the RE2
  subset a `windowrule` matches with, which counts its steps rather than
  backtracking for ever; **`xkb`** turns a keycode into a keysym; and
  **`evecho`** is evdev with no libinput -- the input backend the seat reads
  its devices through, and the argument splitting an `exec` line needs where
  there is no shell to do it.

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

The same file holds the protocol tests that need more than one program at
once, each with the compositor and its clients in threads of one process: a
`clip copy` and a `clip paste` that must agree about what was on the
clipboard, an `lswt` that must be told the windows and must be able to close
one from outside it, and a `shot` whose screenshot must be the renderer's
blessed image pixel for pixel.

`hyprix/probe/real-client.sh` is the other half: it runs `foot`, a Wayland
terminal built against libwayland and every other compositor, and records
what it said. A client written against this tree's own crates can only show
that the two halves agree; a toolkit that knows nothing about this one is
what finds the protocols it does not offer.

`hyprix/probe/hyprctl.sh` is the same idea for the IPC: it drives the
compositor with Hyprland's own `hyprctl`, the program people type and the one
every script and bar is written around, and records what it printed.
