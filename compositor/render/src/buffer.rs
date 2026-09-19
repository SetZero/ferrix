//! The buffers a frame is drawn from and into, checked once when they are
//! described so drawing never reads or writes past one.

use crate::Error;
use crate::canvas::MAX_SIZE;

/// A pixel format a client's buffer can have: the two every `wl_shm` must
/// offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// `ARGB8888`: premultiplied alpha, as `wl_shm` defines it, drawn with
    /// source-over.
    Argb8888,
    /// `XRGB8888`: opaque, the X byte ignored, copied.
    Xrgb8888,
}

impl Format {
    /// The format's `wl_shm.format` value.
    #[must_use]
    pub const fn wl_shm(self) -> u32 {
        match self {
            Self::Argb8888 => 0,
            Self::Xrgb8888 => 1,
        }
    }

    /// The format for a `wl_shm.format` value, if it is one of these.
    #[must_use]
    pub const fn from_wl_shm(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Argb8888),
            1 => Some(Self::Xrgb8888),
            _ => None,
        }
    }

    /// The format's DRM fourcc, which is what Smithay's `ImportMem` names
    /// formats by.
    #[must_use]
    pub const fn fourcc(self) -> u32 {
        match self {
            Self::Argb8888 => u32::from_le_bytes(*b"AR24"),
            Self::Xrgb8888 => u32::from_le_bytes(*b"XR24"),
        }
    }
}

/// Check a buffer's shape: a size in `1..=MAX_SIZE` each way, a stride of at
/// least four bytes a pixel, and bytes through the last pixel of the last
/// row. Returns the bytes needed.
fn check(len: usize, width: u32, height: u32, stride: u32) -> Result<usize, Error> {
    if width == 0 || height == 0 || width > MAX_SIZE || height > MAX_SIZE {
        return Err(Error::Size { width, height });
    }
    let row = u64::from(width) * 4;
    if u64::from(stride) < row {
        return Err(Error::Stride { width, stride });
    }
    let needed = u64::from(stride) * u64::from(height - 1) + row;
    let needed = usize::try_from(needed).unwrap_or(usize::MAX);
    if len < needed {
        return Err(Error::Short { needed, len });
    }
    Ok(needed)
}

/// A client's pixels: a `wl_shm` buffer, or any other 32-bit buffer of the
/// two formats, with the stride the client gave.
#[derive(Debug, Clone, Copy)]
pub struct Surface<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    stride: u32,
    format: Format,
    name: u64,
}

impl<'a> Surface<'a> {
    /// Describe `data` as a `width` × `height` buffer of `stride` bytes a
    /// row, starting at its first byte.
    ///
    /// # Errors
    ///
    /// [`Error::Size`], [`Error::Stride`] or [`Error::Short`] when the shape
    /// does not fit the bytes.
    pub fn new(
        data: &'a [u8],
        width: u32,
        height: u32,
        stride: u32,
        format: Format,
    ) -> Result<Self, Error> {
        let _ = check(data.len(), width, height, stride)?;
        Ok(Self {
            data,
            width,
            height,
            stride,
            format,
            name: 0,
        })
    }

    /// The same pixels, said to be those of the thing called `name`: a
    /// `wl_surface`, say, by its client and its id.
    ///
    /// The software renderer reads a surface's pixels afresh every time and
    /// has no use for this. A renderer that keeps a copy of them somewhere
    /// slower to reach -- a texture on a GPU -- needs to know that this
    /// frame's pixels and the last one's are the *same surface*, so that it
    /// moves only what changed; a client that draws into two buffers in
    /// turn is one surface, and its pixels' address is not. Zero is no name
    /// at all, and such a surface is moved whole every time it is drawn.
    #[must_use]
    pub const fn named(mut self, name: u64) -> Self {
        self.name = name;
        self
    }

    /// What [`Surface::named`] called it, or zero.
    #[must_use]
    pub const fn name(&self) -> u64 {
        self.name
    }

    /// The same bytes read as opaque, whatever the client's format said.
    ///
    /// `windowrule = opaque`: a client that leaves rubbish in its alpha
    /// channel is drawn blotchy, and the rule is a person saying "there is
    /// nothing to see through here". The bytes are not touched; only what
    /// the fourth one *means* changes, which is exactly what the rule says.
    #[must_use]
    pub const fn as_opaque(self) -> Self {
        Self {
            format: Format::Xrgb8888,
            ..self
        }
    }

    /// The width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The stride in bytes.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// The format.
    #[must_use]
    pub const fn format(&self) -> Format {
        self.format
    }

    /// The bytes as given.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Row `y`'s pixels, `width` × 4 bytes, without the padding.
    pub(crate) fn row(&self, y: u32) -> Option<&'a [u8]> {
        let start = usize::try_from(u64::from(y) * u64::from(self.stride)).ok()?;
        let len = usize::try_from(u64::from(self.width) * 4).ok()?;
        self.data.get(start..start.checked_add(len)?)
    }

    /// The pixels as one run of rows with no padding, when they already are
    /// one: a stride of exactly four bytes a pixel.
    pub(crate) fn tight(&self) -> Option<&'a [u8]> {
        if u64::from(self.stride) != u64::from(self.width) * 4 {
            return None;
        }
        let len = usize::try_from(u64::from(self.stride) * u64::from(self.height)).ok()?;
        self.data.get(..len)
    }
}

/// Where a frame is presented: the mapping of an `XRGB8888` dumb buffer,
/// with the stride `MODE_CREATE_DUMB` returned.
#[derive(Debug)]
pub struct Target<'a> {
    data: &'a mut [u8],
    width: u32,
    height: u32,
    stride: u32,
}

impl<'a> Target<'a> {
    /// Describe `data` as a `width` × `height` `XRGB8888` buffer of `stride`
    /// bytes a row.
    ///
    /// # Errors
    ///
    /// [`Error::Size`], [`Error::Stride`] or [`Error::Short`] when the shape
    /// does not fit the bytes.
    pub fn new(data: &'a mut [u8], width: u32, height: u32, stride: u32) -> Result<Self, Error> {
        let _ = check(data.len(), width, height, stride)?;
        Ok(Self {
            data,
            width,
            height,
            stride,
        })
    }

    /// The width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The stride in bytes.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// The bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// The `len` bytes of row `y` from column `x`.
    pub(crate) fn span_mut(&mut self, x: u32, y: u32, len: usize) -> Option<&mut [u8]> {
        let start = u64::from(y) * u64::from(self.stride) + u64::from(x) * 4;
        let start = usize::try_from(start).ok()?;
        self.data.get_mut(start..start.checked_add(len)?)
    }
}
