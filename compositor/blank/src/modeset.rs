//! The choices a modeset makes and the pixels it draws, apart from the card,
//! so the host can test them.

use ferrix_linux_abi::drm::{self, ModeInfo};

/// The colour the screen is filled with, as `XRGB8888`: a dark slate, so a
/// screen that is merely black is not mistaken for success.
pub(crate) const BACKGROUND: u32 = 0x0000_00FF;

/// The colour the negative control draws its first pixel in.
pub(crate) const NEGATIVE: u32 = 0x00FF_00FF;

/// The mode to set from a connector's list: the preferred one, else the
/// first.
pub(crate) fn choose_mode(modes: &[ModeInfo]) -> Option<ModeInfo> {
    modes
        .iter()
        .find(|mode| mode.r#type & drm::MODE_TYPE_PREFERRED != 0)
        .or_else(|| modes.first())
        .copied()
}

/// The CRTC an encoder can drive: the one it is on, else the first its
/// `possible_crtcs` bit mask allows among `crtcs`.
pub(crate) fn choose_crtc(current: u32, possible: u32, crtcs: &[u32]) -> Option<u32> {
    if current != 0 {
        return Some(current);
    }
    crtcs
        .iter()
        .enumerate()
        .find(|&(index, _)| index < 32 && possible & (1 << index) != 0)
        .map(|(_, &crtc)| crtc)
}

/// Fill a `width` × `height` `XRGB8888` buffer of `stride` bytes a row with
/// `color`, and with `negative`, draw pixel (0, 0) in [`NEGATIVE`].
pub(crate) fn fill(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    color: u32,
    negative: bool,
) {
    let bytes = color.to_le_bytes();
    for row in pixels.chunks_exact_mut(stride).take(height) {
        for pixel in row.chunks_exact_mut(4).take(width) {
            pixel.copy_from_slice(&bytes);
        }
    }
    if negative && let Some(first) = pixels.get_mut(..4) {
        first.copy_from_slice(&NEGATIVE.to_le_bytes());
    }
}

/// A mode's name, up to its NUL.
pub(crate) fn mode_name(mode: &ModeInfo) -> String {
    let end = mode
        .name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(mode.name.len());
    String::from_utf8_lossy(mode.name.get(..end).unwrap_or(&[])).into_owned()
}

#[cfg(test)]
mod tests {
    use ferrix_linux_abi::drm::{Field, ModeInfo};

    use super::*;

    fn mode(width: u16, height: u16, preferred: bool) -> ModeInfo {
        let mut mode = ModeInfo::ZERO;
        mode.hdisplay = width;
        mode.vdisplay = height;
        if preferred {
            mode.r#type = drm::MODE_TYPE_PREFERRED | drm::MODE_TYPE_DRIVER;
        }
        mode.name[..8].copy_from_slice(b"1280x800");
        mode
    }

    #[test]
    fn the_preferred_mode_wins_and_otherwise_the_first() {
        let modes = [
            mode(1024, 768, false),
            mode(1280, 800, true),
            mode(800, 600, false),
        ];
        assert_eq!(choose_mode(&modes).map(|m| m.hdisplay), Some(1280));
        assert_eq!(choose_mode(&modes[..1]).map(|m| m.hdisplay), Some(1024));
        assert_eq!(choose_mode(&[]), None);
        assert_eq!(mode_name(&modes[1]), "1280x800");
    }

    #[test]
    fn the_crtc_is_the_current_one_or_the_first_possible() {
        assert_eq!(choose_crtc(41, 0b10, &[40, 41]), Some(41));
        assert_eq!(choose_crtc(0, 0b10, &[40, 41]), Some(41));
        assert_eq!(choose_crtc(0, 0b01, &[40, 41]), Some(40));
        assert_eq!(choose_crtc(0, 0b100, &[40, 41]), None);
    }

    #[test]
    fn fill_covers_every_visible_pixel_and_no_padding() {
        // 3 × 2 pixels in rows of 16 bytes: one pixel of padding a row.
        let mut pixels = vec![0xAAu8; 32];
        fill(&mut pixels, 3, 2, 16, BACKGROUND, false);
        for row in pixels.chunks(16) {
            for pixel in row[..12].chunks(4) {
                assert_eq!(pixel, BACKGROUND.to_le_bytes());
            }
            assert_eq!(row[12..], [0xAA; 4], "padding untouched");
        }

        fill(&mut pixels, 3, 2, 16, BACKGROUND, true);
        assert_eq!(pixels[..4], NEGATIVE.to_le_bytes());
        assert_eq!(pixels[4..8], BACKGROUND.to_le_bytes());
    }
}
