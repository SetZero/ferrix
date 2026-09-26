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

`userland/compositor/waybar` is `/bin/waybar`: it reads `config.jsonc` and
`style.css` the way waybar 0.15 (Alexays/Waybar at 1684389) and GTK 3.24
read them, and draws the bar GTK would draw from them. Upstream's source is
the reference, read file by file; `waybar-probe` is the host-side check
that lists what of the user's real files is not carried out.

* **The config** (`json.rs`, `config.rs`): jsoncpp's defaults (comments,
  trailing commas, the last of a repeated key, waybar's `\x` rewrite, and
  `isUInt`/`isInt`/`asString` as the options read them); the search path
  (`$WAYBAR_CONFIG_DIR`, `$XDG_CONFIG_HOME/waybar`, `~/.config/waybar`, …,
  `config` before `config.jsonc` in each); `include` merging, which fills in
  and never overrides; one bar or an array; `output` by equality against the
  name or the description less its ` (NAME)`, with `!`, `*`, `$VAR` and
  `output-dimensions`.
* **Formats** (`fmt.rs`): libfmt as waybar calls it -- `{}` counts named
  arguments, so a custom module's `{}` is `{text}` and memory's is the
  percentage; `{used:0.1f}`; waybar's own `pow_format` for bandwidths; and
  `strftime` for the clock's `{:%H:%M}`.
* **The style** (`css/`): a GTK3 stylesheet -- `@define-color`, `alpha()`,
  `shade()` (GTK's HLS rule), `mix()`, `lighter()`, `darker()`, `calc()`,
  `url()` and linear-gradient layers with their size, position, repeat,
  clip and origin lists, the shorthands resetting what they do not set,
  GTK's parse errors in its words -- matched against the node tree waybar's
  widgets make (`window#waybar > box > box.modules-* > widget >
  label#cpu.module` or `box#custom-x.module > image, label.flat.text-button`;
  a tooltip is `tooltip.background > box > label`), by specificity then
  order, with GTK3's initial values and inheritance.
* **Layout and drawing** (`layout.rs`, `paint.rs`): GTK3's gadget box model
  (content at least `min-width`, then padding, border and margin, which may
  be negative), box packing with `spacing` and a centre widget, a label's
  natural, ellipsized (`max-length`) and wrapped (a tooltip's 70 characters)
  widths, and each box drawn in GTK's order: outset shadows, colour clipped
  to the last layer's box, layers last listed first, inset shadows, border.
  An `url()` image is rasterised at its own size and scaled, as GTK3's
  pixbuf loader does. Text and images are behind traits, for
  `userland/compositor/text` and `userland/compositor/image`.
* **Modules** (`modules/`): each a state machine over a `Host` (children,
  timers, files, Hyprland requests, interface ioctls), tested with a fake
  one against upstream's rules: `custom/*` (its three workers, `exec-if`,
  `return-type: json`, `signal` from the C library's `SIGRTMIN`, clicks,
  scrolls), `hyprland/window` (`j/monitors`, `j/workspaces`, `j/clients`,
  the bar's `empty`/`solo` classes), `cpu`, `memory`, `network`, `clock`,
  `pulseaudio`, `tray`.

What the user's file does on Ferrix, line by line where it differs:

* `"output": "Lenovo Group Limited R27qe Gen2 UTP03KBB"` matches no QEMU
  screen, which has no EDID; upstream would draw no bar, and so does this.
  The user chose (2026-09-26) to give QEMU's screen that monitor's own EDID
  the way Linux overrides one (`drm.edid_firmware=`), in its own landing
  (branch `edid-override`); with it the line matches as on nazuna.
* The ten `custom/ws-N`, `custom/clock` and `custom/logout`'s click run
  `/home/sebastian/.local/bin/hypr-workspaces`, `ba-calendar` and
  `hypr-logout`, Python scripts that are not on Ferrix. `sh` answers 127,
  and waybar hides a module whose script fails or prints nothing, so the
  desktop chips and the clock are hidden -- and `ba-calendar daemon` logs
  waybar's own `clock stopped unexpectedly, is it endless?`. The launcher
  and logout chips have no `exec` and show. What would fill the gap is a
  Rust equivalent of each script; that is the user's call.
* `pulseaudio`: waybar connects with `PA_CONTEXT_NOFAIL` and shows its
  starting values until a server answers. Ferrix has no PulseAudio-protocol
  server yet (`docs/AUDIO.md` §5, U2), so the chip reads `vol 0%`, and says
  once why. `on-click`'s `wpctl` and `on-click-right`'s `pavucontrol` are not
  on Ferrix either.
* `cpu`'s `{load}`: no `/proc/loadavg` on Ferrix and `sysinfo` loads of 0,
  so `0`, said once.
* `network`: `ethernet` over the default route, the address from the
  `ifreq` ioctls; no nl80211, so never `wifi`, said once.
* `tray`: StatusNotifierItem needs D-Bus; an empty tray is hidden, as
  upstream hides it, and `#tray menu` in the style matches nothing.

Approximations, each said by the probe: GTK's theme (Yaru) is not applied
under the user's rules, so what only the theme sets takes CSS initial
values -- the user's file sets or zeroes everything its bar shows; a border
style other than `solid` is drawn solid; `transition` draws the new state
at once; a blurred shadow is a triple box blur. `hyprland/window`'s
`rewrite` is not carried out (regex replacement; the tree's regex crate only
matches).

### Where it stands

2026-09-26: the part that needs no screen is on branch `waybar`, landing as
its first slice: config, formats, style, layout, painter, modules, the
probe and upstream's command line (the binary checks both files and says it
does not draw yet). The user's `style.css` parses with no error.

Next, in order: the drawing on `userland/compositor/toolkit` (on main) with
`userland/compositor/text` and `userland/compositor/image` as they land -- the bar's layer
surface per matching output, the pointer (hover, `:hover` restyle, clicks,
scrolls, the hand cursor), tooltips as `xdg_popup`s through
`zwlr_layer_surface_v1.get_popup`; hyprix's server refused a parentless
popup, and the fix is on branch `waybar-popup` (server + hyprix, with
clients-base's `a_tooltip_hangs_under_the_bar_it_belongs_to` passing against
it once its client stays connected to the end); branch `waybar-ipc` gives
`j/workspaces` its `lastwindow`/`lastwindowtitle`, which the title needs.
Then `xtask` installs `/bin/waybar`, and a `test-compositor` boot runs it
against a test config whose `output` names QEMU's screen, compared as
`test-compositor` compares its boots. Then a PulseAudio-protocol client.

A text measurement to settle when `userland/compositor/text` lands: the user's
comment measures `line_height='2.0'` as 10.5 px over and 11.5 under at
their `font-size: 15px`; clients-base measured 8.41 each way at 15 px and
11.21 at 15 pt. GTK3 gives Pango a CSS `px` size as `px × PANGO_SCALE × 72
/ 96` points with the screen at 96 dpi, which is 15 px; so either the
comment was measured at another size or the rounding differs. Measure it
on the host with GTK before blessing a golden image.

## 4. fuzzel

### Where it stands

(The fuzzel stream's to fill.)

## 5. hyprlock

`userland/compositor/hyprlock` is hyprlock 0.9.6 (`/var/cache/hyprland-build/src/
hyprlock`) for Ferrix, installed as `/bin/hyprlock`. It reads
`~/.config/hypr/hyprlock.conf` unchanged, or the file `-c` names, found
as upstream's `findConfig` finds it (`$XDG_CONFIG_HOME`, `$HOME/.config`,
`$XDG_CONFIG_DIRS`, `/etc/xdg`). The command line is upstream's: `-c`,
`-g`/`--grace`, `--immediate-render`, `--no-fade-in`, `--display`, `-v`,
`-q`, `-V`, `-h`, and the deprecated `--immediate`.

### 5.1 What of the file it carries out

* **Every option `ConfigManager.cpp` declares**, with upstream's default,
  typed as hyprlang types it (`config.rs`), and the five widget kinds as
  anonymous special categories, one per block. A line it cannot use is a
  diagnostic in hyprlang's words, printed under upstream's `Config has
  errors: … Proceeding ignoring faulty entries`, and the rest applies.
* **Which widgets a screen gets** is `getOrCreateWidgetsFor`'s rule: an
  empty `monitor` is every screen; otherwise the connector, or a prefix of
  the description with or without `desc:`. They are drawn in `zindex`
  order, the file's order kept among equals (`background` is -1).
* **Placement** is `posFromHVAlign` in upstream's bottom-up coordinates
  (`position = 0, 240` is 240 up from the middle), with its rounding rules
  (`roundingForBox`, `roundingForBorderBox`) and `%` positions and sizes.
* **Text**: `$TIME`, `$TIME12`, `$USER`, `$DESC`, `$ATTEMPTS[…]`,
  `$LAYOUT[…]`, `$FAIL`, `$PAMFAIL`, `$PAMPROMPT`, `<br/>`, and
  `cmd[update:N(:force)]`, which runs through `/bin/sh -c` and shows the
  output, trimmed as `general:text_trim` says. A text is Pango markup in a
  Pango font description at a point size (96 dpi), drawn by
  `userland/compositor/text` to its logical rectangle, which is what upstream's
  label texture is. The clock is UTC: Ferrix's image has no `TZ` and no
  `/etc/localtime`, which is when upstream falls back to UTC too.
* **The input field**: dots of `dots_size`, `dots_spacing`, `dots_center`,
  `dots_rounding`, or `dots_text_format`; the placeholder, check and fail
  texts at a quarter of the field's height; the width growing to fit the
  placeholder; `fade_on_empty` and `fade_timeout`; `outer_color`,
  `inner_color`, `font_color`, `check_color`, `fail_color`, the Caps Lock
  and Num Lock colours with their fallbacks; `swap_font_color`.
* **Keys** through `userland/compositor/xkb` as the compositor's keymap has them, so
  `input:kb_layout = de` with `nodeadkeys` types German: text is appended,
  `BackSpace`/`Delete` remove a character and are the only keys that
  repeat, `Escape`, `Ctrl+U`, `Ctrl+A` and `Ctrl+BackSpace` clear, `Return`
  or `KP_Enter` submit unless the field is empty and `ignore_empty_input`
  is set.
* **Animations**: `animations:enabled`, `bezier`, `animation` on
  hyprlock's own tree (`global`, `fade`, `fadeIn`, `fadeOut`, `inputField`,
  `inputFieldColors`, `inputFieldFade`, `inputFieldWidth`,
  `inputFieldDots`), a speed of N being N tenths of a second, over
  `userland/compositor/anim`'s curves.
* **Backgrounds**: `color`, `path` (PNG, JPEG, SVG through
  `userland/compositor/image`; upstream also reads WebP, JPEG XL and BMP),
  `path = screenshot` through `zwlr_screencopy_v1`, `blur_size`,
  `blur_passes`, `noise`, `contrast`, `brightness`, `vibrancy`,
  `vibrancy_darkness` (the dual-Kawase blur of `blurFB`, `blur.rs`), and
  `reload_cmd`/`reload_time`, the new picture crossfading in on `fadeIn`.
  A `reload_cmd` whose program is not there -- the customer's
  `booru-wallpaper`, a Python script -- prints nothing, and the `color`
  shows, as upstream shows it.
* **Shadows** (`shadow_passes`, `shadow_size`, `shadow_color`,
  `shadow_boost`): the widget blurred on its own and coloured behind it.
* **The fade**: upstream takes a screenshot of every screen before it locks
  when a fade is on, and fades from it; so does this.
* **`onclick`** on a label, shape or image runs detached through
  `/bin/sh -c`, as upstream's `spawnAsync`, when a button goes down over
  it.

### 5.2 What `auth { pam { enabled = true } }` means on Ferrix

Ferrix has no PAM. It authenticates through `authd` (`docs/AUTH.md`,
approved by the customer 2026-09-26, all eleven decisions as recommended),
a service owning every credential and answering a conversation on
`/run/ferrix/auth`. hyprlock knows authentication through one interface,
`auth::Backend`, the one `docs/AUTH.md` §4.4 settles on: `begin()` opens a
conversation and gives the first prompt, whose text is `$PAMPROMPT` before
anybody types; `respond(secret)` gives the next prompt or a verdict --
*accepted*, *failed* with its text (`$PAMFAIL`, `$FAIL`) and
`retry_after_ms` (the field shows `check_color` until then, in place of
upstream's PAM delay), or *unavailable* with its text. The widgets, the
field and the session see nothing else. The typed secret is a `Secret`,
zeroed when dropped.

* **`/bin/hyprlock`** has `Missing` until `authd`'s client lands
  (phase 1 slice P1.5, hyprlock's, once `libs/proto/auth-proto` is on
  main): the conversation cannot begin, and hyprlock **does not take the
  lock** and says `not locking: no authentication service is running`
  (§5.4, decision 4: a lock nothing can open would lock the person out).
  So the customer's `SUPER+L` does nothing visible yet.
* **`hyprlock-gate`** (with the program, on branch `hyprlock`), a second binary of the crate (`src/bin/gate.rs`),
  has a test-only backend that accepts the one secret in
  `/etc/hyprlock/gate.secret`, refusing others after two seconds. Only the
  gate boot's image carries the binary and the file; when `authd` lands the
  gate seeds a real store entry and the binary goes (§4.4).
* hyprlock never reads a credential. `$USER` and `$DESC` are the uid's
  `/etc/passwd` line (upstream's `getpwuid(getuid())`); hyprix is init, so
  on a phase-1 desktop that is root, and the lock asks for root's password
  (decision 3).
* **`SIGUSR1`** does not unlock: it becomes root's audited
  `authctl unlock-seat` (decision 11). **`--grace`** is said and not
  honoured: the grace is hyprix's `misc:lock_grace`, default 0.
* `auth:pam:enabled = false` with nothing else to unlock: not locking, said.
  `auth:pam:module` becomes the `authd` service name with `Service`.
  `auth:fingerprint:enabled` needs a reader `authd` does not have yet:
  said, and off.

### 5.3 What cannot work, and what it does instead

| line | on Ferrix |
|---|---|
| `auth:pam:*` | the `Backend` interface above; `authd` behind it from phase 1 |
| `SIGUSR1`, `--grace` | not honoured: `authctl unlock-seat` and `misc:lock_grace` (decision 11) |
| `auth:fingerprint:*` | said, and off: no fprintd, no D-Bus |
| `hide_input` | said: drawn as plain dots for now |
| `reload_cmd` naming a program that is not there | runs, fails as the shell says, and the colour shows |

### Where it stands

2026-09-26. **On main:** `userland/compositor/hyprlock`'s library -- the
configuration model on `compositor/hyprlang` (every option and default of
`ConfigManager.cpp`), layout, formatting, the field's session, the
authentication interface with `Missing`, the blur, and every widget drawn
by `scene::Scene` -- with 34 host tests. The customer's real file reads with
no diagnostic and no unsupported line
(`cargo run -p compositor-hyprlock --example probe`); on a screen that is
not one of their three `desc:` monitors, upstream's rule gives only the
clock panel, `$TIME` and the date, and their Lenovo gets all 8 widgets.

**On branch `hyprlock`, next:** the program (`/bin/hyprlock` on
`compositor/toolkit`, text through `compositor/text`), `hyprlock-gate`,
and the `hyprlock` boot of `test-compositor` (lock, five dots, a wrong
password in `fail_color` with its text, the right one typed on a German
keyboard, unlock), which passed pixel for pixel on x86_64 before the
relayout. It lands when `compositor/text` is on main. After that: the
`Service` backend over `authd` (`docs/AUTH.md` P1.5), once
`libs/proto/auth-proto` lands.

## 6. hypridle

`userland/compositor/hypridle` is upstream hypridle 0.1.8
(`/var/cache/hyprland-build/src/hypridle`) in Rust: `/bin/hypridle`, with
`-c`/`--config`, `-q`, `-v`, `-V` and `-h` as upstream's `main.cpp` has
them, and the file looked for where `Hyprutils::Path::findConfig` looks
(`$XDG_CONFIG_HOME/hypr/`, `~/.config/hypr/`, `$XDG_CONFIG_DIRS`,
`/etc/xdg/hypr/`). Its log lines are upstream's, `[LOG]`/`[WARN]`/`[ERR]`
on the standard output, which is how the boots below read what it did.

**The options.** Every one `ConfigManager.cpp` declares, with its default:
`general { lock_cmd unlock_cmd on_lock_cmd on_unlock_cmd before_sleep_cmd
after_sleep_cmd ignore_dbus_inhibit ignore_systemd_inhibit
ignore_wayland_inhibit inhibit_sleep=2 }` and any number of `listener {
timeout on-timeout on-resume ignore_inhibit condition_cmd condition_retry
}`. A listener without `timeout` is left out ("Category has a missing
timeout setting"). No listener at all is "No rules configured", and the
program runs anyway, as upstream does. `condition_cmd` holds `on-timeout` off
while it exits nonzero and is asked again every `condition_retry` seconds.
Any input ends every listener's retry. An `on-resume` runs only after its
`on-timeout` ran. Commands go to `/bin/sh -c`, detached by
`toolkit::spawn` as upstream's `runAsync` leaves them. `condition_cmd` is
waited for, as `runSync` waits. A detached command's shell is then init's
to reap, and hyprix as pid 1 reaps no orphans (the BACKLOG row on it), so
each command a listener runs leaves one zombie until that row is done.

**Idle.** One `ext_idle_notification_v1` per listener, made with
`get_input_idle_notification` when `ignore_wayland_inhibit` or the
listener's `ignore_inhibit` is set and `get_idle_notification` otherwise,
as upstream's `run()` does. hyprix offers `ext_idle_notifier_v1` version 2
and answers both requests (`server/src/client/desktop.rs`); against a
version-1 notifier hypridle falls back to the inhibitable request and says
so. `hyprland_lock_notifier_v1` drives `on_lock_cmd` and `on_unlock_cmd`.
All of it goes through `userland/compositor/toolkit`: `idle_notification`, `bind`
and `request` for the lock notifier, and `watch_fd` for the `loginctl`
socket. hypridle has no surface and no wire code of its own.

**`loginctl lock-session` -- the decision.** Upstream runs `lock_cmd` when
logind emits `org.freedesktop.login1.Session.Lock`, which `loginctl
lock-session` causes. The user's file uses exactly that: the 10-minute
listener's `on-timeout` is `loginctl lock-session`, and the lock itself is
`lock_cmd`. Ferrix has no logind and no D-Bus. So the crate also builds a
`/bin/loginctl` that does logind's part of that one job: `lock-session`,
`unlock-session`, `lock-sessions` and `unlock-sessions` send one line to
`$XDG_RUNTIME_DIR/hypridle.sock` (the temporary directory when the variable
is unset, as hyprix and `hyprctl` fall back), hypridle runs `lock_cmd` or
`unlock_cmd` and answers `ok`. Why this and not the alternatives:
rewriting the command in the user's file breaks the fidelity rule.
Running `lock_cmd` straight from the listener would skip the `lock_cmd`
indirection the user wrote. A logind-and-D-Bus stand-in would be most of a
session manager for one signal. With nothing listening, `loginctl` says
"nothing is listening for lock-session at … is hypridle running?" and exits
1. systemd's `loginctl` succeeds at nothing there, but a lock that silently
did not happen is the one failure a screen locker must not have. Every
other verb is refused by name: "Ferrix has no logind". A second hypridle
finds the socket answered and runs without it ("Is hypridle already
running?").

**What cannot work, said once at start as a `[WARN]` naming the option.**
`before_sleep_cmd` and `after_sleep_cmd`: Ferrix has no suspend and no
logind to announce one (`PrepareForSleep`), so they never run.
`inhibit_sleep`: there is no sleep to delay. `ignore_dbus_inhibit` and
`ignore_systemd_inhibit`: no program can ask over
`org.freedesktop.ScreenSaver` or `systemd-inhibit` not to idle. Wayland's
`zwp_idle_inhibitor_v1` is still honoured, by the compositor, and upstream's
D-Bus inhibit counter (`onInhibit`) is left out because nothing could raise
it. Upstream exits when the system bus is missing. This one does not.

**The user's file on Ferrix** (`idle-user` boot, 2026-09-26, x86_64 and
aarch64: F
pressed for `hyprctl dispatch forceidle 901`, then N):

* The three `[WARN]`s: `before_sleep_cmd`, `after_sleep_cmd`,
  `ignore_dbus_inhibit = false`.
* 600 s listener: `loginctl lock-session` reached hypridle, which ran
  `lock_cmd = pidof hyprlock || hyprlock`. busybox's `pidof` found none
  (`/proc` is mounted), and then `/bin/sh: hyprlock: not found`: the
  hyprlock stream has not landed. Once `/bin/hyprlock` is in the image this
  line locks.
* 900 s listener: `hyprctl dispatch dpms off` turned the screen black
  (every pixel, by screendump), and the first key's `on-resume`, `hyprctl
  dispatch dpms on`, brought the tiled windows back pixel for pixel.

On the host, a headless hyprix with the same file does the same. hyprix
now tells idle notifications at once when `forceidle` changes. Before,
nothing woke the loop until the next input, so a `forceidle` fired
hypridle's listeners only at the next keypress.

**Tests.** 26 host tests (`hypridle/src/tests.rs`): the file's shape,
hyprlang's line rules and sentences, defaults, the listener, condition and
lock rules, the socket's lines, the log format. Two `test-compositor`
boots (`xtask/src/compositor/idle.rs`):

* `idle`: hypridle's own config, with 5 s and 20 s listeners. Waits for
  the natural 5 s `on-timeout` and for the 20 s `dpms off` (black screen),
  then presses L (`bind = , L, exec, /bin/loginctl lock-session`). It
  requires the resume lines, `dpms on`, `lock_cmd = pidof lock || lock 3`
  locking, `on_lock_cmd` and `on_unlock_cmd` from hyprland-lock-notify,
  and the tiled picture again.
* `idle-user`: the user's `~/.config/hypr/hypridle.conf`, read at build
  time and carried unchanged (never committed). It is skipped where the
  file is absent.

Both carry the gates' busybox for `/bin/sh` and `pidof`, and skip without
it.

### Where it stands

Built and passing: the host tests, `idle` and `idle-user` on x86_64 and
aarch64, and every other `test-compositor` boot on x86_64. The file is read
through `userland/compositor/hyprlang` (`hypridle/src/conf.rs` is the
schema and the adapter into the model), and Wayland is spoken through
`userland/compositor/toolkit`, so hypridle carries no copy of either. Left
for Ferrix to grow: a suspend (`before_sleep_cmd`, `after_sleep_cmd`,
`inhibit_sleep`) and a D-Bus `ScreenSaver.Inhibit`. The user's `lock_cmd`
locks as soon as `/bin/hyprlock` is in the image. Until then it says
`hyprlock: not found`.

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
