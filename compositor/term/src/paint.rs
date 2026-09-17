//! Drawing a grid into a window's buffer.
//!
//! The font is `libs/fbtext`'s -- Spleen 8x16, which the kernel's panic
//! screen already carries, so the terminal and the panic report are written
//! in one typeface and the image carries one font. `fbtext` draws a glyph
//! into a linear 32-bit framebuffer, which is exactly what a `wl_shm` buffer
//! is.
//!
//! Every length here is in buffer pixels: a terminal on a monitor at
//! `scale = 2` is handed a buffer twice the size and draws its glyphs twice
//! as large, which is what a client on a scaled output does.

use ferrix_fbtext::{PixelOrder, Rgb, Surface};

use crate::grid::Grid;

/// The font's cell, in pixels: `libs/fbtext` is Spleen 8x16.
pub const CELL: (usize, usize) = (8, 16);

/// What the terminal is drawn in.
#[derive(Clone, Copy, Debug)]
pub struct Colours {
    /// Behind the text.
    pub background: Rgb,
    /// The eight colours a cell may take, and the eight bright ones.
    pub palette: [Rgb; 8],
    /// The cursor's block.
    pub cursor: Rgb,
}

impl Default for Colours {
    /// The colours a terminal has had since the VT100's successors: black,
    /// red, green, yellow, blue, magenta, cyan and white, over a background
    /// dark enough to read them on.
    fn default() -> Self {
        Self {
            background: Rgb::new(0x10, 0x10, 0x18),
            palette: [
                Rgb::new(0x20, 0x20, 0x28),
                Rgb::new(0xCC, 0x44, 0x44),
                Rgb::new(0x44, 0xCC, 0x66),
                Rgb::new(0xCC, 0xAA, 0x44),
                Rgb::new(0x44, 0x88, 0xCC),
                Rgb::new(0xAA, 0x66, 0xCC),
                Rgb::new(0x44, 0xCC, 0xCC),
                Rgb::new(0xCC, 0xCC, 0xD0),
            ],
            cursor: Rgb::new(0xCC, 0xCC, 0xD0),
        }
    }
}

/// How many columns and rows fit in a window of this many pixels, at
/// `scale` buffer pixels to a font pixel.
#[must_use]
pub fn fits(width: usize, height: usize, scale: usize) -> (usize, usize) {
    let scale = scale.max(1);
    (
        (width / (CELL.0 * scale)).max(1),
        (height / (CELL.1 * scale)).max(1),
    )
}

/// Draw `grid` into `pixels`, a `width` by `height` buffer of `XRGB8888`
/// with `stride` bytes a row.
///
/// The whole buffer is painted: the background first, then a glyph a cell,
/// then the cursor over the cell it is on. A terminal that drew only what
/// changed would need to know what was there before, and this one is handed
/// a fresh buffer whenever the window is resized.
pub fn draw(
    pixels: &mut [u8],
    (width, height): (usize, usize),
    stride: usize,
    grid: &Grid,
    colours: &Colours,
    scale: usize,
) {
    let scale = scale.max(1);
    // `wl_shm`'s `XRGB8888` is little-endian, which is blue, green, red and
    // a byte nothing reads: `fbtext`'s `Bgrx`.
    let Some(mut surface) = Surface::new(pixels, width, height, stride / 4, PixelOrder::Bgrx)
    else {
        return;
    };
    surface.fill(colours.background);
    let (columns, rows) = grid.size();
    for row in 0..rows {
        for column in 0..columns {
            let Some(cell) = grid.cell(column, row) else {
                continue;
            };
            let (x, y) = (column * CELL.0 * scale, row * CELL.1 * scale);
            if y >= height {
                break;
            }
            // The cursor is a block the text is drawn out of, which is what
            // a terminal with no blinking draws.
            let on_cursor = grid.cursor_visible() && grid.cursor() == (column, row);
            let (fg, bg) = if on_cursor {
                (colours.background, Some(colours.cursor))
            } else {
                (colour(cell.colour, cell.bold, colours), None)
            };
            if cell.ch == ' ' && bg.is_none() {
                continue;
            }
            surface.draw_char(x, y, cell.ch, scale, fg, bg);
        }
    }
}

/// One cell's colour: the palette's, brightened when it is bold.
fn colour(index: u8, bold: bool, colours: &Colours) -> Rgb {
    let base = colours
        .palette
        .get(usize::from(index).min(7))
        .copied()
        .unwrap_or(Rgb::new(0xCC, 0xCC, 0xD0));
    if !bold {
        return base;
    }
    // Bold is the same hue, brighter: half the way to white, which is what a
    // terminal with eight colours and a bright bit does.
    let lift = |value: u8| value.saturating_add((0xFF - value) / 2);
    Rgb::new(lift(base.r), lift(base.g), lift(base.b))
}
