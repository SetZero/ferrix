//! Text and images for the bar, on the clients' foundation:
//! `compositor/text` lays labels out and draws them as Pango would, and
//! `compositor/image` rasterises the stylesheet's `url()`s.
//!
//! A label's font is its computed style's: `font-family` in order, the
//! weight, the style, and the size in pixels as GTK3 hands it to Pango
//! (`pango_font_description_set_absolute_size`). Its markup is Pango markup;
//! markup Pango would refuse is refused here too, and GTK's label then logs
//! `Failed to set text from markup due to error parsing markup` and shows
//! nothing new, which is what this shows.
//!
//! Drawn at a buffer scale, the text is laid out again at the scale's size,
//! so glyphs are drawn at the screen's resolution rather than stretched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use compositor_text::markup::{self, Parsed, Rgba as TextRgba};
use compositor_text::{
    Ellipsize, FontDescription, Fonts, Layout, LayoutOptions, Size, Style as FontStyleKind, Weight,
};
use tiny_skia::{Pixmap, PixmapMut, Transform};

use crate::app::Engine;
use crate::css::property::FontStyle;
use crate::css::style::Style;
use crate::css::value::Rgba;
use crate::diag::Diagnostics;
use crate::layout::{Measure, TextSize};
use crate::paint::{Images, Text, TextJob};

/// The fonts, and what has been said about markup that did not parse.
#[derive(Debug)]
pub struct TextEngine {
    fonts: Fonts,
    /// Markup already warned about, so a module updating every second does
    /// not say it every second.
    warned: std::collections::BTreeSet<String>,
    /// The lines to say.
    pub diag: Diagnostics,
}

impl TextEngine {
    /// The machine's fonts, found as fontconfig would.
    #[must_use]
    pub fn system() -> Self {
        Self::with_fonts(Fonts::system())
    }

    /// Given fonts (a test's, or `--fonts-dir`'s).
    #[must_use]
    pub fn with_fonts(fonts: Fonts) -> Self {
        Self {
            fonts,
            warned: std::collections::BTreeSet::new(),
            diag: Diagnostics::default(),
        }
    }

    fn description(style: &Style, scale: f32) -> FontDescription {
        FontDescription {
            families: style.font_family.clone(),
            weight: Weight(style.font_weight),
            style: match style.font_style {
                FontStyle::Normal => FontStyleKind::Normal,
                FontStyle::Italic => FontStyleKind::Italic,
                FontStyle::Oblique => FontStyleKind::Oblique,
            },
            size: Size::Pixels(style.font_size * scale),
        }
    }

    fn parse(&mut self, text: &str) -> Parsed {
        match markup::parse(text) {
            Ok(parsed) => parsed,
            Err(error) => {
                if self.warned.insert(text.to_owned()) {
                    self.diag.warn(format!(
                        "Failed to set text '{text}' from markup due to error parsing markup: {error}"
                    ));
                }
                markup::plain("")
            }
        }
    }

    fn options(style: &Style, scale: f32, color: Rgba) -> LayoutOptions {
        let [r, g, b, a] = color.bytes();
        LayoutOptions {
            font: Self::description(style, scale),
            color: TextRgba { r, g, b, a },
            ..LayoutOptions::default()
        }
    }

    fn lay_out(
        &mut self,
        text: &str,
        style: &Style,
        scale: f32,
        width: Option<f32>,
        wrap: bool,
        color: Option<Rgba>,
    ) -> Layout {
        let parsed = self.parse(text);
        let mut spans = parsed.spans;
        if let Some(color) = color {
            // A text shadow is the text in the shadow's colour, whatever
            // colours its spans have.
            let [r, g, b, a] = color.bytes();
            for span in &mut spans {
                span.style.foreground = Some(TextRgba { r, g, b, a });
            }
        }
        let mut options = Self::options(style, scale, color.unwrap_or(style.color));
        options.max_width = width.map(|w| w * scale);
        if wrap {
            options.wrap = true;
        } else if width.is_some() {
            options.ellipsize = Ellipsize::End;
        }
        self.fonts.layout(&spans, &options)
    }
}

impl Measure for TextEngine {
    fn text(&mut self, markup: &str, style: &Style, wrap_at: Option<f32>) -> TextSize {
        let layout = self.lay_out(markup, style, 1.0, wrap_at, wrap_at.is_some(), None);
        let font = self.fonts.resolve(&Self::description(style, 1.0));
        let metrics = self.fonts.metrics(&font);
        let ellipsis = self.fonts.measure(&font, "…");
        TextSize {
            width: layout.width.ceil(),
            height: layout.height.ceil(),
            min_width: if wrap_at.is_some() {
                layout.width.ceil()
            } else {
                ellipsis.ceil()
            },
            char_width: metrics
                .approximate_char_width
                .max(metrics.approximate_digit_width),
        }
    }
}

impl Text for TextEngine {
    fn draw(&mut self, pixmap: &mut PixmapMut<'_>, job: &TextJob<'_>, transform: Transform) {
        let scale = transform.sx;
        let natural = self.text(job.markup, job.style, None).width;
        let squeezed = (job.rect.width + 0.5 < natural).then_some(job.rect.width);
        let width = if job.wrap {
            Some(job.rect.width)
        } else {
            squeezed
        };
        let layout = self.lay_out(job.markup, job.style, scale, width, job.wrap, job.color);
        self.fonts.draw_layout(
            &layout,
            pixmap,
            (job.rect.x * scale).floor(),
            (job.rect.y * scale).floor(),
        );
    }
}

impl Engine for TextEngine {
    fn diagnostics(&mut self) -> Option<&mut Diagnostics> {
        Some(&mut self.diag)
    }
}

/// The stylesheet's images, each rasterised once at its own size and scale
/// and kept.
#[derive(Debug, Default)]
pub struct ImageCache {
    loaded: BTreeMap<(PathBuf, u32), Option<Pixmap>>,
}

impl Images for ImageCache {
    fn get(&mut self, path: &Path, scale: f32) -> Option<&Pixmap> {
        #[expect(clippy::cast_possible_truncation, reason = "an integer buffer scale")]
        #[expect(clippy::cast_sign_loss, reason = "a scale is positive")]
        let whole = scale.round().max(1.0) as u32;
        let key = (path.to_path_buf(), whole);
        self.loaded
            .entry(key)
            .or_insert_with(|| {
                // GTK loads an image at its own size; at a buffer scale,
                // at that size times the scale, so it is not stretched.
                let natural = compositor_image::load(path, compositor_image::Fit::Natural).ok()?;
                if whole == 1 {
                    return Some(natural);
                }
                compositor_image::load(
                    path,
                    compositor_image::Fit::Exactly(
                        natural.width() * whole,
                        natural.height() * whole,
                    ),
                )
                .ok()
            })
            .as_ref()
    }
}
