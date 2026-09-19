//! Drawing a grid into a window's buffer.
//!
//! The font is Hack, at 20 pixels per em in a 12x24 cell: `font.rs`, which
//! `scripts/gen-term-font.py` rasterises from the TrueType outlines vendored
//! in `font/`. The panic screen's 8x16 bitmap is the right font for a panic --
//! no filesystem, no allocator, every byte a byte of kernel image -- and the
//! wrong one for the thing a person reads all day, which is why the terminal
//! carries its own.
//!
//! A cell is coverage, one byte a pixel, so a stem that falls between two
//! pixels is two grey pixels rather than one snapped to the grid. Drawing is
//! therefore a blend: the cell's background is painted first and known, so the
//! colour of a pixel is [`mix`] of the two, written straight out rather than
//! read back. Runs of equal coverage are filled in one call, as the panic
//! screen's runs of set bits are, because most of a glyph's row is one value.
//!
//! Every length here is in buffer pixels: a terminal on a monitor at
//! `scale = 2` is handed a buffer twice the size and draws its glyphs twice
//! as large, which is what a client on a scaled output does.

use ferrix_fbtext::{PixelOrder, Rgb, Surface};

use crate::font;
use crate::grid::Grid;

/// The font's cell, in pixels: Hack at 20 pixels per em is 12 by 24.
pub const CELL: (usize, usize) = (font::WIDTH, font::HEIGHT);

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
/// The whole buffer is painted: the background first, then the cursor's
/// block, then a glyph a cell. A terminal that drew only what changed would
/// need to know what was there before, and this one is handed a fresh buffer
/// whenever the window is resized.
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
                surface.fill_rect(x, y, CELL.0 * scale, CELL.1 * scale, colours.cursor);
                (colours.background, colours.cursor)
            } else {
                (colour(cell.colour, cell.bold, colours), colours.background)
            };
            if cell.ch == ' ' {
                continue;
            }
            glyph(&mut surface, (x, y), cell.ch, cell.bold, scale, (fg, bg));
        }
    }
}

/// Blend one glyph over a cell whose background is already `bg`.
///
/// A row of coverage is drawn as runs of one value: a filled span is one
/// `fill_rect`, and a span of no coverage is the background, which is already
/// there.
fn glyph(
    surface: &mut Surface<'_>,
    (x, y): (usize, usize),
    ch: char,
    bold: bool,
    scale: usize,
    (fg, bg): (Rgb, Rgb),
) {
    let cell = cell(ch, bold);
    for row in 0..font::HEIGHT {
        let top = y + row * scale;
        if top >= surface.height() {
            return;
        }
        let Some(line) = cell.get(row * font::WIDTH..(row + 1) * font::WIDTH) else {
            return;
        };
        let mut start = 0;
        while start < font::WIDTH {
            let Some(&value) = line.get(start) else {
                return;
            };
            let end = (start..font::WIDTH)
                .find(|column| line.get(*column) != Some(&value))
                .unwrap_or(font::WIDTH);
            if value != 0 {
                let left = x + start * scale;
                let run = (end - start) * scale;
                surface.fill_rect(left, top, run, scale, mix(bg, fg, value));
            }
            start = end;
        }
    }
}

/// The coverage cell for `ch` in the face the cell asks for.
///
/// Printable ASCII has its own glyph; every other character, control
/// characters included, is drawn as a hollow box, as the panic screen's font
/// draws one.
fn cell(ch: char, bold: bool) -> &'static font::Cell {
    let (glyphs, replacement) = if bold {
        (&font::BOLD, &font::BOLD_REPLACEMENT)
    } else {
        (&font::REGULAR, &font::REGULAR_REPLACEMENT)
    };
    u32::from(ch)
        .checked_sub(font::FIRST)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| glyphs.get(index))
        .unwrap_or(replacement)
}

/// `bg` where `coverage` is zero, `fg` where it is `0xFF`, and the line
/// between them elsewhere.
///
/// The blend is on the values in the buffer, which is what every terminal
/// without a colour-managed compositor behind it does: the alternative is to
/// linearise, blend and encode again, and on the greys a terminal actually
/// uses the difference is less than a level.
fn mix(bg: Rgb, fg: Rgb, coverage: u8) -> Rgb {
    let channel = |from: u8, to: u8| {
        let (from, to) = (u32::from(from), u32::from(to));
        let value = from * u32::from(0xFF - coverage) + to * u32::from(coverage) + 0x7F;
        u8::try_from(value / 0xFF).unwrap_or(0xFF)
    };
    Rgb::new(
        channel(bg.r, fg.r),
        channel(bg.g, fg.g),
        channel(bg.b, fg.b),
    )
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
