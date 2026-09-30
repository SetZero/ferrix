//! Drawing one frame: fuzzel's `render.c`, with tiny-skia for pixman and
//! `src/user/linux/compositor/text` for fcft.
//!
//! The order and the operators are fuzzel's: the rounded background and
//! border replace what is under them (`PIXMAN_OP_SRC`), each row erases its
//! own background before anything is drawn on it, the selection is laid
//! over the row (`OVER`), then the icon, then the title with its matched
//! characters in the match colour and an ellipsis where it no longer fits.
//! A frame is always drawn whole: fuzzel keeps the last buffer's pixels and
//! redraws only the rows, which is the same picture.
//!
//! Where this differs: glyphs are shaped with rustybuzz rather than
//! `HarfBuzz` through fcft, and antialiased by tiny-skia rather than `FreeType`,
//! so an edge pixel is not `FreeType`'s; the rounded corners are a tiny-skia
//! path rather than pixman's two-times supersampled rectangles; and the
//! input line scrolls so the cursor stays visible, but by whole characters
//! rather than by fuzzel's measured glyph extents.

use std::collections::HashMap;
use std::path::PathBuf;

use compositor_text::tiny_skia::{
    self, BlendMode, Color, FillRule, Paint, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Rect,
    Transform,
};
use compositor_text::{Font, FontDescription, Fonts, Run, Size};

use crate::config::{Config, Rgba};
use crate::geometry::{FontMetrics, Geometry, Scaling};
use crate::icon::{Found, Kind};
use crate::launcher::Launcher;
use crate::matching::Match;

/// A colour as `src/user/linux/compositor/text` takes it.
fn text_rgba(color: Rgba) -> compositor_text::Rgba {
    let [r, g, b, a] = color.0.to_be_bytes();
    compositor_text::Rgba { r, g, b, a }
}

/// A colour as tiny-skia takes it.
fn skia(color: Rgba) -> Color {
    let [r, g, b, a] = color.0.to_be_bytes();
    Color::from_rgba8(r, g, b, a)
}

/// The fonts and their sizes.
#[derive(Debug)]
pub struct Look {
    /// Every face found.
    pub fonts: Fonts,
    /// `font=`, resolved.
    pub font: Font,
    /// The selected row's font: `font=` in bold with `use-bold`.
    pub bold: Font,
    /// Its metrics, rounded as fcft rounds them.
    pub metrics: FontMetrics,
}

impl Look {
    /// Resolve `config.font` at `scaling`, as fuzzel's `reload_font` sizes
    /// it: by the DPI when fonts are sized by DPI, else by 96 DPI times the
    /// scale.
    #[must_use]
    pub fn new(mut fonts: Fonts, config: &Config, scaling: Scaling) -> Self {
        // `font=` is a comma-separated list; each is a fontconfig pattern,
        // and the first one's size is the size.
        let mut description = FontDescription::fontconfig(&config.font);
        let px = match description.size {
            Size::Points(points) => {
                let dpi = if scaling.by_dpi {
                    scaling.dpi
                } else {
                    96.0 * scaling.scale
                };
                points * dpi / 72.0
            }
            Size::Pixels(pixels) => {
                if scaling.by_dpi {
                    pixels
                } else {
                    pixels * scaling.scale
                }
            }
        };
        description.size = Size::Pixels(px);
        let font = fonts.resolve(&description);
        let bold = if config.use_bold {
            let mut heavier = description.clone();
            heavier.weight = compositor_text::Weight::BOLD;
            fonts.resolve(&heavier)
        } else {
            font.clone()
        };
        let m = fonts.metrics(&font);
        let advance = |fonts: &mut Fonts, c: char| {
            fonts
                .glyph_for(&font, c)
                .map_or(0, |glyph| glyph.advance.round() as i32)
        };
        let metrics = FontMetrics {
            ascent: m.ascent.round() as i32,
            descent: m.descent.round() as i32,
            height: m.height.round() as i32,
            o_advance: advance(&mut fonts, 'o'),
            space_advance: advance(&mut fonts, ' '),
            underline_thickness: (m.underline_thickness.round() as i32).max(1),
        };
        Self {
            fonts,
            font,
            bold,
            metrics,
        }
    }
}

/// Pictures read from disk, by file and size.
#[derive(Debug, Default)]
pub struct Pictures {
    loaded: HashMap<(PathBuf, u32), Option<Pixmap>>,
}

impl Pictures {
    /// `found` drawn at most `size` pixels on its longer side: an SVG
    /// rasterised at that size, a PNG scaled down to it (never up).
    fn get(&mut self, found: &Found, size: u32) -> Option<&Pixmap> {
        let key = (found.path.clone(), size);
        self.loaded
            .entry(key)
            .or_insert_with(|| {
                let fit = match found.kind {
                    Kind::Svg => compositor_image::Fit::Within(size, size),
                    Kind::Png => compositor_image::Fit::Natural,
                };
                let picture = compositor_image::load(&found.path, fit).ok()?;
                if found.kind == Kind::Png && (picture.width() > size || picture.height() > size) {
                    return shrink(&picture, size);
                }
                Some(picture)
            })
            .as_ref()
    }
}

/// `picture` scaled to fit `size`, keeping its shape.
fn shrink(picture: &Pixmap, size: u32) -> Option<Pixmap> {
    let longest = picture.width().max(picture.height());
    let scale = size as f32 / longest as f32;
    let width = ((picture.width() as f32 * scale).round() as u32).max(1);
    let height = ((picture.height() as f32 * scale).round() as u32).max(1);
    let mut out = Pixmap::new(width, height)?;
    let paint = PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    out.draw_pixmap(
        0,
        0,
        picture.as_ref(),
        &paint,
        Transform::from_scale(scale, scale),
        None,
    );
    Some(out)
}

/// A rounded rectangle's outline, with circular corners of radius `r`.
fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    if r <= 0.0 {
        return PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)?).into();
    }
    // The cubic that best follows a quarter circle.
    let k = r * 0.552_284_8;
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    p.line_to(x, y + r);
    p.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    p.close();
    p.finish()
}

/// Fill `path` in `color`, replacing or blending.
fn fill(pixmap: &mut PixmapMut<'_>, path: &tiny_skia::Path, color: Rgba, replace: bool) {
    let mut paint = Paint::default();
    paint.set_color(skia(color));
    paint.anti_alias = true;
    paint.blend_mode = if replace {
        BlendMode::Source
    } else {
        BlendMode::SourceOver
    };
    pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

/// Replace a rectangle with `color`: `pixman_image_fill_rectangles(SRC)`.
fn erase(pixmap: &mut PixmapMut<'_>, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
    if w <= 0 || h <= 0 {
        return;
    }
    let Some(rect) = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(skia(color));
    paint.blend_mode = BlendMode::Source;
    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
}

/// The draws of one frame.
#[derive(Debug)]
pub struct Painter {
    /// The fonts.
    pub look: Look,
    /// The sizes.
    pub geometry: Geometry,
    /// The icon file each entry has, by index into the list.
    pub icons: Vec<Option<Found>>,
    /// Whether any entry has an icon at all, which is what makes room for
    /// one in every row.
    pub have_icons: bool,
    /// The icons and large pictures, read once.
    pub pictures: Pictures,
    /// The first character of the input drawn, so the cursor stays in view.
    pub input_offset: usize,
}

impl Painter {
    /// Draw `launcher` as `config` says into `pixmap`, which is cleared.
    pub fn draw(&mut self, pixmap: &mut PixmapMut<'_>, launcher: &Launcher, config: &Config) {
        let g = self.geometry;
        let colors = &config.colors;
        // The background and the border.
        let (w, h) = (g.width as f32, g.height as f32);
        let bw = g.border as f32;
        let radius = g.radius() as f32;
        if let Some(outer) = rounded(0.0, 0.0, w, h, radius) {
            fill(pixmap, &outer, colors.border, true);
        }
        if let Some(inner) = rounded(bw, bw, w - 2.0 * bw, h - 2.0 * bw, (radius - bw).max(0.0)) {
            fill(pixmap, &inner, colors.background, true);
        }
        self.message(pixmap, config);
        if g.prompt_shown {
            self.prompt(pixmap, launcher, config);
        }
        if launcher.list_shown() {
            self.list(pixmap, launcher, config);
        }
    }

    /// `render_message`: the message's lines above the prompt.
    fn message(&mut self, pixmap: &mut PixmapMut<'_>, config: &Config) {
        let Some(message) = &config.message else {
            return;
        };
        let g = self.geometry;
        for (row, line) in message.split('\n').enumerate() {
            let top = g.border + g.y_margin + i32::try_from(row).unwrap_or(0) * g.row_height;
            let run = self.look.fonts.shape(&self.look.font, line);
            let fitting = cut(&run, (g.max_x() - g.text_x()) as f32);
            self.look.fonts.draw_run(
                &fitting,
                pixmap,
                g.text_x() as f32,
                (top + g.baseline) as f32,
                text_rgba(config.colors.message),
            );
        }
    }

    /// `render_prompt`: the prompt, the input or the placeholder, the cursor
    /// and the match counter.
    fn prompt(&mut self, pixmap: &mut PixmapMut<'_>, launcher: &Launcher, config: &Config) {
        let g = self.geometry;
        let top = g.prompt_y();
        let baseline = top + g.baseline;
        erase(
            pixmap,
            g.row_bg_x(),
            top,
            g.width - 2 * g.row_bg_x(),
            g.row_height,
            config.colors.background,
        );
        let mut max_x = g.max_x() as f32;
        if config.match_counter {
            max_x -= self.counter(pixmap, launcher, config);
        }
        let mut x = g.text_x() as f32;
        let prompt: String = launcher.prompt.prompt.iter().collect();
        let run = self.look.fonts.shape(&self.look.font, &prompt);
        let fitting = cut(&run, max_x - x);
        self.look.fonts.draw_run(
            &fitting,
            pixmap,
            x,
            baseline as f32,
            text_rgba(config.colors.prompt),
        );
        x += fitting.width;
        if fitting.glyphs.len() < run.glyphs.len() {
            return;
        }

        let typed = &launcher.prompt.text;
        let cursor = launcher.prompt.cursor;
        let cursor_height =
            (self.look.metrics.ascent + self.look.metrics.descent).min(g.row_height);
        let bar = |pixmap: &mut PixmapMut<'_>, at: f32| {
            erase(
                pixmap,
                at as i32,
                baseline + self.look.metrics.descent - cursor_height,
                self.look.metrics.underline_thickness,
                cursor_height,
                config.colors.input,
            );
        };
        if typed.is_empty() {
            bar(pixmap, x);
            let placeholder: String = launcher.prompt.placeholder.iter().collect();
            let run = self.look.fonts.shape(&self.look.font, &placeholder);
            let fitting = cut(&run, max_x - x);
            self.look.fonts.draw_run(
                &fitting,
                pixmap,
                x,
                baseline as f32,
                text_rgba(config.colors.placeholder),
            );
            return;
        }
        let password = config.password;
        let shown: Vec<char> = if password.enabled {
            match password.character {
                Some(c) => typed.iter().map(|_| c).collect(),
                None => Vec::new(),
            }
        } else {
            typed.clone()
        };
        if password.enabled && password.character.is_none() {
            // Nothing of the input is drawn, and the cursor stays put.
            bar(pixmap, x);
            return;
        }
        // Scroll by whole characters until the cursor is in view.
        if cursor == 0 {
            self.input_offset = 0;
        }
        self.input_offset = self
            .input_offset
            .min(cursor.saturating_sub(1))
            .min(shown.len());
        let width_of = |fonts: &mut Fonts, font: &Font, chars: &[char]| {
            let s: String = chars.iter().collect();
            fonts.shape(font, &s).width
        };
        while self.input_offset < cursor
            && x + width_of(
                &mut self.look.fonts,
                &self.look.font,
                shown.get(self.input_offset..cursor).unwrap_or_default(),
            ) > max_x
        {
            self.input_offset += 1;
        }
        let visible: String = shown
            .get(self.input_offset..)
            .unwrap_or_default()
            .iter()
            .collect();
        let run = self.look.fonts.shape(&self.look.font, &visible);
        let fitting = cut(&run, max_x - x);
        self.look.fonts.draw_run(
            &fitting,
            pixmap,
            x,
            baseline as f32,
            text_rgba(config.colors.input),
        );
        let before = width_of(
            &mut self.look.fonts,
            &self.look.font,
            shown.get(self.input_offset..cursor).unwrap_or_default(),
        );
        if x + before <= max_x {
            bar(pixmap, x + before);
        }
    }

    /// `render_match_count`: `matched/total` at the right of the prompt
    /// row. Answers its width.
    fn counter(&mut self, pixmap: &mut PixmapMut<'_>, launcher: &Launcher, config: &Config) -> f32 {
        let g = self.geometry;
        let total = launcher.apps.iter().filter(|a| a.visible).count();
        let shown = if launcher.prompt.text.is_empty() {
            total
        } else {
            launcher.matches.list.len()
        };
        let text = format!("{shown}/{total}");
        let run = self.look.fonts.shape(&self.look.font, &text);
        let right = (g.width - g.border - g.x_margin) as f32;
        // fuzzel centres the counter on its own formula, which is not the
        // prompt's baseline.
        let m = self.look.metrics;
        let y = g.border + g.y_margin + (g.row_height + m.height) / 2 - m.descent;
        self.look.fonts.draw_run(
            &run,
            pixmap,
            right - run.width,
            y as f32,
            text_rgba(config.colors.counter),
        );
        run.width
    }

    /// `render_match_list`: the rows of the current page.
    fn list(&mut self, pixmap: &mut PixmapMut<'_>, launcher: &Launcher, config: &Config) {
        let g = self.geometry;
        let page = launcher.matches.on_page();
        let rows = i32::try_from(page.len()).unwrap_or(0);
        // The empty rows after the last match.
        erase(
            pixmap,
            g.row_bg_x(),
            g.first_row_y() + rows * g.row_height,
            g.width - 2 * g.row_bg_x(),
            (g.lines - rows).max(0) * g.row_height,
            config.colors.background,
        );
        let selected = launcher.matches.index_on_page();
        for (row, m) in page.iter().enumerate() {
            self.row(pixmap, launcher, config, m, row, row == selected, rows);
        }
    }

    /// `render_one_match_entry`.
    #[expect(
        clippy::too_many_arguments,
        reason = "fuzzel's own entry point takes this many"
    )]
    fn row(
        &mut self,
        pixmap: &mut PixmapMut<'_>,
        launcher: &Launcher,
        config: &Config,
        m: &Match,
        row: usize,
        selected: bool,
        rows: i32,
    ) {
        let g = self.geometry;
        let row = i32::try_from(row).unwrap_or(0);
        let top = g.first_row_y() + row * g.row_height;
        let bg_x = g.row_bg_x();
        let bg_w = g.width - 2 * bg_x;
        erase(
            pixmap,
            bg_x,
            top,
            bg_w,
            g.row_height,
            config.colors.background,
        );
        let icon = self.icons.get(m.app).cloned().flatten();
        if selected {
            if let Some(path) = rounded(
                bg_x as f32,
                top as f32,
                bg_w as f32,
                g.row_height as f32,
                g.selection_corner() as f32,
            ) {
                fill(pixmap, &path, config.colors.selection, false);
            }
            // The large picture of the selected entry, if it is a drawing
            // and there is room under the list.
            if config.icons_enabled
                && let Some(found) = icon.as_ref().filter(|f| f.kind == Kind::Svg)
                && let Some((x, y, size)) = g.large_icon(config.image_size_ratio, rows)
                && let Some(picture) = self.pictures.get(found, size as u32)
            {
                pixmap.draw_pixmap(
                    x as i32,
                    y as i32,
                    picture.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
        }
        let mut x = g.text_x() as f32;
        if config.icons_enabled
            && let Some(found) = &icon
            && let Ok(size) = u32::try_from(g.icon_size)
            && let Some(picture) = self.pictures.get(found, size)
        {
            let y = top + (g.row_height - g.icon_size) / 2;
            pixmap.draw_pixmap(
                x as i32,
                y,
                picture.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        if config.icons_enabled && self.have_icons {
            x += (g.row_height + self.look.metrics.space_advance) as f32;
        }
        x += g.letter_spacing as f32;
        let Some(app) = launcher.apps.get(m.app) else {
            return;
        };
        // Newlines in a title are drawn as spaces.
        let title: String = app
            .title
            .iter()
            .map(|c| if *c == '\n' { ' ' } else { *c })
            .collect();
        let font = if selected {
            self.look.bold.clone()
        } else {
            self.look.font.clone()
        };
        let run = self.look.fonts.shape(&font, &title);
        let ellipsis = self.look.fonts.glyph_for(&font, '…');
        let ellipsis_width = ellipsis.map_or(0.0, |glyph| glyph.advance);
        let max_x = g.max_x() as f32 - ellipsis_width;
        let (regular, matched) = if selected {
            (config.colors.selection_text, config.colors.selection_match)
        } else {
            (config.colors.text, config.colors.matched)
        };
        let baseline = (top + g.baseline) as f32;
        // Which characters matched, by their byte offset in `title`.
        let offsets: Vec<usize> = title.char_indices().map(|(at, _)| at).collect();
        let is_match = |cluster: usize| {
            let index = offsets.partition_point(|at| *at < cluster);
            m.pos
                .iter()
                .any(|p| index >= p.start && index < p.start + p.len)
        };
        let spacing = g.letter_spacing as f32;
        let mut plain = Run {
            glyphs: Vec::new(),
            ..run.clone()
        };
        let mut hits = plain.clone();
        let mut cut_at = None;
        for (i, glyph) in run.glyphs.iter().enumerate() {
            let shift = spacing * i as f32;
            if x + glyph.x + shift + glyph.advance > max_x {
                cut_at = Some(x + glyph.x + shift);
                break;
            }
            let mut placed = *glyph;
            placed.x += shift;
            if is_match(glyph.cluster) {
                hits.glyphs.push(placed);
            } else {
                plain.glyphs.push(placed);
            }
        }
        let fonts = &mut self.look.fonts;
        fonts.draw_run(&plain, pixmap, x, baseline, text_rgba(regular));
        fonts.draw_run(&hits, pixmap, x, baseline, text_rgba(matched));
        if let (Some(at), Some(glyph)) = (cut_at, ellipsis) {
            let mut dots = Run {
                glyphs: vec![compositor_text::Glyph { x: 0.0, ..glyph }],
                ..run
            };
            dots.width = glyph.advance;
            fonts.draw_run(&dots, pixmap, at, baseline, text_rgba(regular));
        }
    }
}

/// The glyphs of `run` that fit in `width`, from its start.
fn cut(run: &Run, width: f32) -> Run {
    let glyphs: Vec<_> = run
        .glyphs
        .iter()
        .take_while(|glyph| glyph.x + glyph.advance <= width)
        .copied()
        .collect();
    let used = glyphs.last().map_or(0.0, |glyph| glyph.x + glyph.advance);
    Run {
        glyphs,
        width: used,
        ..run.clone()
    }
}
