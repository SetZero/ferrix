//! The mark, in text.
//!
//! `docs/brand/logo-mark.svg`: a cube of iron's body-centred lattice seen
//! isometrically -- six atoms on the outline, the edges from three of them
//! meeting at the centre -- with a molten core where the centre atom is.
//! Drawn here in ASCII, because a terminal's font may have nothing else.

/// What a character of the mark is, which is what colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// An edge of the lattice: the brand's muted grey.
    Lattice,
    /// An atom at a corner: the lighter grey the SVG draws them in.
    Atom,
    /// The core: rust.
    Core,
    /// Nothing: a space.
    Space,
}

/// The mark, a row a line.
pub const ROWS: [&str; 13] = [
    "            o",
    "        _.-' '-._",
    "    _.-'         '-._",
    "  o'-._           _.-'o",
    "  |    '-._   _.-'    |",
    "  |        (@)        |",
    "  |         |         |",
    "  |         |         |",
    "  |         |         |",
    "  o-._      |      _.-o",
    "      '-._  |  _.-'",
    "          '-|-'",
    "            o",
];

/// The widest row, in columns: every character is one. The tests hold it to
/// [`ROWS`].
pub const WIDTH: usize = 23;

/// What `character` of the mark is.
#[must_use]
pub const fn part(character: u8) -> Part {
    match character {
        b' ' => Part::Space,
        b'o' => Part::Atom,
        b'(' | b'@' | b')' => Part::Core,
        _ => Part::Lattice,
    }
}
