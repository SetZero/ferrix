// SPDX-License-Identifier: MIT
//
// Ported from Linux, drivers/gpu/drm/drm_panic_qr.rs; see the crate
// documentation for the upstream commit and LICENSE-MIT for the notice.

//! Drawing the symbol: function patterns, data in the zigzag order, format and
//! version information, and the mask.

use crate::version::{FORMAT_INFOS_QR_L, Version};

/// A QR code image, encoded as a linear binary framebuffer.
/// 1 bit per module (pixel), each new line start at next byte boundary.
/// Max width is 177 for V40 QR code, so `u8` is enough for coordinate.
///
/// A set bit is a **light** module, as upstream draws it.
#[derive(Debug)]
pub(crate) struct QrImage<'a> {
    data: &'a mut [u8],
    width: u8,
    stride: u8,
    version: Version,
}

impl<'a> QrImage<'a> {
    /// Draw the symbol for `data` in `version` into `qrdata`, which the
    /// caller has checked holds `width * stride` bytes.
    pub(crate) fn new(
        version: Version,
        data: impl Iterator<Item = u8>,
        qrdata: &'a mut [u8],
    ) -> Self {
        let width = version.width();
        let stride = width.div_ceil(8);

        let mut qr_image = QrImage {
            data: qrdata,
            width,
            stride,
            version,
        };
        qr_image.draw_all(data);
        qr_image
    }

    /// The finished bitmap and its width.
    pub(crate) fn into_parts(self) -> (&'a [u8], u8) {
        (self.data, self.width)
    }

    /// The byte holding module (x, y), and the module's bit in it.
    ///
    /// Upstream indexes the buffer with this offset. Here a module outside
    /// the buffer is not drawn rather than a panic; the only caller checked
    /// the buffer's length against the version before drawing anything.
    fn module(&mut self, x: u8, y: u8) -> Option<(&mut u8, u8)> {
        let off = usize::from(y) * usize::from(self.stride) + usize::from(x / 8);
        Some((self.data.get_mut(off)?, 0x80 >> (x % 8)))
    }

    /// Set pixel to light color.
    fn set(&mut self, x: u8, y: u8) {
        if let Some((byte, bit)) = self.module(x, y) {
            *byte |= bit;
        }
    }

    /// Invert a module color.
    fn xor(&mut self, x: u8, y: u8) {
        if let Some((byte, bit)) = self.module(x, y) {
            *byte ^= bit;
        }
    }

    /// Draw a light square at (x, y) top left corner.
    fn draw_square(&mut self, x: u8, y: u8, size: u8) {
        for k in 0..size {
            self.set(x + k, y);
            self.set(x, y + k + 1);
            self.set(x + size, y + k);
            self.set(x + k + 1, y + size);
        }
    }

    // Finder pattern: 3 8x8 square at the corners.
    fn draw_finders(&mut self) {
        self.draw_square(1, 1, 4);
        self.draw_square(self.width - 6, 1, 4);
        self.draw_square(1, self.width - 6, 4);
        for k in 0..8 {
            self.set(k, 7);
            self.set(self.width - k - 1, 7);
            self.set(k, self.width - 8);
        }
        for k in 0..7 {
            self.set(7, k);
            self.set(self.width - 8, k);
            self.set(7, self.width - 1 - k);
        }
    }

    fn is_finder(&self, x: u8, y: u8) -> bool {
        let end = self.width - 8;
        #[expect(clippy::nonminimal_bool, reason = "one clause per finder pattern")]
        {
            (x < 8 && y < 8) || (x < 8 && y >= end) || (x >= end && y < 8)
        }
    }

    // Alignment pattern: 5x5 squares in a grid.
    fn draw_alignments(&mut self) {
        let positions = self.version.alignment_pattern();
        for &x in positions {
            for &y in positions {
                if !self.is_finder(x, y) {
                    self.draw_square(x - 1, y - 1, 2);
                }
            }
        }
    }

    fn is_alignment(&self, x: u8, y: u8) -> bool {
        let positions = self.version.alignment_pattern();
        positions.iter().any(|&ax| {
            positions.iter().any(|&ay| {
                // Every centre that is not under a finder is at least 6 from
                // the edge, so `- 2` cannot go below zero.
                !self.is_finder(ax, ay) && x >= ax - 2 && x <= ax + 2 && y >= ay - 2 && y <= ay + 2
            })
        })
    }

    // Timing pattern: 2 dotted line between the finder patterns.
    fn draw_timing_patterns(&mut self) {
        let end = self.width - 8;

        for x in (9..end).step_by(2) {
            self.set(x, 6);
            self.set(6, x);
        }
    }

    const fn is_timing(x: u8, y: u8) -> bool {
        x == 6 || y == 6
    }

    // Mask info: 15 bits around the finders, written twice for redundancy.
    fn draw_maskinfo(&mut self) {
        let info: u16 = FORMAT_INFOS_QR_L[0];
        let mut skip = 0;

        for k in 0..7 {
            if k == 6 {
                skip = 1;
            }
            if info & (1 << (14 - k)) == 0 {
                self.set(k + skip, 8);
                self.set(8, self.width - 1 - k);
            }
        }
        skip = 0;
        for k in 0..8 {
            if k == 2 {
                skip = 1;
            }
            if info & (1 << (7 - k)) == 0 {
                self.set(8, 8 - skip - k);
                self.set(self.width - 8 + k, 8);
            }
        }
    }

    fn is_maskinfo(&self, x: u8, y: u8) -> bool {
        let end = self.width - 8;
        // Count the dark module as mask info.
        (x <= 8 && y == 8) || (y <= 8 && x == 8) || (x == 8 && y >= end) || (x >= end && y == 8)
    }

    // Version info: 18bits written twice, close to the finders.
    fn draw_version_info(&mut self) {
        let vinfo = self.version.version_info();
        let pos = self.width - 11;

        if vinfo == 0 {
            return;
        }
        for x in 0..3 {
            for y in 0..6 {
                if vinfo & (1 << (x + y * 3)) == 0 {
                    self.set(x + pos, y);
                    self.set(y, x + pos);
                }
            }
        }
    }

    fn is_version_info(&self, x: u8, y: u8) -> bool {
        let vinfo = self.version.version_info();
        let pos = self.width - 11;

        vinfo != 0 && ((x >= pos && x < pos + 3 && y < 6) || (y >= pos && y < pos + 3 && x < 6))
    }

    /// Returns true if the module is reserved (Not usable for data and EC).
    fn is_reserved(&self, x: u8, y: u8) -> bool {
        self.is_alignment(x, y)
            || self.is_finder(x, y)
            || Self::is_timing(x, y)
            || self.is_maskinfo(x, y)
            || self.is_version_info(x, y)
    }

    /// Last module to draw, at bottom left corner.
    const fn is_last(&self, x: u8, y: u8) -> bool {
        x == 0 && y == self.width - 1
    }

    /// Move to the next module according to QR code order.
    ///
    /// From bottom right corner, to bottom left corner.
    ///
    /// The subtractions saturate where upstream's would underflow: only a
    /// step from the last module could reach below column 0, and no caller
    /// takes one, but a stuck cursor is a better failure than a panic.
    const fn next(&self, x: u8, y: u8) -> (u8, u8) {
        let x_adj = if x <= 6 { x + 1 } else { x };
        let column_type = (self.width - x_adj) % 4;

        match column_type {
            2 if y > 0 => (x + 1, y - 1),
            0 if y < self.width - 1 => (x + 1, y + 1),
            0 | 2 if x == 7 => (x - 2, y),
            _ => (x.saturating_sub(1), y),
        }
    }

    /// Find next module that can hold data.
    fn next_available(&self, x: u8, y: u8) -> (u8, u8) {
        let (mut x, mut y) = self.next(x, y);
        while self.is_reserved(x, y) && !self.is_last(x, y) {
            (x, y) = self.next(x, y);
        }
        (x, y)
    }

    fn draw_data(&mut self, data: impl Iterator<Item = u8>) {
        let (mut x, mut y) = (self.width - 1, self.width - 1);
        for byte in data {
            for s in 0..8 {
                if byte & (0x80 >> s) == 0 {
                    self.set(x, y);
                }
                (x, y) = self.next_available(x, y);
            }
        }
        // Set the remaining modules (0, 3 or 7 depending on version).
        // because 0 correspond to a light module.
        while !self.is_last(x, y) {
            if !self.is_reserved(x, y) {
                self.set(x, y);
            }
            (x, y) = self.next(x, y);
        }
    }

    /// Apply checkerboard mask to all non-reserved modules.
    fn apply_mask(&mut self) {
        for x in 0..self.width {
            for y in 0..self.width {
                if (x ^ y) % 2 == 0 && !self.is_reserved(x, y) {
                    self.xor(x, y);
                }
            }
        }
    }

    /// Draw the QR code with the provided data iterator.
    fn draw_all(&mut self, data: impl Iterator<Item = u8>) {
        // First clear the table, as it may have already some data.
        self.data.fill(0);
        self.draw_finders();
        self.draw_alignments();
        self.draw_timing_patterns();
        self.draw_version_info();
        self.draw_data(data);
        self.draw_maskinfo();
        self.apply_mask();
    }
}
