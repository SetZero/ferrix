# Handoff: the compositor's frame time

Written 2026-09-17, at the end of the session that landed damage tracking,
and finished the same day with the backdrop wired up.
This is what is done, what is measured, and what is left — with the
reasoning behind it, so that whoever picks it up does not have to
rediscover any of it.

The short version: **a frame now costs what it changed.** An opaque window
went from 42 ms a frame to 0.24 ms; a translucent one — which used to cost
a whole frame whatever it committed, because a blur read the canvas it was
being drawn into — now costs 5–9 ms and redraws as little as 660 pixels of
2073600. The one case left where nothing can be done is a client that
damages its whole buffer every frame, such as a scrolling terminal or a
video wallpaper.

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

Rows 2 and 3 were flat, for two different reasons.

Row 3's is a trap that must not be forgotten when re-measuring: **that
configuration's wallpaper is `mpvpaper` playing a video on a full-screen
layer surface, committing 1920x1080 of damage every frame.** In that run
the damage genuinely *is* the screen and no amount of tracking can move the
number. Measure with a static wallpaper or none.

Row 2's was the blur, and §2 is what was done about it. After the backdrop
(`c5384fb7`), a translucent `foot` overwriting one line runs at **5–9 ms a
frame with a smallest frame of 660 pixels** — the recipe is in §3.

---

## 2. The blur, and what was done about it

### Why a translucent window used to cost a full frame

`Canvas::blur` reads the canvas a kernel's reach outside what it writes.
Outside the damage the canvas still holds the **last** frame — and the last
frame has the translucent surface drawn *over* the blur. So blurring a strip
was blurring the previous blur plus the surface: a smear that grows every
frame.

`Plan::blurs_whole` answered this by growing the damage until every blurred
surface the damage touched was redrawn whole, plus a reach. That was
correct, and it is what made row 2 flat: a near-fullscreen translucent
window meant a near-fullscreen frame however small the client's commit was.

It was found the honest way — it cost a step or two of a channel over 8604
pixels of `dwindle-two-clients`, which is the golden-image suite catching
it.

Hyprland has the same artifact where it blurs live and stops at the reach;
see `CRenderPass::begin` and its comment, "moving a window over blur shows
the edges being wonk".

### The fix, landed in `c5384fb7`

Hyprland's own answer is `decoration:blur:new_optimizations`, which is **on
by default**: blur a backdrop framebuffer nothing draws over, rather than
the live frame.

- `Canvas::blur_from(backdrop, rect, rounding, blur, damage)` reads
  `backdrop` instead of `self`; `Canvas::take_from(other, damage)` is how
  the backdrop is kept, copying the damaged pixels out of the live canvas
  (both in `39634520`).
- `render_onto(canvas, backdrop, …)` takes the snapshot **after the layer
  surfaces below the windows and before the windows**. That is exactly
  where Hyprland fills its `blurFB`: `IHyprRenderer::renderWorkspace`
  queues `CPreBlurElement` after `renderBackground` and the BACKGROUND and
  BOTTOM layers and before `renderWorkspaceWindows`, and
  `preBlurForCurrentMonitor` blurs the main framebuffer into
  `m_blurFB` there.
- `Screen` in `compositor/hyprix/src/state.rs` owns a second `Canvas`,
  handed to the frame as `frame::Output::backdrop`, and
  `crate::frame::draw_windows` calls `render_onto` with it.
- `Plan::blurs_whole` is **gone**, with the `Plan::blurred` field and
  `state.rs`'s `blurs_behind` and `frame.rs`'s `translucent` that fed it.
  Nothing grows the damage for the blur any more.

Why that is exact rather than merely cheaper: the backdrop is maintained
the same way the canvas is. Anything that changes the desktop is in that
frame's damage, and the backdrop is refilled over the damage, so its pixels
outside the damage are the ones the frames before put there — which are the
ones a whole frame would have. A blur over a strip therefore reads what a
whole frame's blur would read and writes the same pixels. The golden-image
suite is the proof: a partial frame and a whole one are compared byte for
byte in `a_frame_with_partial_damage_leaves_the_rest_alone`, and the
compositor's own screenshots are compared with the renderer's images.

`render_with_layers` draws a backdrop of its own over the whole screen, so
a one-shot frame and a kept backdrop draw one picture. Without that the
renderer would have two answers to what a blur reads, which is the trap
that caught this change first: `hyprix`'s expected images are made by the
renderer's convenience entry point, and the two disagreed by 176055 pixels
until both drew from a backdrop.

### What it gives up, and why that is the right trade

A window blurring the *window* behind it, and the dim of a `dim_around`
launcher being blurred — both are drawn after the snapshot. Hyprland's own
optimisation gives up exactly this, and it is what a person gets from
Hyprland out of the box, so matching it is not a compromise but the
fidelity.

Thirteen blessed images moved for it, each only where a blur is and each by
a few levels a channel: `dwindle-two-clients` differs in 176055 pixels by
about four levels, where the neighbouring window used to bleed into the
gradient's blur through the pyramid. Every image with nothing translucent
in it — `locked-screen`, `compositor/term`'s own — is untouched, which is
the check that the change is the blur and nothing else.

### What is left here

Nothing on the blur's side. The two things this leaves:

- **A screen that changes size** would need its backdrop remade with its
  canvas. Neither is resized today — both are made once, in `Screen::all`
  — so a backend that can resize has to remake the pair. A backdrop of the
  wrong size is not a crash: `Canvas::blur_from` falls back to reading the
  canvas, which is the old smear.
- **`xray`** (`decoration:blur:xray`, and the `windowrule`) is still not
  done. It is the option that makes a window's blur read *past* the
  windows to the wallpaper — which the backdrop now holds, so it went from
  needing a second pass to being a flag on which canvas to read. It is the
  cheapest thing on the list in §5 now.

---

## 3. How to measure, exactly

```sh
cd <worktree>/compositor
export CARGO_TARGET_DIR=<worktree>/target
cargo build --release -p hyprix

# A translucent terminal that repaints a little, over no animated
# wallpaper. Write the three files with an editor, not inside a `&&`
# chain: zsh eats the newlines of a heredoc in one.
#
# /tmp/blur/loop.sh   (chmod +x)
#   #!/bin/sh
#   i=0
#   while :; do printf '\rline %s' "$i"; i=$((i+1)); sleep 0.2; done
#
# /tmp/blur/foot.ini
#   [colors]
#   alpha=0.8
#
# /tmp/blur/hyprland.conf
#   monitor = , preferred, auto, 1
#   decoration {
#       rounding = 8
#       blur { enabled = true
#              size = 8
#              passes = 3 }
#   }
#   exec-once = foot -c /tmp/blur/foot.ini /tmp/blur/loop.sh

../target/release/hyprix --headless 1920x1080 \
    --display blur --instance blur \
    --config /tmp/blur/hyprland.conf --deadline 40000 2>&1 | grep '^hyprix:'
```

That run, on the machine this was written on: `slowest of the last 5` sits
at **5–9 ms** and the final line ends `smallest frame 660 of 2073600
pixels`.

Three details of the recipe that are the recipe:

- `alpha=0.8` in foot's own configuration is what makes its buffer
  `ARGB8888`. Without it the window is opaque, the renderer draws no blur
  behind it, and the run measures nothing.
- `printf '\r…'` **overwrites one line**. `printf '…\n'` scrolls the
  terminal once its rows fill, and a scroll is a whole-buffer commit: the
  same run then sits at 116 ms a frame with a 95 ms blur, and it is the
  client's damage, not the compositor's tracking. Both numbers are real
  and they measure different things.
- `--display blur` is a socket name, not a path.

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
- `a_frame_with_partial_damage_leaves_the_rest_alone` (`render`'s tests) —
  the one that holds the backdrop honest: a frame drawn over a partial
  damage on a canvas that already holds a frame must be byte-identical to a
  whole frame of the new layout. It fails the moment a blur reads something
  a whole frame would not have, which is how the first two attempts at
  `render_with_layers`'s own backdrop were found wanting.
- The whole golden-image suite: `cargo test -p compositor-render -p hyprix
  -p compositor-term`. **If any blessed image moves, that is a bug until
  proven otherwise** — and if it is not a bug, the commit message says
  which pixels moved, by how much, and why that is the direction of
  Hyprland's default. Every real bug in this area — the kernel's reach, the
  blur reading its own output, the renderer and the compositor disagreeing
  about the backdrop — was caught by an image moving.

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
- **`xray` and `no_screen_share`.** `no_screen_share` genuinely needs a
  second pass: it means drawing the frame again without one surface in it.
  `xray` no longer does — it means blurring the wallpaper rather than what
  is in front of it, and the backdrop canvas (§2) is the wallpaper, so it
  is now a question of which canvas a surface's blur reads. Everything else
  once on that list has been done.
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
