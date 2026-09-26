//! `caption`: a line of text in a named font on a layer surface.
//!
//! The smallest program built on the desktop clients' whole foundation --
//! `userland/compositor/toolkit` for the surface, `userland/compositor/text` for the font --
//! and what `cargo xtask test-compositor`'s `caption` boot runs with the
//! user's own font carried in. The same code draws the same pixels on the
//! host with `--render`, which is the picture the boot's screendump is
//! compared against: the font is the user's and is never committed, so the
//! expected image cannot be either, and is made at gate time from the same
//! file instead.

use compositor_text::{FontDescription, Fonts, LayoutOptions, Rgba, Size, markup};

/// Where the surface is held: this far from the screen's top-left corner.
pub const MARGIN: i32 = 40;

/// Space around the text inside the surface.
pub const PADDING: u32 = 16;

/// The surface's colour, opaque so what the compositor shows of it is
/// exactly what was drawn.
pub const BACKGROUND: Rgba = Rgba::rgb(0x20, 0x28, 0x30);

/// The text's colour.
pub const FOREGROUND: Rgba = Rgba::rgb(0xf0, 0xf0, 0xf0);

/// What `caption` is asked to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// A Pango font description: `"Ubuntu Light"`.
    pub font: String,
    /// Its size in points.
    pub points: f32,
    /// The text, Pango markup allowed.
    pub text: String,
}

/// The surface's pixels, and a line saying which face drew them.
///
/// # Errors
///
/// A size that cannot be allocated.
pub fn draw(fonts: &mut Fonts, request: &Request) -> Result<(tiny_skia::Pixmap, String), String> {
    let mut description = FontDescription::pango(&request.font);
    description.size = Size::Points(request.points);
    let font = fonts.resolve(&description);
    let face = font
        .primary()
        .and_then(|face| fonts.info(face))
        .map_or_else(
            || "no face".to_owned(),
            |info| format!("{} ({})", info.path.display(), info.full_name),
        );
    let parsed = markup::parse(&request.text).unwrap_or_else(|_| markup::plain(&request.text));
    let options = LayoutOptions {
        font: description,
        color: FOREGROUND,
        ..LayoutOptions::default()
    };
    let layout = fonts.layout(&parsed.spans, &options);
    let (width, height) = layout.pixel_size();
    let (width, height) = (width + 2 * PADDING, height + 2 * PADDING);
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).ok_or_else(|| format!("a {width}x{height} surface"))?;
    pixmap.fill(BACKGROUND.to_skia());
    fonts.draw_layout(&layout, &mut pixmap.as_mut(), PADDING as f32, PADDING as f32);
    let said = format!("{} {}pt -> {face}, {width}x{height}", request.font, request.points);
    Ok((pixmap, said))
}

/// `pixmap` as a binary PPM, the format QEMU's screendump writes.
#[must_use]
pub fn ppm(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", pixmap.width(), pixmap.height()).into_bytes();
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        out.extend([color.red(), color.green(), color.blue()]);
    }
    out
}

pub use compositor_text::tiny_skia;
