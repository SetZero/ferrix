//! A border's gradient: Hyprland's `col.active_border` and its relatives,
//! drawn the way `border.frag` draws them.
//!
//! Hyprland's gradient is not the linear ramp between two sRGB colours it
//! looks like. Three things make it its own, and the picture is wrong
//! without any one of them:
//!
//! * **It is interpolated in `OkLab`.** `CGradientValueData::updateColorsOk`
//!   converts every colour to `OkLab` once, the shader is handed those, and
//!   `gradient.glsl` mixes *them*. A ramp from blue to green through sRGB
//!   passes through a muddy grey that `OkLab` does not.
//! * **The angle is not a rotation.** `getOkColorForCoordArray1` folds the
//!   coordinate into the first quadrant and then blends the two axes by
//!   `sin(angle)`: `progress = y·sin + x·(1 − sin)`. At 45 degrees that is
//!   `0.707·y + 0.293·x`, not the `0.707·(x + y)` a rotation would give, so
//!   a gradient at 45 degrees leans further down the window than its name
//!   suggests. This is a port, so it leans the same way.
//! * **The round trip is asymmetric.** A colour goes *in* through the sRGB
//!   transfer function (`gammaToLinear` in hyprgraphics' `Color.cpp`, the
//!   piecewise 2.4 curve) and comes *out* through a plain gamma of 2.2
//!   (`okLabAToSrgb` asks `fromLinearRGB` for `CM_TRANSFER_FUNCTION_GAMMA22`).
//!   The two do not cancel: `0x33` goes in and `0x36` comes back. Hyprland
//!   draws the lighter one, so a gradient of two or more colours draws it
//!   here too.
//!
//! The exception is a gradient of one colour, which Hyprland still sends
//! through that round trip and still lightens. This crate fills it exactly
//! instead: a border that is one colour is the colour that was written in
//! `hyprland.conf`, which is what somebody reading their own configuration
//! back off the screen means. The divergence is at most three parts in 255
//! and it is the only one.
//!
//! # Where the angles point
//!
//! With `sin(0) = 0` the progress is the coordinate's `x`, so **0 degrees
//! runs left to right** and its bands of equal colour are vertical. With
//! `sin(90°) = 1` it is `y`, which is **top to bottom** with horizontal
//! bands. The bands therefore turn clockwise as the angle grows -- a
//! window's texture coordinate has `y` growing downwards -- which is what
//! `45deg` means in a Hyprland configuration: the bands are a vertical line
//! turned 45 degrees clockwise, and the colours run from the window's
//! top-left corner towards its bottom-right one.

use compositor_config::MAX_GRADIENT_COLORS;

use crate::Color;

/// How finely the span between two colours is sampled when a gradient is
/// drawn.
///
/// The colour at a point is three cube roots and three powers away from the
/// stops, which is far too much arithmetic to do at every pixel of a
/// window-sized fill, so a gradient is drawn from a ramp built once. Each
/// span gets this many samples, so the ramp's resolution follows the number
/// of colours rather than falling as they are added: even a span running
/// from black to white steps by 255/512 of a channel, which is under the
/// half step the rounding to a byte loses anyway.
const SAMPLES_PER_SPAN: usize = 512;

/// A quarter turn, a half turn, three quarters and a whole one, as
/// `gradient.glsl` writes them.
///
/// Rounded to two decimals, and deliberately not `f32::consts::PI` and its
/// relatives: these are the numbers the shader compares an angle against and
/// folds it with, so they are what decides the branch a border drawn at
/// exactly 90, 180 or 270 degrees takes. Writing pi here would be writing a
/// different shader.
#[expect(
    clippy::approx_constant,
    reason = "the shader's own rounded constants, which decide its branches"
)]
const TURN: [f32; 4] = [1.57, 3.14, 4.71, 6.28];

/// A border's gradient: its colours in order, and the angle they run at.
///
/// Fixed-capacity rather than a `Vec` so that [`Style`](crate::Style) stays
/// `Copy` -- a style is passed around by value all over the compositor --
/// and because Hyprland caps a gradient at [`MAX_GRADIENT_COLORS`] anyway.
#[derive(Clone, Copy, Debug)]
pub struct Gradient {
    colors: [Color; MAX_GRADIENT_COLORS],
    len: usize,
    degrees: i32,
}

impl Gradient {
    /// One colour, at angle zero: what a `col.active_border` with a single
    /// colour parses to, and what a border with no gradient is drawn in.
    #[must_use]
    pub const fn solid(color: Color) -> Self {
        Self {
            colors: [color; MAX_GRADIENT_COLORS],
            len: 1,
            degrees: 0,
        }
    }

    /// The first [`MAX_GRADIENT_COLORS`] of `colors` at `degrees`, which is
    /// the angle as it was written: `45deg` is `45`.
    ///
    /// No colours at all is one transparent colour, which draws nothing, as
    /// `gradient.glsl` answers `vec4(0.0)` for an empty gradient.
    /// `compositor/config` refuses to parse one, so this is the shape of a
    /// gradient that cannot be drawn rather than a case anybody meets.
    #[must_use]
    pub fn new(colors: &[Color], degrees: i32) -> Self {
        let mut gradient = Self::solid(Color(0));
        gradient.degrees = degrees;
        gradient.len = colors.len().clamp(1, MAX_GRADIENT_COLORS);
        for (slot, &color) in gradient.colors.iter_mut().zip(colors) {
            *slot = color;
        }
        gradient
    }

    /// The colours, in the order they were written. Never empty.
    #[must_use]
    pub fn colors(&self) -> &[Color] {
        self.colors.get(..self.len).unwrap_or(&self.colors)
    }

    /// The angle as it was written, in whole degrees.
    #[must_use]
    pub const fn angle_degrees(&self) -> i32 {
        self.degrees
    }

    /// The first colour, which is the whole of a gradient that has one.
    #[must_use]
    pub fn first(&self) -> Color {
        self.colors.first().copied().unwrap_or(Color(0))
    }

    /// Whether this is a single colour, which is drawn as a flat fill.
    #[must_use]
    pub const fn is_solid(&self) -> bool {
        self.len < 2
    }

    /// Where in the gradient the point at `(x, y)` of the border's box is,
    /// from 0 at the first colour to 1 at the last.
    ///
    /// `x` and `y` are the shader's `v_texcoord`: the position inside the
    /// box the border is drawn around, each from 0 to 1, with `y` growing
    /// downwards.
    ///
    /// One point at a time, which costs a fold and a sine each; a fill of a
    /// whole window works [`Gradient::axis`] out once and asks that.
    #[must_use]
    pub fn progress(&self, x: f32, y: f32) -> f32 {
        self.axis().at(x, y)
    }

    /// The folds and the sine that turn a point of the box into a progress
    /// along the gradient, worked out once for a whole fill.
    ///
    /// `getOkColorForCoordArray1`'s own arithmetic, including the folds
    /// that bring an angle past 90 degrees back into the first quadrant and
    /// the constants it folds with -- [`TURN`] rather than exact fractions
    /// of pi, because those are what decide the branch at exactly 90, 180
    /// and 270 degrees.
    pub(crate) fn axis(&self) -> Axis {
        // `renderBorder` takes the angle back to whole degrees, wraps it at
        // 360 and turns it into radians again; a negative angle stays
        // negative, as C++'s remainder leaves it.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an angle in radians, which f32 holds to far more than a shader needs"
        )]
        let angle = (f64::from(self.degrees % 360) * core::f64::consts::PI / 180.0) as f32;
        let [quarter, half, three_quarters, whole] = TURN;
        let (flip_x, flip_y, folded) = if angle > three_quarters {
            (false, true, whole - angle)
        } else if angle > half {
            (true, true, angle - half)
        } else if angle > quarter {
            (true, false, half - angle)
        } else {
            (false, false, angle)
        };
        Axis {
            sine: folded.sin(),
            flip_x,
            flip_y,
        }
    }

    /// The gradient sampled evenly from its first colour to its last, as
    /// the colours a fill writes.
    ///
    /// One entry for a gradient of one colour. Otherwise
    /// [`SAMPLES_PER_SPAN`] a span plus the last colour, so
    /// [`Gradient::progress`] scaled by the last index reads it directly.
    pub(crate) fn ramp(&self) -> Vec<Color> {
        let colors = self.colors();
        let spans = colors.len().saturating_sub(1);
        if spans == 0 {
            return vec![self.first()];
        }
        let stops: Vec<OkLab> = colors.iter().map(|&color| OkLab::of(color)).collect();
        let steps = spans.saturating_mul(SAMPLES_PER_SPAN);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a few thousand steps and at most ten spans, both exact in f64"
        )]
        let (last, spans) = (steps as f64, spans as f64);
        (0..=steps)
            .map(|step| {
                #[expect(clippy::cast_precision_loss, reason = "as above")]
                let at = step as f64 / last * spans;
                mix(&stops, at).to_srgb()
            })
            .collect()
    }
}

/// Two gradients are the same when they draw the same thing: the same
/// colours in the same order at the same angle.
///
/// By hand rather than derived, because the array behind the colours is
/// always ten long and what is past the last colour is never drawn: a
/// derived comparison would call two gradients that draw one identical
/// border different because one of them was built from a longer list.
impl PartialEq for Gradient {
    fn eq(&self, other: &Self) -> bool {
        self.degrees == other.degrees && self.colors() == other.colors()
    }
}

impl Eq for Gradient {}

impl From<&compositor_config::Gradient> for Gradient {
    fn from(gradient: &compositor_config::Gradient) -> Self {
        Self::new(&gradient.colors, gradient.angle_degrees)
    }
}

/// How a gradient turns a point of its box into a progress along itself.
///
/// The sine of the angle once it is folded into the first quadrant, and
/// which axes that fold turned round. Held apart from [`Gradient`] because
/// a fill works it out once and then asks it at every pixel it writes,
/// where the fold and the sine would otherwise be per-pixel arithmetic.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Axis {
    sine: f32,
    flip_x: bool,
    flip_y: bool,
}

impl Axis {
    /// The sine, and whether each axis is flipped: what a shader is handed.
    pub(crate) const fn parts(&self) -> (f32, bool, bool) {
        (self.sine, self.flip_x, self.flip_y)
    }

    /// The progress at `(x, y)`, 0 at the first colour and 1 at the last:
    /// `y·sin + x·(1 − sin)` of the folded coordinate.
    ///
    /// Hyprland scales this by `gradientLength - 1` and indexes the array of
    /// stops with it. An angle the folds leave negative can put that outside
    /// the array, which the shader reads anyway; clamping is the colour
    /// every implementation that checks would give.
    pub(crate) fn at(&self, x: f32, y: f32) -> f32 {
        let x = if self.flip_x { 1.0 - x } else { x };
        let y = if self.flip_y { 1.0 - y } else { y };
        (y * self.sine + x * (1.0 - self.sine)).clamp(0.0, 1.0)
    }
}

/// A colour on its way through the gradient: `OkLab`'s three, and the alpha
/// that rides beside them.
///
/// The alpha is the colour's own, untouched: `updateColorsOk` puts it in the
/// fourth slot of the vector the shader mixes, so it is interpolated
/// linearly while the other three go through `OkLab`.
#[derive(Clone, Copy, Debug)]
struct OkLab {
    l: f64,
    a: f64,
    b: f64,
    alpha: f64,
}

impl OkLab {
    /// `CHyprColor::asOkLab`, which is hyprgraphics' `CColor::asOkLab`.
    fn of(color: Color) -> Self {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            // The sRGB transfer function, which is what goes *in*. What
            // comes out is a gamma of 2.2; the module comment says why the
            // two do not cancel.
            if value >= 0.04045 {
                ((value + 0.055) / 1.055).powf(2.4)
            } else {
                value / 12.92
            }
        };
        let (r, g, b) = (
            channel(color.red()),
            channel(color.green()),
            channel(color.blue()),
        );
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        Self {
            l: 0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
            a: 1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
            b: 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
            alpha: f64::from(color.alpha()) / 255.0,
        }
    }

    /// `okLabAToSrgb` from `gradient.glsl`: back to LMS, to linear RGB, and
    /// out through a gamma of 2.2.
    fn to_srgb(self) -> Color {
        let l = (self.l + self.a * 0.396_337_777_4 + self.b * 0.215_803_757_3).powi(3);
        let m = (self.l - self.a * 0.105_561_345_8 - self.b * 0.063_854_172_8).powi(3);
        let s = (self.l - self.a * 0.089_484_177_5 - self.b * 1.291_485_548_0).powi(3);
        let byte = |value: f64| {
            let encoded = value.max(0.0).powf(1.0 / 2.2);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a channel clamped between zero and one becomes a byte"
            )]
            let byte = (encoded.clamp(0.0, 1.0) * 255.0).round() as u32;
            byte
        };
        let red = byte(l * 4.076_741_662_1 - m * 3.307_711_591_3 + s * 0.230_969_929_2);
        let green = byte(-l * 1.268_438_004_6 + m * 2.609_757_401_1 - s * 0.341_319_396_5);
        let blue = byte(-l * 0.004_196_086_3 - m * 0.703_418_614_7 + s * 1.707_614_701_0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "an alpha between zero and one becomes a byte"
        )]
        let alpha = (self.alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
        Color((alpha << 24) | (red << 16) | (green << 8) | blue)
    }
}

/// The colour `at` along `stops`, counted in whole stops: 1.5 is halfway
/// between the second colour and the third.
///
/// `getOkColorForCoordArray1`'s own last two lines: the stop below and the
/// stop above, each weighted by how far between them the point is.
fn mix(stops: &[OkLab], at: f64) -> OkLab {
    let transparent = OkLab {
        l: 0.0,
        a: 0.0,
        b: 0.0,
        alpha: 0.0,
    };
    let last = stops.len().saturating_sub(1);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to zero and to the number of stops, which is at most ten"
    )]
    let bottom = (at.max(0.0).floor() as usize).min(last);
    #[expect(
        clippy::cast_precision_loss,
        reason = "at most ten stops, exact in f64"
    )]
    let fraction = (at - bottom as f64).clamp(0.0, 1.0);
    let (Some(below), Some(above)) = (stops.get(bottom), stops.get(bottom.saturating_add(1)))
    else {
        return stops.last().copied().unwrap_or(transparent);
    };
    let between = |one: f64, other: f64| one * (1.0 - fraction) + other * fraction;
    OkLab {
        l: between(below.l, above.l),
        a: between(below.a, above.a),
        b: between(below.b, above.b),
        alpha: between(below.alpha, above.alpha),
    }
}
