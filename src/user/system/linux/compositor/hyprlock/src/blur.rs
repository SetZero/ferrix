//! `CRenderer::blurFB`: the dual-Kawase blur hyprlock blurs a background
//! and a shadow with, on the CPU, over premultiplied RGBA.
//!
//! The shaders are Hyprland's (`blurprepare`, `blur1`, `blur2`,
//! `blurfinish` in `src/renderer/Shaders.hpp`): contrast and brightening,
//! `passes` five-tap downsamples each ending in the vibrancy boost, as many
//! eight-tap upsamples, then noise, darkening, the alpha boost and, for a
//! shadow, the colour. `src/user/system/linux/compositor/render` has the same blur over the
//! compositor's opaque frame; this one keeps alpha, which a shadow is made
//! of and an opaque canvas has none of.
//!
//! The kernels, in the pixels of the level being read, with `r` the size:
//!
//! ```text
//! down(x) = (4·t(2x+1) + t(2x+1 ± (r, r)) + t(2x+1 ± (r, −r))) / 8
//! up(x)   = Σ of t((x+½)/2 + o) over o = (±r/2, 0), (0, ±r/2) once
//!           and (±r/4, ±r/4) twice, / 12
//! ```
//!
//! sampled bilinearly with the edges clamped, as `GL_LINEAR` and
//! `GL_CLAMP_TO_EDGE` sample.

/// One pixel, premultiplied, each channel 0 to 1.
pub type Pixel = [f32; 4];

/// What `SBlurParams` carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// `size`: how far each tap reaches.
    pub size: f32,
    /// `passes`: how many levels down and up.
    pub passes: u32,
    /// `noise`.
    pub noise: f32,
    /// `contrast`; exactly 1 is none.
    pub contrast: f32,
    /// `brightness`; above 1 brightens first, below 1 darkens last.
    pub brightness: f32,
    /// `vibrancy`.
    pub vibrancy: f32,
    /// `vibrancy_darkness`.
    pub vibrancy_darkness: f32,
    /// `colorize`: the shadow's colour, which replaces every colour.
    pub colorize: Option<[f32; 3]>,
    /// `boostA`: what alpha is multiplied by at the end.
    pub boost_alpha: f32,
}

impl Params {
    /// How far a blur can carry a pixel, in pixels: what a region must be
    /// grown by for its edge not to be cut.
    #[must_use]
    pub fn reach(&self) -> usize {
        let levels = 1usize << self.passes.min(16);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a blur size is a small positive number of pixels"
        )]
        let size = self.size.max(0.0).ceil() as usize;
        (size + 2).saturating_mul(levels).saturating_mul(2)
    }
}

/// An image being blurred.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// Rows, top first.
    pub pixels: Vec<Pixel>,
}

impl Plane {
    /// A clear plane.
    #[must_use]
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![[0.0; 4]; width.saturating_mul(height)],
        }
    }

    fn at(&self, x: usize, y: usize) -> Pixel {
        self.pixels
            .get(y.saturating_mul(self.width).saturating_add(x))
            .copied()
            .unwrap_or([0.0; 4])
    }

    /// Bilinear at `(x, y)` in pixels, centres at half-integers, the edges
    /// clamped.
    fn sample(&self, x: f32, y: f32) -> Pixel {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a plane's size is far inside f32's exact range"
        )]
        let (max_x, max_y) = (
            self.width.saturating_sub(1) as f32,
            self.height.saturating_sub(1) as f32,
        );
        let (x, y) = ((x - 0.5).clamp(0.0, max_x), (y - 0.5).clamp(0.0, max_y));
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to the plane above"
        )]
        let (x0, y0) = (x0 as usize, y0 as usize);
        let (x1, y1) = (
            (x0 + 1).min(self.width.saturating_sub(1)),
            (y0 + 1).min(self.height.saturating_sub(1)),
        );
        let (a, b, c, d) = (
            self.at(x0, y0),
            self.at(x1, y0),
            self.at(x0, y1),
            self.at(x1, y1),
        );
        let mut out = [0.0; 4];
        for (channel, value) in out.iter_mut().enumerate() {
            let pick = |p: Pixel| p.get(channel).copied().unwrap_or(0.0);
            let top = pick(a) + (pick(b) - pick(a)) * fx;
            let bottom = pick(c) + (pick(d) - pick(c)) * fx;
            *value = top + (bottom - top) * fy;
        }
        out
    }
}

/// `blurprepare`'s `gain`.
fn gain(x: f32, k: f32) -> f32 {
    let a = 0.5 * (2.0 * if x < 0.5 { x } else { 1.0 - x }).powf(k);
    if x < 0.5 { a } else { 1.0 - a }
}

/// `blurFinish`'s `hash`, of the pixel's texture coordinate.
fn hash(u: f32, v: f32) -> f32 {
    let value = (u * 12.9898 + v * 78.233).sin() * 43758.547;
    value - value.floor()
}

fn add(sum: &mut Pixel, pixel: Pixel, weight: f32) {
    for (total, value) in sum.iter_mut().zip(pixel) {
        *total += value * weight;
    }
}

fn down(from: &Plane, radius: f32) -> Plane {
    let mut to = Plane::new(
        from.width.div_ceil(2).max(1),
        from.height.div_ceil(2).max(1),
    );
    let width = to.width;
    for (at, out) in to.pixels.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a pixel index, far inside f32's exact range"
        )]
        let (x, y) = (((at % width) * 2 + 1) as f32, ((at / width) * 2 + 1) as f32);
        let mut sum = [0.0; 4];
        add(&mut sum, from.sample(x, y), 4.0);
        for (dx, dy) in [(-1.0, -1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, 1.0)] {
            add(&mut sum, from.sample(x + dx * radius, y + dy * radius), 1.0);
        }
        *out = sum.map(|value| value / 8.0);
    }
    to
}

fn up(from: &Plane, width: usize, height: usize, radius: f32) -> Plane {
    let mut to = Plane::new(width, height);
    const TAPS: [(f32, f32, f32); 8] = [
        (-0.5, 0.0, 1.0),
        (-0.25, 0.25, 2.0),
        (0.0, 0.5, 1.0),
        (0.25, 0.25, 2.0),
        (0.5, 0.0, 1.0),
        (0.25, -0.25, 2.0),
        (0.0, -0.5, 1.0),
        (-0.25, -0.25, 2.0),
    ];
    for (at, out) in to.pixels.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a pixel index, far inside f32's exact range"
        )]
        let (x, y) = (
            ((at % width) as f32 + 0.5) / 2.0,
            ((at / width) as f32 + 0.5) / 2.0,
        );
        let mut sum = [0.0; 4];
        for (dx, dy, weight) in TAPS {
            add(
                &mut sum,
                from.sample(x + dx * radius, y + dy * radius),
                weight,
            );
        }
        *out = sum.map(|value| value / 12.0);
    }
    to
}

/// `blur1`'s vibrancy boost of one pixel.
fn vibrant(pixel: Pixel, params: &Params) -> Pixel {
    const PR: f32 = 0.299;
    const PG: f32 = 0.587;
    const PB: f32 = 0.114;
    const A: f32 = 0.93;
    const B: f32 = 0.11;
    const C: f32 = 0.66;
    let [r, g, b, alpha] = pixel;
    let (hue, saturation, lightness) = rgb_to_hsl(r, g, b);
    let darkness = 1.0 - params.vibrancy_darkness;
    let perceived = circle_sigmoid(
        (r * r * PR + g * g * PG + b * b * PB).sqrt(),
        0.8 * darkness,
    );
    let b1 = B * darkness;
    let boost = if saturation > 0.0 {
        let x = 1.0 - ((1.0 - saturation * A.cos()).powi(2) + (1.0 - perceived * A.sin()).powi(2));
        smoothstep(b1 - C * 0.5, b1 + C * 0.5, x)
    } else {
        0.0
    };
    #[expect(clippy::cast_precision_loss, reason = "a pass count")]
    let passes = params.passes.max(1) as f32;
    let saturation = (saturation + boost * params.vibrancy / passes).clamp(0.0, 1.0);
    let [r, g, b] = hsl_to_rgb(hue, saturation, lightness);
    [r, g, b, alpha]
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn circle_sigmoid(x: f32, a: f32) -> f32 {
    let a = a.clamp(0.0, 1.0);
    if x <= a {
        a - (a * a - x * x).max(0.0).sqrt()
    } else {
        a + ((1.0 - a).powi(2) - (x - 1.0).powi(2)).max(0.0).sqrt()
    }
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (min, max) = (r.min(g).min(b), r.max(g).max(b));
    let delta = max - min;
    let lightness = (min + max) * 0.5;
    let saturation = if lightness > 0.0 && lightness < 1.0 {
        delta
            / (2.0
                * if lightness < 0.5 {
                    lightness
                } else {
                    1.0 - lightness
                })
    } else {
        0.0
    };
    let mut hue = 0.0;
    if delta > 0.0 {
        #[expect(
            clippy::float_cmp,
            reason = "the shader's `equal`: which channel is the maximum"
        )]
        let is = |channel: f32| channel == max;
        hue = if is(r) && max != g {
            (g - b) / delta
        } else if is(g) && max != b {
            2.0 + (b - r) / delta
        } else {
            4.0 + (r - g) / delta
        } / 6.0;
        if hue < 0.0 {
            hue += 1.0;
        }
    }
    (hue, saturation, lightness)
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let (third, two_thirds) = (1.0 / 3.0, 2.0 / 3.0);
    let xt = if hue < third {
        [6.0 * (third - hue), 6.0 * hue, 0.0]
    } else if hue < two_thirds {
        [0.0, 6.0 * (two_thirds - hue), 6.0 * (hue - third)]
    } else {
        [6.0 * (hue - two_thirds), 0.0, 6.0 * (1.0 - hue)]
    }
    .map(|value: f32| value.min(1.0));
    xt.map(|value| {
        let ct = 2.0 * saturation * value + (1.0 - saturation);
        if lightness >= 0.5 {
            (1.0 - lightness) * ct + (2.0 * lightness - 1.0)
        } else {
            lightness * ct
        }
    })
}

/// Blur `plane` in place.
pub fn blur(plane: &mut Plane, params: &Params) {
    if params.passes == 0 || plane.width == 0 || plane.height == 0 {
        return;
    }
    // blurprepare.
    for pixel in &mut plane.pixels {
        if (params.contrast - 1.0).abs() > f32::EPSILON {
            for channel in pixel.iter_mut().take(3) {
                *channel = gain(*channel, params.contrast);
            }
        }
        if params.brightness > 1.0 {
            for channel in pixel.iter_mut().take(3) {
                *channel *= params.brightness;
            }
        }
    }
    let mut levels = vec![std::mem::replace(plane, Plane::new(0, 0))];
    for _ in 0..params.passes {
        let Some(last) = levels.last() else {
            break;
        };
        let mut next = down(last, params.size);
        if params.vibrancy != 0.0 {
            for pixel in &mut next.pixels {
                *pixel = vibrant(*pixel, params);
            }
        }
        levels.push(next);
    }
    let mut current = levels.pop().unwrap_or_else(|| Plane::new(0, 0));
    while let Some(target) = levels.pop() {
        current = up(&current, target.width, target.height, params.size);
    }
    // blurfinish.
    let (width, height) = (current.width, current.height);
    for (at, pixel) in current.pixels.iter_mut().enumerate() {
        if params.noise != 0.0 {
            #[expect(clippy::cast_precision_loss, reason = "a pixel index")]
            let (u, v) = (
                ((at % width) as f32 + 0.5) / width as f32,
                ((at / width) as f32 + 0.5) / height as f32,
            );
            let amount = hash(u, v) - 0.5;
            for channel in pixel.iter_mut().take(3) {
                *channel += amount * params.noise;
            }
        }
        if params.brightness < 1.0 {
            for channel in pixel.iter_mut().take(3) {
                *channel *= params.brightness;
            }
        }
        pixel[3] = (pixel[3] * params.boost_alpha).clamp(0.0, 1.0);
        if let Some(tint) = params.colorize {
            let alpha = pixel[3];
            *pixel = [tint[0] * alpha, tint[1] * alpha, tint[2] * alpha, alpha];
        }
        for channel in pixel.iter_mut() {
            *channel = channel.clamp(0.0, 1.0);
        }
    }
    *plane = current;
}

#[cfg(test)]
mod tests {
    use super::{Params, Plane, blur};

    fn shadow(size: f32, passes: u32) -> Params {
        Params {
            size,
            passes,
            noise: 0.0,
            contrast: 0.0,
            brightness: 0.0,
            vibrancy: 0.0,
            vibrancy_darkness: 0.0,
            colorize: Some([0.0, 0.0, 0.0]),
            boost_alpha: 1.2,
        }
    }

    #[test]
    fn a_dot_spreads_into_a_soft_black_shadow() {
        let mut plane = Plane::new(64, 64);
        for y in 28..36 {
            for x in 28..36 {
                plane.pixels[y * 64 + x] = [1.0, 1.0, 1.0, 1.0];
            }
        }
        blur(&mut plane, &shadow(4.0, 2));
        assert_eq!((plane.width, plane.height), (64, 64));
        let centre = plane.pixels[32 * 64 + 32];
        let edge = plane.pixels[32 * 64 + 20];
        let far = plane.pixels[2 * 64 + 2];
        // Colourised: every colour is the tint (black) times alpha.
        assert!(centre[0].abs() < 1e-6);
        assert!(centre[3] > edge[3], "{centre:?} {edge:?}");
        assert!(edge[3] > 0.0);
        assert!(far[3] < 1e-3);
        // Still symmetric about the centre.
        let left = plane.pixels[32 * 64 + 25][3];
        let right = plane.pixels[32 * 64 + 38][3];
        assert!((left - right).abs() < 0.05, "{left} {right}");
    }

    #[test]
    fn no_passes_is_no_blur() {
        let mut plane = Plane::new(4, 4);
        plane.pixels[5] = [0.5, 0.5, 0.5, 0.5];
        let before = plane.clone();
        blur(&mut plane, &shadow(3.0, 0));
        assert_eq!(plane, before);
    }

    #[test]
    fn a_flat_colour_stays_flat_through_an_ungraded_blur() {
        let mut plane = Plane::new(32, 16);
        for pixel in &mut plane.pixels {
            *pixel = [0.2, 0.4, 0.6, 1.0];
        }
        blur(
            &mut plane,
            &Params {
                contrast: 1.0,
                brightness: 1.0,
                colorize: None,
                boost_alpha: 1.0,
                ..shadow(8.0, 3)
            },
        );
        for pixel in &plane.pixels {
            assert!((pixel[1] - 0.4).abs() < 1e-4, "{pixel:?}");
        }
    }
}
