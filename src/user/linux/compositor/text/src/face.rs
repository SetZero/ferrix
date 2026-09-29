//! One face in one file, and what can be known of it without shaping.

use std::path::PathBuf;

use crate::{Style, Weight};

/// A face the [`crate::Fonts`] knows, by its place in the list.
///
/// The low 20 bits are that place, the index into [`crate::Fonts::faces`].
/// A face [`crate::Fonts::resolve`] puts in a [`crate::Font`] may carry
/// how it is to be drawn in the bits above: the weight a variable face is
/// instanced at (10 bits, none for its default), synthetic bold (bit 30)
/// and synthetic oblique (bit 31). [`crate::Fonts::info`] reads the face's
/// [`FaceInfo`] through them; [`FaceId::index`] is the place alone.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FaceId(pub u32);

/// The bits of a [`FaceId`] that are its index.
const INDEX_BITS: u32 = 20;
/// Where the instance weight starts.
const WEIGHT_SHIFT: u32 = INDEX_BITS;
/// The instance weight's bits, once shifted down.
const WEIGHT_MASK: u32 = (1 << 10) - 1;
/// Synthetic bold.
const BOLD: u32 = 1 << 30;
/// Synthetic oblique.
const OBLIQUE: u32 = 1 << 31;

/// How one face is to be drawn: what a [`FaceId`] says above its index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Instance {
    /// The face's place in the list.
    pub(crate) index: usize,
    /// The `wght` a variable face is set to, `None` for its default.
    pub(crate) weight: Option<u16>,
    /// Whether its outlines are emboldened.
    pub(crate) bold: bool,
    /// Whether its outlines are slanted.
    pub(crate) oblique: bool,
}

impl FaceId {
    /// Its place in [`crate::Fonts::faces`], without what it says about
    /// instancing and synthesis.
    #[must_use]
    pub fn index(self) -> usize {
        (self.0 & ((1 << INDEX_BITS) - 1)) as usize
    }

    /// What it says.
    pub(crate) fn instance(self) -> Instance {
        let weight = (self.0 >> WEIGHT_SHIFT) & WEIGHT_MASK;
        Instance {
            index: self.index(),
            weight: u16::try_from(weight).ok().filter(|weight| *weight != 0),
            bold: self.0 & BOLD != 0,
            oblique: self.0 & OBLIQUE != 0,
        }
    }

    /// The id of `instance`; `None` for an index past what 20 bits hold.
    pub(crate) fn of(instance: Instance) -> Option<Self> {
        let index = u32::try_from(instance.index)
            .ok()
            .filter(|index| *index < (1 << INDEX_BITS))?;
        let weight = u32::from(instance.weight.unwrap_or(0)).min(WEIGHT_MASK);
        let mut id = index | (weight << WEIGHT_SHIFT);
        if instance.bold {
            id |= BOLD;
        }
        if instance.oblique {
            id |= OBLIQUE;
        }
        Some(Self(id))
    }
}

/// What was read of a face when its directory was scanned: enough to match
/// it, not yet its outlines.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceInfo {
    /// Its family names: the typographic family (name id 16) and the legacy
    /// one (name id 1), in every language the file has, first English.
    /// `"Ubuntu"` for Ubuntu Light, whose legacy family is `"Ubuntu Light"`
    /// -- both match.
    pub families: Vec<String>,
    /// The subfamily as the file names it, `"Light"`, `"Bold Italic"`.
    pub style_name: String,
    /// The full name, `"Ubuntu Light"`.
    pub full_name: String,
    /// The weight, from `OS/2.usWeightClass`.
    pub weight: Weight,
    /// The style, from `OS/2.fsSelection` and the subfamily.
    pub style: Style,
    /// Whether every glyph has one advance (`post.isFixedPitch`).
    pub monospace: bool,
    /// Whether it is a variable font with a `wght` axis, so that one file
    /// answers every weight.
    pub variable_weight: Option<(f32, f32)>,
    /// The file.
    pub path: PathBuf,
    /// Its index in a collection (`.ttc`), `0` otherwise.
    pub index: u32,
}

/// A face's vertical metrics at a size, in pixels: what a line of it is
/// laid out by.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Metrics {
    /// From the baseline up (`hhea.ascender`, or `OS/2.sTypoAscender` where
    /// the face says to use typographic metrics).
    pub ascent: f32,
    /// From the baseline down, as a positive number.
    pub descent: f32,
    /// The gap the face asks for between lines.
    pub line_gap: f32,
    /// A line's height: `ascent + descent + line_gap`, which is
    /// `pango_font_metrics_get_height`. A [`crate::Layout`] does not stack
    /// its lines by it: Pango's logical rectangle, which GTK and hyprlock
    /// size by, is `ascent + descent` a line when no line spacing is set,
    /// and so is a layout's line here.
    pub height: f32,
    /// Where an underline goes, below the baseline, positive down.
    pub underline_position: f32,
    /// How thick.
    pub underline_thickness: f32,
    /// Where a strike-through goes, above the baseline, positive up.
    pub strikeout_position: f32,
    /// How thick.
    pub strikeout_thickness: f32,
    /// The average advance of a character, which is what GTK's
    /// `max-width-chars` counts in (Pango's `approximate_char_width`).
    pub approximate_char_width: f32,
    /// The widest digit's advance (Pango's `approximate_digit_width`).
    pub approximate_digit_width: f32,
}
