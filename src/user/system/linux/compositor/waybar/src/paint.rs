//! Drawing the tree: each node's CSS box, then its children, then a label's
//! text -- GTK3's `gtk_css_gadget_draw` order.
//!
//! A box is drawn as `gtk_css_style_render_background` and
//! `gtk_css_style_render_border` draw it (`gtkrenderbackground.c`,
//! `gtkrenderborder.c`):
//!
//! 1. outset `box-shadow`s, outside the border box;
//! 2. `background-color`, clipped to the *last* layer's `background-clip`
//!    box, rounded by `border-radius`;
//! 3. the layers, last listed first, so the first listed is on top: each
//!    sized by `background-size` against its `background-origin` box
//!    (`auto` is the image's own size, and a gradient's is the whole box),
//!    placed by `background-position` -- a percentage is of the room the
//!    box has left around the image, so `100% 50%` puts the right cap flush
//!    right and centred -- repeated or not, and clipped;
//! 4. inset `box-shadow`s, inside the padding box;
//! 5. the border.
//!
//! An image from `url()` is drawn the way GTK3 draws one: loaded at its
//! own size (a 12 × 32 SVG is a 12 × 32 raster) and scaled to the layer's
//! size with a bilinear filter.
//!
//! Approximations, each said in the probe: a border style other than
//! `solid` is drawn solid; a blurred shadow's blur is a box blur, three
//! passes, which is close to cairo's; `text-shadow` is drawn without blur.

use std::path::Path;

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, PathBuilder,
    Pattern, Pixmap, PixmapMut, PixmapPaint, Point, Shader, SpreadMode, Transform,
};

use crate::css::style::{Gradient, Layer, LayerImage, ShadowPx, Style};
use crate::css::value::{Angle, Area, Length, Repeat, Rgba, Size};
use crate::layout::{Placement, Rect};
use crate::tree::{Kind, Tree};

/// Where images come from.
pub trait Images {
    /// The image at `path` rasterised at its own size times `scale`; its
    /// own size in logical pixels is the raster's divided by `scale`.
    fn get(&mut self, path: &Path, scale: f32) -> Option<&Pixmap>;
}

/// One label's text to draw.
#[derive(Clone, Copy, Debug)]
pub struct TextJob<'a> {
    /// The markup.
    pub markup: &'a str,
    /// The label's style.
    pub style: &'a Style,
    /// The text's logical rectangle in logical pixels; narrower than the
    /// text's natural width means ellipsize.
    pub rect: Rect,
    /// Whether it wraps at the rectangle's width.
    pub wrap: bool,
    /// A colour instead of the style's (a text shadow).
    pub color: Option<Rgba>,
}

/// What draws a label's text.
pub trait Text {
    /// Draw `job`, every coordinate mapped through `transform`.
    fn draw(&mut self, pixmap: &mut PixmapMut<'_>, job: &TextJob<'_>, transform: Transform);
}

/// What drawing a box needs besides the box: where images come from, and
/// the scale.
struct Ctx<'a> {
    images: &'a mut dyn Images,
    scale: f32,
    t: Transform,
}

fn skia(color: Rgba) -> Color {
    Color::from_rgba(
        color.r.clamp(0.0, 1.0),
        color.g.clamp(0.0, 1.0),
        color.b.clamp(0.0, 1.0),
        color.a.clamp(0.0, 1.0),
    )
    .unwrap_or(Color::TRANSPARENT)
}

fn paint_of(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(skia(color));
    paint.anti_alias = true;
    paint
}

/// Corner radii, horizontal and vertical, top left first, in pixels.
type Radii = [(f32, f32); 4];

fn radii(style: &Style, border: Rect) -> Radii {
    let mut out = [(0.0, 0.0); 4];
    for (slot, (h, v)) in out.iter_mut().zip(style.radius) {
        *slot = (
            h.resolve(0.0, border.width).max(0.0),
            v.resolve(0.0, border.height).max(0.0),
        );
    }
    // Radii that add up to more than a side are scaled down together.
    let [(tl_h, tl_v), (tr_h, tr_v), (br_h, br_v), (bl_h, bl_v)] = out;
    let mut factor: f32 = 1.0;
    for (sum, side) in [
        (tl_h + tr_h, border.width),
        (bl_h + br_h, border.width),
        (tl_v + bl_v, border.height),
        (tr_v + br_v, border.height),
    ] {
        if sum > side && sum > 0.0 {
            factor = factor.min(side / sum);
        }
    }
    if factor < 1.0 {
        for (h, v) in &mut out {
            *h *= factor;
            *v *= factor;
        }
    }
    out
}

fn shrink_radii(radii: Radii, sides: [f32; 4]) -> Radii {
    let [top, right, bottom, left] = sides;
    let [(a, b), (c, d), (e, f), (g, h)] = radii;
    [
        ((a - left).max(0.0), (b - top).max(0.0)),
        ((c - right).max(0.0), (d - top).max(0.0)),
        ((e - right).max(0.0), (f - bottom).max(0.0)),
        ((g - left).max(0.0), (h - bottom).max(0.0)),
    ]
}

fn is_square(radii: &Radii) -> bool {
    radii.iter().all(|&(h, v)| h <= 0.0 || v <= 0.0)
}

/// A rectangle with elliptical corners, as a path.
fn rounded(rect: Rect, radii: &Radii, builder: &mut PathBuilder) {
    // The cubic that best fits a quarter ellipse.
    const K: f32 = 0.552_284_8;
    let (x0, y0) = (rect.x, rect.y);
    let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);
    let [(tl_h, tl_v), (tr_h, tr_v), (br_h, br_v), (bl_h, bl_v)] = *radii;
    builder.move_to(x0 + tl_h, y0);
    builder.line_to(x1 - tr_h, y0);
    if tr_h > 0.0 && tr_v > 0.0 {
        builder.cubic_to(
            x1 - tr_h * (1.0 - K),
            y0,
            x1,
            y0 + tr_v * (1.0 - K),
            x1,
            y0 + tr_v,
        );
    }
    builder.line_to(x1, y1 - br_v);
    if br_h > 0.0 && br_v > 0.0 {
        builder.cubic_to(
            x1,
            y1 - br_v * (1.0 - K),
            x1 - br_h * (1.0 - K),
            y1,
            x1 - br_h,
            y1,
        );
    }
    builder.line_to(x0 + bl_h, y1);
    if bl_h > 0.0 && bl_v > 0.0 {
        builder.cubic_to(
            x0 + bl_h * (1.0 - K),
            y1,
            x0,
            y1 - bl_v * (1.0 - K),
            x0,
            y1 - bl_v,
        );
    }
    builder.line_to(x0, y0 + tl_v);
    if tl_h > 0.0 && tl_v > 0.0 {
        builder.cubic_to(
            x0,
            y0 + tl_v * (1.0 - K),
            x0 + tl_h * (1.0 - K),
            y0,
            x0 + tl_h,
            y0,
        );
    }
    builder.close();
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

fn skia_rect(rect: Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
}

/// A clip: a rectangle, rounded or not.
#[derive(Clone, Copy, Debug)]
struct Clip {
    rect: Rect,
    radii: Radii,
}

/// Fill `area` (already within `clip.rect`) with `paint`, clipped to the
/// clip's rounding.
fn fill_clipped(
    pixmap: &mut PixmapMut<'_>,
    area: Rect,
    clip: &Clip,
    paint: &Paint<'_>,
    t: Transform,
) {
    let Some(area) = intersect(area, clip.rect) else {
        return;
    };
    if is_square(&clip.radii) {
        if let Some(rect) = skia_rect(area) {
            let path = PathBuilder::from_rect(rect);
            pixmap.fill_path(&path, paint, FillRule::Winding, t, None);
        }
        return;
    }
    let mut builder = PathBuilder::new();
    rounded(clip.rect, &clip.radii, &mut builder);
    let Some(clip_path) = builder.finish() else {
        return;
    };
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
        return;
    };
    mask.fill_path(&clip_path, FillRule::Winding, true, t);
    if let Some(rect) = skia_rect(area) {
        let path = PathBuilder::from_rect(rect);
        pixmap.fill_path(&path, paint, FillRule::Winding, t, Some(&mask));
    }
}

/// The box a layer is clipped to or positioned in.
fn area_rect(style: &Style, border: Rect, area: Area) -> Rect {
    match area {
        Area::Border => border,
        Area::Padding => border.inset(style.border_width),
        Area::Content => border.inset(style.border_width).inset(style.padding),
    }
}

fn clip_for(style: &Style, border: Rect, area: Area, outer: &Radii) -> Clip {
    let rect = area_rect(style, border, area);
    let radii = match area {
        Area::Border => *outer,
        Area::Padding => shrink_radii(*outer, style.border_width),
        Area::Content => {
            let [a, b, c, d] = style.border_width;
            let [e, f, g, h] = style.padding;
            shrink_radii(*outer, [a + e, b + f, c + g, d + h])
        }
    };
    Clip { rect, radii }
}

/// A layer's image size, `gtk_css_bg_size_value_compute_size`: `intrinsic`
/// is the image's own size, `None` for a gradient.
fn layer_size(size: Size, area: Rect, intrinsic: Option<(f32, f32)>) -> (f32, f32) {
    let ratio = intrinsic.and_then(|(w, h)| (h > 0.0).then_some(w / h));
    match size {
        Size::Cover | Size::Contain => {
            let Some((w, h)) = intrinsic else {
                return (area.width, area.height);
            };
            if w <= 0.0 || h <= 0.0 {
                return (area.width, area.height);
            }
            let by_width = area.width / w;
            let by_height = area.height / h;
            let factor = if size == Size::Cover {
                by_width.max(by_height)
            } else {
                by_width.min(by_height)
            };
            (w * factor, h * factor)
        }
        Size::Explicit(width, height) => {
            let resolve =
                |length: Option<Length>, reference: f32| length.map(|l| l.resolve(0.0, reference));
            match (resolve(width, area.width), resolve(height, area.height)) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (
                    w,
                    ratio.map_or(intrinsic.map_or(area.height, |i| i.1), |r| w / r),
                ),
                (None, Some(h)) => (
                    ratio.map_or(intrinsic.map_or(area.width, |i| i.0), |r| h * r),
                    h,
                ),
                (None, None) => intrinsic.unwrap_or((area.width, area.height)),
            }
        }
    }
}

/// Where a gradient's line starts and ends in a box, as CSS Images 3
/// defines it: through the centre, long enough that the corners get the
/// first and last colours.
fn gradient_line(angle: Angle, rect: Rect) -> (Point, Point) {
    let degrees = match angle {
        Angle::Degrees(d) => d,
        Angle::To(x, y) => {
            let (w, h) = (rect.width, rect.height);
            match (x, y) {
                (0, -1) => 0.0,
                (1, 0) => 90.0,
                (0, 1) => 180.0,
                (-1, 0) => 270.0,
                // A corner: the line is perpendicular to the diagonal
                // between the other two corners.
                (x, y) => {
                    let base = (w / h.max(f32::EPSILON)).atan().to_degrees();
                    match (x > 0, y > 0) {
                        (true, false) => base,
                        (true, true) => 180.0 - base,
                        (false, true) => 180.0 + base,
                        (false, false) => 360.0 - base,
                    }
                }
            }
        }
    };
    let radians = degrees.to_radians();
    let (sin, cos) = radians.sin_cos();
    let length = (rect.width * sin).abs() + (rect.height * cos).abs();
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let dx = sin * length / 2.0;
    let dy = -cos * length / 2.0;
    (
        Point::from_xy(cx - dx, cy - dy),
        Point::from_xy(cx + dx, cy + dy),
    )
}

/// Stops with every position filled in, as CSS fixes them up: the first at
/// 0, the last at 1, missing ones spread evenly, none before the one
/// before it.
fn stops(gradient: &Gradient, length: f32) -> Vec<GradientStop> {
    let count = gradient.stops.len();
    let mut at: Vec<Option<f32>> = gradient
        .stops
        .iter()
        .map(|(_, position)| {
            position.map(|p| {
                if length > 0.0 {
                    p.resolve(0.0, length) / length
                } else {
                    p.percent / 100.0
                }
            })
        })
        .collect();
    if let Some(first) = at.first_mut()
        && first.is_none()
    {
        *first = Some(0.0);
    }
    if let Some(last) = at.last_mut()
        && last.is_none()
    {
        *last = Some(1.0);
    }
    let mut highest: f32 = 0.0;
    for slot in at.iter_mut().flatten() {
        highest = highest.max(*slot);
        *slot = highest;
    }
    let mut index = 0;
    while index < count {
        if at.get(index).copied().flatten().is_some() {
            index += 1;
            continue;
        }
        let start = index - 1;
        let mut end = index;
        while at.get(end).copied().flatten().is_none() && end < count {
            end += 1;
        }
        let from = at.get(start).copied().flatten().unwrap_or(0.0);
        let to = at.get(end).copied().flatten().unwrap_or(1.0);
        #[expect(clippy::cast_precision_loss, reason = "a count of stops")]
        let steps = (end - start) as f32;
        for (step, slot) in at.iter_mut().enumerate().take(end).skip(index) {
            #[expect(clippy::cast_precision_loss, reason = "a count of stops")]
            let fraction = (step - start) as f32 / steps;
            *slot = Some(from + (to - from) * fraction);
        }
        index = end;
    }
    gradient
        .stops
        .iter()
        .zip(at)
        .map(|((color, _), position)| {
            GradientStop::new(position.unwrap_or(0.0).clamp(0.0, 1.0), skia(*color))
        })
        .collect()
}

fn draw_layer(
    pixmap: &mut PixmapMut<'_>,
    style: &Style,
    border: Rect,
    outer: &Radii,
    layer: &Layer,
    ctx: &mut Ctx<'_>,
) {
    let (scale, t) = (ctx.scale, ctx.t);
    let clip = clip_for(style, border, layer.clip, outer);
    let origin = area_rect(style, border, layer.origin);
    let raster = match &layer.image {
        LayerImage::File(path) => match ctx.images.get(path, scale) {
            Some(raster) => Some(raster),
            // GTK drops a layer whose image does not load, without a word;
            // the user's style.css warns of exactly that.
            None => return,
        },
        LayerImage::Linear(_) => None,
        LayerImage::Other(_) => return,
    };
    #[expect(clippy::cast_precision_loss, reason = "a raster's size in pixels")]
    let intrinsic = raster.map(|r| (r.width() as f32 / scale, r.height() as f32 / scale));
    let (width, height) = layer_size(layer.size, origin, intrinsic);
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let x = origin.x + layer.position.0.resolve(0.0, origin.width - width);
    let y = origin.y + layer.position.1.resolve(0.0, origin.height - height);
    let (repeat_x, repeat_y) = layer.repeat;
    let area = Rect::new(
        if repeat_x == Repeat::NoRepeat {
            x
        } else {
            clip.rect.x
        },
        if repeat_y == Repeat::NoRepeat {
            y
        } else {
            clip.rect.y
        },
        if repeat_x == Repeat::NoRepeat {
            width
        } else {
            clip.rect.width
        },
        if repeat_y == Repeat::NoRepeat {
            height
        } else {
            clip.rect.height
        },
    );
    let spread = if repeat_x == Repeat::NoRepeat && repeat_y == Repeat::NoRepeat {
        SpreadMode::Pad
    } else {
        SpreadMode::Repeat
    };
    match (&layer.image, raster) {
        (_, Some(raster)) => {
            #[expect(clippy::cast_precision_loss, reason = "a raster's size in pixels")]
            let (rw, rh) = (raster.width() as f32, raster.height() as f32);
            // Pattern space is the raster's pixels; the fill is in logical
            // pixels mapped through `t`, so the pattern's own transform is
            // logical.
            let local = Transform::from_row(width / rw, 0.0, 0.0, height / rh, x, y);
            let shader = Pattern::new(raster.as_ref(), spread, FilterQuality::Bilinear, 1.0, local);
            let paint = Paint {
                shader,
                anti_alias: false,
                ..Paint::default()
            };
            fill_clipped(pixmap, area, &clip, &paint, t);
        }
        (LayerImage::Linear(gradient), None) => {
            let image = Rect::new(x, y, width, height);
            let (start, end) = gradient_line(gradient.angle, image);
            let length = ((end.x - start.x).powi(2) + (end.y - start.y).powi(2)).sqrt();
            let stops = stops(gradient, length);
            let shader = if stops.len() >= 2 {
                LinearGradient::new(
                    start,
                    end,
                    stops,
                    if gradient.repeating {
                        SpreadMode::Repeat
                    } else {
                        SpreadMode::Pad
                    },
                    Transform::identity(),
                )
            } else {
                None
            };
            let shader = shader.unwrap_or_else(|| {
                Shader::SolidColor(
                    gradient
                        .stops
                        .first()
                        .map_or(Color::TRANSPARENT, |(c, _)| skia(*c)),
                )
            });
            let paint = Paint {
                shader,
                anti_alias: false,
                ..Paint::default()
            };
            fill_clipped(pixmap, area, &clip, &paint, t);
        }
        _ => {}
    }
}

/// One box-blur pass of radius `r` along each of `lines` lines of
/// `length` samples, the samples `step` apart and the lines `stride` apart.
fn box_pass(from: &[f32], to: &mut [f32], shape: (usize, usize, usize, usize), r: usize) {
    let (lines, length, stride, step) = shape;
    #[expect(clippy::cast_precision_loss, reason = "a small window")]
    let window = (2 * r + 1) as f32;
    for line in 0..lines {
        let base = line * stride;
        let at = |i: isize| -> f32 {
            usize::try_from(i)
                .ok()
                .filter(|&i| i < length)
                .and_then(|i| from.get(base + i * step).copied())
                .unwrap_or(0.0)
        };
        let r = r.cast_signed();
        let mut sum: f32 = (-r..=r).map(at).sum();
        for i in 0..length {
            if let Some(slot) = to.get_mut(base + i * step) {
                *slot = sum / window;
            }
            let i = i.cast_signed();
            sum += at(i + r + 1) - at(i - r);
        }
    }
}

/// Three box-blur passes over an alpha buffer, close to a Gaussian of the
/// radius, as cairo's shadow blur is.
fn blur(alpha: &mut [f32], width: usize, height: usize, radius: f32) {
    #[expect(clippy::cast_possible_truncation, reason = "a small blur radius")]
    #[expect(clippy::cast_sign_loss, reason = "radius is not negative")]
    let r = (radius / 2.0).round().max(0.0) as usize;
    if r == 0 || width == 0 || height == 0 {
        return;
    }
    let mut scratch = vec![0.0f32; alpha.len()];
    for _ in 0..3 {
        box_pass(alpha, &mut scratch, (height, width, width, 1), r);
        box_pass(&scratch, alpha, (width, height, 1, width), r);
    }
}

fn draw_shadow(
    pixmap: &mut PixmapMut<'_>,
    shadow: &ShadowPx,
    border: Rect,
    padding: &Clip,
    outer: &Radii,
    scale: f32,
    t: Transform,
) {
    if shadow.color.a <= 0.0 {
        return;
    }
    let shape = if shadow.inset { padding.rect } else { border };
    let spread = if shadow.inset {
        -shadow.spread
    } else {
        shadow.spread
    };
    let moved = Rect::new(
        shape.x + shadow.x - spread,
        shape.y + shadow.y - spread,
        (shape.width + 2.0 * spread).max(0.0),
        (shape.height + 2.0 * spread).max(0.0),
    );
    // Work in device pixels on an alpha buffer the size of the surface.
    let (pw, ph) = (pixmap.width() as usize, pixmap.height() as usize);
    let mut alpha = vec![0.0f32; pw * ph];
    let device = |r: Rect| -> (usize, usize, usize, usize) {
        #[expect(clippy::cast_possible_truncation, reason = "clamped to the surface")]
        #[expect(clippy::cast_sign_loss, reason = "clamped to the surface")]
        let clamp = |v: f32, max: usize| {
            #[expect(clippy::cast_precision_loss, reason = "a surface's size")]
            let max_f = max as f32;
            v.clamp(0.0, max_f).round() as usize
        };
        (
            clamp(r.x * scale, pw),
            clamp(r.y * scale, ph),
            clamp((r.x + r.width) * scale, pw),
            clamp((r.y + r.height) * scale, ph),
        )
    };
    let (x0, y0, x1, y1) = device(moved);
    let inside = |x: usize, y: usize, rect: (usize, usize, usize, usize)| {
        x >= rect.0 && x < rect.2 && y >= rect.1 && y < rect.3
    };
    for y in 0..ph {
        for x in 0..pw {
            let covered = inside(x, y, (x0, y0, x1, y1));
            // Outset: the shape; inset: everything but the shape.
            let on = if shadow.inset { !covered } else { covered };
            if on && let Some(slot) = alpha.get_mut(y * pw + x) {
                *slot = 1.0;
            }
        }
    }
    blur(&mut alpha, pw, ph, shadow.blur * scale);
    // Inset shadows stay inside the padding box; outset ones outside the
    // border box.
    let padding_device = device(padding.rect);
    let border_device = device(border);
    let [r, g, b, a] = shadow.color.bytes();
    let data = pixmap.data_mut();
    for y in 0..ph {
        for x in 0..pw {
            let keep = if shadow.inset {
                inside(x, y, padding_device)
            } else {
                !inside(x, y, border_device)
            };
            let coverage = if keep {
                alpha.get(y * pw + x).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            if coverage <= 0.0 {
                continue;
            }
            let at = (y * pw + x) * 4;
            let Some(pixel) = data.get_mut(at..at + 4) else {
                continue;
            };
            let sa = f32::from(a) / 255.0 * coverage;
            let blend = |dst: u8, src: u8| {
                let src = f32::from(src) * sa;
                #[expect(clippy::cast_possible_truncation, reason = "a blended byte")]
                #[expect(clippy::cast_sign_loss, reason = "a blended byte")]
                let out = (src + f32::from(dst) * (1.0 - sa))
                    .round()
                    .clamp(0.0, 255.0) as u8;
                out
            };
            if let [pr, pg, pb, pa] = pixel {
                *pr = blend(*pr, r);
                *pg = blend(*pg, g);
                *pb = blend(*pb, b);
                *pa = blend(*pa, 255);
            }
        }
    }
    let _ = (outer, t);
}

fn draw_border(
    pixmap: &mut PixmapMut<'_>,
    style: &Style,
    border: Rect,
    outer: &Radii,
    t: Transform,
) {
    let widths = style.border_width;
    if widths.iter().all(|w| *w <= 0.0) {
        return;
    }
    let inner = border.inset(widths);
    let colors = style.border_color;
    let same = colors.iter().all(|c| *c == colors[0]);
    if same {
        let mut builder = PathBuilder::new();
        rounded(border, outer, &mut builder);
        rounded(inner, &shrink_radii(*outer, widths), &mut builder);
        if let Some(path) = builder.finish() {
            pixmap.fill_path(&path, &paint_of(colors[0]), FillRule::EvenOdd, t, None);
        }
        return;
    }
    // One trapezoid per side, meeting at the corners' diagonals.
    let (ox0, oy0, ox1, oy1) = (
        border.x,
        border.y,
        border.x + border.width,
        border.y + border.height,
    );
    let (ix0, iy0, ix1, iy1) = (
        inner.x,
        inner.y,
        inner.x + inner.width,
        inner.y + inner.height,
    );
    let sides = [
        [(ox0, oy0), (ox1, oy0), (ix1, iy0), (ix0, iy0)],
        [(ox1, oy0), (ox1, oy1), (ix1, iy1), (ix1, iy0)],
        [(ox1, oy1), (ox0, oy1), (ix0, iy1), (ix1, iy1)],
        [(ox0, oy1), (ox0, oy0), (ix0, iy0), (ix0, iy1)],
    ];
    for (points, (color, width)) in sides.iter().zip(colors.iter().zip(widths)) {
        if width <= 0.0 {
            continue;
        }
        let mut builder = PathBuilder::new();
        for (index, (x, y)) in points.iter().enumerate() {
            if index == 0 {
                builder.move_to(*x, *y);
            } else {
                builder.line_to(*x, *y);
            }
        }
        builder.close();
        if let Some(path) = builder.finish() {
            pixmap.fill_path(&path, &paint_of(*color), FillRule::Winding, t, None);
        }
    }
}

/// Draw one node's CSS box.
pub fn draw_box(
    pixmap: &mut PixmapMut<'_>,
    style: &Style,
    border: Rect,
    images: &mut dyn Images,
    scale: f32,
) {
    if border.width <= 0.0 || border.height <= 0.0 {
        return;
    }
    let t = Transform::from_scale(scale, scale);
    let outer = radii(style, border);
    let padding = clip_for(style, border, Area::Padding, &outer);
    for shadow in style.box_shadow.iter().filter(|s| !s.inset) {
        draw_shadow(pixmap, shadow, border, &padding, &outer, scale, t);
    }
    if style.background_color.a > 0.0 {
        let area = style.layers.last().map_or(Area::Border, |layer| layer.clip);
        let clip = clip_for(style, border, area, &outer);
        fill_clipped(
            pixmap,
            clip.rect,
            &clip,
            &paint_of(style.background_color),
            t,
        );
    }
    let mut ctx = Ctx { images, scale, t };
    for layer in style.layers.iter().rev() {
        draw_layer(pixmap, style, border, &outer, layer, &mut ctx);
    }
    for shadow in style.box_shadow.iter().filter(|s| s.inset) {
        draw_shadow(pixmap, shadow, border, &padding, &outer, scale, t);
    }
    draw_border(pixmap, style, border, &outer, t);
}

/// Draw `tree` as `placement` has it: boxes, then children, then text.
pub fn draw_tree(
    pixmap: &mut PixmapMut<'_>,
    tree: &Tree,
    placement: &Placement,
    images: &mut dyn Images,
    text: &mut dyn Text,
    scale: f32,
) {
    draw_node(pixmap, tree, placement, 0, images, text, scale);
}

fn draw_node(
    pixmap: &mut PixmapMut<'_>,
    tree: &Tree,
    placement: &Placement,
    index: usize,
    images: &mut dyn Images,
    text: &mut dyn Text,
    scale: f32,
) {
    let Some(node) = tree.get(index) else {
        return;
    };
    if !node.visible {
        return;
    }
    let style = &node.style;
    if style.opacity <= 0.0 {
        return;
    }
    if style.opacity < 1.0 {
        // The subtree drawn alone, then laid on with its opacity.
        let Some(mut layer) = Pixmap::new(pixmap.width(), pixmap.height()) else {
            return;
        };
        let mut solo = node.clone();
        solo.style.opacity = 1.0;
        let mut copy = tree.clone();
        if let Some(slot) = copy.nodes.get_mut(index) {
            *slot = solo;
        }
        draw_node(
            &mut layer.as_mut(),
            &copy,
            placement,
            index,
            images,
            text,
            scale,
        );
        let paint = PixmapPaint {
            opacity: style.opacity,
            ..PixmapPaint::default()
        };
        pixmap.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
        return;
    }
    let Some(placed) = placement.nodes.get(index) else {
        return;
    };
    if !matches!(node.kind, Kind::EventBox) {
        draw_box(pixmap, style, placed.border, images, scale);
    }
    for &child in &node.children {
        draw_node(pixmap, tree, placement, child, images, text, scale);
    }
    if let Kind::Label { markup, wrap, .. } = &node.kind {
        let t = Transform::from_scale(scale, scale);
        for shadow in &style.text_shadow {
            let moved = Rect::new(
                placed.text.x + shadow.x,
                placed.text.y + shadow.y,
                placed.text.width,
                placed.text.height,
            );
            let job = TextJob {
                markup,
                style,
                rect: moved,
                wrap: *wrap,
                color: Some(shadow.color),
            };
            text.draw(pixmap, &job, t);
        }
        let job = TextJob {
            markup,
            style,
            rect: placed.text,
            wrap: *wrap,
            color: None,
        };
        text.draw(pixmap, &job, t);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use tiny_skia::{Pixmap, PixmapMut, Transform};

    use super::{Images, Text, TextJob, draw_box, draw_tree};
    use crate::css::Stylesheet;
    use crate::css::style::Style;
    use crate::diag::Diagnostics;
    use crate::layout::{Measure, Rect, TextSize, layout};
    use crate::view::{BarView, ModuleView, Shape, build};

    /// A cap: a solid triangle in a 12 × 32 raster, the right half of which
    /// is the chip's colour, as the user's cap SVGs are.
    struct Caps(BTreeMap<PathBuf, Pixmap>);

    impl Caps {
        fn new() -> Self {
            let mut map = BTreeMap::new();
            let mut left = Pixmap::new(12, 32).expect("a pixmap");
            // Fill the lower-right triangle: x >= 12 - 12*y/32.
            for (at, px) in left.data_mut().chunks_exact_mut(4).enumerate() {
                let (x, y) = (at % 12, at / 12);
                if x * 32 >= 12 * 32 - 12 * y {
                    px.copy_from_slice(&[0x16, 0x27, 0x3c, 0xff]);
                }
            }
            let _ = map.insert(PathBuf::from("/s/icons/cap-l.svg"), left);
            Self(map)
        }
    }

    impl Images for Caps {
        fn get(&mut self, path: &Path, _scale: f32) -> Option<&Pixmap> {
            self.0.get(path)
        }
    }

    struct NoText;

    impl Text for NoText {
        fn draw(&mut self, _: &mut PixmapMut<'_>, _: &TextJob<'_>, _: Transform) {}
    }

    impl Measure for NoText {
        fn text(&mut self, markup: &str, _: &Style, _: Option<f32>) -> TextSize {
            #[expect(clippy::cast_precision_loss, reason = "a test's character count")]
            let width = markup.chars().count() as f32 * 8.0;
            TextSize {
                width,
                height: 18.0,
                min_width: 8.0,
                char_width: 8.0,
            }
        }
    }

    fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * pixmap.width() + x) * 4) as usize;
        let mut out = [0u8; 4];
        if let Some(px) = pixmap.data().get(at..at + 4) {
            out.copy_from_slice(px);
        }
        out
    }

    fn style(css: &str, node: &str) -> Style {
        let mut diag = Diagnostics::default();
        let sheet = Stylesheet::parse(css, Path::new("/s/style.css"), &|_| None, &mut diag);
        assert!(diag.lines.is_empty(), "{:?}", diag.lines);
        let mut view = ModuleView::new(node, Shape::Label);
        view.markup = "x".into();
        let bar = BarView {
            window_classes: vec![],
            spacing: 0.0,
            center: false,
            fixed_center: true,
            sections: [vec![0], vec![], vec![]],
        };
        let (mut tree, built) = build(&bar, &[view]);
        tree.style(&sheet);
        let (_, label) = built.modules.first().copied().flatten().unwrap_or((0, 0));
        tree.get(label).map(|n| n.style.clone()).unwrap_or_default()
    }

    #[test]
    fn a_chip_is_its_caps_and_band() {
        let chip = style(
            r#"#a { background-image: url("icons/cap-l.svg"), linear-gradient(#16273c, #16273c);
                    background-size: 12px 100%, calc(100% - 24px) 100%;
                    background-position: 0% 50%, 12px 50%;
                    background-repeat: no-repeat; }"#,
            "a",
        );
        let mut pixmap = Pixmap::new(60, 32).expect("a pixmap");
        draw_box(
            &mut pixmap.as_mut(),
            &chip,
            Rect::new(0.0, 0.0, 60.0, 32.0),
            &mut Caps::new(),
            1.0,
        );
        let chip_color = [0x16, 0x27, 0x3c, 0xff];
        assert_eq!(pixel(&pixmap, 0, 1), [0, 0, 0, 0], "the cap's empty corner");
        assert_eq!(pixel(&pixmap, 11, 30), chip_color, "the cap's filled half");
        assert_eq!(pixel(&pixmap, 12, 0), chip_color, "the band starts at 12");
        assert_eq!(pixel(&pixmap, 47, 31), chip_color, "and runs to 100% - 12");
        assert_eq!(
            pixel(&pixmap, 48, 16),
            [0, 0, 0, 0],
            "no right cap in this test"
        );
    }

    #[test]
    fn a_background_colour_and_an_inset_shadow() {
        let boxed = style(
            "#a { background-color: #ff0000; box-shadow: inset 0 -3px 0 0 #00ff00; }",
            "a",
        );
        let mut pixmap = Pixmap::new(10, 10).expect("a pixmap");
        draw_box(
            &mut pixmap.as_mut(),
            &boxed,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            &mut Caps::new(),
            1.0,
        );
        assert_eq!(pixel(&pixmap, 5, 2), [255, 0, 0, 255]);
        assert_eq!(
            pixel(&pixmap, 5, 7),
            [0, 255, 0, 255],
            "the bottom 3 px are the shadow"
        );
        assert_eq!(pixel(&pixmap, 5, 6), [255, 0, 0, 255]);
    }

    #[test]
    fn a_border_and_translucent_ground() {
        let boxed = style(
            "#a { background-color: rgba(0, 0, 255, 0.5); border: 2px solid #ffffff; }",
            "a",
        );
        let mut pixmap = Pixmap::new(10, 10).expect("a pixmap");
        draw_box(
            &mut pixmap.as_mut(),
            &boxed,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            &mut Caps::new(),
            1.0,
        );
        assert_eq!(pixel(&pixmap, 0, 5), [255, 255, 255, 255]);
        assert_eq!(pixel(&pixmap, 1, 5), [255, 255, 255, 255]);
        assert_eq!(
            pixel(&pixmap, 5, 5),
            [0, 0, 128, 128],
            "premultiplied half blue"
        );
    }

    #[test]
    fn a_whole_bar_draws() {
        let mut diag = Diagnostics::default();
        let sheet = Stylesheet::parse(
            "window#waybar { background: #000000; } #cpu { background-color: #ffffff; margin: 4px 1px; }",
            Path::new("/s/style.css"),
            &|_| None,
            &mut diag,
        );
        let mut cpu = ModuleView::new("cpu", Shape::Label);
        cpu.markup = "cpu 1%".into();
        let bar = BarView {
            window_classes: vec![],
            spacing: 0.0,
            center: false,
            fixed_center: true,
            sections: [vec![0], vec![], vec![]],
        };
        let (mut tree, _) = build(&bar, &[cpu]);
        tree.style(&sheet);
        let placement = layout(&tree, &mut NoText, 100.0, 20.0);
        let mut pixmap = Pixmap::new(100, 20).expect("a pixmap");
        draw_tree(
            &mut pixmap.as_mut(),
            &tree,
            &placement,
            &mut Caps::new(),
            &mut NoText,
            1.0,
        );
        assert_eq!(pixel(&pixmap, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&pixmap, 1, 4), [255, 255, 255, 255]);
        assert_eq!(pixel(&pixmap, 0, 4), [0, 0, 0, 255], "the margin");
        assert_eq!(pixel(&pixmap, 60, 10), [0, 0, 0, 255], "past the chip");
    }
}
