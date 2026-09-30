//! `.bav`: sixteen shades of grey, in runs, each frame against the last.
//!
//! Bad Apple!! is a shadow play: almost every pixel is black or white, and
//! almost every pixel is what it was a frame ago. A general video codec
//! spends its effort on what this video does not have; this format keeps
//! only what it does. There is no H.264 decoder written in Rust to run on
//! Ferrix, so the host converts the video once, with ffmpeg and
//! [`Encoder`], and the player decodes this instead.
//!
//! # The file
//!
//! Little-endian throughout.
//!
//! | Bytes | What |
//! |---|---|
//! | 8 | [`MAGIC`] |
//! | 2, 2 | width, height in pixels |
//! | 2, 2 | frames per second as a fraction: numerator, denominator |
//! | 4 | `n`, the number of frames |
//! | 4 × (`n` + 1) | where each frame starts in the data, and where the last ends |
//! | ... | the data |
//!
//! # A frame
//!
//! A sequence of tokens, each an unsigned LEB128 number `t`, covering the
//! picture's pixels in raster order exactly once:
//!
//! * `t & 1 == 0`: *paint* `(t >> 5) + 1` pixels with shade `(t >> 1) & 15`;
//! * `t & 1 == 1`: *keep* `(t >> 1) + 1` pixels as the previous frame had
//!   them.
//!
//! Shade `s` is the grey `s × 17`, so 0 is black and 15 is white. The first
//! frame is kept against an all-black picture.

use core::fmt;

/// The first eight bytes of every `.bav` file.
pub const MAGIC: [u8; 8] = *b"FXBAV1\0\0";

/// Bytes before the frame index.
const HEADER: usize = 20;

/// The number of shades.
pub const SHADES: u8 = 16;

/// What went wrong reading a file or a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a `.bav` file: the magic is wrong.
    Magic,
    /// The file ends before its header, index or data says it does.
    Short,
    /// The index goes backwards or past the data.
    Index,
    /// A frame's tokens cover fewer or more pixels than the picture has.
    Coverage,
    /// A token runs past the end of the frame's bytes, or is too large.
    Token,
    /// A picture of no pixels, or a frame rate of zero.
    Shape,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Magic => "not a .bav file",
            Self::Short => "the file is cut short",
            Self::Index => "the frame index is out of order",
            Self::Coverage => "a frame does not cover the picture exactly",
            Self::Token => "a frame ends inside a token",
            Self::Shape => "no pixels, or no frame rate",
        })
    }
}

impl std::error::Error for Error {}

/// What the header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Pixels across.
    pub width: u16,
    /// Pixels down.
    pub height: u16,
    /// Frames per second: this over [`Header::rate_den`].
    pub rate_num: u16,
    /// See [`Header::rate_num`].
    pub rate_den: u16,
    /// How many frames there are.
    pub frames: u32,
}

impl Header {
    /// Pixels in a picture.
    #[must_use]
    pub const fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// The frame showing at `micros` microseconds from the start, which may
    /// be past the last.
    #[must_use]
    pub const fn frame_at(&self, micros: u64) -> u64 {
        micros * self.rate_num as u64 / (self.rate_den as u64 * 1_000_000)
    }
}

/// A `.bav` file, read in place.
#[derive(Clone, Copy, Debug)]
pub struct Video<'a> {
    /// The header.
    pub header: Header,
    index: &'a [u8],
    data: &'a [u8],
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    bytes
        .get(at..at + 2)
        .and_then(|b| b.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(Error::Short)
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(Error::Short)
}

impl<'a> Video<'a> {
    /// Read the header and index of `bytes`, and check the index.
    ///
    /// # Errors
    ///
    /// [`Error::Magic`], [`Error::Short`], [`Error::Index`] or
    /// [`Error::Shape`]: a file this can't have written.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.get(..MAGIC.len()) != Some(&MAGIC[..]) {
            return Err(Error::Magic);
        }
        let header = Header {
            width: u16_at(bytes, 8)?,
            height: u16_at(bytes, 10)?,
            rate_num: u16_at(bytes, 12)?,
            rate_den: u16_at(bytes, 14)?,
            frames: u32_at(bytes, 16)?,
        };
        if header.pixels() == 0 || header.rate_num == 0 || header.rate_den == 0 {
            return Err(Error::Shape);
        }
        let entries = header.frames as usize + 1;
        let index = bytes
            .get(HEADER..HEADER + entries * 4)
            .ok_or(Error::Short)?;
        let data = bytes.get(HEADER + entries * 4..).ok_or(Error::Short)?;
        let mut last = 0;
        for entry in index.chunks_exact(4) {
            let at = u32_at(entry, 0)? as usize;
            if at < last || at > data.len() {
                return Err(Error::Index);
            }
            last = at;
        }
        Ok(Self {
            header,
            index,
            data,
        })
    }

    /// Frame `n`'s tokens, or `None` past the last frame.
    #[must_use]
    pub fn frame(&self, n: u32) -> Option<&'a [u8]> {
        let n = n as usize;
        let start = u32_at(self.index, n * 4).ok()? as usize;
        let end = u32_at(self.index, (n + 1) * 4).ok()? as usize;
        self.data.get(start..end)
    }
}

/// The rows a frame changed, first and last inclusive; `None` if none.
pub type Damage = Option<(u16, u16)>;

/// The picture a player keeps: one shade per pixel, which each frame
/// changes in place.
#[derive(Clone, Debug)]
pub struct Picture {
    width: usize,
    shades: Vec<u8>,
}

impl Picture {
    /// An all-black picture of the header's size.
    #[must_use]
    pub fn new(header: &Header) -> Self {
        Self {
            width: usize::from(header.width),
            shades: vec![0; header.pixels()],
        }
    }

    /// The shades, row after row.
    #[must_use]
    pub fn shades(&self) -> &[u8] {
        &self.shades
    }

    /// Apply one frame's tokens.
    ///
    /// # Errors
    ///
    /// [`Error::Token`] or [`Error::Coverage`]. The picture is then partly
    /// changed; a player shows it anyway, since the next frame mends it.
    pub fn apply(&mut self, frame: &[u8]) -> Result<Damage, Error> {
        let mut at = 0;
        let mut first = usize::MAX;
        let mut last = 0;
        let mut tokens = Tokens { bytes: frame };
        while let Some(token) = tokens.next_token()? {
            if token & 1 == 1 {
                at += (token >> 1) as usize + 1;
                continue;
            }
            let shade = ((token >> 1) & 15) as u8;
            let run = (token >> 5) as usize + 1;
            let pixels = self.shades.get_mut(at..at + run).ok_or(Error::Coverage)?;
            pixels.fill(shade);
            first = first.min(at / self.width);
            last = last.max((at + run - 1) / self.width);
            at += run;
        }
        if at != self.shades.len() {
            return Err(Error::Coverage);
        }
        Ok((first != usize::MAX).then(|| {
            let row = |r: usize| u16::try_from(r).unwrap_or(u16::MAX);
            (row(first), row(last))
        }))
    }
}

struct Tokens<'a> {
    bytes: &'a [u8],
}

impl Tokens<'_> {
    /// The next LEB128 number, `None` at the end, or an error inside one.
    fn next_token(&mut self) -> Result<Option<u64>, Error> {
        if self.bytes.is_empty() {
            return Ok(None);
        }
        let mut value = 0_u64;
        for (i, byte) in self.bytes.iter().enumerate() {
            if i >= 9 {
                return Err(Error::Token);
            }
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                self.bytes = self.bytes.get(i + 1..).unwrap_or(&[]);
                return Ok(Some(value));
            }
        }
        Err(Error::Token)
    }
}

fn push_token(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// The shade nearest an 8-bit grey.
#[must_use]
pub const fn shade_of(grey: u8) -> u8 {
    ((grey as u16 * 15 + 127) / 255) as u8
}

/// The 8-bit grey of a shade.
#[must_use]
pub const fn grey_of(shade: u8) -> u8 {
    shade.saturating_mul(17)
}

/// Makes a `.bav` file from 8-bit grey frames.
#[derive(Debug)]
pub struct Encoder {
    header: Header,
    tolerance: u8,
    shown: Vec<u8>,
    ends: Vec<u32>,
    data: Vec<u8>,
}

/// A run being built.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    Keep,
    Paint(u8),
}

impl Encoder {
    /// An encoder for pictures of `width` by `height` at `rate_num` over
    /// `rate_den` frames a second.
    ///
    /// A pixel within `tolerance` shades of what the previous frame showed
    /// is kept rather than painted: the source is lossy video, whose edges
    /// shimmer by a shade from frame to frame, and repainting that shimmer
    /// would be most of the file. What is shown never drifts further than
    /// `tolerance` from the source, since the comparison is with what the
    /// decoder has, not with the last source frame.
    ///
    /// # Errors
    ///
    /// [`Error::Shape`] for no pixels or no frame rate.
    pub fn new(
        width: u16,
        height: u16,
        rate_num: u16,
        rate_den: u16,
        tolerance: u8,
    ) -> Result<Self, Error> {
        let header = Header {
            width,
            height,
            rate_num,
            rate_den,
            frames: 0,
        };
        if header.pixels() == 0 || rate_num == 0 || rate_den == 0 {
            return Err(Error::Shape);
        }
        Ok(Self {
            header,
            tolerance,
            shown: vec![0; header.pixels()],
            ends: Vec::new(),
            data: Vec::new(),
        })
    }

    /// Add a frame of `width × height` 8-bit greys, row after row.
    ///
    /// # Errors
    ///
    /// [`Error::Coverage`] if `grey` is not one picture's worth.
    pub fn push(&mut self, grey: &[u8]) -> Result<(), Error> {
        if grey.len() != self.shown.len() {
            return Err(Error::Coverage);
        }
        let mut run = Run::Keep;
        let mut length = 0_u64;
        for (source, shown) in grey.iter().zip(self.shown.iter_mut()) {
            let shade = shade_of(*source);
            let next = match run {
                Run::Paint(painting) if painting == shade => Run::Paint(shade),
                _ if shade.abs_diff(*shown) <= self.tolerance => Run::Keep,
                _ => Run::Paint(shade),
            };
            if next != run && length > 0 {
                Self::emit(&mut self.data, run, length);
                length = 0;
            }
            run = next;
            length += 1;
            if let Run::Paint(painted) = run {
                *shown = painted;
            }
        }
        Self::emit(&mut self.data, run, length);
        self.ends
            .push(u32::try_from(self.data.len()).map_err(|_| Error::Index)?);
        self.header.frames += 1;
        Ok(())
    }

    fn emit(out: &mut Vec<u8>, run: Run, length: u64) {
        match run {
            Run::Keep => push_token(out, ((length - 1) << 1) | 1),
            Run::Paint(shade) => push_token(out, ((length - 1) << 5) | (u64::from(shade) << 1)),
        }
    }

    /// The frames pushed so far.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.header.frames
    }

    /// The whole file.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        let h = self.header;
        let mut out = Vec::with_capacity(HEADER + 4 * (self.ends.len() + 1) + self.data.len());
        out.extend_from_slice(&MAGIC);
        for half in [h.width, h.height, h.rate_num, h.rate_den] {
            out.extend_from_slice(&half.to_le_bytes());
        }
        out.extend_from_slice(&h.frames.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        for end in &self.ends {
            out.extend_from_slice(&end.to_le_bytes());
        }
        out.extend_from_slice(&self.data);
        out
    }
}

#[cfg(test)]
mod tests;
