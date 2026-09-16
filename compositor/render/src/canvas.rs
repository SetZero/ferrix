//! The frame being drawn: a tiny-skia pixmap in the canvas byte order the
//! crate docs describe, and the drawing operations clipped to damage.

use tiny_skia::{BlendMode, FilterQuality, Paint, Pattern, Pixmap, PixmapRef, Shader, SpreadMode};

use crate::damage::{intersect, is_empty};
use crate::{Color, Damage, Error, Format, Rect, Surface, Target};

/// The largest width or height a canvas, surface or target may have: past
/// a 16K output, and small enough that every coordinate is exact in the
/// `f32` tiny-skia draws with.
pub const MAX_SIZE: u32 = 16384;

/// A frame being drawn, the size of the output.
///
/// It starts opaque black and stays opaque: clearing ignores alpha, and
/// every other operation blends over what is there. Each operation draws
/// only inside the [`Damage`] it is given and adds what it wrote to the
/// canvas's own record, which [`Canvas::take_damage`] hands over.
#[derive(Debug, Clone)]
pub struct Canvas {
    pixmap: Pixmap,
    damage: Damage,
}

/// tiny-skia's colour for `color`, with red and blue swapped into canvas
/// order.
fn skia_color(color: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(color.blue(), color.green(), color.red(), color.alpha())
}

/// A paint with no anti-aliasing, on the `f32` pipeline.
fn paint(shader: Shader<'_>, blend_mode: BlendMode) -> Paint<'_> {
    Paint {
        shader,
        blend_mode,
        anti_alias: false,
        force_hq_pipeline: true,
        ..Paint::default()
    }
}

/// tiny-skia's rectangle for `rect`, which lies inside a canvas, so every
/// edge is exact.
fn skia_rect(rect: Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_xywh(
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
    )
}

/// `value`, known to be inside a canvas, as an index.
fn index(value: i64) -> usize {
    usize::try_from(value).unwrap_or(0)
}

impl Canvas {
    /// An opaque black `width` × `height` canvas.
    ///
    /// # Errors
    ///
    /// [`Error::Size`] for a size of zero or over [`MAX_SIZE`].
    pub fn new(width: u32, height: u32) -> Result<Self, Error> {
        let size = Error::Size { width, height };
        if width == 0 || height == 0 || width > MAX_SIZE || height > MAX_SIZE {
            return Err(size);
        }
        let mut pixmap = Pixmap::new(width, height).ok_or(size)?;
        pixmap.fill(tiny_skia::Color::BLACK);
        Ok(Self {
            pixmap,
            damage: Damage::new(),
        })
    }

    /// The width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    /// The height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    /// The canvas as a rectangle at the origin.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, i64::from(self.width()), i64::from(self.height()))
    }

    /// The frame as `XRGB8888` bytes, four a pixel with no padding: blue,
    /// green, red and an X byte of `0xFF`.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        self.pixmap.data()
    }

    /// Pixel (`x`, `y`) as an `XRGB8888` value, if it is on the canvas.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width() || y >= self.height() {
            return None;
        }
        let start = usize::try_from(u64::from(y) * u64::from(self.width()) + u64::from(x)).ok()?;
        let bytes = self.data().get(start.checked_mul(4)?..)?.first_chunk()?;
        Some(u32::from_le_bytes(*bytes))
    }

    /// What the operations since the last call wrote.
    #[must_use]
    pub const fn damage(&self) -> &Damage {
        &self.damage
    }

    /// What the operations since the last call wrote, leaving the record
    /// empty.
    pub fn take_damage(&mut self) -> Damage {
        core::mem::take(&mut self.damage)
    }

    /// The parts of `rect` inside both `damage` and the canvas, disjoint.
    fn clips(&self, rect: Rect, damage: &Damage) -> Vec<Rect> {
        let bounds = self.bounds();
        match intersect(rect, bounds) {
            Some(rect) => damage
                .rects()
                .iter()
                .filter_map(|&damaged| intersect(rect, damaged))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Fill each of `clips` with `paint` and record it.
    fn fill_clips(&mut self, clips: &[Rect], paint: &Paint<'_>) {
        for &clip in clips {
            if let Some(rect) = skia_rect(clip) {
                self.pixmap
                    .fill_rect(rect, paint, tiny_skia::Transform::identity(), None);
                self.damage.add(clip);
            }
        }
    }

    /// Set every pixel in `damage` to `color`, whose alpha is ignored: the
    /// output has none.
    pub fn clear(&mut self, color: Color, damage: &Damage) {
        let opaque = Color(color.0 | 0xFF00_0000);
        let clips = self.clips(self.bounds(), damage);
        let paint = paint(Shader::SolidColor(skia_color(opaque)), BlendMode::Source);
        self.fill_clips(&clips, &paint);
    }

    /// Draw `color`, which is not premultiplied (`0xAARRGGBB` as Hyprland
    /// writes colours), over `rect` within `damage`. A colour with no alpha
    /// draws nothing and damages nothing.
    pub fn fill(&mut self, rect: Rect, color: Color, damage: &Damage) {
        if color.alpha() == 0 || is_empty(rect) {
            return;
        }
        let clips = self.clips(rect, damage);
        let paint = paint(Shader::SolidColor(skia_color(color)), BlendMode::SourceOver);
        self.fill_clips(&clips, &paint);
    }

    /// Draw a border `width` pixels wide in `color` around `rect`, outside
    /// it, within `damage`: Hyprland's border with no rounding, whose inner
    /// edge is the client area's edge. The four strips do not overlap, so a
    /// translucent border is blended once at the corners too.
    pub fn border(&mut self, rect: Rect, width: i64, color: Color, damage: &Damage) {
        if width <= 0 || is_empty(rect) {
            return;
        }
        let outer_x = rect.x.saturating_sub(width);
        let outer_width = rect.width.saturating_add(width.saturating_mul(2));
        let strips = [
            Rect::new(outer_x, rect.y.saturating_sub(width), outer_width, width),
            Rect::new(outer_x, rect.bottom(), outer_width, width),
            Rect::new(outer_x, rect.y, width, rect.height),
            Rect::new(rect.right(), rect.y, width, rect.height),
        ];
        for strip in strips {
            self.fill(strip, color, damage);
        }
    }

    /// Draw `surface` with its top-left pixel at `rect`'s, pixel for pixel,
    /// cropped to `rect` and to `damage`: source-over for
    /// [`Format::Argb8888`], a copy that ignores the X byte for
    /// [`Format::Xrgb8888`]. Where the surface is smaller than `rect`,
    /// nothing is drawn.
    pub fn composite(&mut self, surface: &Surface<'_>, rect: Rect, damage: &Damage) {
        let area = Rect::new(
            rect.x,
            rect.y,
            rect.width.min(i64::from(surface.width())),
            rect.height.min(i64::from(surface.height())),
        );
        if is_empty(area) {
            return;
        }
        let clips = self.clips(area, damage);
        if clips.is_empty() {
            return;
        }
        match surface.format() {
            Format::Xrgb8888 => self.copy(surface, rect, &clips),
            Format::Argb8888 => self.blend(surface, rect, &clips),
        }
    }

    /// Copy an `XRGB8888` surface drawn at `rect` into `clips`, making each
    /// pixel opaque.
    fn copy(&mut self, surface: &Surface<'_>, rect: Rect, clips: &[Rect]) {
        let width = index(i64::from(self.width()));
        for &clip in clips {
            let len = index(clip.width) * 4;
            let from = index(clip.x.saturating_sub(rect.x)) * 4;
            for y in clip.y..clip.bottom() {
                let Some(row) = u32::try_from(y.saturating_sub(rect.y))
                    .ok()
                    .and_then(|row| surface.row(row))
                else {
                    continue;
                };
                let start = (index(y) * width + index(clip.x)) * 4;
                let (Some(src), Some(dst)) = (
                    row.get(from..from + len),
                    self.pixmap.data_mut().get_mut(start..start + len),
                ) else {
                    continue;
                };
                for (dst, src) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    if let ([b, g, r, x], [sb, sg, sr, _]) = (dst, src) {
                        (*b, *g, *r, *x) = (*sb, *sg, *sr, 0xFF);
                    }
                }
            }
            self.damage.add(clip);
        }
    }

    /// Blend a premultiplied `ARGB8888` surface drawn at `rect` into
    /// `clips`, through tiny-skia's pattern shader with nearest sampling at
    /// a whole-pixel offset, so each canvas pixel takes exactly one surface
    /// pixel.
    fn blend(&mut self, surface: &Surface<'_>, rect: Rect, clips: &[Rect]) {
        // `wl_shm`'s bytes are blue, green, red, alpha: canvas order. A
        // padded buffer is gathered into tight rows first.
        let gathered: Vec<u8>;
        let bytes = if let Some(tight) = surface.tight() {
            tight
        } else {
            gathered = (0..surface.height())
                .filter_map(|y| surface.row(y))
                .flatten()
                .copied()
                .collect();
            &gathered
        };
        let Some(pixmap) = PixmapRef::from_bytes(bytes, surface.width(), surface.height()) else {
            return;
        };
        let shader = Pattern::new(
            pixmap,
            SpreadMode::Pad,
            FilterQuality::Nearest,
            1.0,
            tiny_skia::Transform::from_translate(rect.x as f32, rect.y as f32),
        );
        let paint = paint(shader, BlendMode::SourceOver);
        self.fill_clips(clips, &paint);
    }

    /// Copy the pixels in `damage` into `target`, which must be the canvas's
    /// size. Nothing outside `damage`, and none of a row's padding past its
    /// last pixel, is written.
    ///
    /// # Errors
    ///
    /// [`Error::Mismatch`] when the target is another size.
    pub fn present(&self, target: &mut Target<'_>, damage: &Damage) -> Result<(), Error> {
        if (target.width(), target.height()) != (self.width(), self.height()) {
            return Err(Error::Mismatch {
                canvas: (self.width(), self.height()),
                target: (target.width(), target.height()),
            });
        }
        let width = index(i64::from(self.width()));
        for clip in damage.clipped(self.bounds()).rects() {
            let len = index(clip.width) * 4;
            for y in clip.y..clip.bottom() {
                let start = (index(y) * width + index(clip.x)) * 4;
                let (Some(src), Some(dst)) = (
                    self.pixmap.data().get(start..start + len),
                    u32::try_from(clip.x)
                        .ok()
                        .zip(u32::try_from(y).ok())
                        .and_then(|(x, y)| target.span_mut(x, y, len)),
                ) else {
                    continue;
                };
                dst.copy_from_slice(src);
            }
        }
        Ok(())
    }
}
