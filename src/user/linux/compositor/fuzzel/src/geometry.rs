//! Where everything goes: the sizes fuzzel's `render.c` works out.
//!
//! `render_resized` sizes the window from the configuration and the font --
//! `width` is counted in the advance of an `o` -- and the rest places the
//! prompt row, the match rows, the selection and the icons inside it. It is
//! all arithmetic on the font's metrics, so it is here, apart from the
//! drawing, and tested against the numbers fuzzel's formulas give.

use crate::config::{Config, PtOrPx};

/// What the sizes depend on in the font, in pixels, as fcft rounds them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    /// Above the baseline.
    pub ascent: i32,
    /// Below the baseline, positive.
    pub descent: i32,
    /// A line's height: ascent, descent and the gap.
    pub height: i32,
    /// The advance of `o`, which `width` counts in.
    pub o_advance: i32,
    /// The advance of a space, which is between an icon and its title.
    pub space_advance: i32,
    /// The underline's thickness, which is the cursor's width.
    pub underline_thickness: i32,
}

/// How points become pixels here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scaling {
    /// The output's scale.
    pub scale: f32,
    /// The output's DPI.
    pub dpi: f32,
    /// Whether fonts are sized by DPI rather than by the scale.
    pub by_dpi: bool,
}

impl Scaling {
    /// One-to-one: scale 1, 96 DPI.
    pub const PLAIN: Self = Self {
        scale: 1.0,
        dpi: 96.0,
        by_dpi: true,
    };

    /// `pt_or_px_as_pixels`.
    #[must_use]
    pub fn pixels(self, size: PtOrPx) -> i32 {
        i32::try_from(size.pixels(self.scale, self.dpi, self.by_dpi)).unwrap_or(0)
    }
}

/// The window's sizes, in buffer pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// The buffer's width.
    pub width: i32,
    /// The buffer's height.
    pub height: i32,
    /// `horizontal-pad`, scaled.
    pub x_margin: i32,
    /// `vertical-pad`, scaled.
    pub y_margin: i32,
    /// `inner-pad`, scaled; zero with no lines.
    pub inner_pad: i32,
    /// The border's width.
    pub border: i32,
    /// The border's radius as configured, scaled.
    pub border_radius: i32,
    /// The selection's radius, scaled.
    pub selection_radius: i32,
    /// One row.
    pub row_height: i32,
    /// An icon in a row.
    pub icon_size: i32,
    /// The message's rows.
    pub message_height: i32,
    /// Where a line's baseline is from its top.
    pub baseline: i32,
    /// `letter-spacing`, in pixels.
    pub letter_spacing: i32,
    /// Whether the prompt row is drawn.
    pub prompt_shown: bool,
    /// Rows a page.
    pub lines: i32,
}

/// Scale a configured length, truncating as fuzzel's `unsigned` does.
fn scaled(value: u32, scale: f32) -> i32 {
    i32::try_from(value).map_or(i32::MAX, |v| (v as f32 * scale) as i32)
}

impl Geometry {
    /// fuzzel's `render_resized`, for a message of `message_lines` rows at
    /// most `longest_message` characters wide.
    #[must_use]
    pub fn new(
        config: &Config,
        font: &FontMetrics,
        scaling: Scaling,
        message_lines: i32,
        longest_message: i32,
    ) -> Self {
        let scale = scaling.scale;
        let x_margin = scaled(config.pad.x, scale);
        let y_margin = scaled(config.pad.y, scale);
        let inner_pad = if config.lines > 0 {
            scaled(config.pad.inner, scale)
        } else {
            0
        };
        let border = scaled(config.border.width, scale);
        let row_height = match config.line_height {
            Some(size) => scaling.pixels(size),
            None => font.height.max(font.ascent + font.descent),
        };
        let icon_size = (row_height - font.descent).max(0);
        let prompt_shown = !config.hide_prompt;
        let lines = i32::try_from(config.lines).unwrap_or(i32::MAX);
        let height = border
            + y_margin
            + if prompt_shown {
                row_height + inner_pad
            } else {
                0
            }
            + lines.saturating_mul(row_height)
            + message_lines * row_height
            + y_margin
            + border;
        let letter_spacing = scaling.pixels(config.letter_spacing);
        let chars = i32::try_from(config.chars)
            .unwrap_or(i32::MAX)
            .max(longest_message);
        let width = border
            + x_margin
            + (font.o_advance + letter_spacing)
                .max(0)
                .saturating_mul(chars)
            + x_margin
            + border;

        // `render_baseline`: a custom line height centres the glyphs.
        let font_height = font.ascent + font.descent;
        let glyph_top = if config.line_height.is_some() && row_height >= font_height {
            (f64::from(row_height - font_height) / 2.0).round() as i32
        } else {
            0
        };
        Self {
            // `wayl_resized` rounds through the logical size.
            width: round_through(width, scale),
            height: round_through(height, scale),
            x_margin,
            y_margin,
            inner_pad,
            border,
            border_radius: scaled(config.border.radius, scale),
            selection_radius: scaled(config.border.selection_radius, scale),
            row_height,
            icon_size,
            message_height: message_lines * row_height,
            baseline: row_height - glyph_top - font.descent,
            letter_spacing,
            prompt_shown,
            lines,
        }
    }

    /// The size the layer surface asks for, in logical pixels.
    #[must_use]
    pub fn logical_size(&self, scale: f32) -> (u32, u32) {
        let logical = |v: i32| (v as f32 / scale).round().max(0.0) as u32;
        (logical(self.width), logical(self.height))
    }

    /// The window's own corner radius: never more than the larger padding,
    /// so the selection cannot overlap the corners.
    #[must_use]
    pub fn radius(&self) -> i32 {
        self.border_radius.min(self.x_margin.max(self.y_margin))
    }

    /// Where a row's background starts: a third of the padding out from
    /// the text.
    #[must_use]
    pub fn row_bg_x(&self) -> i32 {
        self.border + self.x_margin - self.x_margin / 3
    }

    /// The top of the prompt row.
    #[must_use]
    pub fn prompt_y(&self) -> i32 {
        self.border + self.y_margin + self.message_height
    }

    /// The top of the first match row.
    #[must_use]
    pub fn first_row_y(&self) -> i32 {
        self.border
            + self.y_margin
            + if self.prompt_shown {
                self.row_height + self.inner_pad
            } else {
                0
            }
            + self.message_height
    }

    /// Where text starts.
    #[must_use]
    pub fn text_x(&self) -> i32 {
        self.border + self.x_margin
    }

    /// Where text must end.
    #[must_use]
    pub fn max_x(&self) -> i32 {
        self.width - self.border - self.x_margin
    }

    /// The selection's corner radius: at most half a row, and at most the
    /// padding.
    #[must_use]
    pub fn selection_corner(&self) -> i32 {
        self.selection_radius
            .min(self.row_height / 2)
            .min(self.x_margin)
    }

    /// The large icon's square, when the selected entry has an SVG icon and
    /// `image-size-ratio` leaves room for it below `rows` rows of matches.
    #[must_use]
    pub fn large_icon(&self, ratio: f32, rows: i32) -> Option<(f32, f32, f32)> {
        let (width, height) = (f64::from(self.width), f64::from(self.height));
        let ratio = f64::from(ratio);
        let size = (height * ratio).min(width * ratio);
        let x = (width - size) / 2.0;
        let bottom = (height - f64::from(self.first_row_y())).max(0.0);
        let y = (bottom - size).max(0.0);
        let list_end = f64::from(self.first_row_y() + rows * self.row_height);
        (size > 0.0 && y > list_end + f64::from(self.row_height)).then_some((
            x as f32,
            y as f32,
            size as f32,
        ))
    }

    /// Which row of the list a point is in, as `render_get_row_num` says.
    #[must_use]
    pub fn row_at(&self, x: i32, y: i32, rows: i32) -> Option<i32> {
        let first = self.first_row_y();
        let last = first + rows * self.row_height;
        let min_x = self.row_bg_x();
        let max_x = self.width - min_x;
        (y >= first && y < last && x >= min_x && x < max_x).then(|| (y - first) / self.row_height)
    }
}

/// `roundf(roundf(v / scale) * scale)`.
fn round_through(value: i32, scale: f32) -> i32 {
    ((value as f32 / scale).round() * scale).round() as i32
}

#[cfg(test)]
mod tests {
    use super::{FontMetrics, Geometry, Scaling};
    use crate::config::{Config, PtOrPx};

    /// Round numbers for a 16-point face at 96 DPI.
    const FONT: FontMetrics = FontMetrics {
        ascent: 17,
        descent: 5,
        height: 25,
        o_advance: 11,
        space_advance: 5,
        underline_thickness: 1,
    };

    fn users() -> Config {
        let mut c = Config::defaults(1);
        c.chars = 42;
        c.lines = 12;
        c.pad.x = 32;
        c.pad.y = 26;
        c.pad.inner = 14;
        c.line_height = Some(PtOrPx::Pt(26.0));
        c.border.width = 2;
        c.border.radius = 20;
        c
    }

    #[test]
    fn the_users_window() {
        let g = Geometry::new(&users(), &FONT, Scaling::PLAIN, 0, 0);
        // line-height=26 is points: 26 × 96 / 72 = 34.67, so 35 pixels.
        assert_eq!(g.row_height, 35);
        assert_eq!(g.width, 2 + 32 + 11 * 42 + 32 + 2);
        assert_eq!(g.height, 2 + 26 + 35 + 14 + 12 * 35 + 26 + 2);
        // Centred in the 35-pixel row: (35 - 22) / 2 rounds to 7.
        assert_eq!(g.baseline, 35 - 7 - 5);
        assert_eq!(g.icon_size, 30);
        assert_eq!(g.radius(), 20);
        assert_eq!(g.row_bg_x(), 2 + 32 - 10);
        assert_eq!(g.first_row_y(), 2 + 26 + 35 + 14);
        assert_eq!(g.logical_size(1.0), (530, 525));
        // With four matches the large icon fits below them; with five not.
        assert!(g.large_icon(0.32, 4).is_some());
        assert!(g.large_icon(0.32, 5).is_none());
        assert_eq!(g.row_at(100, g.first_row_y() + 36, 12), Some(1));
        assert_eq!(g.row_at(5, g.first_row_y() + 36, 12), None);
    }

    #[test]
    fn defaults_follow_the_font() {
        let g = Geometry::new(&Config::defaults(1), &FONT, Scaling::PLAIN, 0, 0);
        assert_eq!(g.row_height, 25);
        assert_eq!(g.baseline, 25 - 5);
        assert_eq!(g.width, 1 + 40 + 11 * 30 + 40 + 1);
        assert_eq!(g.height, 1 + 8 + 25 + 15 * 25 + 8 + 1);
        // Radius 10 is within the padding.
        assert_eq!(g.radius(), 10);
    }

    #[test]
    fn scale_two() {
        let scaling = Scaling {
            scale: 2.0,
            dpi: 96.0,
            by_dpi: false,
        };
        let g = Geometry::new(&users(), &FONT, scaling, 0, 0);
        assert_eq!(g.x_margin, 64);
        // Points by the scale: 26 × 2 × 96 / 72.
        assert_eq!(g.row_height, 69);
        assert_eq!(g.width % 2, 0);
        let (w, h) = g.logical_size(2.0);
        assert_eq!((w * 2, h * 2), (g.width as u32, g.height as u32));
    }
}
