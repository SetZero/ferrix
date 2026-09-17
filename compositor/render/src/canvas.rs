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
        self.composite_with(surface, rect, 0, 1.0, damage);
    }

    /// The same, with Hyprland's two window decorations applied.
    ///
    /// `rounding` is `decoration:rounding`: the corners are cut to that
    /// radius, and `opacity` is `decoration:active_opacity` and its
    /// relatives, which multiply the surface's alpha as it is drawn. An
    /// opacity of 1 and a rounding of 0 is [`Canvas::composite`] exactly,
    /// including its copy for an opaque surface.
    pub fn composite_with(
        &mut self,
        surface: &Surface<'_>,
        rect: Rect,
        rounding: i64,
        opacity: f32,
        damage: &Damage,
    ) {
        let area = Rect::new(
            rect.x,
            rect.y,
            rect.width.min(i64::from(surface.width())),
            rect.height.min(i64::from(surface.height())),
        );
        if is_empty(area) {
            return;
        }
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity == 0.0 {
            return;
        }
        let clips = if rounding > 0 {
            self.rounded_clips(area, rounding, damage)
        } else {
            self.clips(area, damage)
        };
        if clips.is_empty() {
            return;
        }
        match surface.format() {
            // The copy is the fast path and the exact one, and it is only
            // exact at full opacity: anything else has to be blended.
            Format::Xrgb8888 if opacity >= 1.0 => self.copy(surface, rect, &clips),
            _ => self.blend(surface, rect, opacity, &clips),
        }
    }

    /// Draw `surface` stretched to fill `rect`, with the same rounding and
    /// opacity [`Canvas::composite_with`] takes.
    ///
    /// For a window part-way through an animation, where the rectangle is
    /// between the size the client drew at and the size it is going to.
    /// Hyprland scales the window's texture for the same reason; the client
    /// is configured at the goal and draws once, not once a frame.
    ///
    /// The sampling is bilinear, which is the one place in this crate where
    /// a pixel is not a pixel. It is also the only place it can be: a
    /// stretched surface has no whole-pixel mapping to stretch along. A
    /// window that is not being animated goes through
    /// [`Canvas::composite_with`] and is exact, which is why every expected
    /// image in this tree still holds.
    pub fn composite_scaled(
        &mut self,
        surface: &Surface<'_>,
        rect: Rect,
        rounding: i64,
        opacity: f32,
        damage: &Damage,
    ) {
        if is_empty(rect) || surface.width() == 0 || surface.height() == 0 {
            return;
        }
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity == 0.0 {
            return;
        }
        // The same size is the exact path: an animation that has arrived
        // must draw what a still window draws, to the byte.
        if rect.width == i64::from(surface.width()) && rect.height == i64::from(surface.height()) {
            self.composite_with(surface, rect, rounding, opacity, damage);
            return;
        }
        let clips = if rounding > 0 {
            self.rounded_clips(rect, rounding, damage)
        } else {
            self.clips(rect, damage)
        };
        if clips.is_empty() {
            return;
        }
        let gathered = opaque_rows(surface);
        let Some(pixmap) = PixmapRef::from_bytes(&gathered, surface.width(), surface.height())
        else {
            return;
        };
        #[expect(
            clippy::cast_precision_loss,
            reason = "a window's size in pixels; the loss is far below one"
        )]
        let (scale_x, scale_y) = (
            rect.width as f32 / f32::from(u16::try_from(surface.width()).unwrap_or(u16::MAX)),
            rect.height as f32 / f32::from(u16::try_from(surface.height()).unwrap_or(u16::MAX)),
        );
        #[expect(
            clippy::cast_precision_loss,
            reason = "as above: a position on a screen"
        )]
        let transform = tiny_skia::Transform::from_translate(rect.x as f32, rect.y as f32)
            .pre_scale(scale_x, scale_y);
        let shader = Pattern::new(
            pixmap,
            SpreadMode::Pad,
            FilterQuality::Bilinear,
            opacity,
            transform,
        );
        let paint = paint(shader, BlendMode::SourceOver);
        self.fill_clips(&clips, &paint);
    }

    /// The parts of `rect` inside `damage` and the canvas, with its corners
    /// cut to `radius`.
    ///
    /// A row at a time: the two rows of corners each become one span, and
    /// everything between them is a single rectangle. Anti-aliasing is off
    /// here as everywhere else in this crate, so a pixel is in or out, and a
    /// row's inset is the circle's at that row's centre, rounded to the
    /// nearest pixel.
    fn rounded_clips(&self, rect: Rect, radius: i64, damage: &Damage) -> Vec<Rect> {
        let radius = radius.min(rect.width / 2).min(rect.height / 2).max(0);
        if radius == 0 {
            return self.clips(rect, damage);
        }
        let mut spans = Vec::with_capacity((radius * 2 + 1) as usize);
        for row in 0..radius {
            let inset = corner_inset(radius, row);
            let width = rect.width.saturating_sub(inset.saturating_mul(2));
            if width <= 0 {
                continue;
            }
            spans.push(Rect::new(rect.x + inset, rect.y + row, width, 1));
            spans.push(Rect::new(rect.x + inset, rect.bottom() - row - 1, width, 1));
        }
        let middle = rect.height.saturating_sub(radius.saturating_mul(2));
        if middle > 0 {
            spans.push(Rect::new(rect.x, rect.y + radius, rect.width, middle));
        }
        spans
            .into_iter()
            .flat_map(|span| self.clips(span, damage))
            .collect()
    }

    /// Draw a window's drop shadow: `rect` grown by `range` on every side,
    /// with `color` fading out over that distance.
    ///
    /// A port of Hyprland's `shadow.glsl` (`getShadow` and
    /// `pixAlphaRoundedDistance`), which is the only description of the
    /// shape there is. Inside the box, each pixel's alpha is scaled by:
    ///
    /// * **In a corner** -- past the rounded corner's centre on both axes --
    ///   by `((radius - d) / range)^power`, with `d` the distance to that
    ///   centre and `radius` the range plus the window's rounding; nothing
    ///   further out than `radius`.
    /// * **Along an edge**, by `(smallest / range)^power`, with `smallest`
    ///   the distance to the nearest edge of the shadow's own box.
    /// * **Anywhere else**, not at all.
    ///
    /// The window is drawn over it afterwards, as Hyprland draws it: this
    /// does not cut the window's own shape out, because nothing shows
    /// through an opaque window and a translucent one shows its shadow in
    /// Hyprland too.
    ///
    /// This is the one place in the crate that blends a pixel by hand. It
    /// has to be: every pixel has an alpha of its own, and tiny-skia's
    /// shaders take one colour for a whole rectangle. The arithmetic is the
    /// same source-over its `f32` pipeline does -- `s + d × (255 − a) / 255`,
    /// rounded to the nearest byte -- so a shadow and a fill of the same
    /// colour agree.
    pub fn shadow(&mut self, rect: Rect, shadow: &Shadow, damage: &Damage) {
        if shadow.range <= 0 || shadow.color.alpha() == 0 || is_empty(rect) {
            return;
        }
        let full = Rect::new(
            rect.x
                .saturating_sub(shadow.range)
                .saturating_add(shadow.offset.0),
            rect.y
                .saturating_sub(shadow.range)
                .saturating_add(shadow.offset.1),
            rect.width.saturating_add(shadow.range.saturating_mul(2)),
            rect.height.saturating_add(shadow.range.saturating_mul(2)),
        );
        let clips = self.clips(full, damage);
        if clips.is_empty() {
            return;
        }
        let shape = Falloff::new(full, shadow.rounding, shadow.range, shadow.power);
        let alpha = f32::from(shadow.color.alpha()) / 255.0;
        for clip in clips {
            for y in clip.y..clip.bottom() {
                self.shadow_row(clip, y, full, &shape, shadow.color, alpha);
            }
            self.damage.add(clip);
        }
    }

    /// One row of a shadow, which is where its pixels are actually written.
    fn shadow_row(
        &mut self,
        clip: Rect,
        y: i64,
        full: Rect,
        shape: &Falloff,
        color: Color,
        alpha: f32,
    ) {
        // The canvas holds blue in tiny-skia's red byte; the module comment
        // says why.
        let colour = [color.blue(), color.green(), color.red()];
        let width = index(i64::from(self.width()));
        let row = index(y) * width;
        for x in clip.x..clip.right() {
            let factor = shape.at(x - full.x, y - full.y) * alpha;
            if factor > 0.0
                && let Some(pixel) = self
                    .pixmap
                    .data_mut()
                    .get_mut((row + index(x)) * 4..(row + index(x)) * 4 + 4)
            {
                over(pixel, colour, factor);
            }
        }
    }

    /// Blur what has already been drawn inside `rect`, in place.
    ///
    /// Hyprland blurs what is *behind* a translucent window: the frame so
    /// far is taken, blurred, and put back before the window is drawn over
    /// it. So this reads the canvas's own pixels and writes them back, and
    /// it has to be called between the things behind and the thing in front.
    ///
    /// The region read is `rect` grown by the blur's reach on every side,
    /// because a blur that only read what it writes would pull the frame's
    /// own edge inwards and leave a bright rim; only `rect`'s rounded shape
    /// is written back. Nothing outside the canvas is read: the edges are
    /// clamped, as `GL_CLAMP_TO_EDGE` clamps them.
    pub fn blur(&mut self, rect: Rect, rounding: i64, size: i64, passes: u32, damage: &Damage) {
        if size <= 0 || passes == 0 || is_empty(rect) {
            return;
        }
        // The reach: each pass doubles the scale the offsets apply at.
        let reach = size.saturating_mul(1_i64 << passes.min(6));
        let Some(read) = intersect(
            Rect::new(
                rect.x.saturating_sub(reach),
                rect.y.saturating_sub(reach),
                rect.width.saturating_add(reach.saturating_mul(2)),
                rect.height.saturating_add(reach.saturating_mul(2)),
            ),
            self.bounds(),
        ) else {
            return;
        };
        let clips = if rounding > 0 {
            self.rounded_clips(rect, rounding, damage)
        } else {
            self.clips(rect, damage)
        };
        if clips.is_empty() {
            return;
        }

        let (wide, tall) = (index(read.width), index(read.height));
        let stride = index(i64::from(self.width())) * 4;
        let mut block = vec![0_u8; wide * tall * 4];
        for row in 0..tall {
            let from = (index(read.y) + row) * stride + index(read.x) * 4;
            let to = row * wide * 4;
            if let (Some(source), Some(target)) = (
                self.pixmap.data().get(from..from + wide * 4),
                block.get_mut(to..to + wide * 4),
            ) {
                target.copy_from_slice(source);
            }
        }
        crate::blur::blur(&mut block, wide, tall, size, passes);

        for clip in clips {
            for y in clip.y..clip.bottom() {
                let from = (index(y - read.y) * wide + index(clip.x - read.x)) * 4;
                let to = index(y) * stride + index(clip.x) * 4;
                let len = index(clip.width) * 4;
                if let (Some(source), Some(target)) = (
                    block.get(from..from + len),
                    self.pixmap.data_mut().get_mut(to..to + len),
                ) {
                    target.copy_from_slice(source);
                }
            }
            self.damage.add(clip);
        }
    }

    /// Fill `rect` with `color` and its corners cut to `radius`: the shape a
    /// rounded window's border is drawn as, before its surface is put inside
    /// it.
    pub fn fill_rounded(&mut self, rect: Rect, radius: i64, color: Color, damage: &Damage) {
        if color.alpha() == 0 || is_empty(rect) {
            return;
        }
        let clips = self.rounded_clips(rect, radius, damage);
        let paint = paint(Shader::SolidColor(skia_color(color)), BlendMode::SourceOver);
        self.fill_clips(&clips, &paint);
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
                copy_opaque(dst, src);
            }
            self.damage.add(clip);
        }
    }

    /// Blend a premultiplied `ARGB8888` surface drawn at `rect` into
    /// `clips`, through tiny-skia's pattern shader with nearest sampling at
    /// a whole-pixel offset, so each canvas pixel takes exactly one surface
    /// pixel.
    fn blend(&mut self, surface: &Surface<'_>, rect: Rect, opacity: f32, clips: &[Rect]) {
        // `wl_shm`'s bytes are blue, green, red, alpha: canvas order. A
        // padded buffer is gathered into tight rows first.
        let gathered: Vec<u8>;
        let bytes = if surface.format() == Format::Xrgb8888 {
            gathered = opaque_rows(surface);
            &gathered
        } else if let Some(tight) = surface.tight() {
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
            opacity,
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

/// Copy `src` over `dst` pixel for pixel, taking the three colour bytes and
/// making each pixel opaque: an `XRGB8888` client leaves its X byte zero, and
/// a canvas pixel with a zero alpha would draw nothing.
fn copy_opaque(dst: &mut [u8], src: &[u8]) {
    for (dst, src) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        if let ([b, g, r, x], [sb, sg, sr, _]) = (dst, src) {
            (*b, *g, *r, *x) = (*sb, *sg, *sr, 0xFF);
        }
    }
}

/// How far a rounded corner's row is inset from the rectangle's edge.
///
/// `row` counts from the corner's own edge, so row 0 is the outermost and
/// `radius - 1` the innermost. The inset is the circle's at that row's
/// centre -- `radius - sqrt(radius² - dy²)` with `dy` the distance from the
/// circle's centre to the row's middle -- rounded to the nearest pixel,
/// because coverage here is all or nothing.
fn corner_inset(radius: i64, row: i64) -> i64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a radius is a few dozen pixels; the loss is beyond any of them"
    )]
    let (radius_f, row_f) = (radius as f64, row as f64);
    let dy = radius_f - row_f - 0.5;
    let dx = (radius_f * radius_f - dy * dy).max(0.0).sqrt();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is between zero and the radius, which is an i64 already"
    )]
    let inset = (radius_f - dx).round() as i64;
    inset.clamp(0, radius)
}

/// A surface's pixels as tight rows the shader can read.
///
/// An opaque surface's fourth byte is the X byte, which a client leaves at
/// whatever it likes; read as alpha it would make the window blotchy, so it
/// is forced to `0xFF`. A premultiplied one is copied as it is.
fn opaque_rows(surface: &Surface<'_>) -> Vec<u8> {
    let opaque = surface.format() == Format::Xrgb8888;
    (0..surface.height())
        .filter_map(|y| surface.row(y))
        .flat_map(|row| row.chunks(4))
        .flat_map(|pixel| {
            [
                pixel.first().copied().unwrap_or(0),
                pixel.get(1).copied().unwrap_or(0),
                pixel.get(2).copied().unwrap_or(0),
                if opaque {
                    0xFF
                } else {
                    pixel.get(3).copied().unwrap_or(0)
                },
            ]
        })
        .collect()
}

/// What a window's drop shadow is: Hyprland's `decoration:shadow:*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shadow {
    /// The window's own corner rounding, which the shadow follows.
    pub rounding: i64,
    /// `shadow:range`: how far it reaches past the window, in pixels.
    pub range: i64,
    /// `shadow:render_power`: how fast it fades, 1 to 4.
    pub power: u32,
    /// `shadow:color`, whose alpha is the shadow's own.
    pub color: Color,
    /// `shadow:offset`: how far the whole shadow is moved.
    pub offset: (i64, i64),
}

/// A shadow's alpha at each point of its box, as `shadow.glsl` computes it.
#[derive(Clone, Copy, Debug)]
struct Falloff {
    size: (f32, f32),
    /// The rounded corner's centre, inset from the box by `range + rounding`
    /// on both axes: `TOPLEFT` in `renderRoundedShadow`.
    inset: f32,
    range: f32,
    power: i32,
}

impl Falloff {
    fn new(full: Rect, rounding: i64, range: i64, power: u32) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a window's size and a shadow's range, in pixels"
        )]
        Self {
            size: (full.width as f32, full.height as f32),
            inset: (range.saturating_add(rounding.max(0))) as f32,
            range: range as f32,
            // Hyprland clamps the power to 1..=4 before it reaches the
            // shader.
            power: power.clamp(1, 4) as i32,
        }
    }

    /// The alpha at `(x, y)` inside the box, 0 to 1.
    fn at(&self, x: i64, y: i64) -> f32 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a position inside a shadow's box"
        )]
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (right, bottom) = (self.size.0 - self.inset, self.size.1 - self.inset);
        let radius = self.inset;

        // The four corners, which are the only places both axes are past
        // the rounding's centre.
        let corner = match (px < self.inset, px > right, py < self.inset, py > bottom) {
            (true, _, true, _) => Some((self.inset, self.inset)),
            (true, _, _, true) => Some((self.inset, bottom)),
            (_, true, true, _) => Some((right, self.inset)),
            (_, true, _, true) => Some((right, bottom)),
            _ => None,
        };
        if let Some((cx, cy)) = corner {
            let distance = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            return rounded_distance(distance, radius, self.range, self.power);
        }

        // An edge: the distance to the nearest side of the shadow's own box.
        let smallest = py.min(self.size.1 - py).min(px).min(self.size.0 - px);
        if smallest < self.range {
            return (smallest / self.range).max(0.0).powi(self.power);
        }
        1.0
    }
}

/// `pixAlphaRoundedDistance` from `shadow.glsl`.
fn rounded_distance(distance: f32, radius: f32, range: f32, power: i32) -> f32 {
    if distance > radius {
        return 0.0;
    }
    if distance > radius - range {
        return ((radius - distance) / range).clamp(0.0, 1.0).powi(power);
    }
    1.0
}

/// Blend one premultiplied colour over one canvas pixel.
///
/// `colour` is not premultiplied; `alpha` is how much of it shows. The
/// arithmetic is tiny-skia's source-over, rounded the way its `f32` pipeline
/// rounds, so a shadow and a fill of the same colour agree to the byte.
fn over(pixel: &mut [u8], colour: [u8; 3], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    let keep = 1.0 - alpha;
    for (at, channel) in colour.iter().enumerate() {
        let Some(slot) = pixel.get_mut(at) else {
            continue;
        };
        let source = f32::from(*channel) * alpha;
        let blended = source + f32::from(*slot) * keep;
        *slot = blended.round().clamp(0.0, 255.0) as u8;
    }
}
