//! The regular expressions a window rule matches with.
//!
//! Hyprland matches a window's class and title with RE2 and
//! `RE2::FullMatch`: the whole string must match, not a part of it, which is
//! why every example rule in the wild is written `class:^(foot)$` and why
//! leaving the anchors off changes nothing. This is that, for the part of
//! the syntax a window rule uses:
//!
//! * literals, and `\` before one to take it literally;
//! * `.`, any character;
//! * `[abc]`, `[a-z]`, `[^a-z]`, a class of them, with `\` escapes inside;
//! * `*`, `+`, `?` after any of those, greedy;
//! * `(a|b)`, a group with alternatives, which may nest;
//! * `^` and `$`, which a full match makes redundant and which every rule
//!   has anyway, so they parse and mean nothing.
//!
//! What is not here: counted repetition `{n,m}`, backreferences, lookaround,
//! named groups, character classes such as `\d`, and non-greedy quantifiers.
//! A pattern using one is refused with a sentence rather than matched
//! wrongly: a rule that silently matched everything would float every window
//! a person owns.
//!
//! # Why not a crate
//!
//! The compositor's dependency policy is to take nothing it can write, and
//! the shape above is a few hundred lines with every rule testable. A
//! backtracking matcher is exponential on patterns that nest a quantified
//! alternation; the matcher counts its steps and gives up rather than
//! hanging the compositor, which is the failure a regular expression from a
//! configuration file can have.

#[cfg(test)]
mod tests;

/// How many matching steps a pattern may take before it is given up on.
///
/// A window's class and title are short and a rule's pattern is simple, so a
/// match that has not finished by here is one that never will: a
/// backtracking matcher on `(a*)*b` against forty `a`s would still be going
/// when the sun goes out.
const STEPS: u32 = 200_000;

/// One piece of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    /// One character.
    Literal(char),
    /// Any character: `.`.
    Any,
    /// A class of them: the set, and whether it is negated.
    Class(Vec<Range>, bool),
    /// A group with alternatives.
    Group(Vec<Vec<Piece>>),
    /// An anchor, which a full match makes redundant.
    Anchor,
    /// The piece before it, repeated.
    Repeat {
        /// What is repeated.
        of: Box<Piece>,
        /// The fewest times it may appear.
        least: u32,
        /// The most, or `None` for any number.
        most: Option<u32>,
    },
}

/// One range of a character class, a single character being a range of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Range {
    first: char,
    last: char,
}

/// A compiled pattern.
#[derive(Clone, Debug)]
pub struct Regex {
    pieces: Vec<Piece>,
    /// Whether a match means "does not match": Hyprland's `negative:`
    /// prefix, which its own engine handles and which therefore belongs
    /// here.
    negative: bool,
    /// The pattern as written, for anything that prints it.
    source: String,
}

impl Regex {
    /// Compile `pattern`.
    ///
    /// A `negative:` prefix inverts the answer, as Hyprland's
    /// `CRegexMatchEngine` does with it.
    ///
    /// # Errors
    ///
    /// A sentence saying what in the pattern could not be read.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let (negative, rest) = match pattern.strip_prefix("negative:") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let characters: Vec<char> = rest.chars().collect();
        let mut at = 0;
        let pieces = parse(&characters, &mut at, false)?;
        if at != characters.len() {
            return Err(format!("`{rest}`: a `)` with no `(`"));
        }
        Ok(Self {
            pieces,
            negative,
            source: pattern.to_owned(),
        })
    }

    /// Whether `text` matches the whole pattern, as `RE2::FullMatch` does.
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        let characters: Vec<char> = text.chars().collect();
        let mut steps = STEPS;
        let matched = full(&self.pieces, &characters, 0, &mut steps).is_some();
        matched != self.negative
    }

    /// The pattern as written.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Parse a sequence of pieces, stopping at `)` when `nested`.
fn parse(characters: &[char], at: &mut usize, nested: bool) -> Result<Vec<Piece>, String> {
    let mut pieces: Vec<Piece> = Vec::new();
    while let Some(&character) = characters.get(*at) {
        *at += 1;
        let piece = match character {
            ')' if nested => return Ok(pieces),
            ')' => return Err("a `)` with no `(`".to_owned()),
            '(' => {
                let mut alternatives = Vec::new();
                let mut current = parse(characters, at, true)?;
                // `parse` stopped at `)` or at `|`; a `|` leaves the
                // alternative behind and goes on.
                while characters.get(at.wrapping_sub(1)) == Some(&'|') {
                    alternatives.push(core::mem::take(&mut current));
                    current = parse(characters, at, true)?;
                }
                alternatives.push(current);
                Piece::Group(alternatives)
            }
            '|' if nested => return Ok(pieces),
            // A bare `|` outside a group splits the whole pattern, which is
            // a group around everything.
            '|' => {
                let rest = parse(characters, at, nested)?;
                return Ok(vec![Piece::Group(vec![pieces, rest])]);
            }
            '[' => class(characters, at)?,
            '.' => Piece::Any,
            '^' | '$' => Piece::Anchor,
            '\\' => {
                let escaped = characters
                    .get(*at)
                    .copied()
                    .ok_or_else(|| "a `\\` at the end".to_owned())?;
                *at += 1;
                match escaped {
                    // The classes RE2 has and this does not: refused rather
                    // than read as the letter.
                    'd' | 'D' | 'w' | 'W' | 's' | 'S' | 'b' | 'B' => {
                        return Err(format!("`\\{escaped}` is not done here"));
                    }
                    'n' => Piece::Literal('\n'),
                    't' => Piece::Literal('\t'),
                    other => Piece::Literal(other),
                }
            }
            '{' => return Err("counted repetition `{n,m}` is not done here".to_owned()),
            '*' | '+' | '?' => {
                return Err(format!("`{character}` with nothing before it"));
            }
            other => Piece::Literal(other),
        };
        // A quantifier applies to the piece just read.
        let piece = match characters.get(*at) {
            Some('*') => {
                *at += 1;
                repeat(piece, 0, None)
            }
            Some('+') => {
                *at += 1;
                repeat(piece, 1, None)
            }
            Some('?') => {
                *at += 1;
                repeat(piece, 0, Some(1))
            }
            _ => piece,
        };
        // A second quantifier is either lazy (`*?`) or a mistake.
        if matches!(characters.get(*at), Some('*' | '+' | '?'))
            && matches!(piece, Piece::Repeat { .. })
        {
            return Err("a second quantifier: non-greedy matching is not done here".to_owned());
        }
        pieces.push(piece);
    }
    if nested {
        return Err("a `(` with no `)`".to_owned());
    }
    Ok(pieces)
}

/// Wrap a piece in a repetition, unless it is an anchor.
fn repeat(of: Piece, least: u32, most: Option<u32>) -> Piece {
    Piece::Repeat {
        of: Box::new(of),
        least,
        most,
    }
}

/// Parse a character class, the `[` already read.
fn class(characters: &[char], at: &mut usize) -> Result<Piece, String> {
    let mut ranges = Vec::new();
    let negated = characters.get(*at) == Some(&'^');
    if negated {
        *at += 1;
    }
    // A `]` first is a literal `]`, as every regular expression has it.
    let mut first = true;
    loop {
        let Some(&character) = characters.get(*at) else {
            return Err("a `[` with no `]`".to_owned());
        };
        *at += 1;
        if character == ']' && !first {
            return Ok(Piece::Class(ranges, negated));
        }
        first = false;
        let start = if character == '\\' {
            let escaped = characters
                .get(*at)
                .copied()
                .ok_or_else(|| "a `\\` at the end".to_owned())?;
            *at += 1;
            escaped
        } else {
            character
        };
        // `a-z`, unless the `-` is the last character before the `]`.
        if characters.get(*at) == Some(&'-') && characters.get(*at + 1) != Some(&']') {
            *at += 1;
            let Some(&last) = characters.get(*at) else {
                return Err("a `[` with no `]`".to_owned());
            };
            *at += 1;
            if last < start {
                return Err(format!("`{start}-{last}` is back to front"));
            }
            ranges.push(Range { first: start, last });
        } else {
            ranges.push(Range {
                first: start,
                last: start,
            });
        }
    }
}

/// Whether the whole of `text` from `at` matches `pieces` and ends there.
///
/// Gives the position it reached, which for a full match is the end.
fn full(pieces: &[Piece], text: &[char], at: usize, steps: &mut u32) -> Option<usize> {
    let end = match_here(pieces, text, at, steps)?;
    (end == text.len()).then_some(end)
}

/// Match `pieces` from `at`, giving where the match ended.
///
/// Backtracking: a quantifier takes as much as it can and gives characters
/// back until the rest of the pattern fits, which is what a greedy match is.
fn match_here(pieces: &[Piece], text: &[char], at: usize, steps: &mut u32) -> Option<usize> {
    let Some((piece, rest)) = pieces.split_first() else {
        return Some(at);
    };
    if *steps == 0 {
        return None;
    }
    *steps -= 1;
    match piece {
        Piece::Anchor => match_here(rest, text, at, steps),
        Piece::Repeat { of, least, most } => {
            // Every length this repetition could take, longest first.
            let mut ends = vec![at];
            let mut here = at;
            while most.is_none_or(|most| (ends.len() as u32) <= most) {
                let Some(next) = match_here(core::slice::from_ref(of.as_ref()), text, here, steps)
                else {
                    break;
                };
                // A piece that matched nothing would loop for ever.
                if next == here {
                    break;
                }
                here = next;
                ends.push(here);
            }
            for (taken, end) in ends.iter().enumerate().rev() {
                if (taken as u32) < *least {
                    break;
                }
                if let Some(finished) = match_here(rest, text, *end, steps) {
                    return Some(finished);
                }
            }
            None
        }
        Piece::Group(alternatives) => {
            for alternative in alternatives {
                // The alternative, then the rest of the pattern after it: a
                // group is not a match of its own until what follows fits.
                let mut whole = alternative.clone();
                whole.extend_from_slice(rest);
                if let Some(end) = match_here(&whole, text, at, steps) {
                    return Some(end);
                }
            }
            None
        }
        single => {
            let character = text.get(at).copied()?;
            let matched = match single {
                Piece::Literal(wanted) => character == *wanted,
                Piece::Any => true,
                Piece::Class(ranges, negated) => {
                    let inside = ranges
                        .iter()
                        .any(|range| character >= range.first && character <= range.last);
                    inside != *negated
                }
                // The three above are the only ones left here.
                _ => false,
            };
            if !matched {
                return None;
            }
            match_here(rest, text, at + 1, steps)
        }
    }
}
