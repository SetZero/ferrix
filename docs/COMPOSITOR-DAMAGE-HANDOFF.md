# Handoff: the compositor's frame time

Written 2026-09-17, at the end of the session that landed damage tracking.
This is what is done, what is measured, and the one change that is left —
with the reasoning behind it, so that whoever picks it up does not have to
rediscover any of it.

The short version: **an opaque window is now cheap (42 ms → 0.24 ms a
frame). A translucent one still costs a full frame, and the fix is written
but not wired up.**

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

Row 2 is the open problem.

---

## 2. The open problem, and the fix

### Why a translucent window still costs a full frame

`Canvas::blur` reads the canvas a kernel's reach outside what it writes.
Outside the damage the canvas still holds the **last** frame — and the last
frame has the translucent surface drawn *over* the blur. So blurring a strip
is blurring the previous blur plus the surface: a smear that grows every
frame.

`Plan::blurs_whole` in `compositor/hyprix/src/damage.rs` answers this by
growing the damage until every blurred surface the damage touches is redrawn
whole, plus a reach. That is correct and it is what makes row 2 flat: a
near-fullscreen translucent window means a near-fullscreen frame.

It was found the honest way — it cost a step or two of a channel over 8604
pixels of `dwindle-two-clients`, which is the golden-image suite catching
it.

Hyprland has the same artifact and stops at the reach; see `CRenderPass::begin`
and its comment, "moving a window over blur shows the edges being wonk".

### The fix, half of which is landed

Hyprland's actual answer is `decoration:blur:new_optimizations`, which is
**on by default**: keep the blurred backdrop in its own framebuffer rather
than blurring the live one.

`compositor/render` has this now (commit `39634520`):

- `Canvas::blur_from(backdrop, rect, rounding, blur, damage)` — reads
  `backdrop` instead of `self`.
- `Canvas::take_from(other, damage)` — how the backdrop is kept: copies the
  damaged pixels out of the live canvas.
- `render_onto(canvas, backdrop, …)` — `render_with_layers` with one. It
  takes the snapshot **after the layer surfaces below the windows and before
  the windows**, which is exactly where Hyprland's optimisation takes it.

Nothing ever draws over the backdrop, so a strip of it holds the same pixels
a whole frame would have put there, and a blur over that strip is exact.

**Nothing calls `render_onto` yet.** `render_with_layers` passes no backdrop,
so every pixel is what it was and no expected image moves.

### What is left to do

1. **A backdrop canvas per screen.** `Screen` in
   `compositor/hyprix/src/state.rs` already owns a `Canvas`; give it a
   second one of the same size, made and resized alongside the first.
2. **`frame::Output` gains `backdrop: &'a mut Canvas`**, and
   `crate::frame::draw_windows` calls `render_onto` with `Some(backdrop)`
   instead of `render_with_layers`. The edit was started and is *not* in the
   tree — it was interrupted deliberately, so start clean.
3. **Drop `Plan::blurs_whole`** from `compositor/hyprix/src/damage.rs`, or
   reduce it to the reach around the damage rather than the whole surface.
   This is the change that buys the time; do it *after* 1 and 2 and check
   the goldens at each step.
4. **Re-measure row 2** with the recipe in §3 and put the number in the
   commit message.

### What it gives up, and why that is the right trade

A window blurring the *window* behind it. The backdrop stops at the layer
surfaces, so two translucent windows one over the other each blur the
desktop rather than each other. Hyprland's own optimisation gives up exactly
this, and it is what a person gets from Hyprland out of the box — so
matching it is not a compromise, it is the fidelity.

It may move expected images that have a translucent window over another
window. `only_a_translucent_window_has_its_background_blurred` is the one to
look at first. If an image moves, say so in the commit message and say which
way: toward Hyprland's default, not away from it.

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

Two traps, both of which cost time in this session:

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
