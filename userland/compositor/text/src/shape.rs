//! A resolved font and a shaped run of it.

use crate::FaceId;

/// A font resolved against what is installed: the faces to take glyphs
/// from, in order, at one pixel size. Made by [`crate::Fonts::resolve`].
#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// The faces, best first: the asked-for family at the nearest weight
    /// and style, then each later family, then the generic fallbacks.
    pub faces: Vec<FaceId>,
    /// The size in pixels.
    pub px: f32,
    /// The weight asked for, which a variable face is instanced at and a
    /// face lighter than asked is emboldened towards (Pango's synthetic
    /// bold, which is what `<b>` on a face with no bold gives upstream).
    pub weight: crate::Weight,
    /// The style asked for, which a face with no italic is slanted towards.
    pub style: crate::Style,
}

impl Font {
    /// The face glyphs come from first.
    #[must_use]
    pub fn primary(&self) -> Option<FaceId> {
        self.faces.first().copied()
    }
}

/// One glyph placed in a [`Run`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    /// The face it is from.
    pub face: FaceId,
    /// Its id in that face.
    pub id: u16,
    /// Where its origin is, from the run's start along the baseline, in
    /// pixels, kerning included.
    pub x: f32,
    /// How far above the baseline its origin is moved (a mark), in pixels.
    pub y: f32,
    /// How far it moves the pen.
    pub advance: f32,
    /// The byte offset in the text of the character it came from.
    pub cluster: usize,
}

/// A line of text shaped in one font: its glyphs and its size.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Run {
    /// The glyphs, in visual order.
    pub glyphs: Vec<Glyph>,
    /// The advance of the whole run: its logical width.
    pub width: f32,
    /// The run's ascent: the largest of its faces'.
    pub ascent: f32,
    /// The run's descent, positive.
    pub descent: f32,
    /// The size it was shaped at.
    pub px: f32,
}
