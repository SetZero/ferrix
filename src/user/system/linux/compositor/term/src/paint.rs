//! Drawing a grid into a window's buffer.
//!
//! The font is Hack, at 20 pixels per em in a 12x24 cell: `font.rs`, which
//! `tools/common/gen/gen-term-font.py` rasterises from the TrueType outlines vendored
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
//! Braille, U+2800 to U+28FF, is not in Hack, and is drawn here instead: a
//! character is eight dots in two columns of four, each on or off by one bit
//! of its code point. btop draws every graph it has in braille, two samples a
//! cell across and four levels a cell high, and so do most programs that plot
//! in a terminal; foot, kitty and alacritty draw it themselves for the same
//! reason.
//!
//! Every length here is in buffer pixels: a terminal on a monitor at
//! `scale = 2` is handed a buffer twice the size and draws its glyphs twice
//! as large, which is what a client on a scaled output does.

use ferrix_fbtext::{PixelOrder, Rgb, Surface};

use crate::font;
use crate::grid::{CellDamage, Grid};

/// The font's cell, in pixels: Hack at 20 pixels per em is 12 by 24.
pub const CELL: (usize, usize) = (font::WIDTH, font::HEIGHT);

/// What the terminal is drawn in.
#[derive(Clone, Copy, Debug)]
pub struct Colours {
    /// Behind the text.
    pub background: Rgb,
    /// The sixteen colours a cell may take by number: the eight, then the
    /// eight bright ones. A program asking for any other colour gets it
    /// exactly (`grid::Cell::rgb`).
    pub palette: [Rgb; 16],
    /// The cursor's block.
    pub cursor: Rgb,
    /// Behind selected text.
    pub selection: Rgb,
}

impl Default for Colours {
    /// The colours a terminal has had since the VT100's successors: black,
    /// red, green, yellow, blue, magenta, cyan and white, then each half the
    /// way to white for its bright form -- what bold used to draw them as --
    /// over a background dark enough to read them on.
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
                Rgb::new(0x8F, 0x8F, 0x93),
                Rgb::new(0xE5, 0xA1, 0xA1),
                Rgb::new(0xA1, 0xE5, 0xB2),
                Rgb::new(0xE5, 0xD4, 0xA1),
                Rgb::new(0xA1, 0xC3, 0xE5),
                Rgb::new(0xD4, 0xB2, 0xE5),
                Rgb::new(0xA1, 0xE5, 0xE5),
                Rgb::new(0xE5, 0xE5, 0xE7),
            ],
            cursor: Rgb::new(0xCC, 0xCC, 0xD0),
            selection: Rgb::new(0x3A, 0x4A, 0x6A),
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
    // A terminal window is not necessarily an exact multiple of the cell
    // size. The initial/full path must paint those trailing pixels too;
    // incremental damage is always cell-aligned and can keep them.
    {
        let Some(mut surface) = Surface::new(pixels, width, height, stride / 4, PixelOrder::Bgrx)
        else {
            return;
        };
        surface.fill(colours.background);
    }
    let (columns, rows) = grid.size();
    draw_damage(
        pixels,
        (width, height),
        stride,
        grid,
        colours,
        scale,
        CellDamage::full(columns, rows),
    );
}

/// Draw only `damage`'s cells into a buffer that already holds the preceding
/// frame. [`draw`] is the initial/full-buffer form; the Wayland client uses
/// this after a small terminal update so its rendering and surface damage have
/// the same boundary.
pub fn draw_damage(
    pixels: &mut [u8],
    (width, height): (usize, usize),
    stride: usize,
    grid: &Grid,
    colours: &Colours,
    scale: usize,
    damage: CellDamage,
) {
    let scale = scale.max(1);
    // `wl_shm`'s `XRGB8888` is little-endian, which is blue, green, red and
    // a byte nothing reads: `fbtext`'s `Bgrx`.
    let Some(mut surface) = Surface::new(pixels, width, height, stride / 4, PixelOrder::Bgrx)
    else {
        return;
    };
    let (columns, rows) = grid.size();
    let left = damage.left.min(columns);
    let top = damage.top.min(rows);
    let right = damage.left.saturating_add(damage.width).min(columns);
    let bottom = damage.top.saturating_add(damage.height).min(rows);
    let cell_width = CELL.0 * scale;
    let cell_height = CELL.1 * scale;
    surface.fill_rect(
        left * cell_width,
        top * cell_height,
        right.saturating_sub(left) * cell_width,
        bottom.saturating_sub(top) * cell_height,
        colours.background,
    );
    for row in top..bottom {
        for column in left..right {
            let Some((cell, selected)) = grid.shown(column, row) else {
                continue;
            };
            let (x, y) = (column * cell_width, row * cell_height);
            if y >= height {
                break;
            }
            // The cursor is a block the text is drawn out of, which is what
            // a terminal with no blinking draws.
            let on_cursor = grid.shown_cursor() == Some((column, row));
            let fg = match cell.rgb {
                Some([r, g, b]) => Rgb::new(r, g, b),
                None => colour(cell.colour, colours),
            };
            let (fg, bg) = if on_cursor {
                surface.fill_rect(x, y, CELL.0 * scale, CELL.1 * scale, colours.cursor);
                (colours.background, colours.cursor)
            } else if selected {
                // Selected text keeps its own colour over the selection's,
                // so a selected prompt still reads as that prompt.
                surface.fill_rect(x, y, CELL.0 * scale, CELL.1 * scale, colours.selection);
                (fg, colours.selection)
            } else if let Some(index) = cell.background {
                // A cell of its own colour: a prompt's segments, a
                // highlighted line, btop's whole screen. Painted before the
                // glyph, which is blended over it.
                let bg = match cell.background_rgb {
                    Some([r, g, b]) => Rgb::new(r, g, b),
                    None => colour(index, colours),
                };
                surface.fill_rect(x, y, CELL.0 * scale, CELL.1 * scale, bg);
                (fg, bg)
            } else {
                (fg, colours.background)
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
    let dots;
    let cell = match braille(ch) {
        Some(drawn) => {
            dots = drawn;
            &dots
        }
        None => cell(ch, bold),
    };
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
/// Printable ASCII has its own glyph, and so does each character of the
/// extra table: Latin-1, arrows, box drawing, the Powerline glyphs and the
/// rest a prompt draws. Every other character, control characters included,
/// is drawn as a hollow box, as the panic screen's font draws one.
fn cell(ch: char, bold: bool) -> &'static font::Cell {
    let (glyphs, extra, replacement) = if bold {
        (&font::BOLD, &font::BOLD_EXTRA, &font::BOLD_REPLACEMENT)
    } else {
        (
            &font::REGULAR,
            &font::REGULAR_EXTRA,
            &font::REGULAR_REPLACEMENT,
        )
    };
    let code = u32::from(ch);
    code.checked_sub(font::FIRST)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| glyphs.get(index))
        .or_else(|| {
            extra
                .binary_search_by_key(&code, |&(at, _)| at)
                .ok()
                .and_then(|index| extra.get(index))
                .map(|(_, cell)| cell)
        })
        .unwrap_or(replacement)
}

/// The first braille pattern, U+2800, which has no dots.
const BRAILLE: u32 = 0x2800;

/// Each dot's bit in a braille pattern's code point, left column then right,
/// top to bottom: dots 1, 2, 3 and 7, then 4, 5, 6 and 8, which is the order
/// Unicode numbers them in, the bottom row having been added last.
const BRAILLE_DOTS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// A braille pattern's cell, or `None` for any other character.
///
/// The cell is two columns and four rows of equal slots, and a dot is a
/// square in the middle of its slot, a third of the slot's width on each
/// side of it: in a 12x24 cell a dot is four pixels square and a column of
/// them is spaced six pixels apart, so that a full column reads as a bar and
/// two of them side by side still read as two.
pub(crate) fn braille(ch: char) -> Option<font::Cell> {
    let bits = u32::from(ch)
        .checked_sub(BRAILLE)
        .filter(|bits| *bits <= 0xFF)?;
    let (slot_width, slot_height) = (font::WIDTH / 2, font::HEIGHT / 4);
    let side = slot_width * 2 / 3;
    let (inset_x, inset_y) = ((slot_width - side) / 2, (slot_height - side) / 2);
    let mut cell = [0u8; font::WIDTH * font::HEIGHT];
    for (column, dots) in BRAILLE_DOTS.iter().enumerate() {
        for (row, bit) in dots.iter().enumerate() {
            if bits & bit == 0 {
                continue;
            }
            let (left, top) = (column * slot_width + inset_x, row * slot_height + inset_y);
            for y in top..top + side {
                if let Some(line) =
                    cell.get_mut(y * font::WIDTH + left..y * font::WIDTH + left + side)
                {
                    line.fill(0xFF);
                }
            }
        }
    }
    Some(cell)
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

/// One of the sixteen colours, by number.
fn colour(index: u8, colours: &Colours) -> Rgb {
    colours
        .palette
        .get(usize::from(index & 15))
        .copied()
        .unwrap_or(Rgb::new(0xCC, 0xCC, 0xD0))
}
