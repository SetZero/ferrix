//! What a frame has to redraw, and nothing else.
//!
//! Hyprland keeps a damage region for each monitor and adds to it as things
//! happen: `IHyprRenderer::damageSurface` for the pixels a client says it
//! drew, `damageWindow` for a window that moved -- the whole of its
//! bounding box, where it was and where it is -- and `damageMonitor` for
//! everything else (`src/render/Renderer.cpp`). This is the same region,
//! gathered in two halves that between them cover every pixel a frame can
//! change.
//!
//! The first half is what is *inside* a surface, which only its client can
//! change and only by committing: `wl_surface.damage` and `damage_buffer`
//! say which part, and those arrive here as [`Painted`] while the
//! connections are read.
//!
//! The second half is everything the compositor itself decides: where each
//! window is, which one has the focus, what a `windowrule` gave it, where
//! the bars and the menus are, where the pointer is, whether the session is
//! locked. There are some forty places in the loop that change one of
//! those, and a damage region built by instrumenting each of them is one
//! stale pixel away from a bug nobody can explain. So the *whole
//! description* a frame is drawn from is kept as a [`Plan`] and compared
//! with the last frame's, and what differs is damaged in both places --
//! which is what `damageWindow` does for a window that moved.
//!
//! Anything neither half can place damages the whole screen: a needless
//! redraw is only slow, and [`Told::everything`] is this module's
//! `damageMonitor`.
//!
//! # The blur reads more than it writes
//!
//! There is one thing a frame draws that *reads* outside what it writes:
//! the blur behind a translucent surface, which takes a kernel's reach of
//! pixels from around the part it redraws. Nothing here grows the region
//! for it, because what it reads is not this canvas: the compositor keeps a
//! backdrop canvas per screen holding everything behind the windows, and
//! nothing is ever drawn over that (`crate::frame::Output::backdrop`,
//! Hyprland's `decoration:blur:new_optimizations`). Its pixels outside the
//! damage are the ones the frames before put there, which are the ones
//! this frame would have -- anything that changed them is in the damage --
//! so a blur over a strip reads what a whole frame's blur would and writes
//! the same pixels.
//!
//! Before the backdrop this region had to grow until every blurred surface
//! it touched was redrawn whole, which made a near-fullscreen translucent
//! window cost a near-fullscreen frame.
//!
//! # Two buffers, two frames
//!
//! The canvas keeps the whole picture from frame to frame -- that is what
//! makes redrawing a strip of it legal -- but the screen may not. The card
//! backend draws into two buffers in turn, so the one being drawn into
//! holds the frame from *two* frames ago, and copying only this frame's
//! damage into it would leave the frame before's changes off the screen.
//! [`Frame::screen`] is therefore this frame's damage and the last one's,
//! which is Hyprland's damage ring at a buffer age of two
//! (`CMonitor::addDamage`).

use std::collections::BTreeMap;

use compositor_layout::{MonitorLayout, Rect, WindowId};
use compositor_render::{Damage, Style, WindowStyle, outer};
use compositor_wire::ObjectId;

use crate::frame::{Cursor, Placed};

/// How many commits are kept before a pass gives up and damages everything.
///
/// A frame is drawn on the pass a commit arrives, so this is only reached
/// when something is holding frames back; past it the region costs more to
/// work out than the screen costs to redraw.
const MOST: usize = 64;

/// What one client said about its own pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Painted {
    /// Which connection.
    pub(crate) client: usize,
    /// The `wl_surface` it committed.
    pub(crate) surface: ObjectId,
    /// The buffer's size in its own pixels, or `None` where the surface
    /// shows none.
    pub(crate) buffer: Option<(i64, i64)>,
    /// What it damaged, in the buffer's own pixels. Empty is the whole
    /// surface: a commit that named no damage at all has changed something
    /// the compositor cannot place inside it.
    pub(crate) rects: Vec<Rect>,
}

impl Painted {
    /// The parts of `into` this commit changed, in the screen's own pixels.
    ///
    /// The whole of it for a commit that named no damage and for a surface
    /// whose buffer's size is not known: a client that attached a buffer and
    /// said nothing about what it drew in it has changed something the
    /// compositor cannot place inside the surface.
    pub(crate) fn parts(&self, into: Rect) -> Vec<Rect> {
        let Some(buffer) = self
            .buffer
            .filter(|(width, height)| *width > 0 && *height > 0)
        else {
            return vec![into];
        };
        if self.rects.is_empty() {
            return vec![into];
        }
        self.rects
            .iter()
            .map(|rect| inside(*rect, buffer, into))
            .collect()
    }
}

/// Where a commit's pixels went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Landed {
    /// Nowhere on this screen: a window on another monitor or another
    /// workspace, which this screen does not draw.
    Nowhere,
    /// Into this rectangle of the screen, which the surface's buffer is
    /// drawn into whole.
    In(Rect),
    /// Somewhere this screen cannot place: a subsurface, a cursor surface
    /// that is not the one being drawn, a window that has not been mapped
    /// yet. The whole screen, then, which is the rule the module comment
    /// gives.
    Anywhere,
}

/// What the clients said since the last frame was drawn.
///
/// Held across passes rather than a pass, because a pass that changes
/// something does not always draw: what a client said has to survive until
/// the frame that shows it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Told {
    /// Whether something happened that cannot be placed on a screen at all,
    /// and every pixel of every screen has to be drawn again.
    everything: bool,
    /// The commits, in the order they arrived.
    painted: Vec<Painted>,
}

impl Told {
    /// Everything: a pool was remapped or a buffer destroyed under a
    /// surface that is still shown, and which pixels that moved cannot be
    /// worked out from what arrived.
    pub(crate) fn everything(&mut self) {
        self.everything = true;
        self.painted.clear();
    }

    /// One commit.
    pub(crate) fn painted(&mut self, painted: Painted) {
        if self.everything {
            return;
        }
        if self.painted.len() >= MOST {
            self.everything();
            return;
        }
        self.painted.push(painted);
    }

    /// Forget it all, which is what drawing a frame from it does.
    pub(crate) fn taken(&mut self) {
        self.everything = false;
        self.painted.clear();
    }

    /// Where all of it lands on one screen, and whether any of it is beyond
    /// placing -- which is the whole of that screen.
    pub(crate) fn on(
        &self,
        plan: &Plan,
        sources: &BTreeMap<WindowId, crate::frame::Source>,
    ) -> (Damage, bool) {
        let mut region = Damage::new();
        let mut everything = self.everything;
        for painted in &self.painted {
            match plan.landed(painted, sources) {
                Landed::Nowhere => {}
                Landed::Anywhere => everything = true,
                Landed::In(rect) => {
                    for part in painted.parts(rect) {
                        region.add(part);
                    }
                }
            }
        }
        (region, everything)
    }
}

/// Where a damaged rectangle of a buffer lands on the screen.
///
/// The renderer draws the whole of a surface's buffer into the rectangle
/// the layout gave it, so a part of the buffer lands on the same part of
/// that rectangle. The edges are rounded outwards and then grown by a pixel,
/// because a rectangle that is half a pixel short is a row of stale pixels.
#[must_use]
fn inside(rect: Rect, buffer: (i64, i64), into: Rect) -> Rect {
    let (width, height) = buffer;
    if width <= 0 || height <= 0 {
        return into;
    }
    // The part of the buffer that is really there: a client may damage
    // whatever rectangle it likes, and `INT32_MAX` wide is what a toolkit
    // sends for "all of it".
    let left = rect.x.clamp(0, width);
    let top = rect.y.clamp(0, height);
    let right = rect.right().clamp(left, width);
    let bottom = rect.bottom().clamp(top, height);
    let along = |value: i64, of: i64, from: i64, span: i64| -> i64 {
        from.saturating_add(value.saturating_mul(span).div_euclid(of.max(1)))
    };
    let x = along(left, width, into.x, into.width);
    let y = along(top, height, into.y, into.height);
    let to_x = along(right, width, into.x, into.width);
    let to_y = along(bottom, height, into.y, into.height);
    grown(
        Rect::new(
            x,
            y,
            to_x.saturating_sub(x).max(0),
            to_y.saturating_sub(y).max(0),
        ),
        (1, 1),
    )
}

/// `rect` with `margin` pixels added on every side.
#[must_use]
fn grown(rect: Rect, margin: (i64, i64)) -> Rect {
    Rect::new(
        rect.x.saturating_sub(margin.0),
        rect.y.saturating_sub(margin.1),
        rect.width.saturating_add(margin.0.saturating_mul(2)),
        rect.height.saturating_add(margin.1.saturating_mul(2)),
    )
}

/// What one screen's frame is drawn from, beside the clients' own pixels.
///
/// Everything the renderer reads that is not inside a surface, so that two
/// frames with equal plans differ only where a client has drawn. The
/// rectangles are the screen's own pixels: the layout as
/// `compositor_render::scaled` gives it, and the pointer and the drag icon
/// where the frame puts them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plan {
    /// The screen's size in its own pixels.
    pub(crate) size: (u32, u32),
    /// Where the monitor is in the space every window's rectangle is in.
    pub(crate) origin: (i64, i64),
    /// How many of its pixels one logical pixel is.
    pub(crate) scale: f64,
    /// The colours and decorations, already at the screen's scale.
    pub(crate) style: Style,
    /// Whether `dpms off` has turned the screen off.
    pub(crate) dark: bool,
    /// Whether the session is locked.
    pub(crate) locked: bool,
    /// The ramps a night-light set on it.
    pub(crate) gamma: Option<crate::frame::Gamma>,
    /// The windows, scaled into the screen's own pixels.
    pub(crate) layout: MonitorLayout,
    /// What a `windowrule` gave each of them.
    pub(crate) styles: BTreeMap<WindowId, WindowStyle>,
    /// The bars, the menus and the lock's own surfaces, in drawing order.
    pub(crate) layers: Vec<Placed>,
    /// The pointer as the frame draws it, and where its pixels go.
    pub(crate) cursor: Option<(Cursor, Rect)>,
    /// The surface a drag is carrying, the same way.
    pub(crate) drag_icon: Option<(Placed, Rect)>,
}

impl Plan {
    /// One rectangle of the layout's global space in this screen's own
    /// pixels.
    fn local(&self, rect: Rect) -> Rect {
        crate::frame::local(rect, self.origin, self.scale)
    }

    /// Where a commit's pixels are on this screen.
    ///
    /// Which is to say: what this frame draws that surface's buffer into.
    /// A surface this frame draws nothing from is [`Landed::Anywhere`] --
    /// the compositor knows a client painted and not where -- unless it is
    /// a window of a workspace this screen does not show, which is the one
    /// case where nothing of it can be on this screen at all.
    pub(crate) fn landed(
        &self,
        painted: &Painted,
        sources: &BTreeMap<WindowId, crate::frame::Source>,
    ) -> Landed {
        let named = |client: usize, surface: ObjectId| {
            client == painted.client && surface == painted.surface
        };
        if let Some(placed) = self
            .layers
            .iter()
            .find(|placed| named(placed.client, placed.surface))
        {
            return Landed::In(self.local(placed.rect));
        }
        if let Some((_, at)) = self
            .drag_icon
            .filter(|(icon, _)| named(icon.client, icon.surface))
        {
            return Landed::In(at);
        }
        if let Some((_, at)) = self.cursor.filter(|(cursor, _)| {
            cursor
                .surface
                .is_some_and(|(client, surface, _)| named(client, surface))
        }) {
            return Landed::In(at);
        }
        let Some((window, _)) = sources
            .iter()
            .find(|(_, source)| named(source.client, source.surface))
        else {
            return Landed::Anywhere;
        };
        // The layout is already in the screen's own pixels, so only the
        // monitor's corner is left to take off. A window this screen does
        // not show is on another monitor or another workspace.
        self.layout
            .windows
            .iter()
            .find(|placed| placed.window == *window)
            .map_or(Landed::Nowhere, |placed| {
                Landed::In(placed.rect.translate(
                    self.origin.0.saturating_neg(),
                    self.origin.1.saturating_neg(),
                ))
            })
    }

    /// How far outside a window's border box its decorations reach.
    ///
    /// The shadow is drawn around the border box, grown by its range and
    /// moved by its offset, so a window that went leaves its shadow behind
    /// unless the damage covers that too. A `windowrule = bordersize` is
    /// added on top of it: the renderer draws that border instead of the
    /// layout's, and the layout's is what a rectangle is measured from.
    fn margin(&self) -> (i64, i64) {
        let border = self
            .styles
            .values()
            .filter_map(|style| style.border)
            .fold(0, i64::max);
        let (wide, tall) = self.style.shadow.map_or((0, 0), |shadow| {
            (
                shadow.range.saturating_add(shadow.offset.0.abs()),
                shadow.range.saturating_add(shadow.offset.1.abs()),
            )
        });
        (
            wide.saturating_add(border).max(0),
            tall.saturating_add(border).max(0),
        )
    }

    /// The part of the screen this plan differs from `old` in.
    ///
    /// Whole-screen for anything that changes every pixel -- the screen's
    /// size, where it is, its scale, the style, a night-light's ramps,
    /// `dpms`, the lock -- because each of those is drawn into every pixel
    /// and none of them happens often enough to be worth a region.
    fn since(&self, old: &Self) -> Damage {
        let (width, height) = self.size;
        if self.size != old.size
            || self.origin != old.origin
            || (self.scale - old.scale).abs() >= f64::EPSILON
            || self.style != old.style
            || self.dark != old.dark
            || self.locked != old.locked
            || self.gamma != old.gamma
        {
            return Damage::full(width, height);
        }
        let mut region = Damage::new();
        self.windows_since(old, &mut region);
        self.layers_since(old, &mut region);
        self.pointer_since(old, &mut region);
        region
    }

    /// The windows: what `compositor_render::damage_between` says moved,
    /// appeared, went, took the focus or was restacked, and any window a
    /// rule now draws differently.
    fn windows_since(&self, old: &Self, region: &mut Damage) {
        let margin = self.margin();
        let between = compositor_render::damage_between(&old.layout, &self.layout, self.origin);
        for rect in between.rects() {
            region.add(grown(*rect, margin));
        }
        // A rule changed between the two frames -- `hyprctl keyword
        // windowrule`, or a window that has just been renamed into a rule's
        // reach -- so what the window is drawn with is not what it was,
        // wherever it is.
        let ours = self.styles.keys().chain(old.styles.keys());
        for window in ours.copied().collect::<std::collections::BTreeSet<_>>() {
            if self.styles.get(&window) == old.styles.get(&window) {
                continue;
            }
            for layout in [&old.layout, &self.layout] {
                if let Some(placed) = layout.windows.iter().find(|placed| placed.window == window) {
                    region.add(grown(
                        outer(placed).translate(
                            self.origin.0.saturating_neg(),
                            self.origin.1.saturating_neg(),
                        ),
                        margin,
                    ));
                }
            }
        }
    }

    /// The bars, the menus and the lock's own surfaces.
    fn layers_since(&self, old: &Self, region: &mut Damage) {
        if old.layers == self.layers {
            return;
        }
        // One at a time, and each where it was and where it is: a bar that
        // changed nothing does not have to be drawn again because a dock
        // beside it opened. They are compared by their place in the list,
        // which is what decides which of them covers the other, so a list
        // whose order changed damages every surface that moved in it.
        for at in 0..old.layers.len().max(self.layers.len()) {
            let (was, now) = (old.layers.get(at), self.layers.get(at));
            if was == now {
                continue;
            }
            for placed in was.into_iter().chain(now) {
                region.add(self.local(placed.rect));
            }
        }
    }

    /// The pointer and the drag icon, each where it was and where it is.
    fn pointer_since(&self, old: &Self, region: &mut Damage) {
        if old.cursor != self.cursor {
            for at in [old.cursor, self.cursor].into_iter().flatten() {
                region.add(at.1);
            }
        }
        if old.drag_icon != self.drag_icon {
            for at in [old.drag_icon, self.drag_icon].into_iter().flatten() {
                region.add(at.1);
            }
        }
    }
}

/// What one frame draws and what it copies to the screen.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    /// The part of the canvas this frame redraws.
    pub(crate) canvas: Damage,
    /// The part of the screen it copies into, which is this frame's damage
    /// and the frame before's: the module comment says why.
    pub(crate) screen: Damage,
}

/// One screen's memory of what it last drew.
#[derive(Clone, Debug, Default)]
pub(crate) struct Watch {
    /// The plan the last frame was drawn from, or `None` before there was
    /// one -- which is why a screen's first frame is a whole one.
    plan: Option<Plan>,
    /// What that frame redrew.
    previous: Damage,
}

impl Watch {
    /// The damage for the frame `plan` describes, given what the clients
    /// said they drew.
    ///
    /// `told` is already in this screen's own pixels: only the caller knows
    /// where a surface's rectangle is.
    pub(crate) fn frame(&mut self, plan: Plan, told: &Damage, everything: bool) -> Frame {
        let (width, height) = plan.size;
        let mut region = match self.plan.as_ref() {
            Some(old) if !everything => plan.since(old),
            _ => Damage::full(width, height),
        };
        region.extend(told);
        let region = region.clipped(Rect::new(0, 0, i64::from(width), i64::from(height)));
        let mut screen = region.clone();
        screen.extend(&self.previous);
        self.previous = region.clone();
        self.plan = Some(plan);
        Frame {
            canvas: region,
            screen,
        }
    }
}
