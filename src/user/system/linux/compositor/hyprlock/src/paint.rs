//! The four things upstream's renderer draws with -- `renderRect`,
//! `renderBorder`, `renderTexture` and a blurred framebuffer -- on a
//! tiny-skia pixmap, top-down, premultiplied, antialiased.

use compositor_config::Color;
use tiny_skia::{
    FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Pattern,
    Pixmap, PixmapMut, PixmapPaint, PixmapRef, Point, Shader, SpreadMode, Transform,
};

use crate::blur::{self, Plane};
use crate::config::Gradient;

/// A box in pixels, top-down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// A box.
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Grown by `by` on every side.
    #[must_use]
    pub fn grown(self, by: f32) -> Self {
        Self::new(
            self.x - by,
            self.y - by,
            self.width + 2.0 * by,
            self.height + 2.0 * by,
        )
    }

    /// With the corners on whole pixels, as `CBox::round` puts them.
    #[must_use]
    pub fn rounded(self) -> Self {
        Self::new(
            self.x.round(),
            self.y.round(),
            self.width.round(),
            self.height.round(),
        )
    }
}

/// The colour tiny-skia draws `color` (`0xAARRGGBB`) in, its alpha
/// multiplied by `alpha`.
#[must_use]
pub fn skia(color: Color, alpha: f32) -> tiny_skia::Color {
    let channel = |value: u8| f32::from(value) / 255.0;
    tiny_skia::Color::from_rgba(
        channel(color.red()),
        channel(color.green()),
        channel(color.blue()),
        (channel(color.alpha()) * alpha).clamp(0.0, 1.0),
    )
    .unwrap_or(tiny_skia::Color::TRANSPARENT)
}

/// A rectangle with its corners cut to `radius`, as a path.
fn rounded_path(rect: Rect, radius: f32) -> Option<tiny_skia::Path> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let radius = radius.clamp(0.0, rect.width.min(rect.height) / 2.0);
    if radius <= 0.0 {
        return tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
            .map(PathBuilder::from_rect);
    }
    // A quarter circle as one cubic.
    let k = radius * 0.552_284_8;
    let (left, top) = (rect.x, rect.y);
    let (right, bottom) = (rect.x + rect.width, rect.y + rect.height);
    let mut path = PathBuilder::new();
    path.move_to(left + radius, top);
    path.line_to(right - radius, top);
    path.cubic_to(
        right - radius + k,
        top,
        right,
        top + radius - k,
        right,
        top + radius,
    );
    path.line_to(right, bottom - radius);
    path.cubic_to(
        right,
        bottom - radius + k,
        right - radius + k,
        bottom,
        right - radius,
        bottom,
    );
    path.line_to(left + radius, bottom);
    path.cubic_to(
        left + radius - k,
        bottom,
        left,
        bottom - radius + k,
        left,
        bottom - radius,
    );
    path.line_to(left, top + radius);
    path.cubic_to(
        left,
        top + radius - k,
        left + radius - k,
        top,
        left + radius,
        top,
    );
    path.close();
    path.finish()
}

/// `renderRect`: `rect` filled with `color`, alpha times `alpha`, corners
/// cut to `radius`.
pub fn fill(pixmap: &mut PixmapMut<'_>, rect: Rect, radius: f32, color: Color, alpha: f32) {
    let Some(path) = rounded_path(rect, radius) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(skia(color, alpha));
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// The shader a gradient paints with across `rect`, at its angle.
///
/// Upstream mixes a gradient's colours in `OkLab`; this mixes them in sRGB,
/// which is the same picture for the one-colour gradients every border in
/// the customer's file has and a slightly different middle for more.
fn gradient_shader(gradient: &Gradient, rect: Rect, alpha: f32) -> Shader<'static> {
    if gradient.colors.len() < 2 {
        return Shader::SolidColor(skia(gradient.first(), alpha));
    }
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let (dx, dy) = (gradient.angle.cos(), gradient.angle.sin());
    let half = (rect.width * dx.abs() + rect.height * dy.abs()) / 2.0;
    #[expect(clippy::cast_precision_loss, reason = "at most ten colours")]
    let last = (gradient.colors.len() - 1) as f32;
    let stops = gradient
        .colors
        .iter()
        .enumerate()
        .map(|(at, color)| {
            #[expect(clippy::cast_precision_loss, reason = "at most ten colours")]
            let position = at as f32 / last;
            GradientStop::new(position, skia(*color, alpha))
        })
        .collect();
    LinearGradient::new(
        Point::from_xy(cx - dx * half, cy + dy * half),
        Point::from_xy(cx + dx * half, cy - dy * half),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap_or(Shader::SolidColor(skia(gradient.first(), alpha)))
}

/// `renderBorder`: a ring `thickness` wide inside `outer`, whose outer
/// corners are cut to `radius` and inner ones to what is left of it.
pub fn ring(
    pixmap: &mut PixmapMut<'_>,
    outer: Rect,
    thickness: f32,
    radius: f32,
    gradient: &Gradient,
    alpha: f32,
) {
    if thickness <= 0.0 {
        return;
    }
    let inner = outer.grown(-thickness);
    let (Some(outer_path), inner_path) = (
        rounded_path(outer, radius),
        rounded_path(inner, (radius - thickness).max(0.0)),
    ) else {
        return;
    };
    let mut builder = PathBuilder::new();
    builder.push_path(&outer_path);
    if let Some(inner_path) = inner_path {
        builder.push_path(&inner_path);
    }
    let Some(path) = builder.finish() else {
        return;
    };
    let paint = Paint {
        shader: gradient_shader(gradient, outer, alpha),
        anti_alias: true,
        ..Paint::default()
    };
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::EvenOdd,
        Transform::identity(),
        None,
    );
}

/// `renderTexture` of an unscaled texture: `source` at `(x, y)`, on whole
/// pixels, with its alpha times `alpha`, turned by `angle` degrees about
/// its centre.
pub fn blit(
    pixmap: &mut PixmapMut<'_>,
    source: PixmapRef<'_>,
    x: f32,
    y: f32,
    alpha: f32,
    angle: f32,
) {
    if alpha <= 0.0 {
        return;
    }
    let paint = PixmapPaint {
        opacity: alpha.clamp(0.0, 1.0),
        quality: if angle == 0.0 {
            FilterQuality::Nearest
        } else {
            FilterQuality::Bilinear
        },
        ..PixmapPaint::default()
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a screen position, far inside i32"
    )]
    let (left, top) = (x.round() as i32, y.round() as i32);
    if angle == 0.0 {
        pixmap.draw_pixmap(left, top, source, &paint, Transform::identity(), None);
        return;
    }
    #[expect(clippy::cast_precision_loss, reason = "a texture's size")]
    let (cx, cy) = (
        left as f32 + source.width() as f32 / 2.0,
        top as f32 + source.height() as f32 / 2.0,
    );
    pixmap.draw_pixmap(
        left,
        top,
        source,
        &paint,
        Transform::from_rotate_at(angle, cx, cy),
        None,
    );
}

/// `source` scaled to fill `rect` (which it may overhang), clipped to
/// `rect` with its corners cut to `radius`, alpha times `alpha`.
pub fn picture(
    pixmap: &mut PixmapMut<'_>,
    source: PixmapRef<'_>,
    place: Rect,
    clip: Rect,
    radius: f32,
    alpha: f32,
) {
    if source.width() == 0 || source.height() == 0 || alpha <= 0.0 {
        return;
    }
    let Some(path) = rounded_path(clip, radius) else {
        return;
    };
    #[expect(clippy::cast_precision_loss, reason = "a texture's size")]
    let (sx, sy) = (
        place.width / source.width() as f32,
        place.height / source.height() as f32,
    );
    let paint = Paint {
        shader: Pattern::new(
            source,
            SpreadMode::Pad,
            FilterQuality::Bilinear,
            alpha.clamp(0.0, 1.0),
            Transform::from_row(sx, 0.0, 0.0, sy, place.x, place.y),
        ),
        anti_alias: true,
        ..Paint::default()
    };
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// `getScaledBoxForTextureSize`: where a `size` picture goes to cover a
/// `viewport`, centred, on whole pixels.
#[must_use]
pub fn cover(size: (f32, f32), viewport: (f32, f32)) -> Rect {
    let scale = (viewport.0 / size.0).max(viewport.1 / size.1);
    let (width, height) = (size.0 * scale, size.1 * scale);
    let (x, y) = if viewport.0 / size.0 > viewport.1 / size.1 {
        (0.0, -(height - viewport.1) / 2.0)
    } else {
        (-(width - viewport.0) / 2.0, 0.0)
    };
    Rect::new(x, y, width, height).rounded()
}

/// A pixmap as a blur plane, and back.
fn to_plane(pixmap: &Pixmap) -> Plane {
    let mut plane = Plane::new(pixmap.width() as usize, pixmap.height() as usize);
    for (to, from) in plane.pixels.iter_mut().zip(pixmap.pixels()) {
        *to = [
            f32::from(from.red()) / 255.0,
            f32::from(from.green()) / 255.0,
            f32::from(from.blue()) / 255.0,
            f32::from(from.alpha()) / 255.0,
        ];
    }
    plane
}

fn from_plane(plane: &Plane, pixmap: &mut Pixmap) {
    for (to, from) in pixmap.pixels_mut().iter_mut().zip(&plane.pixels) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "each channel is clamped to 0..=1 by the blur"
        )]
        let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        let alpha = byte(from[3]);
        let channel = |value: f32| byte(value).min(alpha);
        if let Some(pixel) = tiny_skia::PremultipliedColorU8::from_rgba(
            channel(from[0]),
            channel(from[1]),
            channel(from[2]),
            alpha,
        ) {
            *to = pixel;
        }
    }
}

/// Blur a pixmap in place.
pub fn blur_pixmap(pixmap: &mut Pixmap, params: &blur::Params) {
    let mut plane = to_plane(pixmap);
    blur::blur(&mut plane, params);
    from_plane(&plane, pixmap);
}

/// A widget's shadow: `draw` paints the widget, at full opacity, into a
/// pixmap `bounds` grown by the blur's reach; the result is that blurred
/// and coloured, with where its top-left corner goes.
pub fn shadow(
    bounds: Rect,
    params: &blur::Params,
    draw: impl FnOnce(&mut PixmapMut<'_>, f32, f32),
) -> Option<(Pixmap, f32, f32)> {
    #[expect(clippy::cast_precision_loss, reason = "a few hundred pixels")]
    let reach = params.reach() as f32;
    let area = bounds.grown(reach).rounded();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a widget's size, positive and small"
    )]
    let mut pixmap = Pixmap::new(area.width.max(1.0) as u32, area.height.max(1.0) as u32)?;
    draw(&mut pixmap.as_mut(), -area.x, -area.y);
    blur_pixmap(&mut pixmap, params);
    Some((pixmap, area.x, area.y))
}

/// A mask of `rect` rounded by `radius`, the size of a `width` × `height`
/// pixmap: what clips the placeholder to its field.
#[must_use]
pub fn clip(width: u32, height: u32, rect: Rect) -> Option<Mask> {
    let mut mask = Mask::new(width, height)?;
    let path = rounded_path(rect, 0.0)?;
    mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
    Some(mask)
}

#[cfg(test)]
mod tests {
    use compositor_config::Color;
    use tiny_skia::Pixmap;

    use super::{Rect, cover, fill, ring};
    use crate::config::Gradient;

    #[test]
    fn cover_fills_the_screen_and_centres() {
        let placed = cover((100.0, 100.0), (200.0, 100.0));
        assert_eq!(placed, Rect::new(0.0, -50.0, 200.0, 200.0));
        let placed = cover((400.0, 100.0), (200.0, 100.0));
        assert_eq!(placed, Rect::new(-100.0, 0.0, 400.0, 100.0));
    }

    #[test]
    fn a_ring_leaves_its_inside_alone() {
        let Some(mut pixmap) = Pixmap::new(40, 40) else {
            panic!("pixmap");
        };
        ring(
            &mut pixmap.as_mut(),
            Rect::new(0.0, 0.0, 40.0, 40.0),
            4.0,
            8.0,
            &Gradient::solid(0xFFFF_0000),
            1.0,
        );
        let at = |x: u32, y: u32| pixmap.pixel(x, y).map(|pixel| (pixel.red(), pixel.alpha()));
        assert_eq!(at(20, 1), Some((255, 255)));
        assert_eq!(at(20, 20), Some((0, 0)));
        // The corner is cut.
        assert_eq!(at(0, 0), Some((0, 0)));
    }

    #[test]
    fn a_translucent_fill_is_premultiplied() {
        let Some(mut pixmap) = Pixmap::new(10, 10) else {
            panic!("pixmap");
        };
        fill(
            &mut pixmap.as_mut(),
            Rect::new(0.0, 0.0, 10.0, 10.0),
            0.0,
            Color(0x5900_0000),
            1.0,
        );
        let pixel = pixmap.pixel(5, 5).map(|pixel| (pixel.red(), pixel.alpha()));
        assert_eq!(pixel, Some((0, 0x59)));
    }
}
