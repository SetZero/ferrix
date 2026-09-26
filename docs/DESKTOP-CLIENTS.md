# The desktop's own clients: waybar, fuzzel, hyprlock and hypridle in Rust

## 1. The aim

The customer runs Hyprland on nazuna, and Ferrix's compositor
(`userland/compositor/hyprix`) already reads their `~/.config/hypr/hyprland.conf`.
That file starts `waybar` and `hypridle`, binds fuzzel to `SUPER+R` and
hyprlock to `SUPER+L`. None of the four is on Ferrix, and all four are C or
C++ on GTK, Pango, cairo, fontconfig and libwayland, none of which is either.
The customer asked (2026-09-26) for the four to be written in Rust for
Ferrix, so that their own configuration works there *unchanged*:

| program | the file it reads | its upstream |
|---|---|---|
| `waybar` | `~/.config/waybar/config.jsonc`, `style.css`, `icons/*.svg` | Alexays/Waybar (GTK3) |
| `fuzzel` | `~/.config/fuzzel/fuzzel.ini` | dnkl/fuzzel (fcft, pixman) |
| `hyprlock` | `~/.config/hypr/hyprlock.conf` | hyprwm/hyprlock 0.9 (hyprlang, hyprgraphics) |
| `hypridle` | `~/.config/hypr/hypridle.conf` | hyprwm/hypridle (hyprlang) |

Five streams do it at once: one for each program and one, `clients-base`,
for the foundation the four share (§2). The fidelity target is the file:
every line either does what it does upstream, or is reported by name on
standard error in the upstream program's own words -- never ignored in
silence, and never a reason to refuse the file. What cannot work on Ferrix
is said (§7), not faked.

Each program is a binary named after its upstream, in a crate of the
`userland/compositor/` workspace, installed as `/bin/<name>` in the desktop image,
so `exec-once = waybar` and `bind = …, exec, hyprlock` find it.

## 2. The foundation

Four crates, on branch `clients-base` until each lands on `main`. They
exist so that the four programs do not each write a fifth copy of the
hand-rolled Wayland client that `userland/compositor/term`, `userland/compositor/lock` and
`userland/compositor/pattern` each carry.

### 2.1 `userland/compositor/toolkit` -- the Wayland client runtime

`compositor_toolkit::Client` is one connection and everything bound on it,
over `userland/compositor/wire`, `socket`, `shm`, `protocol` and `xkb` -- no
libwayland. Pull, not callbacks: a program makes its surfaces and turns
`Client::dispatch(timeout)`, which blocks in `poll` on the Wayland socket,
the timers, the children's pipes, a `signalfd` and any descriptor the
program hands it, and returns a `Vec<Event>`.

* **Connecting**: `WAYLAND_DISPLAY`, joined to `XDG_RUNTIME_DIR` unless it
  is a path (hyprix hands its children a path). `connect` binds every
  global below at the lower of both sides' versions and round-trips until
  every screen has had its `done`.
* **Screens**: `wl_output` (geometry, mode, scale, `name` and `description`
  from version 4) and `zxdg_output_v1` (logical position and size, and its
  own name and description) into one `Output`, with `OutputAdded`,
  `OutputChanged` and `OutputRemoved` as they come and go.
  `Output::matches_hyprland` is hyprlock's `monitor =` rule (empty is every
  screen, `desc:` a description prefix, else a connector).
* **Surfaces**: `layer_surface(LayerOptions)` -- layer, anchor, size,
  exclusive zone, margin, keyboard interactivity, namespace, output --
  with `set_layer_options` for changes; `lock()` and `lock_surface(output)`
  for `ext-session-lock-v1`; `popup(parent, PopupOptions)` for an
  `xdg_popup` on a layer surface through `zwlr_layer_surface_v1.get_popup`
  (waybar's tooltips), with `reposition_popup`; `set_input_region` (an
  empty one lets clicks through); `request_frame` for frame callbacks.
* **Drawing**: `draw(surface, |pixmap| …)` hands the closure a
  `tiny_skia::PixmapMut` of the configured size times the scale, cleared,
  in tiny-skia's premultiplied RGBA; the runtime swaps it into `ARGB8888`
  in one of the surface's `wl_shm` buffers (two, a third while the
  compositor holds both), attaches, damages and commits. A program draws
  with tiny-skia directly -- there is no widget layer; waybar's box model
  is waybar's.
* **The seat**: the keyboard through `userland/compositor/xkb` (the keymap's groups
  matched as `term` does, keysym names, the text a key types, modifiers,
  and repeat from `repeat_info`, done by the runtime and marked
  `repeat: true`); the pointer's enter, leave, motion, button (with its
  serial) and axis (with `value120` or discrete clicks), in surface
  coordinates; `set_cursor(CursorShape)` through `wp_cursor_shape_v1`, and
  `hide_cursor`.
* **Idle**: `idle_notification(timeout, respect_inhibitors)` over
  `ext_idle_notifier_v1`, `Idled`/`Resumed`.
* **The loop's other sources**: timers (`add_timer(after, every)`),
  children (`run(Command)`: `/bin/sh -c` in its own process group with
  `PR_SET_PDEATHSIG`, extra environment, output a line at a time or whole
  at exit, reaped, `kill` signals the group), signals (`watch_signals`,
  through `signalfd4`, `SIGRTMIN+N` included), descriptors (`watch_fd`),
  and a `Waker` another thread can wake the loop with.
* **Detached commands**: `toolkit::spawn(line)` is upstream's `exec`:
  `/bin/sh -c`, double-forked into a session of its own, so `pidof
  hyprlock || hyprlock` means what it means on Hyprland.
* **Anything else**: `bind`, `new_object`, `request` and `adopt` let a
  program speak a protocol the runtime does not (hyprlock's
  `zwlr_screencopy_v1`), its events arriving as `Event::Object` read with
  the interface's own generated table; `new_buffer` gives it a `wl_shm`
  buffer to hand to one.

### 2.2 `userland/compositor/text` -- fonts, shaping, glyphs, layout, markup

* **Finding a face**: `Fonts::system()` scans `$FERRIX_FONT_DIRS`,
  `~/.local/share/fonts`, `~/.fonts`, `/usr/share/fonts`,
  `/usr/local/share/fonts` and `/usr/share/ferrix/fonts`, reading each
  face's name and OS/2 tables. `Fonts::find(family, weight, style)` is a
  small fontconfig: the family (any of the face's typographic or legacy
  names, case and spaces ignored), then CSS's nearest-weight rule, then the
  style; `sans-serif`, `serif`, `monospace` and `system-ui` map onto what
  is installed. `FontDescription::pango("Ubuntu Light 11")` and
  `FontDescription::fontconfig("GFS Didot:size=16")` read the two spellings
  the user's files use. `Fonts::resolve` makes a `Font`: the families in
  order, then the generic sans-serif, then every face, so a character any
  face has is drawn (the per-glyph fallback that gives fuzzel its `…`).
* **Shaping**: rustybuzz (HarfBuzz's algorithm in Rust) over ttf-parser,
  so GPOS kerning and ligatures match Pango's, and a line is as wide here
  as there -- which decides where a bar's modules sit.
* **Glyphs**: each outline filled with antialiasing by tiny-skia into an
  alpha `Mask`, cached by face, glyph, size and quarter-pixel position.
  `draw_run` blends one in a colour into a premultiplied pixmap.
* **Layout**: `Fonts::layout(spans, LayoutOptions)` breaks at `\n` (and at
  spaces when wrapping), aligns, ellipsizes (start, middle, end), applies
  Pango 1.50's `line_height` and fuzzel's fixed `line-height`, and measures
  by Pango's *logical* rectangle -- what GTK sizes a label by and hyprlock
  sizes its label texture by. `Metrics::approximate_char_width` is what
  GTK's `max-width-chars` counts in.
* **Markup**: `markup::parse` is `pango_parse_markup`'s subset: `<span>`
  with `foreground`, `background`, `font_weight`, `size`, `line_height`,
  `font_family`, `style`, `underline`, `strikethrough`, `rise`,
  `letter_spacing` and their synonyms; `<b> <i> <u> <s> <tt> <small> <big>
  <sub> <sup>`; the entities. It refuses what Pango refuses, in Pango's
  words, so a program falls back to plain text as upstream does.

Points are pixels × 72 / 96, as hyprgraphics and fcft both have it.

### 2.3 `userland/compositor/hyprlang` -- the configuration language

hyprlock.conf and hypridle.conf are hyprlang, the language of
hyprland.conf without Hyprland's options. The crate is hyprlang 0.6's
`CConfig::parseLine` (`/var/cache/hyprland-build/src/hyprlang/src/
config.cpp`): comments (a line whose first character is `#` is all
comment; elsewhere `##` is a literal `#`), backslash continuation,
categories that nest (`auth { pam { enabled } }` is `auth:pam:enabled`, and
so is that one line), `$variables` expanded longest first (`$font Light` is
`Ubuntu Light`), `{{a + b}}`, keywords inside categories (`bezier` inside
`animations { }` is the keyword), special categories -- anonymous ones
make an instance per block, keyed ones per key -- and `source =`.

A program hands `parse` a `Schema` (option names, special categories and
their options, keywords) as upstream's `ConfigManager.cpp` registers them,
and gets a `Document`: the options set, the instances in file order, the
keyword lines in order, and a `Diagnostic` per line hyprlang would have
refused, in its words (`Config error in file … at line 12: config option
<general:foo> does not exist.`). Values stay text; `hyprlang::value` reads
them as hyprlang's `INT` (colours `rgb()`/`rgba()`/`0xAARRGGBB`, booleans),
`FLOAT` and `VEC2` do.

`userland/compositor/config` keeps its own grammar: it is Hyprland's, with
Hyprland's options wired to the compositor, and it was just fixed to match
hyprlang on the two points above (`cc2eb016`). Moving it onto this crate
would be a change to the compositor's startup path for no behaviour; it can
be done later, gated by `userland/compositor/config`'s own tests.

### 2.4 `userland/compositor/image` -- PNG, JPEG, SVG

`compositor_image::load(path, Fit)` sniffs the bytes and returns a
`tiny_skia::Pixmap`: PNG through tiny-skia's own decoder, JPEG through
`zune-jpeg`, SVG through `resvg`/`usvg` 0.48, which render with the same
tiny-skia 0.12 the compositor pins. resvg rather than a subset rasteriser
because fuzzel draws icon themes, which are whatever Inkscape writes; the
user's own waybar icons (paths with arcs, a circle, linear gradients in
both unit systems, round caps and joins, `preserveAspectRatio="none"`) are
a small part of what it does. `Fit::Exactly(w, h)` is how waybar stretches
its 12-pixel caps to the bar's height.

### 2.5 The image: dotfiles, fonts and `/bin/<name>`

`cargo xtask run-compositor --config ~/.config/hypr/hyprland.conf` carries
the configuration directory's siblings -- `~/.config/{hypr,waybar,fuzzel}`,
found as the directories beside the one `--config` is in -- into the
image's `$HOME/.config`, and the fonts those files name, resolved on the
host with `fc-match` and copied into `/usr/share/fonts/host/` at
image-build time (host fonts are never committed). `HOME` on the desktop
is `/`: the compositor is init, and init's environment is `HOME=/`
(`kernel/src/init.rs`), which every `exec-once` inherits. The four
programs are carried as `/bin/waybar`, `/bin/fuzzel`, `/bin/hyprlock`,
`/bin/hypridle` by one line each in `xtask/src/compositor.rs`'s
`DESKTOP_CLIENTS`.

## 3. waybar

### Where it stands

(The waybar stream's to fill.)

## 4. fuzzel

`userland/compositor/fuzzel`, `/bin/fuzzel`: a port of fuzzel 1.12
(codeberg.org/dnkl/fuzzel, read from a shallow clone in
`~/.local/share/ferrix/clients-ref/fuzzel`). Each module names the part of
fuzzel's source it follows: `config` is `config.c`, with every option and its
default, the file search, line splitting, unquoting, `include`, `--override`,
the key-binding table with its collision check, and fuzzel's own diagnostic
for each refused line (the unknown-option line is byte for byte what the
host's `fuzzel --check-config` prints). `desktop` is `xdg.c` and `path.c`.
`matching` is `match.c`: `fzf`, `exact` and `fuzzy`, with the ranking.
`icon` is `icon.c`. `cli` is the whole `getopt_long` table. `keys` is
`keyboard_key`'s two lookups, untranslated then translated. `exec` is
`application_execute`. `geometry` and `paint` are `render.c`, drawn with
tiny-skia and `compositor/text` in place of pixman and fcft. `window` is
`main.c` and `wayland.c` on `compositor/toolkit`.

The surface is a `zwlr_layer_surface_v1` on `layer=` (`overlay`) under
fuzzel's namespace, `launcher`. That is what the user's `layerrule = blur
true, match:namespace launcher` matches. The comment in their `fuzzel.ini`
says `fuzzel`, which matches nothing, and their `hyprland.conf` says so.
hyprix gives an interactive layer surface above the windows the keyboard
while it is mapped (`hyprix/src/deliver.rs`, `Focus::interactive_layer`).
Binds still fire first.

The image lists `.desktop` entries of the tree's own, for what it carries:
`data/applications` (the terminal running zinc, the test pattern, busybox's
`top`), plus Chrome under `--chrome`, with icons in a `hicolor` of their own
(`xtask/src/fuzzel.rs`).

### What each of the user's lines does on Ferrix

Every line of their `fuzzel.ini` is read and carried out: the font
(GFS Didot, which clients-base's `run-compositor --config` carries from the host), `layer`,
`anchor`, `width`, `lines`, the paddings, `line-height` (26 points, so
35-pixel rows at 96 DPI), `letter-spacing`, `icons-enabled`,
`image-size-ratio` (the selected entry's SVG is drawn large when there is
room under the list), the quoted two-space `prompt`, the `placeholder`,
`filter-desktop` (hyprix sets `XDG_CURRENT_DESKTOP=Hyprland`), every colour,
the border and `[dmenu]`. What differs:

* `terminal=foot`: foot is not in the desktop image, so a `Terminal=true`
  entry (`top`) fails as fuzzel says a missing program fails:
  `foot top: failed to execute: No such file or directory (2)`, exit 1.
* The user opens fuzzel through `/home/sebastian/.local/bin/hypr-launcher`
  (SUPER+R, and a bare SUPER tap). That script and its GTK scrim are not
  in the image, so on Ferrix their binds start nothing. `/bin/fuzzel` itself
  works; the wrapper's `--keyboard-focus=on-demand
  --no-exit-on-keyboard-focus-loss` is taken.
* The single-instance lock: hyprix hands its children `WAYLAND_DISPLAY` as a
  path, so fuzzel's `$XDG_RUNTIME_DIR/fuzzel-$WAYLAND_DISPLAY.lock` cannot be
  made. It warns and runs, as upstream does.
* Not done, and said by name when asked for: `gamma-correct-blending` (hyprix
  has no color-management protocol), a `scaling-filter` other than `box`
  (PNG icons are scaled bilinearly), `message-mode=expand`, and the
  clipboard pastes (`clipboard-paste`, `primary-paste`). Not done silently,
  because they change nothing visible: `render-workers`, `match-workers`
  and `delayed-filter-*` (matching is synchronous) and xdg-activation
  tokens.
* Drawn differently at the pixel level: glyphs are rustybuzz and tiny-skia,
  not HarfBuzz and FreeType, and the corners are a tiny-skia path. The input
  line scrolls by whole characters. `qsort`'s order for equal matches is
  made stable.

### Where it stands

Landed: the pure core, `a29407d6` and `02afa6c8` (2026-09-26): config,
entries, matching, prompt, exec, keys, icons, cache, dmenu, command line,
geometry and the launcher state, with 66 host tests, and `examples/probe`,
which reads the real `~/.config/fuzzel/fuzzel.ini` and `.desktop` files on
the host. Against nazuna's files it finds 144 entries, 67 shown, and ranks
Terminal first for `term`.

On branch `fuzzel-window`: the window, the hyprix focus change, the image's
entries and icons, and `test-compositor --boot fuzzel`. The boot starts
fuzzel with `data/boot/fuzzel.ini` (the user's settings in Liberation Serif,
`dpi-aware=no`, `terminal=/bin/term`). It has `/bin/vkbd` type `pat` and
then Return, and requires three screendumps pixel for pixel: the list with
its icons and the large icon, the test pattern ranked first and selected,
and the pattern window fuzzel started, alone. The first two are blessed by
`tests/boot_frames.rs`. It passed first time on x86_64 (2026-09-26).

Run once against the user's real `fuzzel.ini` and fonts (a local boot, not
committed): the window came up 530x525 at the centre in GFS Didot, with the
placeholder, three entries with icons, Terminal selected and its icon large
underneath. It is the boot's first picture in the other face, 3607 pixels
apart. Its screendump is kept in `~/.local/share/ferrix/logs/fuzzel/`. It
took about 30 seconds from start to window with 20 host font files. The
boot's tree font took about 4. `--print-timing-info` is in the boot now to
say which stage it is. Suspect the font scan under TCG.

Left: the clipboard pastes (need `wl_data_device` in the toolkit); taking
the keyboard back from an `on-demand` fuzzel when another window is clicked
(hyprix); the startup time above; aarch64 boot.

## 5. hyprlock

### Where it stands

(The hyprlock stream's to fill.)

## 6. hypridle

### Where it stands

(The hypridle stream's to fill.)

## 7. What cannot work on Ferrix, and what each such line does instead

Ferrix has no D-Bus, no systemd or logind, no PipeWire (its audio server
speaks the PulseAudio protocol, `docs/AUDIO.md`), no StatusNotifierItem
tray and no PAM. Each program's section says what its lines that need one
of those do on Ferrix; the foundation's part is only that a command line
naming a program that is not there (`loginctl`, `wpctl`) fails the way a
shell says it does, and the program reports it.

## 8. The foundation's own state

### Where it stands

* **`userland/compositor/toolkit` is on `main`** (2026-09-26). Its tests run hyprix
  in the test process, headless, and judge the frame it composed: a bar
  anchored across the top is drawn there, a lock surface covers the screen
  and comes off at unlock, and timers, children, signals and the waker come
  back as events. Two things are not tried end to end: the keyboard, since
  a headless seat has no keyboard (the key translation is tested as a
  function against `userland/compositor/xkb`'s `us` and `de` tables), and popups,
  since hyprix refuses an `xdg_popup` whose parent comes from
  `zwlr_layer_surface_v1.get_popup` -- a gap in the compositor, which the
  waybar stream is fixing; the test for it is `#[ignore]`d with that reason.
* **`userland/compositor/hyprlang` and `userland/compositor/image` are on
  `main`** (2026-09-26). hyprlang's tests pin each place config.cpp behaves
  unexpectedly (a top-level name no keyword takes is accepted silently; a
  scoped keyword written out in full keeps its full name; a shorthand line
  and a following block are one instance). image renders all fifteen of the
  user's waybar icons on nazuna (an ignored probe test); JPEG decodes with
  zune-jpeg's SIMD off, so it forbids unsafe and is slower than it could be.
* **`userland/compositor/text` is on `main`** (2026-09-26). 29 host tests on
  the tree's own Liberation and Inter; an ignored probe of the host's fonts
  (`cargo test -p compositor-text -- --ignored`) found 419 faces in about
  200 ms, instanced the variable Ubuntu file at wght 300 for "Ubuntu Light",
  and measured `line_height='2.0'` on Ubuntu at 15pt as 11.2 pixels above and
  below, which is the user's own measurement of their waybar tooltips. Not
  done: colour glyphs (Noto Color Emoji draws nothing), bidi reordering,
  instancing on axes other than `wght`, and a font cache on disk
  (`Fonts::system()` scans every time).
* Next: the xtask slice -- `run-compositor --config` carrying the user's
  dotfiles and fonts, `DESKTOP_CLIENTS`, and the `caption` boot.
* Not yet: the xtask flag that carries the user's dotfiles and fonts, and the
  boot check of a toolkit client drawing text in the user's font.
