//! A screenshot held to an expected image, inside the guest.
//!
//! [`crate::Shot::line`] prints a digest, and a test on the host compares it
//! with the digest of the image it expected: every pixel, through a serial
//! port, in sixteen characters. That is the judgement for a frame the
//! software renderer drew, which is the expected image byte for byte.
//!
//! A frame a GPU drew is the same picture and not the same bytes -- a step
//! or two of a channel here and there, which `compositor/render`'s `gpu`
//! module explains -- and no digest says "nearly". So the comparison moves
//! to where the pixels are: the expected image is put on the guest's own
//! filesystem, `shot` reads it, and what crosses the serial port is the
//! verdict. It matters more here than it would elsewhere, because a guest
//! with a GPU behind its card is one QEMU's `screendump` cannot read
//! (`docs/GPU.md` §3.1): this is the only judge such a frame has.
//!
//! The format is `compositor/render`'s `golden`: read again here, without
//! its panics, because that reader is a test's and this is a program's.

use crate::Shot;

/// The file's first bytes.
const MAGIC: &[u8; 16] = b"ferrix-xrgb-rle\n";

/// How a screenshot compares with an expected image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// How many channels are more than the step apart.
    pub apart: usize,
    /// How far apart the furthest channel is.
    pub furthest: u8,
}

/// The image in `bytes` as a PPM holds one: its size, and red, green and
/// blue a pixel in row order.
///
/// # Errors
///
/// A sentence saying what is wrong with the file.
pub fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let rest = bytes
        .strip_prefix(MAGIC)
        .ok_or_else(|| "not an expected image: the magic is wrong".to_owned())?;
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(rest.get(at..at + 4)?.try_into().ok()?))
    };
    let (Some(width), Some(height)) = (word(0), word(4)) else {
        return Err("an expected image with no size".to_owned());
    };
    let short = || "an expected image that stops early".to_owned();
    let mut at = 8;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for _ in 0..height {
        let start = pixels.len();
        match rest.get(at).ok_or_else(short)? {
            // The row above's.
            0 => {
                let above = start
                    .checked_sub(width as usize * 3)
                    .ok_or_else(|| "a first row that repeats the one above it".to_owned())?;
                pixels.extend_from_within(above..start);
                at += 1;
            }
            _ => {
                at += 1;
                while pixels.len() - start < width as usize * 3 {
                    let run = rest.get(at..at + 6).ok_or_else(short)?;
                    let (Some(count), Some(pixel)) = (run.get(..2), run.get(2..6)) else {
                        return Err(short());
                    };
                    let count = u16::from_le_bytes(count.try_into().map_err(|_| short())?);
                    // Blue, green, red, X in the file; red, green, blue here.
                    let [blue, green, red, _] = <[u8; 4]>::try_from(pixel).map_err(|_| short())?;
                    for _ in 0..count {
                        pixels.extend_from_slice(&[red, green, blue]);
                    }
                    at += 6;
                }
            }
        }
    }
    Ok((width, height, pixels))
}

/// How `shot` compares with the expected image in `bytes`, counting a
/// channel as apart when it differs by more than `step`.
///
/// # Errors
///
/// A sentence saying why the two cannot be compared: an image that cannot be
/// read, or one of another size.
pub fn against(shot: &Shot, bytes: &[u8], step: u8) -> Result<Verdict, String> {
    let (width, height, want) = decode(bytes)?;
    if (width, height) != (shot.width, shot.height) || want.len() != shot.pixels.len() {
        return Err(format!(
            "the screenshot is {}x{} and the expected image {width}x{height}",
            shot.width, shot.height
        ));
    }
    let mut verdict = Verdict {
        apart: 0,
        furthest: 0,
    };
    for (mine, theirs) in shot.pixels.iter().zip(&want) {
        let apart = mine.abs_diff(*theirs);
        verdict.furthest = verdict.furthest.max(apart);
        if apart > step {
            verdict.apart += 1;
        }
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rows of three pixels: a run of two and one alone, then the same
    /// row again.
    fn image() -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(3_u32.to_le_bytes());
        bytes.extend(2_u32.to_le_bytes());
        bytes.push(1);
        bytes.extend(2_u16.to_le_bytes());
        bytes.extend(0xff10_2030_u32.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(0xff40_5060_u32.to_le_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn an_image_decodes_to_rows_of_red_green_and_blue() {
        let (width, height, pixels) = decode(&image()).expect("it decodes");
        assert_eq!((width, height), (3, 2));
        let row = [0x10, 0x20, 0x30, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        assert_eq!(pixels, [row, row].concat());
    }

    #[test]
    fn a_step_is_allowed_and_more_is_counted() {
        let (width, height, mut pixels) = decode(&image()).expect("it decodes");
        pixels[0] += 2;
        pixels[4] -= 9;
        let shot = Shot {
            width,
            height,
            pixels,
        };
        assert_eq!(
            against(&shot, &image(), 3),
            Ok(Verdict {
                apart: 1,
                furthest: 9
            })
        );
    }

    #[test]
    fn what_cannot_be_compared_says_so() {
        let shot = Shot {
            width: 2,
            height: 2,
            pixels: vec![0; 12],
        };
        assert!(against(&shot, &image(), 3).is_err(), "another size");
        assert!(against(&shot, b"not an image", 3).is_err());
        let mut short = image();
        short.truncate(30);
        assert!(decode(&short).is_err());
        // A first row cannot be the row above's.
        let mut first = MAGIC.to_vec();
        first.extend(1_u32.to_le_bytes());
        first.extend(1_u32.to_le_bytes());
        first.push(0);
        assert!(decode(&first).is_err());
    }
}
