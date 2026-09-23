//! The pointer on a cursor plane of its own.
//!
//! Drawn into the frame, the pointer costs a frame every time it moves: the
//! damage round the old place and the new, a flush to the host, and -- on a
//! screen served over VNC -- an update the viewer gets only when the server
//! next looks, which is every 30 ms. A card with a cursor plane shows a small
//! image over the frame and moves it for the asking. virtio-gpu has one: the
//! image goes to the host once, and every move after that is a command on
//! the cursor queue that nobody waits for. A host that serves the screen
//! over VNC hands the image to the viewer, which draws it where its own
//! mouse is, and the pointer a person watching sees has no lag at all
//! (`docs/GPU.md` §3.9).
//!
//! So a screen whose backend has a plane shows the pointer on it, and its
//! frames neither draw the pointer nor count its moves as damage. What does
//! not fit -- a client's cursor larger than the plane -- is drawn into the
//! frame as before, and so is everything on a screen with no plane.
//! Hyprland's `cursor:no_hardware_cursors = 1` asks for that everywhere.

use compositor_render::{Format, Surface};

/// Whether `cursor:no_hardware_cursors` leaves the pointer to a plane:
/// Hyprland's 0 and its default 2 do, where a screen has one, and 1 never
/// does.
#[must_use]
pub const fn wanted(no_hardware_cursors: i64) -> bool {
    no_hardware_cursors != 1
}

/// An image for a plane `size` big: `surface` in its top-left corner and
/// nothing around it, premultiplied `ARGB8888` with rows packed, which is
/// what the plane shows. No surface is an image of nothing, which is how a
/// hidden pointer is hidden: a host that serves the screen keeps the last
/// image it was given, and only a clear one clears it.
///
/// `None` when the surface is larger than the plane: that pointer is the
/// frame's to draw.
#[must_use]
pub fn image(surface: Option<&Surface<'_>>, size: (u32, u32)) -> Option<Vec<u8>> {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let mut image = vec![0u8; width * height * 4];
    let Some(surface) = surface else {
        return Some(image);
    };
    let (wide, tall) = (surface.width() as usize, surface.height() as usize);
    if wide > width || tall > height {
        return None;
    }
    let stride = surface.stride() as usize;
    let opaque = surface.format() == Format::Xrgb8888;
    for row in 0..tall {
        let from = surface.data().get(row * stride..row * stride + wide * 4)?;
        let to = image.get_mut(row * width * 4..(row * width + wide) * 4)?;
        to.copy_from_slice(from);
        if opaque {
            for pixel in to.chunks_exact_mut(4) {
                if let Some(alpha) = pixel.get_mut(3) {
                    *alpha = 0xFF;
                }
            }
        }
    }
    Some(image)
}

/// What a screen's plane is to be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tell {
    /// Nothing: it shows this already.
    Nothing,
    /// The same image, its top-left corner here.
    Move((i32, i32)),
    /// This image with its hotspot here, its top-left corner there.
    Set {
        /// The image, the plane's size.
        image: Vec<u8>,
        /// Its hotspot.
        hot: (i32, i32),
        /// Its top-left corner.
        at: (i32, i32),
    },
}

/// What a screen's plane was last told, and whether the pointer is on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plane {
    /// Whether this screen's pointer is on the plane, rather than drawn into
    /// its frames.
    pub on: bool,
    /// A digest of the image and hotspot last set, so that one unchanged is
    /// not set again: setting an image waits for it to reach the host, and
    /// moving one waits for nothing.
    shown: Option<u64>,
    /// Where its top-left corner last went.
    at: Option<(i32, i32)>,
}

impl Plane {
    /// What to tell the plane so that it shows `image` with its hotspot at
    /// `hot` and its top-left corner at `at`. A clear image is not moved:
    /// there is nothing to see go anywhere.
    pub fn tell(&mut self, image: Vec<u8>, hot: (i32, i32), at: (i32, i32)) -> Tell {
        let digest = digest(&image, hot);
        if self.shown != Some(digest) {
            self.shown = Some(digest);
            self.at = Some(at);
            return Tell::Set { image, hot, at };
        }
        let clear = image.iter().all(|&byte| byte == 0);
        if clear || self.at == Some(at) {
            return Tell::Nothing;
        }
        self.at = Some(at);
        Tell::Move(at)
    }

    /// Forget what the plane was told: the next image is set whatever it
    /// is. For a screen whose card came back, which has seen none of it,
    /// and for one whose last telling failed.
    pub fn forget(&mut self) {
        self.shown = None;
        self.at = None;
    }
}

/// FNV-1a over the image and its hotspot: enough to tell one pointer's
/// image from the next, which is all it is asked.
fn digest(image: &[u8], hot: (i32, i32)) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in image
        .iter()
        .chain(&hot.0.to_le_bytes())
        .chain(&hot.1.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_option_is_hyprlands() {
        assert!(wanted(0));
        assert!(wanted(2), "the default leaves it to the compositor");
        assert!(!wanted(1));
    }

    #[test]
    fn an_image_is_the_surface_in_the_corner_and_nothing_else() {
        // Three by two, padded rows, one pixel translucent.
        let mut pixels = vec![0u8; 16 * 2];
        pixels[..4].copy_from_slice(&[1, 2, 3, 0x80]);
        pixels[16 + 8..16 + 12].copy_from_slice(&[4, 5, 6, 0xFF]);
        let surface = Surface::new(&pixels, 3, 2, 16, Format::Argb8888).expect("a surface");
        let image = image(Some(&surface), (4, 4)).expect("it fits");
        assert_eq!(image.len(), 4 * 4 * 4);
        assert_eq!(image[..4], [1, 2, 3, 0x80]);
        assert_eq!(image[16 + 8..16 + 12], [4, 5, 6, 0xFF]);
        assert_eq!(image.iter().filter(|&&byte| byte != 0).count(), 8);

        // An opaque format is opaque on the plane, whatever its padding
        // byte says.
        let opaque = Surface::new(&pixels, 3, 2, 16, Format::Xrgb8888).expect("a surface");
        let image = super::image(Some(&opaque), (4, 4)).expect("it fits");
        assert_eq!(image[3], 0xFF);
        assert_eq!(image[16 + 11], 0xFF);

        // Nothing is a clear image; too large is the frame's to draw.
        assert!(
            super::image(None, (4, 4))
                .expect("clear")
                .iter()
                .all(|&byte| byte == 0)
        );
        assert_eq!(super::image(Some(&surface), (2, 2)), None);
    }

    #[test]
    fn an_image_is_set_once_and_then_only_moved() {
        let mut plane = Plane::default();
        let arrow = vec![7u8; 16];
        assert!(matches!(
            plane.tell(arrow.clone(), (0, 0), (5, 5)),
            Tell::Set { at: (5, 5), .. }
        ));
        assert_eq!(plane.tell(arrow.clone(), (0, 0), (5, 5)), Tell::Nothing);
        assert_eq!(
            plane.tell(arrow.clone(), (0, 0), (6, 5)),
            Tell::Move((6, 5))
        );
        // A new hotspot is a new image, and so is a clear one -- which is
        // then not moved about.
        assert!(matches!(
            plane.tell(arrow.clone(), (1, 1), (6, 5)),
            Tell::Set { .. }
        ));
        assert!(matches!(
            plane.tell(vec![0; 16], (0, 0), (6, 5)),
            Tell::Set { .. }
        ));
        assert_eq!(plane.tell(vec![0; 16], (0, 0), (9, 9)), Tell::Nothing);
        // A plane that forgot is told again.
        plane.forget();
        assert!(matches!(
            plane.tell(vec![0; 16], (0, 0), (9, 9)),
            Tell::Set { .. }
        ));
    }
}
