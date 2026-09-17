//! A monitor's frame, drawn from the layout's answer and the clients'
//! buffers.

use std::collections::BTreeMap;

use compositor_config::Config;
use compositor_layout::{MonitorLayout, Placed, WindowId};

use crate::{Canvas, Color, Damage, Rect, Surface};

/// The colours a frame is drawn in, and the decorations it is drawn with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// What shows where no window is: Hyprland's `misc:background_color`,
    /// which `compositor/config` does not read yet, at its default.
    pub background: Color,
    /// The focused window's border: the first colour of
    /// `general:col.active_border`.
    pub active_border: Color,
    /// Every other window's border: the first colour of
    /// `general:col.inactive_border`.
    pub inactive_border: Color,
    /// `decoration:rounding`: how far a window's corners are cut, in pixels.
    /// Zero is a square window, which is Hyprland's default.
    pub rounding: i64,
    /// `decoration:active_opacity`: how much of the focused window shows.
    pub active_opacity: f32,
    /// `decoration:inactive_opacity`: the same for every other window.
    pub inactive_opacity: f32,
    /// `decoration:fullscreen_opacity`: the same for a fullscreen one, which
    /// Hyprland keeps apart because a translucent fullscreen window shows
    /// the background and nothing else.
    pub fullscreen_opacity: f32,
}

impl Style {
    /// Hyprland's `misc:background_color` default.
    pub const BACKGROUND: Color = Color(0xFF11_1111);

    /// The colours `config` gives. A border gradient is drawn as its first
    /// colour until gradients are written.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let first = |name: &str, default: u32| {
            config
                .gradient(name)
                .and_then(|gradient| gradient.colors.first().copied())
                .unwrap_or(Color(default))
        };
        let opacity = |name: &str| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "an opacity is between zero and one; `f32` holds it"
            )]
            let value = config.float(name).unwrap_or(1.0) as f32;
            value.clamp(0.0, 1.0)
        };
        Self {
            background: Self::BACKGROUND,
            active_border: first("general:col.active_border", 0xFFFF_FFFF),
            inactive_border: first("general:col.inactive_border", 0xFF44_4444),
            rounding: config.int("decoration:rounding").unwrap_or(0).max(0),
            active_opacity: opacity("decoration:active_opacity"),
            inactive_opacity: opacity("decoration:inactive_opacity"),
            fullscreen_opacity: opacity("decoration:fullscreen_opacity"),
        }
    }

    /// How much of a window shows, by what it is.
    #[must_use]
    pub const fn opacity(&self, focused: bool, fullscreen: bool) -> f32 {
        if fullscreen {
            self.fullscreen_opacity
        } else if focused {
            self.active_opacity
        } else {
            self.inactive_opacity
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl Eq for Style {}

/// A window's client rectangle with its border around it, in the layout's
/// global coordinates.
#[must_use]
pub fn outer(placed: &Placed) -> Rect {
    let border = placed.border.max(0);
    Rect::new(
        placed.rect.x.saturating_sub(border),
        placed.rect.y.saturating_sub(border),
        placed.rect.width.saturating_add(border.saturating_mul(2)),
        placed.rect.height.saturating_add(border.saturating_mul(2)),
    )
}

/// One layer surface to draw: where it is, whether it is above the windows,
/// and its pixels.
///
/// The compositor works out the rectangle from the protocol's anchor rules
/// (`compositor_layout::layers`); this crate only draws it.
#[derive(Debug)]
pub struct LayerFrame<'pixels> {
    /// Where it is, in the layout's global coordinates.
    pub rect: Rect,
    /// Whether it is drawn above the windows: `top` and `overlay` are,
    /// `background` and `bottom` are not.
    pub above: bool,
    /// Its pixels, or `None` for one that has not drawn yet.
    pub surface: Option<Surface<'pixels>>,
}

/// Draw `output`, the layout of the monitor whose top-left corner is at
/// `origin` in the layout's global coordinates, into `canvas` within
/// `damage`.
///
/// In order: the background; the layer surfaces that are under the windows;
/// each window bottom to top, its border in [`Style`]'s active colour if it
/// has focus and the inactive one if not, then its surface from `surfaces`
/// inside the border; and the layer surfaces that are above them. A window
/// or a layer surface with no pixels yet shows what is under it.
///
/// Returns the damage the frame produced, which is `damage` on the canvas,
/// since the background covers all of it.
pub fn render(
    canvas: &mut Canvas,
    output: &MonitorLayout,
    origin: (i64, i64),
    style: &Style,
    surfaces: &BTreeMap<WindowId, Surface<'_>>,
    damage: &Damage,
) -> Damage {
    render_with_layers(canvas, output, origin, style, surfaces, &[], damage)
}

/// [`render`], with the layer surfaces `zwlr_layer_shell_v1` put on the
/// monitor.
pub fn render_with_layers(
    canvas: &mut Canvas,
    output: &MonitorLayout,
    origin: (i64, i64),
    style: &Style,
    surfaces: &BTreeMap<WindowId, Surface<'_>>,
    layers: &[LayerFrame<'_>],
    damage: &Damage,
) -> Damage {
    let local = |rect: Rect| rect.translate(origin.0.saturating_neg(), origin.1.saturating_neg());
    canvas.clear(style.background, damage);
    for layer in layers.iter().filter(|layer| !layer.above) {
        if let Some(surface) = layer.surface.as_ref() {
            canvas.composite(surface, local(layer.rect), damage);
        }
    }
    for placed in &output.windows {
        let rect = local(placed.rect);
        let color = if placed.focused {
            style.active_border
        } else {
            style.inactive_border
        };
        // A rounded window's border follows its corners, so it cannot be
        // four strips: Hyprland draws the outer rounding as the window's
        // plus the border's width, and the surface goes inside it. A square
        // window keeps the four strips, which blend a translucent border
        // once at the corners.
        if style.rounding > 0 {
            let outer = Rect::new(
                rect.x.saturating_sub(placed.border.max(0)),
                rect.y.saturating_sub(placed.border.max(0)),
                rect.width
                    .saturating_add(placed.border.max(0).saturating_mul(2)),
                rect.height
                    .saturating_add(placed.border.max(0).saturating_mul(2)),
            );
            canvas.fill_rounded(
                outer,
                style.rounding.saturating_add(placed.border.max(0)),
                color,
                damage,
            );
        } else {
            canvas.border(rect, placed.border, color, damage);
        }
        if let Some(surface) = surfaces.get(&placed.window) {
            // Scaled, which is the exact path when the surface is already
            // the rectangle's size -- which it is for every window that is
            // not part-way through an animation.
            canvas.composite_scaled(
                surface,
                rect,
                style.rounding,
                style.opacity(placed.focused, placed.fullscreen),
                damage,
            );
        }
    }
    for layer in layers.iter().filter(|layer| layer.above) {
        if let Some(surface) = layer.surface.as_ref() {
            canvas.composite(surface, local(layer.rect), damage);
        }
    }
    damage.clipped(canvas.bounds())
}

/// The part of a monitor whose top-left corner is at `origin` that differs
/// between layouts `old` and `new`, in the monitor's coordinates: the
/// border-and-client rectangle of each window that appeared, went, moved,
/// changed its border or its focus, both where it was and where it is. If the
/// stacking order changed, every window in either layout counts.
///
/// Changes to a surface's own pixels are the caller's to add.
#[must_use]
pub fn damage_between(old: &MonitorLayout, new: &MonitorLayout, origin: (i64, i64)) -> Damage {
    let local = |rect: Rect| rect.translate(origin.0.saturating_neg(), origin.1.saturating_neg());
    let order = |layout: &MonitorLayout| -> Vec<WindowId> {
        layout.windows.iter().map(|placed| placed.window).collect()
    };
    let restacked = {
        let (before, after) = (order(old), order(new));
        let common = |a: &[WindowId], b: &[WindowId]| -> Vec<WindowId> {
            a.iter().copied().filter(|id| b.contains(id)).collect()
        };
        common(&before, &after) != common(&after, &before)
    };
    let find = |layout: &MonitorLayout, window: WindowId| {
        layout
            .windows
            .iter()
            .find(|placed| placed.window == window)
            .copied()
    };
    let mut damage = Damage::new();
    for placed in &old.windows {
        if restacked || find(new, placed.window) != Some(*placed) {
            damage.add(local(outer(placed)));
        }
    }
    for placed in &new.windows {
        if restacked || find(old, placed.window) != Some(*placed) {
            damage.add(local(outer(placed)));
        }
    }
    damage
}
