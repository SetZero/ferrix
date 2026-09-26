//! Fitting the picture to the screen: as large as it goes whole, centred,
//! each screen pixel taking the nearest picture pixel.

use core::ops::Range;

use media_bav::grey_of;

/// Where the picture goes on the screen, and which picture pixel each
/// screen pixel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fit {
    /// Left edge on the screen.
    pub(crate) x: usize,
    /// Top edge on the screen.
    pub(crate) y: usize,
    /// Width on the screen.
    pub(crate) width: usize,
    /// Height on the screen.
    pub(crate) height: usize,
    source_width: usize,
    /// For each screen column of the picture, the picture's column.
    columns: Vec<usize>,
    /// For each screen row of the picture, the picture's row.
    rows: Vec<usize>,
}

impl Fit {
    /// Fit a `width × height` picture to a `screen_width × screen_height`
    /// screen, keeping its shape.
    pub(crate) fn new(
        width: usize,
        height: usize,
        screen_width: usize,
        screen_height: usize,
    ) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        // The larger scale that fits both ways: compare w'/w with h'/h
        // without dividing.
        let (out_width, out_height) = if screen_width * height <= screen_height * width {
            (screen_width, height * screen_width / width)
        } else {
            (width * screen_height / height, screen_height)
        };
        let columns = (0..out_width).map(|x| x * width / out_width).collect();
        let rows = (0..out_height).map(|y| y * height / out_height).collect();
        Self {
            x: (screen_width - out_width) / 2,
            y: (screen_height - out_height) / 2,
            width: out_width,
            height: out_height,
            source_width: width,
            columns,
            rows,
        }
    }

    /// The screen rows, counted within the picture, that show picture rows
    /// `first..=last`.
    pub(crate) fn screen_rows(&self, first: usize, last: usize) -> Range<usize> {
        let start = self.rows.partition_point(|&row| row < first);
        let end = self.rows.partition_point(|&row| row <= last);
        start..end
    }

    #[cfg(test)]
    /// The picture row screen row `row` (counted within the picture) shows.
    pub(crate) fn source_row(&self, row: usize) -> Option<usize> {
        self.rows.get(row).copied()
    }

    #[cfg(test)]
    /// The picture column screen column `column` shows.
    pub(crate) fn source_column(&self, column: usize) -> Option<usize> {
        self.columns.get(column).copied()
    }

    /// Draw screen rows `rows` (counted within the picture) of `shades` into
    /// an `XRGB8888` buffer whose rows are `pitch` bytes apart.
    pub(crate) fn draw(
        &self,
        shades: &[u8],
        pixels: &mut [u8],
        pitch: usize,
        rows: Range<usize>,
        invert: bool,
    ) {
        let mut previous: Option<(usize, usize)> = None;
        for row in rows {
            let Some(source) = self.rows.get(row).copied() else {
                break;
            };
            let start = (self.y + row) * pitch + self.x * 4;
            let end = start + self.width * 4;
            if end > pixels.len() {
                break;
            }
            // A row that shows the same picture row as the one above is a
            // copy of it.
            if let Some((above, above_start)) = previous
                && above == source
            {
                pixels.copy_within(above_start..above_start + self.width * 4, start);
                previous = Some((source, start));
                continue;
            }
            let Some(line) =
                shades.get(source * self.source_width..(source + 1) * self.source_width)
            else {
                break;
            };
            let Some(out) = pixels.get_mut(start..end) else {
                break;
            };
            for (pixel, &column) in out.chunks_exact_mut(4).zip(&self.columns) {
                let shade = line.get(column).copied().unwrap_or(0);
                let shade = if invert { 15 - shade.min(15) } else { shade };
                let grey = grey_of(shade);
                pixel.copy_from_slice(&[grey, grey, grey, 0]);
            }
            previous = Some((source, start));
        }
    }
}
