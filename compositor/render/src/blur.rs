//! Dual-Kawase blur, on the CPU.
//!
//! Hyprland blurs what is *behind* a translucent window: the frame so far is
//! taken, blurred, and written back under the window before the window is
//! drawn over it. The blur itself is two shaders, `blur1.glsl`'s downsample
//! and `blur2.glsl`'s upsample, run `decoration:blur:passes` times down and
//! the same number back up, with `decoration:blur:size` as the radius.
//!
//! This is those two kernels, exactly, in software:
//!
//! ```text
//! down(uv) = (4·t(2uv) + t(2uv − h·r) + t(2uv + h·r)
//!             + t(2uv + (hx, −hy)·r) + t(2uv − (hx, −hy)·r)) / 8
//!
//! up(uv)   = ( t(uv/2 + (−2hx, 0)·r) + 2·t(uv/2 + (−hx,  hy)·r)
//!            + t(uv/2 + (0,  2hy)·r) + 2·t(uv/2 + ( hx,  hy)·r)
//!            + t(uv/2 + ( 2hx, 0)·r) + 2·t(uv/2 + ( hx, −hy)·r)
//!            + t(uv/2 + (0, −2hy)·r) + 2·t(uv/2 + (−hx, −hy)·r) ) / 12
//! ```
//!
//! with `h` the half-pixel of the buffer being written and `r` the radius.
//! Sampling is bilinear with the edges clamped, which is what
//! `GL_CLAMP_TO_EDGE` and `GL_LINEAR` give the shader.
//!
//! # What it costs
//!
//! Each pass halves the size, so the whole chain touches at most twice the
//! source's pixels going down and twice going up: about `4 × w × h` samples
//! of five and eight taps. For a 485×726 window at one pass that is around
//! 1.4 million taps, which is single-digit milliseconds on one core --
//! the software fallback `docs/ROADMAP.md` stage 19 asks each effect to
//! have, with a bound that is stated rather than hoped for.
//!
//! # What is not here
//!
//! `blur:noise`, `contrast`, `brightness`, `vibrancy` and
//! `vibrancy_darkness`, which are the colour grading `blur1.glsl` does after
//! its five taps. They change the colour of the blur and not its shape, and
//! a compositor that does the shape and not the grading is one whose blur is
//! flatter than Hyprland's rather than one whose blur is wrong.

/// A buffer of premultiplied `RGBA` floats, as the blur works on it.
///
/// Floats, not bytes: the chain writes and reads its own output up to twenty
/// times, and rounding to a byte each time loses the low bits that make a
/// blur smooth rather than banded.
#[derive(Clone, Debug)]
struct Plane {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 4]>,
}

impl Plane {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            pixels: vec![[0.0; 4]; width.max(1) * height.max(1)],
        }
    }

    /// One pixel, with the edges clamped: what `GL_CLAMP_TO_EDGE` gives.
    fn at(&self, x: isize, y: isize) -> [f32; 4] {
        let x = x
            .clamp(0, self.width.saturating_sub(1) as isize)
            .unsigned_abs();
        let y = y
            .clamp(0, self.height.saturating_sub(1) as isize)
            .unsigned_abs();
        self.pixels
            .get(y.saturating_mul(self.width).saturating_add(x))
            .copied()
            .unwrap_or([0.0; 4])
    }

    /// A sample at a fractional position, bilinear, as `GL_LINEAR` gives it.
    fn sample(&self, x: f32, y: f32) -> [f32; 4] {
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a position inside a buffer, floored; `at` clamps it"
        )]
        let (ix, iy) = (x0 as isize, y0 as isize);
        let (a, b, c, d) = (
            self.at(ix, iy),
            self.at(ix + 1, iy),
            self.at(ix, iy + 1),
            self.at(ix + 1, iy + 1),
        );
        let mut out = [0.0; 4];
        for (channel, value) in out.iter_mut().enumerate() {
            let take = |from: &[f32; 4]| from.get(channel).copied().unwrap_or(0.0);
            let top = take(&a) + (take(&b) - take(&a)) * tx;
            let bottom = take(&c) + (take(&d) - take(&c)) * tx;
            *value = top + (bottom - top) * ty;
        }
        out
    }

    fn set(&mut self, x: usize, y: usize, value: [f32; 4]) {
        if let Some(slot) = self
            .pixels
            .get_mut(y.saturating_mul(self.width).saturating_add(x))
        {
            *slot = value;
        }
    }
}

/// `blur1.glsl`'s five taps: read `from` at twice this buffer's scale.
///
/// In pixels rather than in the shader's normalised coordinates, which is
/// the same kernel written the way a software renderer can check it: the
/// target pixel `(x, y)` reads the source around `(2x + 1, 2y + 1)`, its own
/// centre, with four taps a `radius` away on the diagonals.
fn down(from: &Plane, radius: f32) -> Plane {
    let mut to = Plane::new(from.width.div_ceil(2), from.height.div_ceil(2));
    for y in 0..to.height {
        for x in 0..to.width {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a position inside a buffer at most 4096 wide"
            )]
            let (sx, sy) = ((x as f32 + 0.5) * 2.0, (y as f32 + 0.5) * 2.0);
            let taps: [(f32, f32, f32); 5] = [
                (0.0, 0.0, 4.0),
                (-radius, -radius, 1.0),
                (radius, radius, 1.0),
                (radius, -radius, 1.0),
                (-radius, radius, 1.0),
            ];
            to.set(x, y, weighted(from, sx, sy, &taps, 8.0));
        }
    }
    to
}

/// `blur2.glsl`'s eight taps: read `from` at half this buffer's scale.
fn up(from: &Plane, width: usize, height: usize, radius: f32) -> Plane {
    let mut to = Plane::new(width, height);
    // The two passes do not use the same offsets. `blur1`'s `halfpixel` is
    // `0.5 / (size / 2)` and `blur2`'s is `0.5 / (size * 2)`, both of the
    // monitor's size: four times smaller. In the sampled plane's pixels that
    // is `radius` going down and a quarter of it coming back up, and the
    // ratio is what makes the eight taps a reconstruction rather than a
    // ring -- a ring of that width turns a bright block into a dark hole
    // with a bright halo, which is what this looked like when both were
    // `radius`.
    let r = radius / 4.0;
    for y in 0..to.height {
        for x in 0..to.width {
            #[expect(clippy::cast_precision_loss, reason = "as in `down`")]
            let (sx, sy) = ((x as f32 + 0.5) / 2.0, (y as f32 + 0.5) / 2.0);
            let taps: [(f32, f32, f32); 8] = [
                (-2.0 * r, 0.0, 1.0),
                (-r, r, 2.0),
                (0.0, 2.0 * r, 1.0),
                (r, r, 2.0),
                (2.0 * r, 0.0, 1.0),
                (r, -r, 2.0),
                (0.0, -2.0 * r, 1.0),
                (-r, -r, 2.0),
            ];
            to.set(x, y, weighted(from, sx, sy, &taps, 12.0));
        }
    }
    to
}

/// The weighted sum of `taps` around `(x, y)` in `from`, over `total`.
fn weighted(from: &Plane, x: f32, y: f32, taps: &[(f32, f32, f32)], total: f32) -> [f32; 4] {
    let mut sum = [0.0_f32; 4];
    for &(dx, dy, weight) in taps {
        let sample = from.sample(x + dx, y + dy);
        for (channel, value) in sum.iter_mut().enumerate() {
            *value += sample.get(channel).copied().unwrap_or(0.0) * weight;
        }
    }
    for value in &mut sum {
        *value /= total;
    }
    sum
}

/// Blur `pixels`, a tight `width × height` block of premultiplied `RGBA`
/// bytes, in place.
///
/// `passes` is `decoration:blur:passes` and `size` is
/// `decoration:blur:size`. Zero of either leaves the block alone, which is
/// what Hyprland's own zero does.
pub(crate) fn blur(pixels: &mut [u8], width: usize, height: usize, size: i64, passes: u32) {
    if passes == 0 || size <= 0 || width == 0 || height == 0 {
        return;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a blur size is at most 100, which `f32` holds exactly"
    )]
    let radius = size as f32;

    let mut plane = Plane::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 4;
            let Some(bytes) = pixels.get(at..at + 4) else {
                continue;
            };
            let byte = |at: usize| f32::from(bytes.get(at).copied().unwrap_or(0)) / 255.0;
            plane.set(x, y, [byte(0), byte(1), byte(2), byte(3)]);
        }
    }

    // Down, keeping each level's size so the way back up lands on it.
    let mut sizes = Vec::with_capacity(passes as usize);
    for _ in 0..passes {
        sizes.push((plane.width, plane.height));
        plane = down(&plane, radius);
    }
    // And back up, in reverse.
    for (wide, tall) in sizes.into_iter().rev() {
        plane = up(&plane, wide, tall, radius);
    }

    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 4;
            let value = plane.at(x as isize, y as isize);
            let Some(bytes) = pixels.get_mut(at..at + 4) else {
                continue;
            };
            for (channel, slot) in bytes.iter_mut().enumerate() {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a channel between zero and one becomes a byte"
                )]
                let byte = (value.get(channel).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0)
                    .round() as u8;
                *slot = byte;
            }
        }
    }
}

#[cfg(test)]
mod tests;
