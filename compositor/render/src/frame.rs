//! A monitor's frame, drawn from the layout's answer and the clients'
//! buffers.

use std::collections::BTreeMap;

use compositor_config::Config;
use compositor_layout::{MonitorLayout, Placed, WindowId};

use crate::{Blur, Canvas, Color, Damage, Format, Gradient, Rect, Rounding, Shadow, Surface};

/// The colours a frame is drawn in, and the decorations it is drawn with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// What shows where no window is: Hyprland's `misc:background_color`.
    pub background: Color,
    /// The focused window's border: `general:col.active_border`, whole.
    ///
    /// A gradient, because Hyprland's is: `col.active_border = rgba(33ccffee)
    /// rgba(00ff99ee) 45deg` is two colours running across the window, and a
    /// compositor that took the first of them would draw a configuration
    /// nobody wrote.
    pub active_border: Gradient,
    /// Every other window's border: `general:col.inactive_border`.
    pub inactive_border: Gradient,
    /// `decoration:rounding` and `decoration:rounding_power`: how far a
    /// window's corners are cut and by what curve. Zero is a square
    /// window, which is Hyprland's default.
    pub rounding: Rounding,
    /// `decoration:active_opacity`: how much of the focused window shows.
    pub active_opacity: f32,
    /// `decoration:inactive_opacity`: the same for every other window.
    pub inactive_opacity: f32,
    /// `decoration:fullscreen_opacity`: the same for a fullscreen one, which
    /// Hyprland keeps apart because a translucent fullscreen window shows
    /// the background and nothing else.
    pub fullscreen_opacity: f32,
    /// `decoration:shadow:*`, or `None` when `shadow:enabled` is off.
    pub shadow: Option<Shadow>,
    /// `decoration:dim_strength` when `decoration:dim_inactive` is on: how
    /// much black is laid over a window that is not focused. Zero when it is
    /// off.
    pub dim: f32,
    /// `decoration:dim_around`: how much black is laid over everything
    /// *behind* a window or layer surface whose rule asked for it.
    pub dim_around: f32,
    /// `decoration:blur:*`, or `None` when `blur:enabled` is off: how much
    /// of what is behind a translucent window is blurred, how many times,
    /// and the colour grading over it.
    pub blur: Option<Blur>,
}

impl Style {
    /// Hyprland's `misc:background_color` default.
    pub const BACKGROUND: Color = Color(0xFF11_1111);

    /// The same style on a monitor at `scale`: every length in buffer
    /// pixels rather than logical ones.
    ///
    /// Hyprland scales its decorations by the monitor's scale -- a rounding
    /// of 12 on a screen at `scale = 2` cuts 24 buffer pixels -- and a
    /// renderer that scaled the windows and not the decorations would draw
    /// a hairline border round a doubled window.
    #[must_use]
    pub fn at_scale(&self, scale: f64) -> Self {
        if (scale - 1.0).abs() < f64::EPSILON {
            return *self;
        }
        let grow = |value: i64| -> i64 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                reason = "a decoration's pixels are far inside f64's exact range"
            )]
            let scaled = (value as f64 * scale).round() as i64;
            scaled
        };
        let scaled = |rounding: Rounding| Rounding {
            radius: grow(rounding.radius),
            ..rounding
        };
        Self {
            rounding: scaled(self.rounding),
            shadow: self.shadow.map(|shadow| Shadow {
                rounding: scaled(shadow.rounding),
                range: grow(shadow.range),
                offset: (grow(shadow.offset.0), grow(shadow.offset.1)),
                ..shadow
            }),
            blur: self.blur.map(|blur| Blur {
                size: grow(blur.size),
                ..blur
            }),
            ..*self
        }
    }

    /// The colours and the decorations `config` gives.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        // `decoration:shadow:color` is a gradient option in Hyprland too,
        // because every colour option is; the shadow shader takes one
        // colour, so this takes the first of them, as Hyprland's
        // `CHyprDropShadowDecoration` does.
        let first = |name: &str, default: u32| {
            config
                .gradient(name)
                .and_then(|gradient| gradient.colors.first().copied())
                .unwrap_or(Color(default))
        };
        let border = |name: &str, default: u32| {
            config
                .gradient(name)
                .map_or_else(|| Gradient::solid(Color(default)), Gradient::from)
        };
        let opacity = |name: &str| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "an opacity is between zero and one; `f32` holds it"
            )]
            let value = config.float(name).unwrap_or(1.0) as f32;
            value.clamp(0.0, 1.0)
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a rounding power is between one and ten; `f32` holds it"
        )]
        let power = config
            .float("decoration:rounding_power")
            .unwrap_or(f64::from(Rounding::POWER)) as f32;
        let rounding = Rounding {
            radius: config.int("decoration:rounding").unwrap_or(0).max(0),
            power: power.clamp(1.0, 10.0),
        };
        Self {
            // Hyprland stores a colour as an integer and reads this one
            // as a colour rather than a gradient, so the first stop of
            // whatever was written is what it is.
            background: config
                .gradient("misc:background_color")
                .and_then(|written| written.colors.first().copied())
                .unwrap_or(Self::BACKGROUND),
            active_border: border("general:col.active_border", 0xFFFF_FFFF),
            inactive_border: border("general:col.inactive_border", 0xFF44_4444),
            rounding,
            active_opacity: opacity("decoration:active_opacity"),
            inactive_opacity: opacity("decoration:inactive_opacity"),
            fullscreen_opacity: opacity("decoration:fullscreen_opacity"),
            shadow: config
                .bool("decoration:shadow:enabled")
                .unwrap_or(true)
                .then(|| Shadow {
                    rounding,
                    range: config.int("decoration:shadow:range").unwrap_or(4).max(0),
                    power: u32::try_from(config.int("decoration:shadow:render_power").unwrap_or(3))
                        .unwrap_or(3),
                    color: first("decoration:shadow:color", 0xEE1A_1A1A),
                    offset: offset(config.str("decoration:shadow:offset").unwrap_or("")),
                }),
            dim: if config.bool("decoration:dim_inactive").unwrap_or(false) {
                opacity("decoration:dim_strength")
            } else {
                0.0
            },
            // Not gated on `dim_inactive`: this one is a *rule*'s, and the
            // option is only how strong it is. Hyprland's default is 0.4.
            dim_around: {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a share between zero and one; `f32` holds it"
                )]
                let value = config.float("decoration:dim_around").unwrap_or(0.4) as f32;
                value.clamp(0.0, 1.0)
            },
            blur: config
                .bool("decoration:blur:enabled")
                .unwrap_or(true)
                .then(|| blur_of(config)),
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

impl Style {
    /// The same style with the blur's dither off.
    ///
    /// For the expected images, and for nothing else. They are run-length
    /// encoded, and a dither that moves every pixel by a step or two is the
    /// one thing a run of pixels cannot survive: blessed from Hyprland's
    /// own `decoration:blur:noise` the committed images grow from one
    /// megabyte to eight and a half, and the format's reason for existing
    /// goes with them.
    ///
    /// What is given up is a picture *of* a dither of about one part in
    /// 170 in most of the images. What is kept is the dither itself,
    /// exactly as `blurFinish.glsl` computes it, drawn for any
    /// configuration that asks for one and held by an image of its own.
    #[must_use]
    pub fn undithered(mut self) -> Self {
        if let Some(blur) = self.blur.as_mut() {
            blur.noise = 0.0;
        }
        self
    }
}

impl Eq for Style {}

/// `decoration:blur:*`: the shape of the blur and the five values that
/// grade it.
///
/// Each grading value falls back to Hyprland's own default rather than to
/// the one that does nothing, because Hyprland's defaults are not nothing --
/// `contrast` is 0.8916 and `vibrancy` 0.1696 out of the box -- so a
/// configuration that turns the blur on and says no more asks for a graded
/// blur, dither and all.
///
/// Each is clamped to the range Hyprland's `ConfigValues.cpp` gives it, so
/// a value outside it is the nearest one inside rather than a picture
/// nobody has seen.
fn blur_of(config: &Config) -> Blur {
    let graded = |name: &str, default: f32, top: f32| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a grading value between zero and two; `f32` holds it"
        )]
        let value = config
            .float(name)
            .map_or(default, |value| value as f32)
            .clamp(0.0, top);
        value
    };
    Blur {
        size: config.int("decoration:blur:size").unwrap_or(8).max(0),
        passes: u32::try_from(config.int("decoration:blur:passes").unwrap_or(1)).unwrap_or(1),
        noise: graded("decoration:blur:noise", Blur::NOISE, 1.0),
        contrast: graded("decoration:blur:contrast", Blur::CONTRAST, 2.0),
        brightness: graded("decoration:blur:brightness", Blur::BRIGHTNESS, 2.0),
        vibrancy: graded("decoration:blur:vibrancy", Blur::VIBRANCY, 1.0),
        vibrancy_darkness: graded(
            "decoration:blur:vibrancy_darkness",
            Blur::VIBRANCY_DARKNESS,
            1.0,
        ),
    }
}

/// `shadow:offset`, which Hyprland reads as a vector: two numbers separated
/// by a space or a comma. Anything else is no offset, which is its default.
fn offset(text: &str) -> (i64, i64) {
    let mut parts = text
        .split([' ', ','])
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let number = |part: Option<&str>| {
        part.and_then(|text| text.parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .map_or(0, |value| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "Hyprland clamps the offset to ±250; an i64 holds it"
                )]
                let whole = value.round() as i64;
                whole.clamp(-250, 250)
            })
    };
    (number(parts.next()), number(parts.next()))
}

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
    /// Whether everything behind it is darkened while it is up:
    /// `layerrule = dim_around`. What a launcher does to the desktop.
    pub dim_around: bool,
    /// Whether what is behind it is blurred: `layerrule = blur, waybar`.
    ///
    /// This is what makes a bar with a translucent background look like
    /// Hyprland's, and it is a rule rather than the default because
    /// blurring behind an opaque bar costs a pyramid of passes and changes
    /// not one pixel.
    pub blur: bool,
}

/// What one window is drawn with, where a `windowrule` asked for something
/// other than the style every window has.
///
/// Hyprland's rules change a single window's decorations -- `opacity 0.8`,
/// `rounding 0`, `no_blur` -- and a compositor that read those and drew
/// every window the same would be one whose rules do nothing. Each field is
/// "as the style says" until a rule fills it in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowStyle {
    /// `opacity`: how much of the window shows, over the style's own.
    pub opacity: Option<f32>,
    /// `rounding`: how far its corners are cut.
    pub rounding: Option<i64>,
    /// `border_size`: how wide its border is.
    pub border: Option<i64>,
    /// `no_blur`: whether what is behind it is blurred.
    pub blur: bool,
    /// `no_shadow`.
    pub shadow: bool,
    /// `no_dim`: whether it is dimmed when it is not focused.
    pub dim: bool,
    /// `rounding_power`: the curve its corners are cut by.
    pub rounding_power: Option<f32>,
    /// `border_color`: its border, instead of the focused and unfocused
    /// ones. Hyprland's rule takes one or two gradients -- the second for
    /// the unfocused state -- and this carries the first.
    pub border_color: Option<Gradient>,
    /// `decorate`: whether it is drawn with a border and a shadow at all.
    /// `decorate false` is what a person writes for a window that draws
    /// its own frame.
    pub decorate: bool,
    /// `opaque`: it is drawn as if every pixel were opaque, whatever its
    /// buffer's alpha says. Hyprland's rule for a client that leaves
    /// rubbish in its alpha channel, and it also takes the blur out of the
    /// frame, since there is nothing to see behind an opaque window.
    pub opaque: bool,
    /// `nearest_neighbor`: a stretched window is sampled nearest rather
    /// than bilinear, which is what a person writes for pixel art -- a
    /// sprite must stay a sprite and not become a smear.
    pub nearest: bool,
    /// `dim_around`: everything behind this window is darkened by
    /// `decoration:dim_around` while it is up, which is what a launcher or
    /// a confirmation dialog does to the desktop behind it.
    pub dim_around: bool,
}

impl Default for WindowStyle {
    /// Everything as the style says, which is what a window with no rule
    /// gets.
    fn default() -> Self {
        Self {
            opacity: None,
            rounding: None,
            border: None,
            blur: true,
            shadow: true,
            dim: true,
            rounding_power: None,
            border_color: None,
            decorate: true,
            opaque: false,
            nearest: false,
            dim_around: false,
        }
    }
}

/// The style a frame is drawn with: the one every window has, and the
/// windows a rule gave something else.
#[derive(Clone, Copy, Debug)]
pub struct Styles<'a> {
    /// What every window is drawn with.
    pub base: &'a Style,
    /// What a rule changed, by window.
    pub windows: &'a BTreeMap<WindowId, WindowStyle>,
}

impl<'a> Styles<'a> {
    /// A style with no window given anything of its own.
    #[must_use]
    pub fn plain(base: &'a Style) -> Self {
        Self {
            base,
            windows: Self::none(),
        }
    }

    /// The empty map, which a style with no rules borrows.
    fn none() -> &'static BTreeMap<WindowId, WindowStyle> {
        static NONE: std::sync::OnceLock<BTreeMap<WindowId, WindowStyle>> =
            std::sync::OnceLock::new();
        NONE.get_or_init(BTreeMap::new)
    }

    /// What `window` is drawn with.
    #[must_use]
    pub fn of(&self, window: WindowId) -> WindowStyle {
        self.windows.get(&window).copied().unwrap_or_default()
    }
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
    render_with_layers(
        canvas,
        output,
        origin,
        &Styles::plain(style),
        surfaces,
        &[],
        damage,
    )
}

/// A monitor's layout in the buffer pixels a scaled screen draws.
///
/// Hyprland lays a scaled monitor out in logical pixels -- a 1024x768 screen
/// at `scale = 2` tiles its windows in 512x384 -- and draws every one of
/// them as `scale` buffer pixels. Everything above the renderer therefore
/// works in logical pixels, and this is where they become the screen's own:
/// each rectangle grows away from the monitor's corner, so a window at the
/// monitor's top left stays there.
///
/// The decorations scale with it, which is what [`Style::at_scale`] is for:
/// a border one logical pixel wide is two buffer pixels at `scale = 2`, as
/// it is in Hyprland.
#[must_use]
pub fn scaled(output: &MonitorLayout, origin: (i64, i64), scale: f64) -> MonitorLayout {
    if (scale - 1.0).abs() < f64::EPSILON {
        return output.clone();
    }
    let grow = |value: i64| -> i64 {
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "a screen's pixels are far inside f64's exact range, and a scaled length is                       rounded to the pixel it lands on"
        )]
        let scaled = (value as f64 * scale).round() as i64;
        scaled
    };
    let at = |value: i64, from: i64| from.saturating_add(grow(value.saturating_sub(from)));
    let mut out = output.clone();
    for placed in &mut out.windows {
        placed.rect = Rect::new(
            at(placed.rect.x, origin.0),
            at(placed.rect.y, origin.1),
            grow(placed.rect.width),
            grow(placed.rect.height),
        );
        placed.border = grow(placed.border);
    }
    out
}

/// [`render`], with the layer surfaces `zwlr_layer_shell_v1` put on the
/// monitor.
pub fn render_with_layers(
    canvas: &mut Canvas,
    output: &MonitorLayout,
    origin: (i64, i64),
    styles: &Styles<'_>,
    surfaces: &BTreeMap<WindowId, Surface<'_>>,
    layers: &[LayerFrame<'_>],
    damage: &Damage,
) -> Damage {
    render_onto(
        canvas, None, output, origin, styles, surfaces, layers, damage,
    )
}

/// The same, with a canvas for the blur's backdrop.
///
/// `backdrop` is what [`Canvas::blur_from`] reads: a second canvas holding
/// everything *behind* the windows, kept between frames. Nothing is ever
/// drawn over it, so a strip of it is the same pixels a whole frame would
/// have put there -- which is what lets a blur be drawn over the damage
/// alone rather than over the whole of every translucent window. It is
/// Hyprland's `decoration:blur:new_optimizations`, which is on by default.
///
/// Without one, a blur reads the canvas and the caller has to redraw each
/// blurred surface whole.
#[expect(
    clippy::too_many_arguments,
    reason = "a frame is its canvas, its backdrop, its layout, its styles, its pixels and its damage"
)]
pub fn render_onto(
    canvas: &mut Canvas,
    mut backdrop: Option<&mut Canvas>,
    output: &MonitorLayout,
    origin: (i64, i64),
    styles: &Styles<'_>,
    surfaces: &BTreeMap<WindowId, Surface<'_>>,
    layers: &[LayerFrame<'_>],
    damage: &Damage,
) -> Damage {
    let style = styles.base;
    let local = |rect: Rect| rect.translate(origin.0.saturating_neg(), origin.1.saturating_neg());
    canvas.clear(style.background, damage);
    // `dim_around`: black over everything drawn *so far*, laid down just
    // before the window or surface that asked for it. Hyprland dims what
    // is behind such a thing; drawing in order means "behind" is "already
    // drawn", so one fill in the right place is the whole of it -- no
    // second pass and no second canvas.
    let dim_behind = |canvas: &mut Canvas, damage: &Damage| {
        if style.dim_around <= 0.0 {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a share between zero and one becomes a byte of alpha"
        )]
        let alpha = (style.dim_around.clamp(0.0, 1.0) * 255.0).round() as u32;
        canvas.fill(canvas.bounds(), Color(alpha << 24), damage);
    };
    for layer in layers.iter().filter(|layer| !layer.above) {
        if layer.dim_around {
            dim_behind(canvas, damage);
        }
        draw_layer(canvas, None, layer, local(layer.rect), style, damage);
    }
    // Everything behind the windows is drawn; that is the backdrop, and it
    // is taken now, before a window goes over it.
    if let Some(behind) = backdrop.as_deref_mut() {
        behind.take_from(canvas, damage);
    }
    let behind = backdrop.as_deref();
    for placed in &output.windows {
        if styles.of(placed.window).dim_around {
            dim_behind(canvas, damage);
        }
        window(
            canvas,
            behind,
            placed,
            local(placed.rect),
            styles,
            surfaces,
            damage,
        );
    }
    for layer in layers.iter().filter(|layer| layer.above) {
        if layer.dim_around {
            dim_behind(canvas, damage);
        }
        draw_layer(canvas, behind, layer, local(layer.rect), style, damage);
    }
    damage.clipped(canvas.bounds())
}

/// One layer surface: the blur behind it, where a `layerrule` asked for
/// one, and then its pixels.
///
/// No border, no rounding and no shadow: a bar draws its own corners, and a
/// compositor that put a border round a wallpaper would be drawing a line
/// across the screen.
fn draw_layer(
    canvas: &mut Canvas,
    backdrop: Option<&Canvas>,
    layer: &LayerFrame<'_>,
    rect: Rect,
    style: &Style,
    damage: &Damage,
) {
    let Some(surface) = layer.surface.as_ref() else {
        return;
    };
    // Only for a surface that can be seen through: blurring behind an
    // opaque bar costs a pyramid of passes and changes nothing.
    if let Some(blur) = style.blur.filter(|_| layer.blur)
        && surface.format() == Format::Argb8888
    {
        canvas.blur_from(backdrop, rect, Rounding::none(), &blur, damage);
    }
    canvas.composite(surface, rect, damage);
}

/// One window: its shadow, its border, the blur behind it, its own pixels
/// and the dim over them, with whatever a `windowrule` changed.
fn window(
    canvas: &mut Canvas,
    backdrop: Option<&Canvas>,
    placed: &Placed,
    rect: Rect,
    styles: &Styles<'_>,
    surfaces: &BTreeMap<WindowId, Surface<'_>>,
    damage: &Damage,
) {
    let style = styles.base;
    let own = styles.of(placed.window);
    // A rule's `rounding` and `rounding_power` each stand in for the
    // style's own, and either may be set without the other.
    let rounding = Rounding {
        radius: own.rounding.unwrap_or(style.rounding.radius).max(0),
        power: own.rounding_power.unwrap_or(style.rounding.power),
    };
    let width = own.border.unwrap_or(placed.border).max(0);
    let opacity = own
        .opacity
        .unwrap_or_else(|| style.opacity(placed.focused, placed.fullscreen));
    let gradient = own.border_color.as_ref().unwrap_or(if placed.focused {
        &style.active_border
    } else {
        &style.inactive_border
    });
    // `decorate false`: no border and no shadow, for a window that draws
    // its own frame.
    let width = if own.decorate { width } else { 0 };
    // The shadow first, under the border and the window: Hyprland draws it
    // as a decoration behind them and does not cut the window's own shape
    // out of it.
    if let Some(shadow) = style.shadow.as_ref().filter(|_| own.shadow && own.decorate) {
        canvas.shadow(
            Rect::new(
                rect.x.saturating_sub(width),
                rect.y.saturating_sub(width),
                rect.width.saturating_add(width.saturating_mul(2)),
                rect.height.saturating_add(width.saturating_mul(2)),
            ),
            &Shadow {
                rounding,
                ..*shadow
            },
            damage,
        );
    }
    // A rounded window's border follows its corners, so it cannot be four
    // strips: Hyprland draws the outer rounding as the window's plus the
    // border's width, and the surface goes inside it. A square window keeps
    // the four strips, which blend a translucent border once at the corners.
    //
    // Either way the gradient runs across the border's whole box, which is
    // the box `renderBorder` gives the shader: the rounded path fills that
    // box and the square one draws four windows onto it, so a window's
    // corner is the same colour whichever path drew it.
    if !rounding.is_square() {
        let outer = Rect::new(
            rect.x.saturating_sub(width),
            rect.y.saturating_sub(width),
            rect.width.saturating_add(width.saturating_mul(2)),
            rect.height.saturating_add(width.saturating_mul(2)),
        );
        canvas.fill_rounded_gradient(
            outer,
            Rounding {
                radius: rounding.radius.saturating_add(width),
                ..rounding
            },
            gradient,
            damage,
        );
    } else {
        canvas.border_gradient(rect, width, gradient, damage);
    }
    // What is behind a window that can be seen through, blurred. Only for a
    // window that can be: blurring behind an opaque one costs a pyramid of
    // passes and changes not one pixel of the frame. A surface in a format
    // with alpha may be translucent anywhere, and a window drawn at less
    // than full opacity is translucent everywhere.
    // `opaque`: the window is drawn as if every pixel were opaque, so
    // there is nothing to see behind it and nothing to blur.
    let translucent = !own.opaque
        && (surfaces
            .get(&placed.window)
            .is_some_and(|surface| surface.format() == Format::Argb8888)
            || opacity < 1.0);
    if let Some(blur) = style.blur.filter(|_| own.blur)
        && translucent
    {
        canvas.blur_from(backdrop, rect, rounding, &blur, damage);
    }
    if let Some(surface) = surfaces.get(&placed.window) {
        // Scaled, which is the exact path when the surface is already the
        // rectangle's size -- which it is for every window that is not
        // part-way through an animation.
        let surface = if own.opaque {
            (*surface).as_opaque()
        } else {
            *surface
        };
        canvas.composite_scaled(&surface, rect, rounding, opacity, own.nearest, damage);
    }
    // `decoration:dim_inactive`: black over a window that is not focused, at
    // `dim_strength`. Over the surface, because it dims the window and not
    // the background behind it.
    if style.dim > 0.0 && !placed.focused && own.dim {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "an opacity between zero and one becomes a byte of alpha"
        )]
        let alpha = (style.dim.clamp(0.0, 1.0) * 255.0).round() as u32;
        canvas.fill_rounded(rect, rounding, Color(alpha << 24), damage);
    }
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
