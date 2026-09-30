//! The pattern clients' pixels: what stage 18's two test clients draw, made
//! in code so the renderer's tests and the clients draw the same thing.

use crate::Format;

/// A test client's pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// Squares of [`Pattern::CELL`] pixels in two greys, starting light at
    /// the top-left, as `XRGB8888` with the X byte left zero, the way a
    /// client that never sets it writes: a renderer that read that byte as
    /// alpha would draw nothing.
    Checkerboard,
    /// A premultiplied `ARGB8888` gradient in steps of [`Pattern::CELL`]
    /// pixels: red grows to the right, green downwards, blue is constant,
    /// and the bottom half is translucent, so the background shows through
    /// it.
    Gradient,
}

impl Pattern {
    /// The size of a checkerboard square and of a gradient step, in pixels.
    pub const CELL: u32 = 16;

    /// The checkerboard's light grey, `XRGB8888`.
    pub const LIGHT: u32 = 0x00E0_E0E0;

    /// The checkerboard's dark grey, `XRGB8888`.
    pub const DARK: u32 = 0x0030_3030;

    /// The gradient's constant blue, before premultiplying.
    pub const BLUE: u8 = 0x80;

    /// The alpha of the gradient's bottom half.
    pub const TRANSLUCENT: u8 = 0xC0;

    /// The format the pattern's buffer is in.
    #[must_use]
    pub const fn format(self) -> Format {
        match self {
            Self::Checkerboard => Format::Xrgb8888,
            Self::Gradient => Format::Argb8888,
        }
    }

    /// Pixel (`x`, `y`) of a `width` × `height` buffer, as the 32-bit value
    /// the buffer stores little-endian.
    #[must_use]
    pub fn pixel(self, x: u32, y: u32, height: u32) -> u32 {
        let (cx, cy) = (x / Self::CELL, y / Self::CELL);
        match self {
            Self::Checkerboard => {
                if (cx + cy) % 2 == 0 {
                    Self::LIGHT
                } else {
                    Self::DARK
                }
            }
            Self::Gradient => {
                let step = |cell: u32| cell.saturating_mul(8).min(255);
                let alpha = if y < height / 2 {
                    0xFF
                } else {
                    u32::from(Self::TRANSLUCENT)
                };
                // Rounded to nearest: c × a / 255.
                let premultiply = |c: u32| (c * alpha + 127) / 255;
                (alpha << 24)
                    | (premultiply(step(cx)) << 16)
                    | (premultiply(step(cy)) << 8)
                    | premultiply(u32::from(Self::BLUE))
            }
        }
    }

    /// A `width` × `height` buffer of the pattern, with a stride of four
    /// bytes a pixel.
    #[must_use]
    pub fn draw(self, width: u32, height: u32) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| self.pixel(x, y, height).to_le_bytes())
            .collect()
    }
}
