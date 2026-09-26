//! Text for Ferrix's desktop clients.
//!
//! waybar and hyprlock draw with Pango, fuzzel with fcft; all three find a
//! face with fontconfig. None of those is on Ferrix, and all three are C.
//! This crate is the part of them the user's files reach:
//!
//! * **Finding a face** ([`Fonts`], [`FontDescription`]). A family name, a
//!   weight and a style -- `"Ubuntu"`, `"Ubuntu Light"`, `"GFS Didot"`,
//!   `sans-serif` -- are matched against the faces under the font
//!   directories the way fontconfig matches them: the family first (with
//!   the generic names mapped onto what is there), then the closest weight,
//!   then the style. Both of the spellings the user's files use parse:
//!   Pango's `"Ubuntu Light 11"` ([`FontDescription::pango`]) and
//!   fontconfig's `"GFS Didot:size=16"` ([`FontDescription::fontconfig`]).
//! * **Shaping** ([`Fonts::shape`]): rustybuzz, so kerning and ligatures are
//!   `HarfBuzz`'s, with each character that the first face lacks taken from
//!   the next face down the family list, and then from any face that has it.
//! * **Glyphs** ([`Fonts::glyph`]): each outline filled with antialiasing by
//!   tiny-skia into an alpha mask, cached by face, glyph, size and quarter
//!   pixel of horizontal position.
//! * **Layout** ([`Fonts::layout`]): lines broken at `\n` (and at spaces
//!   when a width is given), aligned, ellipsized, with Pango's line height,
//!   measured by Pango's *logical* rectangle -- which is what GTK sizes a
//!   label by and hyprlock sizes its label texture by.
//! * **Markup** ([`markup`]): the Pango subset the user's files use --
//!   `<span>` with `foreground`, `background`, `font_weight`, `size`,
//!   `line_height` and the rest, `<b> <i> <u> <s> <tt> <small> <big> <sub>
//!   <sup>`, and the five entities with numeric ones.
//!
//! Sizes: Pango and fontconfig sizes are points, and both turn them into
//! pixels at 96 dots per inch (hyprgraphics' `TextResource`, fcft's
//! default), so [`Size::Points`] of 16 is 21⅓ pixels. CSS `px` is
//! [`Size::Pixels`].

mod colors;
mod describe;
mod face;
mod fonts;
mod layout;
pub mod markup;
mod raster;
mod scan;
mod shape;

pub use describe::{FontDescription, Size, Style, Weight};
pub use face::{FaceId, FaceInfo, Metrics};
pub use fonts::Fonts;
pub use layout::{Align, Ellipsize, Layout, LayoutOptions, Line, Piece, plain_spans};
pub use markup::{Rgba, Span, SpanStyle};
pub use raster::Mask;
pub use shape::{Font, Glyph, Run};

/// The rasteriser text is drawn with, at the version this crate pins.
pub use tiny_skia;

/// Pango's and fcft's dots per inch, which turn a point size into pixels.
pub const DPI: f32 = 96.0;

#[cfg(test)]
mod tests;
