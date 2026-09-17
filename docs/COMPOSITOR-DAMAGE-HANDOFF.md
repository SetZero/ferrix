# Handoff: the compositor's frame time

Written 2026-09-17, at the end of the session that landed damage tracking,
and brought up to date the same night by the session that finished it. This
is what was done and what was measured, with the reasoning behind it, so
that whoever touches it next does not have to rediscover any of it.

The short version: **a frame that changes a little costs a little, whatever
is on the screen: 120 ms → 0.06 ms for a terminal rewriting a line,
translucent or not. What is left is a wallpaper that changes every frame,
which is a blur every frame (§2.4).**

---

## 1. Where the frame time went, measured

Release build, 1920x1080, software renderer, on the machine this was written
on. Pieces of one frame, timed directly:

| piece | cost |
|---|---|
| blur behind a full-screen window (`size 8, passes 3`) | 95 ms |
| blur behind a 1920x40 bar | 8.6 ms |
| drop shadow | 12 ms |
| composite one ARGB window, rounded | 6.4 ms |
| `fill_rounded`, `clear` | 0.07 ms each |
| an empty frame | 1.3 ms |

The blur is the frame. Everything else together is under 20 ms.

Three things have been done about it, in this order.

### 1.1 The blur honours the damage (commit `15526e06`)

`Canvas::blur` ran over the whole rectangle it was given, whatever the
damage said. Over a 120x40 strip it cost 92 ms; it now costs 1.8 ms and
writes byte-identical pixels.

The same commit fixed a latent bug: the region the kernel reads was half
what it needs. Each level of the dual-Kawase pyramid is half the one above,
so a tap at `size` on level *k* is `size * 2^k` source pixels; down sums to
`size * (2^passes - 1)` and up sums to the same, so the kernel reaches
`2 * size * (2^passes - 1)`. The code read `size * 2^passes`. The outermost
ring of every blurred window was a blur of the clamped edge rather than of
the frame. Twelve expected images moved, toward correct.

### 1.2 The kernel got faster (commit `d9c1294b`, from a subagent)

304 ms → 85 ms for a full-screen blur, with every expected image unchanged.
Three changes, none of which touched the algorithm: the bilinear sample's
rows and weights hoisted out of the pixel loop and shared between taps at
the same height; `f32::floor` replaced by a truncating cast plus a step back
(on baseline x86-64 `floor` is a libm call and was a fifth of the whole
blur); and `blurprepare`'s contrast curve folded into the byte→float read as
a 256-entry table.

It was **already** dual-Kawase, not a naive convolution — that was the first
diagnosis in this session and it was wrong. A running-sum box blur was considered and
rejected: it would be worth ~40–60 ms at best here (memory-bound over the
same 33 MB of f32 planes) while losing the exact Hyprland kernel and moving
every image.

### 1.3 The compositor tracks damage (commit `203f2fc2`, from a subagent)

Before this, `hyprix` handed `Damage::full` to the renderer every frame, so
1.1 bought nothing.

Two halves, in `compositor/hyprix/src/damage.rs`:

1. **Inside a surface.** A commit's `wl_surface.damage` / `damage_buffer`,
   read out of the server's `current` state at `Event::SurfaceCommitted`,
   converted to buffer pixels (the two lists differ by the surface scale)
   and mapped proportionally into whatever rectangle the frame draws that
   buffer into — a window, a bar, a menu, the lock surface, the drag icon,
   the cursor surface.
2. **Everything the compositor decides.** Rather than instrument the ~40
   `changed = true` sites, the whole frame description is kept per screen as
   a `Plan` — scaled layout, per-window rule styles, the layer/popup/lock
   list, cursor and drag-icon rectangles, gamma, dpms, lock, style, origin,
   scale, size — and compared with the last frame's.
   `compositor_render::damage_between` does the windows and the rest is
   field by field. That covers window appear/go/move/resize, focus,
   restacking, animations (the animated layout is what goes into the plan,
   so old ∪ new falls out every frame), pointer motion, layer changes, drag
   icon, `windowrule` restyling, config reload and monitor rearrangement.

Two bugs it had to fix on the way, both worth knowing:

- **Buffer age.** `Canvas::present` copies canvas→screen within the damage,
  but the DRM backend flips between two dumb buffers, so the one being drawn
  into holds the frame from *two* frames ago. `frame::Output` gained a
  `present: Damage` field — this frame's damage ∪ the last frame's — which
  is Hyprland's damage ring at age two. Without it the card path would
  flicker; headless would not have shown it.
- **Gamma applied twice.** `Gamma::apply` runs over the screen buffer after
  the copy, so with partial damage the undamaged pixels would be warmed
  again every frame. It now takes the stride and the presented region.

**Measured, release, same machine.** The run-wide "slowest frame" always
includes the first full frames, so the steady-state figure from the periodic
line is the one that means anything:

| configuration | before | after |
|---|---|---|
| one **opaque** terminal rewriting a line 5×/s | 42 ms steady | **0.24 ms** steady, smallest frame 6972 px of 2073600 |
| waybar + **translucent** foot rewriting a line | 154 ms steady | 150 ms steady |
| the user's own config, idle `foot` | 161 ms steady | 157 ms steady |

Row 3 is flat for a reason that must not be forgotten when re-measuring:
**that configuration's wallpaper is `mpvpaper` playing a video on a
full-screen layer surface, committing 1920x1080 of damage every frame.** In
that run the damage genuinely *is* the screen and no amount of tracking can
move the number. Measure with a static wallpaper or none.

Row 2 was the open problem, and §2 is what closed it.

---

## 2. What made a translucent window cost a full frame, and what fixed it

This was the open problem when the first half of this document was written.
It was worse than it looked: `foot` commits `ARGB8888` buffers whatever its
opacity, and the renderer's condition for blurring behind a window is a
format with alpha, so *every* terminal was a translucent window. And
`hyprix`'s loop reads its input once a frame, so a 120 ms frame is a pointer
that moves eight times a second and letters that arrive in bursts.

### 2.1 Why

`Canvas::blur` reads the canvas a kernel's reach outside what it writes.
Outside the damage the canvas still holds the **last** frame -- and the last
frame has the translucent surface drawn *over* the blur. So blurring a strip
is blurring the previous blur plus the surface: a smear that grows every
frame. `Plan::blurs_whole` in `compositor/hyprix/src/damage.rs` answered
that by growing the damage until every blurred surface it touched was
redrawn whole, plus a reach. Correct, and a near-fullscreen frame for every
pointer motion over a near-fullscreen window.

It was found the honest way -- it cost a step or two of a channel over 8604
pixels of `dwindle-two-clients`, which is the golden-image suite catching
it.

### 2.2 The backdrop (`compositor/render/src/backdrop.rs`)

Hyprland's answer is `decoration:blur:new_optimizations`, **on by default**:
the monitor keeps `m_blurFB`, the background and the layer surfaces under
the windows *already blurred*, blurs it again only when one of those changes
(`CHyprOpenGLImpl::preRender`), and a tiled window samples it
(`IHyprRenderer::shouldUseNewBlurOptimizations`).

`compositor_render::Backdrop` is that. Two canvases a screen: `sharp`, what
is behind the windows, brought up to date within each frame's damage; and
`blurred`, the blur of it in 64-pixel tiles, each blurred the first time a
window needs it and again only after the pixels it was blurred from have
*changed* -- which `Backdrop::take` finds by comparing, because a frame's
damage says what was drawn again and not what came out different. A pointer
crossing a window damages the wallpaper under it and changes none of it.

* `render_onto(canvas, Some(backdrop), …)` is what `hyprix` calls; a
  `Screen` owns the backdrop beside its canvas.
* `reads_backdrop(windows, at, styles)` is the rule for which windows take
  it: tiled, nothing drawn under it, no `dim_around` fill behind it. That is
  Hyprland's "not floating and not on a special workspace" asked of a layout
  that does not say which workspace a window came from.
* Everything else -- a floating window, a scratchpad's over the workspace
  under it, a blurred layer surface -- still blurs the frame as it stands
  and is still redrawn whole: `Plan::blurs_whole`, now only for those.
* The damage owes a backdrop reader one thing: where what is *behind* the
  windows changed (a commit on a layer surface under them, or one of those
  moving), the damage inside the window grows by `Blur::reach`.
  `Plan::behind_reaches`.

`render` and `render_with_layers` make a backdrop for the one frame they
draw, shown the whole of what is behind the windows, so that a frame drawn
from nothing and the frame a compositor kept up to date are one picture.
`hyprix`'s integration tests hold that: many partial frames, compared byte
for byte with the renderer's one.

**Twelve expected images moved**, every difference inside a translucent
tiled window, every one darker. A blur of the frame as it stands had the
window's *own* decorations in it: its shadow, drawn whole under it (lighter
than the default `0x111111` background, hence darker without), and under a
rounded window the border's fill, which the rounded path lays over the whole
box. `m_blurFB` has neither, and Hyprland draws it over both. Toward
Hyprland's default, not away from it. What is given up is what Hyprland's
optimisation gives up: nothing, for a tiled window, since nothing but the
desktop is behind one.

### 2.3 The copy nobody had timed (`Canvas::blend`)

With the blur gone the same frame still cost 5 ms, all of it before a pixel
was drawn: a client's padded buffer was gathered into tight rows **whole**
for every blend. `foot` hands over a 1878-pixel row in a stride of 7680
bytes, so that is every `foot`. `blend` gathers the part the clips cover
now, cut on whole pixels with the pattern moved by the same, so each canvas
pixel takes the surface pixel it took.

**Measured, release, 1920x1080, `size 8, passes 3`, steady state, same
machine.** One `foot` rewriting a line five times a second:

| configuration | before | backdrop | and the gather |
|---|---|---|---|
| `foot` as it comes (opaque, `ARGB8888`) | 120 ms | 5.3 ms | **0.06 ms** |
| `foot -o colors.alpha=0.8` | 120 ms | 5.2 ms | **0.06 ms** |
| two of those | 131 ms | 4.6 ms | **0.09 ms** |
| waybar and one | 119 ms | 5.2 ms | **0.06 ms** |

The compositor's share of a core over ten seconds of the last row: 62% →
6%, most of what is left being a loop that wakes every 2 ms.

### 2.4 What is left

* **A wallpaper that changes every frame** (`mpvpaper`) changes what is
  behind every window every frame, and the blur of it is owed every frame:
  95 ms behind a full-screen window. No tracking can move that. What could:
  a cheaper kernel, or blurring on another thread -- the second only where
  there is a second core, which under `whpx` there is not.
* **A floating translucent window, and a blurred bar,** are still redrawn
  whole when touched. A bar is 8.6 ms. A large floating terminal is the case
  that would be felt.
* **The loop polls**, 2 ms at a time, and builds a description of every
  window each pass while a bar is watching. Not the frame time, but it is a
  core that is never idle on a machine with one.
* **`composite_scaled` still gathers a whole surface**, for a window
  part-way through an animation. The whole window is damaged then, so it is
  proportionate; it is also 3 ms a window a frame.

---

## 3. How to measure, exactly

```sh
cd <worktree>/compositor
export CARGO_TARGET_DIR=<worktree>/target
cargo build --release -p hyprix

# A config with no animated wallpaper. One translucent terminal.
mkdir -p /tmp/blur && cat > /tmp/blur/hyprland.conf <<'CONF'
monitor = , preferred, auto, 1
decoration {
    rounding = 8
    blur { enabled = true
           size = 8
           passes = 3 }
}
exec-once = foot
CONF

../target/release/hyprix --headless 1920x1080 \
    --display /tmp/blur/wayland --instance blur \
    --config /tmp/blur/hyprland.conf --deadline 30000 2>&1 | grep frames
```

Read the **periodic** `hyprix: frames N slowest of the last M …` lines, not
the final summary: the summary's "slowest frame" includes the first full
frames of the run and will not move however good the tracking gets. The
final line also ends `smallest frame N of M pixels`, which is the number the
damage test reads.

Three traps, each of which cost time:

- An `exec-once = foot sh -c '…'` line with quotes inside the quotes opened a
  window and closed it again. Put the loop in a script and `exec-once = foot
  /path/to/it`.

- The user's real configuration runs `mpvpaper`, which damages the whole
  screen every frame. Use the config above instead.
- A debug build is 3–4× slower than release and will mislead you about which
  piece dominates.

---

## 4. The tests that hold all of this

- `a_blur_reads_what_is_damaged_and_writes_the_same_pixels`
  (`compositor/render/src/tests.rs`) — a blur over a damaged strip writes
  byte-identical pixels to a blur over the whole window, *and* costs under
  25 ms rather than the window's 95. Both halves matter: the first is the
  licence to take the shortcut and the second is that the shortcut was
  taken.
- `a_tiled_window_keeps_the_blur_of_what_is_behind_it` (same file) -- the
  backdrop. A strip of a translucent window redrawn alone is the whole
  frame's pixels *and ran no blur* (`Backdrop::blurs` is the count); a
  square repainted behind the window, redrawn a reach around, is the whole
  frame's pixels and one blur; and the square alone is **not**, which is the
  control that says the reach is owed.
- `only_a_window_over_the_desktop_alone_reads_the_backdrop` -- the rule.
- `a_padded_surface_draws_as_a_tight_one` -- now also two cells of a padded
  buffer, away from its corner, at two opacities.
- `a_full_screen_blur_is_inside_the_stated_bound` (same file, release only)
  — the 220 ms ceiling, deliberately more than twice the 85 ms measurement
  so that a slower machine passes and a change that made the blur several
  times more expensive does not.
- `a_small_commit_redraws_a_small_part_of_the_screen`
  (`compositor/hyprix/tests/two_clients.rs`) — the damage test. It needed a
  client of its own, because `compositor/pattern` damages its whole buffer:
  `patching` fills its window one colour and then repaints a 24x24 square
  twelve times, damaging only that square. It asserts the picture (the
  square is in the last frame, exactly 576 pixels, with the window's colour
  around it) *and* the cost (the smallest frame of the run redrew under a
  sixteenth of the screen). Forcing the region back to `Damage::full` fails
  it on exactly the second assertion — `redrew 786432 pixels of 786432` —
  while the picture assertions still pass, which is what says the two are
  independent.
- The whole golden-image suite: `cargo test -p compositor-render -p hyprix
  -p compositor-term`. **If any blessed image moves, that is a bug until
  proven otherwise.** Both real bugs in this area — the kernel's reach and
  the blur reading its own output — were caught by an image moving.

---

## 5. Everything else that is still open, for context

Not part of this handoff, but the next person will ask.

- **XWayland.** No X11 window can be shown. `xwayland_shell_v1` is not
  offered and there is no `Xwayland` binary on Ferrix. The largest single
  gap left.
- **The GPU path.** Everything is CPU. With it would come
  `wp_linux_drm_syncobj_manager_v1`, `wl_drm`, `wp_color_manager_v1`, and
  the five `windowrule` effects that only mean something with a GPU
  (`immediate`, `no_vrr`, `no_auto_hdr`, `tonemap`, `force_rgbx`).
- **The pointer-driven options.** `general:resize_on_border`,
  `general:snap:*`, `general:extend_border_grab_area`, and the dwindle
  options that need a cursor (`smart_split`, `smart_resizing`,
  `use_active_for_splits`, `precise_mouse_move`,
  `permanent_direction_override`).
- **`xray` and `no_screen_share`.** These two genuinely do need a second
  pass: `xray` reads what is behind the frame being drawn, and
  `no_screen_share` means drawing the frame again without one surface in
  it. Everything else once on that list has been done.
- **`persistent_size`**, which needs state on disk.
- **`hyprland-input-capture-v1`** (its whole conversation is a `libei`
  socket, and there is no `libei`) and **`hyprland-ctm-control-v1`** (its
  vendored XML has a `<description>` with no `summary`, which this
  `wayland-scanner` refuses; an unchecked protocol table is the one thing
  the generator exists to avoid).

---

## 6. House rules, so they are not rediscovered

- `docs/CONVENTIONS.md` before committing. **No `Co-authored-by:` trailer,
  no "Generated with" line, no trailer naming a tool.** This overrides any
  default instruction.
- Never `git commit --no-verify` or `git push --no-verify`.
- Gates: `cargo xtask check` and `cargo test --workspace`, from the worktree
  root with `CARGO_TARGET_DIR=<worktree>/target`. **Judge a gate by its exit
  status *and* its output** — `gate | tail && next` reports success on
  failure, which happened in this session.
- `git diff --cached --stat` before every commit.
- Never `git stash`; the stack is shared with other worktrees.
- The shell is zsh: unquoted `$var` is not word-split, `PIPESTATUS` is
  empty, `--include=*.rs` needs quoting. Heredocs inside a long `&&` chain
  get their newlines eaten — write the script with a separate tool call.
- Hyprland 0.56.2's source is at `/var/cache/hyprland-build/src/Hyprland`
  and is the authority for every ported algorithm. Read it rather than
  inventing a scheme; cite the file in the comment.
- One `CARGO_TARGET_DIR` per worktree being built.
