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

/// One row of a grid as it was painted: its cells, and the column the cursor
/// was drawn on if it was on this row.
///
/// What a terminal that draws only what changed has to know about what was
/// there before: a row is painted again when this differs from the row it
/// would paint now, and at no other time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Painted {
    cells: Vec<crate::grid::Cell>,
    cursor: Option<usize>,
}

impl Painted {
    /// Row `row` of `grid`, as [`draw_rows`] would paint it now.
    #[must_use]
    pub fn of(grid: &Grid, row: usize) -> Self {
        let (columns, _) = grid.size();
        Self {
            cells: (0..columns)
                .filter_map(|column| grid.cell(column, row).copied())
                .collect(),
            cursor: (grid.cursor_visible() && grid.cursor().1 == row).then(|| grid.cursor().0),
        }
    }
}

/// Draw `grid` into `pixels`, a `width` by `height` buffer of `XRGB8888`
/// with `stride` bytes a row.
///
/// The whole buffer is painted: the background first, then a glyph a cell,
/// then the cursor over the cell it is on. This is for a buffer that holds
/// nothing yet, which is what a terminal is handed whenever its window is
/// resized; [`draw_rows`] is for one that holds the frame before.
pub fn draw(
    pixels: &mut [u8],
    (width, height): (usize, usize),
    stride: usize,
    grid: &Grid,
    colours: &Colours,
    scale: usize,
) {
    // `wl_shm`'s `XRGB8888` is little-endian, which is blue, green, red and
    // a byte nothing reads: `fbtext`'s `Bgrx`.
    if let Some(mut surface) = Surface::new(pixels, width, height, stride / 4, PixelOrder::Bgrx) {
        surface.fill(colours.background);
    }
    let (_, rows) = grid.size();
    draw_rows(
        pixels,
        (width, height),
        stride,
        grid,
        colours,
        scale,
        0..rows,
    );
}

/// Where row `row` is in a buffer at `scale`: its top and its height, in
/// buffer pixels.
#[must_use]
pub fn row_span(row: usize, scale: usize) -> (usize, usize) {
    let tall = CELL.1 * scale.max(1);
    (row * tall, tall)
}

/// Paint `rows` of `grid` again, over whatever the buffer holds: each row's
/// background, then its glyphs, then the cursor if it is on it.
///
/// A key typed into a shell changes one row of sixty-four. Painting all of
/// them, and telling the compositor all of the window changed, makes every
/// letter a whole window for the terminal to paint, for the compositor to
/// draw again and for the card to send to the screen -- which on a guest
/// with one processor was most of a second a letter.
pub fn draw_rows(
    pixels: &mut [u8],
    (width, height): (usize, usize),
    stride: usize,
    grid: &Grid,
    colours: &Colours,
    scale: usize,
    rows: core::ops::Range<usize>,
) {
    let scale = scale.max(1);
    let Some(mut surface) = Surface::new(pixels, width, height, stride / 4, PixelOrder::Bgrx)
    else {
        return;
    };
    let (columns, _) = grid.size();
    for row in rows {
        let (top, tall) = row_span(row, scale);
        if top >= height {
            break;
        }
        surface.fill_rect(0, top, width, tall.min(height - top), colours.background);
        for column in 0..columns {
            let Some(cell) = grid.cell(column, row) else {
                continue;
            };
            let (x, y) = (column * CELL.0 * scale, top);
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
