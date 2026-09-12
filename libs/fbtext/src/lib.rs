//! Text and filled rectangles on a linear framebuffer.
//!
//! The kernel's panic handler paints its report on whatever framebuffer the
//! firmware left behind, which means this code runs on a machine that is
//! already in a bad state: a nested panic here loses the report entirely. So
//! everything is a pure function of a borrowed byte slice, nothing allocates,
//! and no argument — a rectangle hanging off the edge, a width of
//! `usize::MAX`, a character the font does not have — can make it index out of
//! bounds or overflow. Drawing is clipped, not refused.
//!
//! The font is Spleen 8x16 by Frederic Cambus (BSD-2-Clause, see
//! `LICENSE-spleen`), embedded as rows by `scripts/gen-font.py`.
//!
//! # Speed
//!
//! Under QEMU's interpreter every guest instruction is expensive, and a panic
//! screen starts by clearing a full-HD surface. A fill therefore computes its
//! bounds once per scanline and fills the span by doubling copies, which are
//! `memcpy`s, rather than calling a per-pixel function that clips again for each
//! of two million pixels. Glyphs are drawn a run of like pixels at a time.
//!
//! ```
//! # use core::fmt::Write;
//! # use ferrix_fbtext::{PixelOrder, Rect, Rgb, Surface, TextArea};
//! let mut pixels = [0u8; 320 * 200 * 4];
//! let mut surface = Surface::new(&mut pixels, 320, 200, 320, PixelOrder::Bgrx).unwrap();
//! surface.fill(Rgb::new(0, 0, 0xAA));
//!
//! let region = Rect { x: 8, y: 8, width: 304, height: 184 };
//! let mut text = TextArea::new(&mut surface, region, 1, Rgb::WHITE, None);
//! writeln!(text, "FERRIX-PANIC something went wrong").unwrap();
//! ```

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

mod font;

#[cfg(test)]
mod tests;

/// Width of one glyph cell at scale 1, in pixels.
pub const GLYPH_WIDTH: usize = 8;

/// Height of one glyph cell at scale 1, in pixels.
pub const GLYPH_HEIGHT: usize = 16;

/// Bytes in one pixel. Both supported layouts are 32 bits per pixel.
pub const BYTES_PER_PIXEL: usize = 4;

/// Column width of a tab stop, in cells.
const TAB_STOP: usize = 8;

// ---------------------------------------------------------------------------
// Colours and pixels
// ---------------------------------------------------------------------------

/// The byte order of a pixel, which is all that distinguishes the two
/// framebuffer layouts the loader passes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelOrder {
    /// Blue, green, red, unused — the usual UEFI graphics output format.
    Bgrx,
    /// Red, green, blue, unused.
    Rgbx,
}

/// A colour, eight bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// Black.
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    /// White.
    pub const WHITE: Rgb = Rgb::new(0xFF, 0xFF, 0xFF);

    /// A colour from its three channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// The four bytes this colour occupies in a framebuffer of `order`. The
    /// unused byte is written as zero.
    #[must_use]
    pub const fn to_bytes(self, order: PixelOrder) -> [u8; BYTES_PER_PIXEL] {
        match order {
            PixelOrder::Bgrx => [self.b, self.g, self.r, 0],
            PixelOrder::Rgbx => [self.r, self.g, self.b, 0],
        }
    }
}

/// A rectangle in pixels, by its top-left corner and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: usize,
    /// Top edge.
    pub y: usize,
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
}

/// The rows of the glyph drawn for `ch`: one byte per scanline, top first,
/// most significant bit leftmost.
///
/// Printable ASCII has its own glyph; every other character, control
/// characters included, is drawn as a hollow box.
#[must_use]
pub fn glyph(ch: char) -> &'static [u8; GLYPH_HEIGHT] {
    u32::from(ch)
        .checked_sub(font::FIRST)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| font::GLYPHS.get(index))
        .unwrap_or(&font::REPLACEMENT)
}

// ---------------------------------------------------------------------------
// Surface
// ---------------------------------------------------------------------------

/// A linear 32-bit framebuffer, borrowed.
///
/// Scanline `y` starts `y * stride * 4` bytes into the slice. The bytes past
/// `width` in each scanline are padding that nothing here writes.
pub struct Surface<'a> {
    /// Exactly `height * stride * 4` bytes.
    pixels: &'a mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    order: PixelOrder,
}

impl fmt::Debug for Surface<'_> {
    // By hand, because the derived one would print every byte of the screen.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Surface")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

impl<'a> Surface<'a> {
    /// A surface over `pixels`, `width` by `height` visible pixels with
    /// `stride` pixels from the start of one scanline to the next.
    ///
    /// `None` if `stride < width`, if `height * stride * 4` overflows, or if
    /// the slice is shorter than that. A longer slice is accepted and its tail
    /// left alone.
    #[must_use]
    pub fn new(
        pixels: &'a mut [u8],
        width: usize,
        height: usize,
        stride: usize,
        order: PixelOrder,
    ) -> Option<Self> {
        if stride < width {
            return None;
        }
        let size = height.checked_mul(stride)?.checked_mul(BYTES_PER_PIXEL)?;
        let pixels = pixels.get_mut(..size)?;
        Some(Surface {
            pixels,
            width,
            height,
            stride,
            order,
        })
    }

    /// Visible width in pixels.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Visible height in pixels.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// The pixel layout.
    #[must_use]
    pub const fn order(&self) -> PixelOrder {
        self.order
    }

    /// Paint every visible pixel.
    pub fn fill(&mut self, color: Rgb) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Paint a `w` by `h` rectangle whose top-left corner is at (`x`, `y`),
    /// clipped to the surface. Any arguments are accepted; a rectangle wholly
    /// outside the surface paints nothing.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Rgb) {
        self.fill_bytes(x, y, w, h, color.to_bytes(self.order));
    }

    /// Draw the glyph for `ch` with its top-left corner at (`x`, `y`), each
    /// font pixel a `scale` by `scale` square, clipped to the surface.
    ///
    /// Set pixels are painted `fg`. Clear pixels are painted `bg`, or left as
    /// they are when `bg` is `None`. A `scale` of zero draws nothing.
    pub fn draw_char(
        &mut self,
        x: usize,
        y: usize,
        ch: char,
        scale: usize,
        fg: Rgb,
        bg: Option<Rgb>,
    ) {
        let fg = fg.to_bytes(self.order);
        let bg = bg.map(|bg| bg.to_bytes(self.order));
        for (row, &bits) in glyph(ch).iter().enumerate() {
            let Some(top) = row.checked_mul(scale).and_then(|dy| y.checked_add(dy)) else {
                return;
            };
            if top >= self.height {
                return;
            }
            self.draw_glyph_row(x, top, bits, scale, fg, bg);
        }
    }

    /// One scanline of a glyph, `scale` pixels tall, drawn a run at a time.
    fn draw_glyph_row(
        &mut self,
        x: usize,
        top: usize,
        bits: u8,
        scale: usize,
        fg: [u8; BYTES_PER_PIXEL],
        bg: Option<[u8; BYTES_PER_PIXEL]>,
    ) {
        let mut start = 0;
        while start < GLYPH_WIDTH {
            let set = bit(bits, start);
            let end = (start..GLYPH_WIDTH)
                .find(|&column| bit(bits, column) != set)
                .unwrap_or(GLYPH_WIDTH);
            let color = if set { Some(fg) } else { bg };
            let left = start.checked_mul(scale).and_then(|dx| x.checked_add(dx));
            let run = end.saturating_sub(start).saturating_mul(scale);
            match (color, left) {
                (Some(color), Some(left)) => self.fill_bytes(left, top, run, scale, color),
                (_, None) => return,
                (None, Some(_)) => {}
            }
            start = end;
        }
    }

    /// [`Surface::fill_rect`], with the colour already in framebuffer order.
    fn fill_bytes(&mut self, x: usize, y: usize, w: usize, h: usize, color: [u8; BYTES_PER_PIXEL]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let right = x.saturating_add(w).min(self.width);
        let bottom = y.saturating_add(h).min(self.height);
        for row in y..bottom {
            match self.span_mut(row, x, right) {
                Some(span) => fill_span(span, color),
                None => return,
            }
        }
    }

    /// The bytes of pixels `left..right` on scanline `row`, or `None` if that
    /// is not inside the slice.
    ///
    /// Callers clip to the visible area first, so `None` never happens; the
    /// arithmetic is checked anyway, because this is the one place a wrong
    /// bound would turn into a write, and a `None` costs nothing.
    fn span_mut(&mut self, row: usize, left: usize, right: usize) -> Option<&mut [u8]> {
        let line = row.checked_mul(self.stride)?;
        let start = line.checked_add(left)?.checked_mul(BYTES_PER_PIXEL)?;
        let end = line.checked_add(right)?.checked_mul(BYTES_PER_PIXEL)?;
        self.pixels.get_mut(start..end)
    }
}

/// Whether column `column` of a glyph row is set; column 0 is the MSB.
fn bit(bits: u8, column: usize) -> bool {
    u32::try_from(column)
        .ok()
        .and_then(|shift| 0x80u8.checked_shr(shift))
        .is_some_and(|mask| bits & mask != 0)
}

/// Paint every whole pixel of `span` with `color`.
///
/// One pixel is written and then the filled prefix copied onto the rest,
/// doubling each time, so a scanline of `n` pixels is `log2 n` block copies
/// rather than `n` four-byte writes.
fn fill_span(span: &mut [u8], color: [u8; BYTES_PER_PIXEL]) {
    let Some(first) = span.get_mut(..BYTES_PER_PIXEL) else {
        return;
    };
    first.copy_from_slice(&color);
    let mut filled = BYTES_PER_PIXEL;
    while filled < span.len() {
        let Some((done, rest)) = span.split_at_mut_checked(filled) else {
            return;
        };
        let count = filled.min(rest.len());
        let (Some(to), Some(from)) = (rest.get_mut(..count), done.get(..count)) else {
            return;
        };
        to.copy_from_slice(from);
        filled = filled.saturating_add(count);
    }
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// A text cursor over a rectangular region of a [`Surface`].
///
/// Text wraps at the region's right edge and `'\n'` starts a new line. Once
/// the last line is used up, drawing stops but writing still succeeds: a panic
/// report that is too long for the screen is truncated, never turned into a
/// formatting error the caller has to handle.
///
/// Wrapping is deferred, as on a terminal: filling the last column does not by
/// itself move to the next line, so a line exactly as wide as the region
/// followed by `'\n'` does not leave a blank line behind it.
#[derive(Debug)]
pub struct TextArea<'s, 'a> {
    surface: &'s mut Surface<'a>,
    /// The region, already clipped to the surface.
    region: Rect,
    scale: usize,
    fg: Rgb,
    bg: Option<Rgb>,
    columns: usize,
    rows: usize,
    /// In `0..=columns`; `columns` means the line is full and the next
    /// character wraps.
    column: usize,
    row: usize,
}

impl<'s, 'a> TextArea<'s, 'a> {
    /// A text area over `region` of `surface`, with the cursor at its top-left.
    ///
    /// The region is clipped to the surface, and holds as many whole cells of
    /// `GLYPH_WIDTH * scale` by `GLYPH_HEIGHT * scale` pixels as fit. A
    /// `scale` of zero is taken as one. Characters are painted `fg` on `bg`,
    /// or on whatever is already there when `bg` is `None`.
    #[must_use]
    pub fn new(
        surface: &'s mut Surface<'a>,
        region: Rect,
        scale: usize,
        fg: Rgb,
        bg: Option<Rgb>,
    ) -> Self {
        let scale = scale.max(1);
        let x = region.x.min(surface.width);
        let y = region.y.min(surface.height);
        let region = Rect {
            x,
            y,
            width: region.width.min(surface.width.saturating_sub(x)),
            height: region.height.min(surface.height.saturating_sub(y)),
        };
        let cells = |extent: usize, cell: usize| {
            cell.checked_mul(scale)
                .and_then(|size| extent.checked_div(size))
                .unwrap_or(0)
        };
        TextArea {
            columns: cells(region.width, GLYPH_WIDTH),
            rows: cells(region.height, GLYPH_HEIGHT),
            surface,
            region,
            scale,
            fg,
            bg,
            column: 0,
            row: 0,
        }
    }

    /// Paint subsequent characters in `fg`.
    pub fn set_color(&mut self, fg: Rgb) {
        self.fg = fg;
    }

    /// Move the cursor to the start of the next line.
    pub fn newline(&mut self) {
        self.column = 0;
        self.row = self.row.saturating_add(1);
    }

    /// True once there is no line left to draw on.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.row >= self.rows || self.columns == 0
    }

    /// Cells per line.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    /// Lines in the region.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Write one character at the cursor and advance it.
    pub fn put_char(&mut self, ch: char) {
        match ch {
            '\n' => self.newline(),
            '\r' => self.column = 0,
            '\t' => {
                self.put_char(' ');
                while !self.column.is_multiple_of(TAB_STOP) && self.column < self.columns {
                    self.put_char(' ');
                }
            }
            _ => self.draw(ch),
        }
    }

    /// Draw a printable cell at the cursor, wrapping first if the line is full.
    fn draw(&mut self, ch: char) {
        if self.column >= self.columns {
            self.newline();
        }
        if self.is_full() {
            return;
        }
        let cell = |index: usize, size: usize| index.checked_mul(size)?.checked_mul(self.scale);
        let x = cell(self.column, GLYPH_WIDTH).and_then(|dx| self.region.x.checked_add(dx));
        let y = cell(self.row, GLYPH_HEIGHT).and_then(|dy| self.region.y.checked_add(dy));
        if let (Some(x), Some(y)) = (x, y) {
            self.surface
                .draw_char(x, y, ch, self.scale, self.fg, self.bg);
        }
        self.column = self.column.saturating_add(1);
    }
}

impl fmt::Write for TextArea<'_, '_> {
    /// Never fails: text past the end of the region is dropped.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            self.put_char(ch);
        }
        Ok(())
    }
}
