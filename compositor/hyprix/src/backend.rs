//! Where the frame goes: memory, or a screen.

use std::io;
use std::path::Path;

/// A place a frame can be put.
///
/// `Debug` so a caller holding one can derive it; a backend's own debug
/// output is its name and size, never its pixels.
pub trait Backend: core::fmt::Debug {
    /// The size of the screen in pixels.
    fn size(&self) -> (u32, u32);

    /// Bytes of an `XRGB8888` buffer of that size, to draw into.
    fn buffer(&mut self) -> &mut [u8];

    /// Bytes from one row's start to the next.
    fn stride(&self) -> u32;

    /// Show what was drawn.
    ///
    /// # Errors
    ///
    /// Whatever the screen said.
    fn present(&mut self) -> io::Result<()>;

    /// What to say about this backend in the compositor's log line.
    fn describe(&self) -> String;
}

/// A screen that is only memory: the everyday one, and the one a test reads.
#[derive(Debug)]
pub struct Headless {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    frames: u32,
}

impl Headless {
    /// A `width` by `height` screen of memory.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let bytes = vec![0; width as usize * height as usize * 4];
        Self {
            width,
            height,
            bytes,
            frames: 0,
        }
    }

    /// How many frames have been shown.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }

    /// Write the last frame as a binary PPM, which every image viewer reads
    /// and which needs no library to produce.
    ///
    /// # Errors
    ///
    /// Whatever the write said.
    pub fn write_ppm(&self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        let mut out = Vec::with_capacity(self.bytes.len());
        write!(out, "P6\n{} {}\n255\n", self.width, self.height)?;
        for pixel in self.bytes.chunks_exact(4) {
            // The buffer is XRGB8888 little-endian: blue, green, red, unused.
            if let [blue, green, red, _] = pixel {
                out.extend_from_slice(&[*red, *green, *blue]);
            }
        }
        std::fs::write(path, out)
    }
}

impl Backend for Headless {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    fn stride(&self) -> u32 {
        self.width.saturating_mul(4)
    }

    fn present(&mut self) -> io::Result<()> {
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }

    fn describe(&self) -> String {
        format!("headless {}x{}", self.width, self.height)
    }
}
