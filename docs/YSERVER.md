# Steam's X server: yserver, rootless on hyprix

Version 1, a draft written on 2026-09-28 for the customer, who decides §8.
The customer chose the server on 2026-09-27 (`docs/BACKLOG.md` Decisions):
[yserver](https://github.com/joske/yserver), Rust and MIT, with a rootless
Wayland backend that Ferrix adds. This document is the design that decision
asked for, written after the feasibility pass (§2), and before anything is
built.

## 1. What this is, and what it is not

The Steam client is an X11 program, and hyprix speaks only Wayland. This
document says how yserver becomes the X server between them:

* **how it is built and carried**: a pinned release, Ferrix's patches, and
  a data volume (§3);
* **the backend**: every top-level X window becomes a window on hyprix, and
  hyprix's input goes back into X (§4);
* **what hyprix needs**: nothing new to start with (§5);
* **the tests**, which follow `test-foot` (§6);
* **the slices and their points** (§7).

It is not GPU acceleration for X clients. hyprix has no
`zwp_linux_dmabuf_v1`, so frames reach it as `wl_shm` copies, and yserver
draws on the CPU through lavapipe. That is enough to show and use a window.
It is not fast. §4.3 says where the dmabuf path attaches later.

It is not a window manager either. yserver keeps no decorations and does no
tiling, because hyprix does both. It is also not Xwayland's `xwayland_shell_v1`
protocol, which is for a server whose surfaces the compositor has to match up
with X windows itself. Here the X server is the Wayland client, so it knows
which window is which.

**Exit of this design:** on x86-64 under KVM, `run-compositor --everything`
starts yserver as `:0`. An X client started from the terminal opens a window
that hyprix shows and tiles, takes keys and clicks, opens a menu where X puts
it, and closes when hyprix's close key is pressed. After that comes Steam's
bootstrapper (I5b of `docs/I386.md`).

## 2. What the feasibility pass found (2026-09-28)

`cargo xtask test-yserver` boots Ferrix with a volume holding Debian 13's
x86-64 libraries, Mesa's lavapipe, `x11-utils`, and yserver 1.6.0 (`0d00e81`)
built against them. yserver starts on `:1` with no DRM card and no input
device, renders through Vulkan on the CPU, and answers `xdpyinfo` about one
second after the boot's shell starts. It lists 21 extensions, among them
GLX, Present, RENDER, RANDR, MIT-SHM, Composite, XInputExtension, XKEYBOARD
and XTEST. Ferrix changed nothing for it.

Ferrix gets three things wrong, and none of them matters to this design:

* a `NETLINK_KOBJECT_UEVENT` socket is `EPROTONOSUPPORT`, so yserver's udev
  monitor and libinput's seat fail. The Wayland backend uses neither;
* `KDGKBMODE` on the console is `ENOTTY`, so yserver does not take the
  console over, which a rootless server must not do anyway;
* with no outputs, the root window is 0×0. The backend has to give it a
  size (§4.2).

It took two changes to yserver: `YSERVER_ALLOW_NO_INPUT`, a patch of ours
that lets it start without an input device, and the shader compiler named
through `GLSLC` at build time.

## 3. The build and the volume

yserver's own binary links libdrm, gbm, libinput, udev, xkbcommon, freetype
and fontconfig, so it is built as Chrome is carried: an x86-64 glibc program
against Debian 13's `-dev` packages, running on those packages' libraries
from the data volume. It is not built on ferrousli. That port is a question
for after Steam runs.

* **The source** is a fork of yserver under the customer's GitHub account
  (decision 1, §8), branched from the 1.6.0 tag (`0d00e81`) and pinned by
  commit. Ferrix's changes are commits on the fork, starting with
  `YSERVER_ALLOW_NO_INPUT`, and the Wayland backend is added there.
  `scripts/fetch/fetch-yserver.sh` fetches the pinned commit and builds it.
* **The Wayland client** is Ferrix's own client runtime,
  `compositor-toolkit`, over `compositor-wire`, `compositor-protocol` and
  `compositor-shm` (decision 2, §8). The fork takes it as a git dependency
  on this repository at a pinned commit. It is MIT, uses only std and
  `libc`, and has already been tested against hyprix. The toolkit waits in
  its own `dispatch`, so it hands its socket to yserver's core loop
  (`Client::as_raw_fd`) and is dispatched with a zero timeout when the
  socket is readable, which reads without blocking.
* **The volume** carries the packages listed in the fetch script, each
  pinned by SHA-256 as `fetch-chrome.sh` pins them, plus yserver, stripped. `run-compositor --everything` merges it into its volume, as it
  already merges steamcmd's.

## 4. The backend

### 4.1 Shape

`WaylandBackend` wraps the existing `KmsBackend`, running headless, and
passes its roughly 200 drawing, RENDER, DRI3, Present and GLX methods
through unchanged. The survey counted about 215 methods on the trait
(`yserver-core/src/backend/trait_def.rs`), 94 of them required. Writing them
again would mean writing an X renderer. What is new sits around the renderer:

* **the connection.** A Unix socket to `$WAYLAND_DISPLAY` binds
  `wl_compositor`, `wl_shm`, `xdg_wm_base`, `wl_seat`, `wl_output`,
  `zxdg_decoration_manager_v1` and, later, `ext_data_control`. Its fd is a new
  `BackendFdKind::Wayland`, and its dispatch arm receives `&mut ServerState`.
  That is one arm in `core_loop/run.rs`. The alternative is to disguise the fd
  as `Libinput`, which would save a core change and cost a lie.
* **a top-level map** from X window to Wayland objects, driven by the hooks
  the core already calls: `register_top_level`, `map_subwindow`,
  `unmap_subwindow`, `configure_subwindow` and `sync_top_level_order`. The
  hooks without state look the window up in `ServerState.resources` at their
  next state-bearing call.
* **selection at startup.** The backend is chosen when `WAYLAND_DISPLAY` is
  set and `YSERVER_BACKEND=wayland` asks for it, so a KMS boot never takes it
  by accident.

### 4.2 Windows

* **Screen.** One RandR output, named `WAYLAND-1`, the size of hyprix's first
  `wl_output`, gives the root its size. `HostX11Backend`'s synthetic
  `ynest-0` output is the template. A second screen is later work.
* **Contents.** Every child of the root is redirected inside the server,
  through the COMPOSITE redirect backing that `KmsBackend` already supports
  (`supports_redirect_activation`). One backing then holds a top-level's
  whole subtree, which is the image the compositor needs. As built in Y3,
  a core helper, `redirect_subwindows_for_server`, makes the record that a
  client's `RedirectSubwindows(root, Manual)` would make, owned by the
  server (`SERVER_OWNER`), as Xwayland does for its `serverClient`. The
  backend calls it on every loop iteration, which also puts it back after
  a server reset. A client that asks for the same redirect afterwards gets
  `BadAccess`, as it would under Xwayland. So no X compositing manager runs
  under this server, and Steam does not need one.
* **Mapping.** Each window is classified when it maps:
  * a normal window becomes an `xdg_toplevel`. It gets a title from
    `_NET_WM_NAME` or `WM_NAME` and an app id from `WM_CLASS`'s class, and
    each is updated through `on_window_property_changed`.
    `WM_TRANSIENT_FOR` becomes `set_parent`. `_MOTIF_WM_HINTS` without
    decorations asks for client-side decoration, which is what Steam's
    frameless windows want.
  * an override-redirect window (menus, tooltips, drop-downs) becomes an
    `xdg_popup` on the top-level that last had the pointer or the keyboard. Its
    positioner uses a 1×1 anchor rectangle at the window's X position,
    relative to that parent, with gravity bottom-right and no constraint
    adjustment. hyprix places a menu exactly there (`test_menu` checks it).
    Subsurfaces are not an option, because hyprix does not draw them.
* **Coordinates.** Each top-level keeps the position X gave it in root
  coordinates, wherever hyprix actually tiles it. Pointer input is
  translated as the window's X position plus the surface-local position, so
  X hit-testing and popup placement agree with each other. Wayland does not
  let yserver learn where hyprix put a window, and a program that reads its
  own root position gets X's answer, as it would under Xwayland.
* **Size from the compositor.** An `xdg_toplevel.configure` with a size
  becomes a `ConfigureWindow` of the X window, and `close` becomes
  `WM_DELETE_WINDOW`, or `KillClient` for a window without that protocol.
  The core has no call that lets a backend start either, so two small core
  helpers are added.

### 4.3 Frames

After each loop iteration, the backend takes each top-level whose backing
changed and whose last frame callback has come. It reads the window back
through the engine's `get_image` into that window's `wl_shm` buffers, then
attaches, damages, commits and asks for the next frame callback. On lavapipe
this is one extra copy of memory the CPU already drew into.

As built in Y3, "changed" is the backing's `content_version`, which every
drawing into it bumps. The whole window is read back and damaged, not the
damaged rectangles: an idle window costs nothing, and a busy one is read
once per frame of hyprix's. Reading only the damaged rectangles is the next
step if a large window turns out to be slow. The headless renderer's own
composition of the root is left as it was. Under a Manual redirect the
windows take no part in it.

When hyprix gains `zwp_linux_dmabuf_v1` (stage 19's remainder), the backing's
existing dmabuf export (`kms/vk/dri3.rs`) replaces the readback, and a GPU
render node replaces lavapipe. Neither changes the backend's shape.

### 4.4 Input and the cursor

* `wl_keyboard.key` becomes a `HostInputEvent::Key` with evdev + 8, and then
  goes through yserver's own XKB state (`cook_host_key`). The keymap hyprix
  sends is not used. yserver is started with `XKB_DEFAULT_LAYOUT` and
  `XKB_DEFAULT_VARIANT` from hyprix's `input:kb_layout`, so both agree.
  Keyboard enter and leave become X input focus on the window.
* `wl_pointer` motion becomes `PointerMotion` in root coordinates (§4.2).
  Buttons pass through. Wheel `value120` becomes the synthetic scroll buttons
  that yserver's libinput thread already makes.
* The cursor comes from `get_active_cursor_image`. It is copied into an shm
  buffer and set with `wl_pointer.set_cursor` on each enter and each cursor
  change.

As built in Y4, the seat is its own module on the fork,
`crates/yserver/src/wayland/input.rs`. Everything reaches X through
`handle_host_input`, as a KMS server's libinput thread's events do, so
yserver's grabs, key repeat, XKB and XI2 apply unchanged. It differs from
the plan above in three places:

* **The keymap.** yserver takes hyprix's layout when it connects: the
  toolkit reads the keymap hyprix sends, and the server compiles the same
  layout and variant by name. No `XKB_DEFAULT_LAYOUT` has to be passed.
  A layout hyprix changes later is not followed, because a new keymap
  would owe the X clients a MappingNotify.
* **Hit-testing.** X top-levels keep their root positions, so two of them
  that hyprix shows apart can overlap in root coordinates. A new core
  field, `ServerState::pointer_scope`, keeps the hit test to the top-level
  the pointer is on, as Xwayland's `xwl_xy_to_window` does. Off every X
  window the pointer is on the root, which sends the last window its
  `LeaveNotify`.
* **Focus.** Keyboard enter goes through a new core helper,
  `set_input_focus_for_server`, which does what a validated SetInputFocus
  does. Keyboard leave sets the focus to None and releases every key still
  down. The toolkit's own key repeat is dropped, because the X server
  repeats a held key itself.

The wheel takes hyprix's clicks where it counts them, else ten units of
`wl_pointer.axis` a click. The cursor goes through the toolkit's new
`Client::set_cursor_image` (Ferrix `e348bd13`). Two yserver bugs, found by
the test and fixed in the core for every backend, are the fork's `ed44c36`
and `e81b3a5` (§9).

### 4.5 The clipboard

X selections are handled only inside yserver's core, which has no internal
client. The bridge adds core hooks for a selection that is owned or asked
for. The Wayland side is an `ext_data_control` client, as `vdagent` is, so no
focus serial is needed. It carries text only at first (`UTF8_STRING`,
`text/plain;charset=utf-8`), the same as the SPICE bridge.

## 5. What hyprix needs

Nothing, for §7's first four slices. Every protocol above is already
advertised. There are two defaults to add:

* **A window rule for tiling.** hyprix tiles every toplevel, as Hyprland
  tiles X windows by default. Steam's main window tiles well. Its small
  dialogs are better floated, which a `windowrule` matching Steam's class
  does in the desktop's configuration, not in code.
* **A `exec-once` line** for yserver in `--everything`'s configuration, with
  `DISPLAY=:0` in the environment of programs started from the terminal.

## 6. The tests

* **`test-yserver`** stays as it is now: headless, with `xdpyinfo`, on the
  volume `fetch-yserver.sh` makes (Y1). It runs on demand, because it
  attaches a volume, like `test-steamcmd`.
* **`test-xwindow`** follows `test-foot`. Since Y2 it boots the compositor
  with yserver as its client and requires `xdpyinfo`'s screen to be the
  compositor's screen, the size yserver says it took. From Y3 on it also
  runs `xev` from the volume, then:
  * requires the window on the screendump (Y3). The test looks for xev's
    subwindow, a white square in a 4-pixel black border, which reaches
    hyprix only through the top-level's backing. It also requires
    `hyprctl clients` to list the window with xev's title, `Event Tester`,
    and the class `Xev`. xev sets no `WM_CLASS`, so the script sets one
    with `xprop` once the window is up, which also tests a class that
    changes after the window is made;
  * injects keys and a click through QMP and requires `xev`'s KeyPress and
    ButtonPress lines (Y4). The pointer is put at (120, 120) in xev's
    window, then come a left click, a wheel click down and the keys `a` and
    `z`. xev must report its focus, the pointer's entry, the motion, the
    press and release of button 1, button 5 and the `a` key, each at
    (120, 120) give or take 2 pixels. Then `xwininfo` asks for a window to
    be picked, and the test clicks xev's window again. That window must be
    the one picked, and the server's log must show the cursor it gave hyprix
    changing when `xwininfo` grabbed the pointer;
  * presses the close key and requires the client to exit (Y5).

  A menu case (`xfontsel`'s menu) is added with slice Y5.
* Each slice runs `cargo xtask check` before it lands. The tests are
  on-demand boots and do not join the item gate, because test time is the
  customer's first priority.

## 7. Slices and points

| Slice | What | Points |
|---|---|---|
| Y1 | `fetch-yserver.sh`: the fork at a pinned commit, pinned packages, the build, the volume; `test-yserver` on it. **Done 2026-09-28** | 3 |
| Y2 | `WaylandBackend` skeleton: connection, fd kind, `WAYLAND-1` output and root size; `xdpyinfo` shows hyprix's size (`test-xwindow`). **Done 2026-09-28** | 5 |
| Y3 | Top-levels: redirect, `xdg_toplevel`, shm readback, frame callbacks, title and app id; `test-xwindow` sees `xev`'s window. **Done 2026-09-28** | 8 |
| Y4 | Input and cursor: keys, pointer, wheel, focus, `set_cursor`; `xev` reports the injected events. **Done 2026-09-28** | 5 |
| Y5 | Popups, transients, compositor resize and close; the core helpers; the menu case | 8 |
| Y6 | Clipboard: core selection hooks, `ext_data_control` bridge, text | 5 |
| Y7 | yserver in `--everything`: `exec-once`, `DISPLAY`, window rules | 2 |

That is 36 points, against stage 19's 40-point first guess for an X server.
Y1 to Y4 are the smallest thing that shows a usable X window. After that,
I5b (the Steam bootstrapper) needs Y5 and Y7, and is sized separately once it
has run.

## 8. Decisions for the customer

Taken on 2026-09-28:

1. **Where the backend's code lives: a fork of yserver on the customer's
   GitHub account.** It was chosen over a patch series in this repository and
   over offering it upstream from the start. The fork is easier to offer
   upstream later.
2. **The Wayland client library: Ferrix's own** `compositor-wire`,
   `compositor-protocol` and `compositor-shm`. It was chosen over the
   `wayland-client` crate.
3. **The design is approved** for building, starting with Y1.

Still open:

4. **System V semaphores** (`docs/I386.md` I5). steamcmd carries on without
   them. Whether the Steam client does is found out at I5b, and the answer
   comes back as a decision then.

## 9. Where it stands

2026-09-28: Y1 to Y4 are done, 21 of the 36 points. `fetch-yserver.sh`
pins the fork at `ef858f2`. X top-levels show on hyprix as windows with
their titles and classes, and take keys, clicks, the wheel, the focus and
their cursors from it (`test-xwindow`). What Y4 found:

* **Two yserver bugs, fixed for every backend.** A pointer event
  redirected to a grab carried `child` None, so `xwininfo`, `xprop` and
  `xkill`, which pick a window by the child of a grabbed click on the root,
  always picked the root (`ed44c36`). Key events carried the root position
  as their window position (`e81b3a5`). Xorg sets both in
  `FixUpEventFromWindow`.
* **A Ferrix bug that drops X clients.** An accepted socket inherits the
  listener's `O_NONBLOCK` on Ferrix. yserver's listener is non-blocking, so
  a client whose setup had not arrived when yserver first read it was
  dropped, and said `unable to open display`: 3 of 13 `test-xwindow` runs.
  It was also hyprix's empty `hyprctl` request. Fixed in the kernel the
  same day: the listener's flag now decides only whether `accept` waits
  (12 of 12 runs clean after it).
* **Estimate against spend.** 5 points estimated, about 5 spent: one
  `test-xwindow` failure, the pick, which led to the first bug, and the
  second run green.
* **Not done.** Keys held when the keyboard enters a window are not
  pressed in X (the toolkit does not pass `wl_keyboard.enter`'s keys on).
  A layout hyprix changes after yserver starts is not followed. Neither
  `WM_TAKE_FOCUS` nor `_NET_ACTIVE_WINDOW` is sent on focus. Steam may want
  them, and that is found out at I5b.

What Y3 found for the slices after it:

* **Sizes.** An X window keeps the size its client gave it. It is drawn at
  the top left of the window hyprix tiles, and the rest is black.
  xev's 178×178 window sits in a 982×726 tile. Y5's
  `configure` → `ConfigureWindow` fixes it. Until then the X pixels are
  copied one to one, so on a screen scaled above 1 the window shows smaller
  by that scale.
* **A yserver bug fixed on the way.** Redirecting a window seeded its
  backing with each descendant's content but not the descendant's border,
  so xev's subwindow came out without its black ring. The fork's
  `6708199` fixes it for any COMPOSITE client, not only this backend.
* **Close.** `xdg_toplevel.close` is only logged. Y5 makes it
  `WM_DELETE_WINDOW` or `KillClient`.
* **Override-redirect windows are not shown yet.** Menus and tooltips have
  backings of their own, like every child of the root, so Y5 needs only the
  popup and its placement.
